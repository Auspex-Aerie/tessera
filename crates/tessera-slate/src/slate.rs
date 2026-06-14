//! The Slate snapshot primitive: a flat, seqlock-protected table of
//! latest-value byte slots.
//!
//! A writer overwrites slot `index` in place; a reader polls slot `index`
//! and converges to the latest coherent bytes. There is no history. One
//! writer per slot is the protocol (distinct slots may be written from
//! distinct threads / processes concurrently); readers are lock-free and
//! torn-read-tolerant via the per-slot seqlock with bounded retry.
//!
//! ### Independence note
//!
//! The seqlock here is implemented against Slate's fixed-table layout. It
//! intentionally does NOT share code with `tessera-ring`'s ring-buffer
//! seqlock: Tessera primitives do not depend on one another (only layer-2
//! services like Sink compose primitives). The duplication is deliberate —
//! a shared seqlock crate would be a public surface coupling two
//! primitives, so do not "DRY" the two together.

use std::sync::atomic::{fence, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Result, TesseraSlateError};
use crate::header::READ_RETRIES;
use crate::namespace::NamespaceHandle;
use crate::region::Region;

/// Construction parameters for a writer-side [`Slate`].
#[derive(Clone, Debug)]
pub struct SlateConfig {
    /// Human-readable namespace; BLAKE3-derived into the SHM name.
    pub description: String,
    /// Number of slots in the table.
    pub slot_count: u32,
    /// Per-slot payload capacity in bytes (must be a multiple of 8).
    pub slot_size_bytes: u32,
    /// Caller-defined layout hash; attachers and readers must supply the
    /// same value or attach fails (drift guard). Use `0` for "no schema".
    pub schema_hash: u64,
    /// Create the region (owner) vs attach to an existing one.
    pub is_owner: bool,
    /// Owner-side recovery escape hatch: unconditionally unlink an
    /// existing region of the same name first. Ignored on attach.
    pub force_recreate: bool,
}

/// Outcome of [`SlateReader::read_slot`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadResult {
    /// The slot has never been written (`sequence == 0`).
    Empty,
    /// Every retry collided with a concurrent write; the caller should
    /// keep its previous value for this slot and poll again later.
    Torn,
    /// A coherent snapshot of the slot.
    Slot {
        /// Payload bytes, of the length the writer wrote.
        bytes: Vec<u8>,
        /// The (even) seqlock value the bytes were read at.
        sequence: u64,
        /// Wall-clock nanoseconds of the write.
        timestamp_nanos: u64,
    },
}

/// Snapshot of the region-global header counters.
#[derive(Copy, Clone, Debug)]
pub struct HeaderSnapshot {
    /// Monotonic count of writes across all slots since creation.
    pub writer_seq: u64,
    /// Wall-clock nanoseconds of the most recent write to any slot.
    pub last_update_ns: u64,
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Writer handle over a Slate region.
///
/// `open` with `is_owner=true` creates the region; `is_owner=false`
/// attaches to an existing one (e.g. a worker process writing its own
/// slots). Clone shares the same mapping.
#[derive(Clone, Debug)]
pub struct Slate {
    region: Arc<Region>,
}

impl Slate {
    /// Open a Slate as owner (create) or attacher (attach), per
    /// `config.is_owner`.
    pub fn open(config: SlateConfig) -> Result<Self> {
        let handle = NamespaceHandle::derive(&config.description);
        let region = if config.is_owner {
            Region::create(
                &handle,
                config.slot_count,
                config.slot_size_bytes,
                config.schema_hash,
                config.force_recreate,
            )?
        } else {
            Region::attach(
                &handle,
                config.slot_count,
                config.slot_size_bytes,
                config.schema_hash,
            )?
        };
        Ok(Self {
            region: Arc::new(region),
        })
    }

    /// True if this handle created the region.
    pub fn is_owner(&self) -> bool {
        self.region.is_owner()
    }

    /// Configured slot count.
    pub fn slot_count(&self) -> u32 {
        self.region.slot_count()
    }

    /// Configured per-slot payload capacity in bytes.
    pub fn slot_size_bytes(&self) -> u32 {
        self.region.slot_size_bytes()
    }

