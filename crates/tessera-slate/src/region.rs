//! SHM region lifecycle for Tessera Slate: create / attach / unlink, plus
//! the accessors the seqlock layer in `crate::slate` builds on.
//!
//! Layout is per `crate::header`: a `GlobalHeader` at offset 0, then a
//! dense array of `slot_count` slots, each a `SlotHeader` followed by
//! `slot_size_bytes` of opaque payload. This module owns the raw mapped
//! bytes; all `unsafe` byte-slice → typed-pointer reinterpretation lives
//! here.

use std::sync::atomic::AtomicU64;
use std::time::{SystemTime, UNIX_EPOCH};

use bytemuck::Zeroable;
use shared_memory::{Shmem, ShmemConf, ShmemError};

use crate::error::{Result, TesseraSlateError};
use crate::header::{
    region_size_bytes, slot_offset, GlobalHeader, SlotHeader, FORMAT_VERSION,
    LAST_UPDATE_NS_OFFSET, MAGIC, SLOT_LENGTH_OFFSET, SLOT_SEQUENCE_OFFSET, SLOT_TIMESTAMP_OFFSET,
    WRITER_SEQ_OFFSET,
};
use crate::namespace::NamespaceHandle;

/// One mapped Tessera Slate region. Owns the `Shmem` handle so the region
/// stays mapped until this struct is dropped.
pub struct Region {
    shmem: Shmem,
    slot_count: u32,
    slot_size_bytes: u32,
    schema_hash: u64,
    shm_name: String,
    is_owner: bool,
    manually_unlinked: bool,
}

// SAFETY: same justification as the other Tessera primitives. `Shmem`
// holds a raw pointer into a process-global mapping valid from any
// thread. Slate's slot accessors use the per-slot seqlock + atomic
// counters in `crate::slate`, correct under one writer per slot and many
// readers. Drop is a thread-agnostic munmap / shm_unlink.
unsafe impl Send for Region {}
unsafe impl Sync for Region {}

impl core::fmt::Debug for Region {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Region")
            .field("slot_count", &self.slot_count)
            .field("slot_size_bytes", &self.slot_size_bytes)
            .field("is_owner", &self.is_owner)
            .field("len", &self.shmem.len())
            .finish()
    }
}

impl Region {
    /// Owner path: create a fresh region and stamp the global header.
    ///
    /// If the SHM name already exists and `force_recreate` is false, this
    /// refuses to clobber it (another owner may be alive, or mid-init).
    /// With `force_recreate` the caller asserts no live owner; the stale
    /// segment is unlinked and recreated.
    pub fn create(
        handle: &NamespaceHandle,
        slot_count: u32,
        slot_size_bytes: u32,
        schema_hash: u64,
        force_recreate: bool,
    ) -> Result<Self> {
        validate_geometry(slot_count, slot_size_bytes)?;
        let size = region_size_bytes(slot_count, slot_size_bytes).ok_or_else(|| {
            TesseraSlateError::Config(format!(
                "region size overflow (slot_count {slot_count} * (header + slot_size_bytes \
                {slot_size_bytes}) exceeds usize::MAX)"
            ))
        })?;
        let name = handle.shm_name();

        let shmem = match ShmemConf::new().size(size).os_id(&name).create() {
            Ok(shmem) => shmem,
            Err(ShmemError::LinkExists) | Err(ShmemError::MappingIdExists) => {
                if force_recreate {
                    // Operator-asserted recovery: no live owner. Unlink +
                    // recreate unconditionally; we do NOT attach-validate
                    // first (that would re-introduce a startup-race where a
                    // fresh segment mid-init looks "invalid").
                    let _ = unlink_named_region(&name);
                    ShmemConf::new()
                        .size(size)
                        .os_id(&name)
                        .create()
                        .map_err(|e| {
                            TesseraSlateError::Region(format!(
                                "create after force_recreate unlink: {e}"
                            ))
                        })?
                } else {
                    return Err(TesseraSlateError::Region(format!(
                        "Slate region '{name}' already exists. Refusing to clobber. Either \
                        another owner is alive (do not create a second), or a prior owner \
                        crashed without unlinking; for recovery, retry with \
                        force_recreate=true only after confirming no live owner exists."
                    )));
                }
            }
            Err(e) => return Err(TesseraSlateError::Region(format!("create: {e}"))),
        };

        let mut region = Region {
            shmem,
            slot_count,
            slot_size_bytes,
            schema_hash,
            shm_name: name,
            is_owner: true,
            manually_unlinked: false,
        };
        region.write_global_header(handle);
        // The slot array is born zeroed (Shmem create zeroes on Linux):
        // every slot's sequence is 0 (never written). Nothing else to init.
        Ok(region)
    }

