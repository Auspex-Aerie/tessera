//! Region layout for Tessera Slate.
//!
//! ```text
//! offset 0:           GlobalHeader (HEADER_SIZE = 128 bytes, Plain-Old-Data)
//! offset HEADER_SIZE: slot array: slot_count entries, each
//!                     SlotHeader (32 bytes) + slot_size_bytes payload.
//! ```
//!
//! Slate is a single flat slot table — there is no section table (unlike
//! Ring). Logical grouping, if a caller wants it, is layered above by
//! partitioning the index space.
//!
//! All structs are `repr(C)` with explicit padding and `bytemuck::Pod`,
//! so they reinterpret out of the mapped bytes without copy. Numeric
//! fields are native byte order; the IPC namespace is the trust /
//! architecture boundary, the same stance as the other Tessera crates.
//!
//! ### Seqlock model
//!
//! Each `SlotHeader` carries its own `sequence` counter; the single
//! writer of a slot stamps odd-then-even around the payload copy, and
//! readers retry until they see the same even sequence before and after.
//! `sequence == 0` means the slot has never been written.

use bytemuck::{Pod, Zeroable};

/// Magic at the top of every Slate region. ASCII `"TESSLATE"` — verifies
/// on attach that we are looking at a Slate region (vs garbage, a
/// different Tessera component, or a partially-initialized region).
pub const MAGIC: u64 = u64::from_le_bytes(*b"TESSLATE");

/// Layout version. Bump on any incompatible change to `GlobalHeader` or
/// `SlotHeader`; attachers reject a mismatched version rather than read
/// garbage.
pub const FORMAT_VERSION: u32 = 1;

/// Bounded seqlock read retries per slot per poll before a read reports
/// [`crate::ReadResult::Torn`].
pub const READ_RETRIES: usize = 3;

/// Global region header. Stamped by the creator; read and validated by
/// attachers.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug)]
pub struct GlobalHeader {
    /// Constant [`MAGIC`]. First field so a zeroed region trivially fails
    /// `magic == MAGIC`.
    pub magic: u64,
    /// [`FORMAT_VERSION`] at creation. Attachers reject a mismatch.
    pub format_version: u32,
    /// Explicit padding to 8-align `epoch_micros`.
    pub _pad0: u32,
    /// Creator's epoch (microseconds since UNIX epoch at creation).
    pub epoch_micros: u64,
    /// Slot count. Fixed at creation.
    pub slot_count: u32,
    /// Per-slot payload size in bytes (excludes the `SlotHeader`). Fixed
    /// at creation.
    pub slot_size_bytes: u32,
    /// Caller-defined layout hash. Attachers must supply the same value;
    /// a mismatch is rejected (writer/reader built different slot
    /// layouts). `0` means "no schema".
    pub schema_hash: u64,
    /// Monotonic count of writes across all slots. Accessed atomically at
    /// runtime via an `AtomicU64` view at [`WRITER_SEQ_OFFSET`]; declared
    /// here as a plain `u64` because `bytemuck::Pod` does not admit
    /// atomics.
    pub writer_seq: u64,
    /// Nanoseconds since UNIX epoch of the most recent write to any slot.
    /// Accessed atomically at runtime via [`LAST_UPDATE_NS_OFFSET`].
    pub last_update_ns: u64,
    /// BLAKE3(description) at creation; attachers recompute and verify.
    pub handle_blake3: [u8; 32],
    /// Reserved bytes for future fields without a format-version bump.
    pub _reserved: [u8; 40],
}