    /// Overwrite slot `index` with `bytes` (≤ `slot_size_bytes`).
    ///
    /// One writer per slot: distinct slots may be written concurrently
    /// from distinct threads / processes, but two writers on the *same*
    /// slot is a protocol violation (debug-asserted). On success, the
    /// global `writer_seq` and `last_update_ns` counters advance.
    pub fn write_slot(&self, index: u32, bytes: &[u8]) -> Result<()> {
        let cap = self.region.slot_size_bytes() as usize;
        if bytes.len() > cap {
            return Err(TesseraSlateError::OversizedPayload {
                len: bytes.len(),
                capacity: cap,
            });
        }
        // Resolve (and bounds-check) the slot before touching the seqlock,
        // so a bad index never leaves a slot half-written / odd.
        let seq = self.region.slot_sequence_atomic(index)?;
        let ts = now_ns();

        let s = seq.load(Ordering::Relaxed);
        debug_assert_eq!(s % 2, 0, "two writers on slot {index} (one-writer-per-slot violated)");
        seq.store(s.wrapping_add(1), Ordering::Relaxed); // odd: write in progress
        fence(Ordering::Release);

        // SAFETY: we hold the slot's seqlock-odd state and bytes.len() <=
        // cap; readers that overlap this window are rejected by their
        // before/after sequence check. A reader only ever copies `length`
        // bytes, so leaving bytes beyond `length` stale is harmless.
        unsafe {
            let dst = self.region.slot_payload_ptr_mut(index)?;
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
            self.region.write_slot_meta(index, bytes.len() as u32, ts)?;
        }

        fence(Ordering::Release);
        seq.store(s.wrapping_add(2), Ordering::Release); // even: published

        self.region.writer_seq_atomic().fetch_add(1, Ordering::AcqRel);
        self.region.last_update_ns_atomic().store(ts, Ordering::Release);
        Ok(())
    }

    /// Issue an in-process reader sharing this handle's mapping.
    pub fn reader(&self) -> SlateReader {
        SlateReader {
            region: Arc::clone(&self.region),
        }
    }

    /// Unlink the SHM name (owner only).
    ///
    /// Requires this to be the sole live handle to the region (no other
    /// `Slate`/`SlateReader` clones outstanding); otherwise returns an
    /// error. Dropping the owning handle also unlinks, so explicit unlink
    /// is only needed for deterministic, early cleanup.
    pub fn unlink(&mut self) -> Result<()> {
        Arc::get_mut(&mut self.region)
            .ok_or_else(|| {
                TesseraSlateError::Region(
                    "cannot unlink while other handles to this Slate region are alive".into(),
                )
            })?
            .unlink()
    }
}

/// Read-only handle over a Slate region (the polling / display side).
#[derive(Clone, Debug)]
pub struct SlateReader {
    region: Arc<Region>,
}

impl SlateReader {
    /// Attach to an existing Slate region for reading. `slot_count`,
    /// `slot_size_bytes`, and `schema_hash` must match the creator's or
    /// the attach is rejected.
    pub fn open(
        description: &str,
        slot_count: u32,
        slot_size_bytes: u32,
        schema_hash: u64,
    ) -> Result<Self> {
        let handle = NamespaceHandle::derive(description);
        let region = Region::attach(&handle, slot_count, slot_size_bytes, schema_hash)?;
        Ok(Self {
            region: Arc::new(region),
        })
    }

    /// Configured slot count.
    pub fn slot_count(&self) -> u32 {
        self.region.slot_count()
    }

    /// Configured per-slot payload capacity in bytes.
    pub fn slot_size_bytes(&self) -> u32 {
        self.region.slot_size_bytes()
    }

    /// Region-global counters: total writes so far and the last write time.
    pub fn header(&self) -> HeaderSnapshot {
        HeaderSnapshot {
            writer_seq: self.region.writer_seq_atomic().load(Ordering::Acquire),
            last_update_ns: self.region.last_update_ns_atomic().load(Ordering::Acquire),
        }
    }