    /// Non-owner path: attach to an existing region and validate its
    /// header against the caller's geometry + schema hash.
    pub fn attach(
        handle: &NamespaceHandle,
        slot_count: u32,
        slot_size_bytes: u32,
        schema_hash: u64,
    ) -> Result<Self> {
        validate_geometry(slot_count, slot_size_bytes)?;
        let name = handle.shm_name();
        let shmem = ShmemConf::new()
            .os_id(&name)
            .open()
            .map_err(|e| TesseraSlateError::Region(format!("attach: {e}")))?;

        // Bounds-safety before the first raw copy: the mapped region must
        // be at least as large as our expected layout, or a truncated /
        // stale leftover of the same name would let header reads over-read.
        let expected_size = region_size_bytes(slot_count, slot_size_bytes).ok_or_else(|| {
            TesseraSlateError::Config("region size overflow in caller geometry".into())
        })?;
        if shmem.len() < expected_size {
            return Err(TesseraSlateError::Region(format!(
                "attached SHM region '{name}' is {} bytes, smaller than the {expected_size} \
                bytes the caller's geometry requires; stale segment or geometry mismatch.",
                shmem.len()
            )));
        }

        let region = Region {
            shmem,
            slot_count,
            slot_size_bytes,
            schema_hash,
            shm_name: name,
            is_owner: false,
            manually_unlinked: false,
        };
        region.validate_attached_header(handle)?;
        Ok(region)
    }

    /// Whether this region was created (owner) vs attached.
    pub fn is_owner(&self) -> bool {
        self.is_owner
    }

    /// Configured slot count.
    pub fn slot_count(&self) -> u32 {
        self.slot_count
    }

    /// Configured per-slot payload size in bytes.
    pub fn slot_size_bytes(&self) -> u32 {
        self.slot_size_bytes
    }

    /// Caller-supplied schema hash stamped at creation.
    pub fn schema_hash(&self) -> u64 {
        self.schema_hash
    }

    /// Global epoch (microseconds since UNIX epoch at creation).
    pub fn epoch_micros(&self) -> u64 {
        self.read_global_header().epoch_micros
    }

