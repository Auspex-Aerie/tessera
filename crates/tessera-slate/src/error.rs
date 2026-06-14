//! Error types for the Tessera Slate crate.

use thiserror::Error;

/// All Slate errors flow through this enum.
///
/// Slate is a snapshot primitive: latest-value, overwrite-in-place,
/// random-access by slot index. There is no history to lose, so there is
/// no "dropped" or "lapped" variant — a reader either sees the latest
/// coherent bytes, an as-yet-unwritten slot, or (transiently) a torn read
/// it should retry. Those three outcomes are values (`ReadResult`), not
/// errors; this enum covers construction / attach and caller misuse.
#[derive(Error, Debug)]
pub enum TesseraSlateError {
    /// Caller-side configuration bug detected at construction
    /// (`slot_count == 0`, `slot_size_bytes == 0`, or a slot size that is
    /// not a multiple of 8).
    #[error("invalid Slate config: {0}")]
    Config(/** Human-readable explanation of which field was invalid. */ String),

    /// SHM region creation / attach / unmap failed (OS resource issue,
    /// permissions, name collision, etc.).
    #[error("shared-memory region error: {0}")]
    Region(/** Underlying OS / library message. */ String),

    /// Attached region was created with a different magic or format
    /// version; reading it would corrupt.
    #[error(
        "attached SHM region has incompatible header: {message} \
        (expected format_version={expected_format}, found {found_format})"
    )]
    HeaderMismatch {
        /// Short description of which field disagrees.
        message: String,
        /// Format version this library was built with.
        expected_format: u32,
        /// Format version stamped in the attached region.
        found_format: u32,
    },

    /// Attached region's geometry (slot_count / slot_size_bytes) disagrees
    /// with the caller's config.
    #[error(
        "geometry mismatch: expected slot_count={expected_count}, found {found_count}; \
        expected slot_size_bytes={expected_size}, found {found_size}"
    )]
    GeometryMismatch {
        /// Slot count expected by the caller.
        expected_count: u32,
        /// Slot count stamped in the attached region.
        found_count: u32,
        /// Slot size expected by the caller.
        expected_size: u32,
        /// Slot size stamped in the attached region.
        found_size: u32,
    },

    /// Attached region's caller-supplied schema hash differs from the
    /// creator's: writer and reader were built against different layouts
    /// of whatever the caller packs into a slot. Rejecting here turns a
    /// silent field-misalignment into a loud refusal at attach.
    #[error(
        "schema hash mismatch: expected {expected:#018x}, found {found:#018x} \
        — writer and reader were built against different slot layouts"
    )]
    SchemaHashMismatch {
        /// Schema hash the caller supplied.
        expected: u64,
        /// Schema hash stamped in the attached region.
        found: u64,
    },

    /// A `write_slot` / `read_slot` named a slot index outside
    /// `0..slot_count`.
    #[error("slot index {index} out of range (slot_count={slot_count})")]
    SlotIndexOutOfRange {
        /// Index supplied by the caller.
        index: u32,
        /// Configured slot count.
        slot_count: u32,
    },

    /// `write_slot` was handed more bytes than one slot can hold.
    #[error("payload size {len} exceeds slot capacity {capacity}")]
    OversizedPayload {
        /// Caller-supplied byte length.
        len: usize,
        /// Per-slot capacity (slot_size_bytes).
        capacity: usize,
    },
}

/// Result alias for `tessera-slate` operations.
pub type Result<T> = core::result::Result<T, TesseraSlateError>;