impl GlobalHeader {
    /// On-disk size of the global header in bytes.
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

/// Per-slot header. Precedes each slot's payload bytes.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable, Debug)]
pub struct SlotHeader {
    /// Seqlock counter. `0` = never written; even = stable / readable;
    /// odd = write in progress. Accessed atomically via an `AtomicU64`
    /// view; declared `u64` because `bytemuck::Pod` does not admit
    /// atomics.
    pub sequence: u64,
    /// Actual payload byte length (≤ the section's `slot_size_bytes`).
    pub length: u32,
    /// Explicit padding to 8-align `timestamp_nanos`.
    pub _pad0: u32,
    /// Nanoseconds since UNIX epoch at write time.
    pub timestamp_nanos: u64,
    /// Reserved bytes for future per-slot fields without a version bump.
    pub _reserved: [u8; 8],
}

impl SlotHeader {
    /// On-disk size of a slot header in bytes.
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

/// Byte offset of `writer_seq` within the [`GlobalHeader`]. Used for the
/// runtime `AtomicU64` view; locked in by a unit test.
pub const WRITER_SEQ_OFFSET: usize = 40;

/// Byte offset of `last_update_ns` within the [`GlobalHeader`].
pub const LAST_UPDATE_NS_OFFSET: usize = 48;

/// Byte offset of `sequence` within a [`SlotHeader`] (first field, 0).
pub const SLOT_SEQUENCE_OFFSET: usize = 0;

/// Byte offset of `length` within a [`SlotHeader`].
pub const SLOT_LENGTH_OFFSET: usize = 8;

/// Byte offset of `timestamp_nanos` within a [`SlotHeader`].
pub const SLOT_TIMESTAMP_OFFSET: usize = 16;

/// Byte offset where the slot array starts (immediately after the global
/// header).
pub fn slots_data_offset() -> usize {
    GlobalHeader::SIZE
}

/// Byte stride between successive slots: `SlotHeader::SIZE + slot_size_bytes`.
pub fn slot_stride(slot_size_bytes: u32) -> usize {
    SlotHeader::SIZE + slot_size_bytes as usize
}

/// Byte offset of slot `index`'s header within the region.
pub fn slot_offset(index: u32, slot_size_bytes: u32) -> usize {
    slots_data_offset() + (index as usize) * slot_stride(slot_size_bytes)
}

/// Total region size for the given geometry, or `None` on overflow.
pub fn region_size_bytes(slot_count: u32, slot_size_bytes: u32) -> Option<usize> {
    let slots = slot_stride(slot_size_bytes).checked_mul(slot_count as usize)?;
    slots_data_offset().checked_add(slots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_header_is_128_bytes() {
        // magic 8 + format_version 4 + _pad0 4 + epoch_micros 8 + slot_count 4
        // + slot_size_bytes 4 + schema_hash 8 + writer_seq 8 + last_update_ns 8
        // + handle_blake3 32 + _reserved 40 = 128.
        assert_eq!(GlobalHeader::SIZE, 128);
    }

    #[test]
    fn slot_header_is_32_bytes() {
        // sequence 8 + length 4 + _pad0 4 + timestamp_nanos 8 + _reserved 8 = 32.
        assert_eq!(SlotHeader::SIZE, 32);
    }

    #[test]
    fn magic_is_ascii_marker() {
        assert_eq!(&MAGIC.to_le_bytes(), b"TESSLATE");
    }

    #[test]
    fn global_atomic_offsets_match_fields() {
        let h = GlobalHeader {
            magic: MAGIC,
            format_version: FORMAT_VERSION,
            _pad0: 0,
            epoch_micros: 7,
            slot_count: 4,
            slot_size_bytes: 64,
            schema_hash: 0,
            writer_seq: 0x0BAD_F00D,
            last_update_ns: 0x0102_0304_0506_0708,
            handle_blake3: [0; 32],
            _reserved: [0; 40],
        };
        let bytes = bytemuck::bytes_of(&h);
        let ws = u64::from_le_bytes(
            bytes[WRITER_SEQ_OFFSET..WRITER_SEQ_OFFSET + 8].try_into().unwrap(),
        );
        let lu = u64::from_le_bytes(
            bytes[LAST_UPDATE_NS_OFFSET..LAST_UPDATE_NS_OFFSET + 8].try_into().unwrap(),
        );
        assert_eq!(ws, 0x0BAD_F00D);
        assert_eq!(lu, 0x0102_0304_0506_0708);
    }

    #[test]
    fn slot_meta_offsets_match_fields() {
        let s = SlotHeader {
            sequence: 2,
            length: 0x1234,
            _pad0: 0,
            timestamp_nanos: 0xAABB_CCDD,
            _reserved: [0; 8],
        };
        let bytes = bytemuck::bytes_of(&s);
        let len = u32::from_le_bytes(
            bytes[SLOT_LENGTH_OFFSET..SLOT_LENGTH_OFFSET + 4].try_into().unwrap(),
        );
        let ts = u64::from_le_bytes(
            bytes[SLOT_TIMESTAMP_OFFSET..SLOT_TIMESTAMP_OFFSET + 8].try_into().unwrap(),
        );
        assert_eq!(len, 0x1234);
        assert_eq!(ts, 0xAABB_CCDD);
    }

    #[test]
    fn region_size_is_header_plus_slots() {
        assert_eq!(region_size_bytes(4, 64), Some(128 + 4 * (32 + 64)));
        assert_eq!(region_size_bytes(1, 8), Some(128 + (32 + 8)));
    }

    #[test]
    fn region_size_overflow_is_none() {
        assert_eq!(region_size_bytes(u32::MAX, u32::MAX), None);
    }
}