    fn write_global_header(&mut self, handle: &NamespaceHandle) {
        let header = GlobalHeader {
            magic: MAGIC,
            format_version: FORMAT_VERSION,
            _pad0: 0,
            epoch_micros: current_epoch_micros(),
            slot_count: self.slot_count,
            slot_size_bytes: self.slot_size_bytes,
            schema_hash: self.schema_hash,
            writer_seq: 0,
            last_update_ns: 0,
            handle_blake3: handle.full_digest(),
            _reserved: [0; 40],
        };
        let bytes = bytemuck::bytes_of(&header);
        // SAFETY: we just created the mapping; offset 0 + SIZE is in bounds
        // (region_size_bytes includes GlobalHeader::SIZE).
        unsafe {
            let dst = self.shmem.as_ptr();
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, GlobalHeader::SIZE);
        }
    }

    fn read_global_header(&self) -> GlobalHeader {
        let mut header = GlobalHeader::zeroed();
        let bytes = bytemuck::bytes_of_mut(&mut header);
        // SAFETY: offset 0 + SIZE in bounds; GlobalHeader is Pod so any
        // byte pattern is a valid value (magic / version are validated
        // separately before this is trusted).
        unsafe {
            let src = self.shmem.as_ptr();
            core::ptr::copy_nonoverlapping(src, bytes.as_mut_ptr(), GlobalHeader::SIZE);
        }
        header
    }

    fn validate_attached_header(&self, handle: &NamespaceHandle) -> Result<()> {
        let h = self.read_global_header();
        if h.magic != MAGIC {
            return Err(TesseraSlateError::Region(format!(
                "magic mismatch: expected {MAGIC:#x}, found {:#x} (not a Slate region?)",
                h.magic
            )));
        }
        if h.format_version != FORMAT_VERSION {
            return Err(TesseraSlateError::HeaderMismatch {
                message: "format_version mismatch".into(),
                expected_format: FORMAT_VERSION,
                found_format: h.format_version,
            });
        }
        if h.slot_count != self.slot_count || h.slot_size_bytes != self.slot_size_bytes {
            return Err(TesseraSlateError::GeometryMismatch {
                expected_count: self.slot_count,
                found_count: h.slot_count,
                expected_size: self.slot_size_bytes,
                found_size: h.slot_size_bytes,
            });
        }
        if h.schema_hash != self.schema_hash {
            return Err(TesseraSlateError::SchemaHashMismatch {
                expected: self.schema_hash,
                found: h.schema_hash,
            });
        }
        if h.handle_blake3 != handle.full_digest() {
            return Err(TesseraSlateError::Region(
                "handle digest mismatch on attach — the description derives a different handle \
                than the creator's; verify the description matches across processes."
                    .into(),
            ));
        }
        Ok(())
    }

    fn check_slot_index(&self, index: u32) -> Result<()> {
        if index >= self.slot_count {
            return Err(TesseraSlateError::SlotIndexOutOfRange {
                index,
                slot_count: self.slot_count,
            });
        }
        Ok(())
    }

    fn slot_base(&self, index: u32) -> usize {
        slot_offset(index, self.slot_size_bytes)
    }

    /// Atomic view of the global monotonic `writer_seq` counter.
    pub fn writer_seq_atomic(&self) -> &AtomicU64 {
        // SAFETY: WRITER_SEQ_OFFSET (40) is 8-aligned and inside the
        // GlobalHeader, which is in bounds; AtomicU64 has the u64 layout.
        unsafe { &*(self.shmem.as_ptr().add(WRITER_SEQ_OFFSET) as *const AtomicU64) }
    }

    /// Atomic view of the global `last_update_ns` counter.
    pub fn last_update_ns_atomic(&self) -> &AtomicU64 {
        // SAFETY: LAST_UPDATE_NS_OFFSET (48) is 8-aligned and in bounds.
        unsafe { &*(self.shmem.as_ptr().add(LAST_UPDATE_NS_OFFSET) as *const AtomicU64) }
    }

    /// Atomic view of a slot's seqlock `sequence` counter.
    pub fn slot_sequence_atomic(&self, index: u32) -> Result<&AtomicU64> {
        self.check_slot_index(index)?;
        let offset = self.slot_base(index) + SLOT_SEQUENCE_OFFSET;
        // SAFETY: index checked. slot_base is 8-aligned: GlobalHeader::SIZE
        // (128) + index * stride, and stride = SlotHeader::SIZE (32) +
        // slot_size_bytes with slot_size_bytes % 8 == 0 (validate_geometry),
        // so every slot start is 8-aligned. `sequence` is at slot offset 0.
        Ok(unsafe { &*(self.shmem.as_ptr().add(offset) as *const AtomicU64) })
    }

    /// Raw mutable pointer to a slot's payload area.
    ///
    /// # Safety
    /// Caller must hold the slot's seqlock-odd state so no reader observes
    /// mid-write data; writes must not exceed `slot_size_bytes`.
    pub unsafe fn slot_payload_ptr_mut(&self, index: u32) -> Result<*mut u8> {
        self.check_slot_index(index)?;
        let offset = self.slot_base(index) + SlotHeader::SIZE;
        // SAFETY: index checked; offset is the start of this slot's payload
        // area, which lies within the mapped region.
        Ok(unsafe { self.shmem.as_ptr().add(offset) })
    }

    /// Raw const pointer to a slot's payload area.
    ///
    /// # Safety
    /// Caller must bracket the read with the seqlock before/after check so
    /// any copy overlapping a write is rejected and retried.
    pub unsafe fn slot_payload_ptr(&self, index: u32) -> Result<*const u8> {
        self.check_slot_index(index)?;
        let offset = self.slot_base(index) + SlotHeader::SIZE;
        // SAFETY: index checked; offset within this slot's payload area.
        Ok(unsafe { self.shmem.as_ptr().add(offset) as *const u8 })
    }

    /// Write a slot's non-atomic header fields (`length`, `timestamp`)
    /// inside the seqlock-odd window. `sequence` is managed by the caller
    /// via [`Region::slot_sequence_atomic`].
    ///
    /// # Safety
    /// Caller must hold the slot's seqlock-odd state.
    pub unsafe fn write_slot_meta(
        &self,
        index: u32,
        length: u32,
        timestamp_nanos: u64,
    ) -> Result<()> {
        self.check_slot_index(index)?;
        let base = self.slot_base(index);
        // SAFETY: index checked; offsets stay within the SlotHeader;
        // caller-asserted seqlock-odd protection.
        unsafe {
            let p = self.shmem.as_ptr().add(base);
            core::ptr::write_unaligned(p.add(SLOT_LENGTH_OFFSET) as *mut u32, length);
            core::ptr::write_unaligned(p.add(SLOT_TIMESTAMP_OFFSET) as *mut u64, timestamp_nanos);
        }
        Ok(())
    }

    /// Read a slot's non-atomic header fields between seqlock checks.
    ///
    /// # Safety
    /// Caller must bracket with the seqlock before/after check.
    pub unsafe fn read_slot_meta(&self, index: u32) -> Result<(u32, u64)> {
        self.check_slot_index(index)?;
        let base = self.slot_base(index);
        // SAFETY: index checked; offsets within the SlotHeader; caller's
        // seqlock check brackets the read window.
        unsafe {
            let p = self.shmem.as_ptr().add(base);
            let length = core::ptr::read_unaligned(p.add(SLOT_LENGTH_OFFSET) as *const u32);
            let ts = core::ptr::read_unaligned(p.add(SLOT_TIMESTAMP_OFFSET) as *const u64);
            Ok((length, ts))
        }
    }

    /// Owner-side unlink of the SHM name. Idempotent; attacher calls are
    /// rejected (only the creator may remove the name). Existing mappings
    /// stay valid after unlink; the name just becomes unattachable.
    pub fn unlink(&mut self) -> Result<()> {
        if self.manually_unlinked {
            return Ok(());
        }
        if !self.is_owner {
            return Err(TesseraSlateError::Region(
                "Region::unlink called by an attacher (is_owner=false). Only the creator may \
                unlink the shared-memory name; drop this Region to release the mapping."
                    .into(),
            ));
        }
        #[cfg(unix)]
        {
            let cname = std::ffi::CString::new(self.shm_name.as_str()).map_err(|_| {
                TesseraSlateError::Region("shm_name contains an interior NUL byte".into())
            })?;
            // SAFETY: cname is a valid NUL-terminated C string; shm_unlink
            // is thread-safe POSIX. Only flip state on success (rc == 0 or
            // ENOENT) so a real failure leaves drop-time cleanup active.
            let rc = unsafe { libc::shm_unlink(cname.as_ptr()) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::ENOENT) {
                    return Err(TesseraSlateError::Region(format!(
                        "shm_unlink('{}') failed: {err}",
                        self.shm_name
                    )));
                }
            }
            // Suppress Shmem's drop-time unlink so an owner-handoff sequence
            // (A unlinks, B recreates the same name, A finally drops) does
            // not have A's drop remove B's freshly-created name.
            self.shmem.set_owner(false);
            self.manually_unlinked = true;
            Ok(())
        }
        #[cfg(not(unix))]
        {
            Err(TesseraSlateError::Region(
                "Region::unlink is not supported on non-Unix platforms.".into(),
            ))
        }
    }
}