    /// Read the latest coherent snapshot of slot `index` with bounded
    /// seqlock retry.
    ///
    /// Returns [`ReadResult::Empty`] if the slot has never been written,
    /// [`ReadResult::Torn`] if every retry collided with a writer (keep
    /// the previous value and poll again), or [`ReadResult::Slot`] with a
    /// coherent copy.
    pub fn read_slot(&self, index: u32) -> Result<ReadResult> {
        let seq = self.region.slot_sequence_atomic(index)?;
        let cap = self.region.slot_size_bytes() as usize;

        for _ in 0..READ_RETRIES {
            let s1 = seq.load(Ordering::Acquire);
            if s1 == 0 {
                return Ok(ReadResult::Empty);
            }
            if s1 % 2 == 1 {
                continue; // writer mid-flight; counts as an attempt
            }
            // SAFETY: the payload is racy by design; the before/after
            // sequence check below rejects any copy that overlapped a
            // write. `length` is clamped to capacity so a mid-update length
            // can never make the copy over-read the slot.
            let (length, ts) = unsafe { self.region.read_slot_meta(index)? };
            let n = (length as usize).min(cap);
            let mut buf = vec![0u8; n];
            unsafe {
                let src = self.region.slot_payload_ptr(index)?;
                core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), n);
            }
            fence(Ordering::Acquire);
            let s2 = seq.load(Ordering::Relaxed);
            if s1 == s2 {
                return Ok(ReadResult::Slot {
                    bytes: buf,
                    sequence: s1,
                    timestamp_nanos: ts,
                });
            }
        }
        Ok(ReadResult::Torn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn unique_desc(tag: &str) -> String {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("tessera-slate-test/{tag}/{pid}/{nanos}")
    }

    fn owner(desc: &str, slot_count: u32, slot_size_bytes: u32) -> Slate {
        Slate::open(SlateConfig {
            description: desc.to_string(),
            slot_count,
            slot_size_bytes,
            schema_hash: 0,
            is_owner: true,
            force_recreate: false,
        })
        .unwrap()
    }

    #[test]
    fn empty_until_first_write() {
        let desc = unique_desc("empty");
        let w = owner(&desc, 4, 64);
        let r = w.reader();
        assert_eq!(r.read_slot(0).unwrap(), ReadResult::Empty);
        assert_eq!(r.header().writer_seq, 0);
    }

    #[test]
    fn write_then_read_round_trip() {
        let desc = unique_desc("roundtrip");
        let w = owner(&desc, 4, 64);
        w.write_slot(1, b"hello slate").unwrap();
        let r = w.reader();
        match r.read_slot(1).unwrap() {
            ReadResult::Slot {
                bytes,
                sequence,
                timestamp_nanos,
            } => {
                assert_eq!(bytes, b"hello slate");
                assert_eq!(sequence, 2);
                assert!(timestamp_nanos > 0);
            }
            other => panic!("expected Slot, got {other:?}"),
        }
        assert_eq!(r.read_slot(0).unwrap(), ReadResult::Empty);
        assert_eq!(r.header().writer_seq, 1);
    }

    #[test]
    fn overwrite_returns_latest_only() {
        let desc = unique_desc("overwrite");
        let w = owner(&desc, 2, 64);
        w.write_slot(0, b"first").unwrap();
        w.write_slot(0, b"second value").unwrap();
        match w.reader().read_slot(0).unwrap() {
            ReadResult::Slot { bytes, sequence, .. } => {
                assert_eq!(bytes, b"second value");
                assert_eq!(sequence, 4);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn shorter_rewrite_leaves_no_stale_tail() {
        let desc = unique_desc("lengths");
        let w = owner(&desc, 1, 64);
        let r = w.reader();
        w.write_slot(0, b"running-long-value").unwrap();
        w.write_slot(0, b"ok").unwrap();
        match r.read_slot(0).unwrap() {
            ReadResult::Slot { bytes, .. } => assert_eq!(bytes, b"ok"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn oversized_payload_rejected_without_half_write() {
        let desc = unique_desc("oversize");
        let w = owner(&desc, 1, 8);
        let err = w.write_slot(0, b"this is way too long").unwrap_err();
        assert!(matches!(err, TesseraSlateError::OversizedPayload { .. }));
        // The seqlock was never taken: the slot still reads Empty.
        assert_eq!(w.reader().read_slot(0).unwrap(), ReadResult::Empty);
    }

    #[test]
    fn slot_index_out_of_range() {
        let desc = unique_desc("oob");
        let w = owner(&desc, 2, 8);
        assert!(matches!(
            w.write_slot(2, b"x").unwrap_err(),
            TesseraSlateError::SlotIndexOutOfRange { index: 2, slot_count: 2 }
        ));
        assert!(matches!(
            w.reader().read_slot(9).unwrap_err(),
            TesseraSlateError::SlotIndexOutOfRange { index: 9, slot_count: 2 }
        ));
    }

    #[test]
    fn refuses_to_clobber_without_force() {
        let desc = unique_desc("clobber");
        let _w = owner(&desc, 2, 8);
        let again = Slate::open(SlateConfig {
            description: desc.clone(),
            slot_count: 2,
            slot_size_bytes: 8,
            schema_hash: 0,
            is_owner: true,
            force_recreate: false,
        });
        assert!(matches!(again, Err(TesseraSlateError::Region(_))));
        // force_recreate recovers.
        let _forced = Slate::open(SlateConfig {
            description: desc,
            slot_count: 2,
            slot_size_bytes: 8,
            schema_hash: 0,
            is_owner: true,
            force_recreate: true,
        })
        .unwrap();
    }

    #[test]
    fn attach_verifies_geometry() {
        // Creator larger than the attacher's claim, so the bounds-safety
        // check (mapped region must be >= the caller's expected size) passes
        // and the semantic geometry check is what fires. (A claim LARGER
        // than the creator trips the bounds check first — also a rejection,
        // but a Region error, not GeometryMismatch.)
        let desc = unique_desc("geometry");
        let _w = owner(&desc, 8, 64);
        let err = SlateReader::open(&desc, 4, 64, 0).unwrap_err();
        assert!(matches!(err, TesseraSlateError::GeometryMismatch { .. }));
    }

    #[test]
    fn attach_verifies_schema_hash() {
        let desc = unique_desc("schemahash");
        let _w = Slate::open(SlateConfig {
            description: desc.clone(),
            slot_count: 4,
            slot_size_bytes: 64,
            schema_hash: 0xABCD,
            is_owner: true,
            force_recreate: false,
        })
        .unwrap();
        // Same hash attaches fine.
        let _ok = SlateReader::open(&desc, 4, 64, 0xABCD).unwrap();
        // A drifted layout hash is refused.
        let err = SlateReader::open(&desc, 4, 64, 0x1234).unwrap_err();
        assert!(matches!(
            err,
            TesseraSlateError::SchemaHashMismatch {
                expected: 0x1234,
                found: 0xABCD
            }
        ));
    }

    #[test]
    fn second_writer_owns_its_own_slot() {
        let desc = unique_desc("twowriters");
        let owner_h = owner(&desc, 4, 64);
        let worker = Slate::open(SlateConfig {
            description: desc.clone(),
            slot_count: 4,
            slot_size_bytes: 64,
            schema_hash: 0,
            is_owner: false,
            force_recreate: false,
        })
        .unwrap();
        owner_h.write_slot(0, b"owner").unwrap();
        worker.write_slot(1, b"worker").unwrap();

        let r = owner_h.reader();
        assert!(matches!(r.read_slot(0).unwrap(), ReadResult::Slot { bytes, .. } if bytes == b"owner"));
        assert!(matches!(r.read_slot(1).unwrap(), ReadResult::Slot { bytes, .. } if bytes == b"worker"));
        assert_eq!(r.header().writer_seq, 2);
    }

    #[test]
    fn forced_odd_reports_torn() {
        let desc = unique_desc("torn");
        let w = owner(&desc, 1, 64);
        w.write_slot(0, b"clean").unwrap();
        // Simulate a writer dying mid-write: force the sequence odd.
        w.region.slot_sequence_atomic(0).unwrap().store(3, Ordering::Release);
        assert_eq!(w.reader().read_slot(0).unwrap(), ReadResult::Torn);
    }

    #[test]
    fn hammer_reader_never_sees_torn_state() {
        // One writer thread rewrites slot 0 with an internally consistent
        // 16-byte payload (second u64 == 2 * first); a reader on a SEPARATE
        // mapping of the same SHM segment polls concurrently. Every clean
        // read must satisfy the invariant — a single torn read breaks it.
        let desc = unique_desc("hammer");
        let writer = owner(&desc, 1, 16);
        let reader = SlateReader::open(&desc, 1, 16, 0).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let wstop = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let mut n: u64 = 0;
            while !wstop.load(Ordering::Relaxed) {
                n += 1;
                let mut buf = [0u8; 16];
                buf[..8].copy_from_slice(&n.to_le_bytes());
                buf[8..].copy_from_slice(&(n.wrapping_mul(2)).to_le_bytes());
                writer.write_slot(0, &buf).unwrap();
                std::thread::yield_now();
            }
            n
        });

        let mut clean: u64 = 0;
        for _ in 0..200_000 {
            match reader.read_slot(0).unwrap() {
                ReadResult::Slot { bytes, .. } => {
                    clean += 1;
                    let a = u64::from_le_bytes(bytes[..8].try_into().unwrap());
                    let b = u64::from_le_bytes(bytes[8..].try_into().unwrap());
                    assert_eq!(b, a.wrapping_mul(2), "torn read: {b} != 2*{a}");
                }
                ReadResult::Torn => {}
                ReadResult::Empty => {} // before the first write lands
            }
        }
        stop.store(true, Ordering::Relaxed);
        let writes = handle.join().unwrap();
        assert!(clean > 0, "reader never got a clean snapshot");
        assert!(writes > 0);

        // Convergence (the snapshot guarantee): with the writer stopped,
        // the next read is a clean snapshot of the final write.
        match reader.read_slot(0).unwrap() {
            ReadResult::Slot { bytes, .. } => {
                let a = u64::from_le_bytes(bytes[..8].try_into().unwrap());
                assert_eq!(a, writes);
            }
            other => panic!("expected clean final snapshot, got {other:?}"),
        }
    }
}