fn validate_geometry(slot_count: u32, slot_size_bytes: u32) -> Result<()> {
    if slot_count == 0 {
        return Err(TesseraSlateError::Config("slot_count must be > 0".into()));
    }
    if slot_size_bytes == 0 {
        return Err(TesseraSlateError::Config("slot_size_bytes must be > 0".into()));
    }
    // slot_size_bytes must be a multiple of 8 so successive slot starts
    // (and therefore each slot's `sequence` offset) stay 8-byte-aligned
    // for the AtomicU64 view. Fail at construction, not at first access.
    if slot_size_bytes % 8 != 0 {
        return Err(TesseraSlateError::Config(format!(
            "slot_size_bytes={slot_size_bytes} must be a multiple of 8 so successive slot \
            starts stay 8-byte-aligned for the AtomicU64 seqlock view (round up to {})",
            (slot_size_bytes + 7) & !7
        )));
    }
    Ok(())
}

fn current_epoch_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

/// Best-effort unlink of a stale SHM region by name, used when a
/// `force_recreate` create finds a leftover from a crashed prior owner.
fn unlink_named_region(name: &str) -> Result<()> {
    if let Ok(shmem) = ShmemConf::new().os_id(name).open() {
        drop(shmem);
        #[cfg(unix)]
        {
            let cname = std::ffi::CString::new(name).map_err(|_| {
                TesseraSlateError::Region("region name contains a NUL byte".into())
            })?;
            // SAFETY: valid C string; best-effort cleanup, rc ignored.
            unsafe {
                libc::shm_unlink(cname.as_ptr());
            }
        }
    }
    Ok(())
}
