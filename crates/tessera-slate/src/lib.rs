//! Tessera Slate — seqlock-protected latest-value snapshot slot table.
//!
//! Slate is the snapshot member of the Tessera primitive set: a fixed
//! table of byte slots that a writer overwrites in place and readers poll
//! for the current value. Unlike Ring (a lossy *stream*), Slate has no
//! history — a reader always converges to the latest written bytes.
//! Unlike Pool (lease-backed handoff), slots are statically addressed and
//! rewritten continuously with no acquire/release lifecycle.
//!
//! - **Snapshot semantics**: read slot N for its latest value, not a
//!   history of updates.
//! - **One writer per slot**, many lock-free readers; a per-slot seqlock
//!   with bounded retry makes torn reads transient and self-correcting.
//! - **Bytes-only**: payloads are caller-owned bytes. Any typing, field
//!   layout, or presence tracking lives above Slate — that is what keeps
//!   it a primitive, like the rest of Tessera.
//!
//! ```
//! use tessera_slate::{Slate, SlateConfig, SlateReader, ReadResult};
//!
//! # fn main() -> Result<(), tessera_slate::TesseraSlateError> {
//! let desc = format!("tessera-slate-doctest/{}", std::process::id());
//! let writer = Slate::open(SlateConfig {
//!     description: desc.clone(),
//!     slot_count: 8,
//!     slot_size_bytes: 64,
//!     schema_hash: 0,
//!     is_owner: true,
//!     force_recreate: false,
//! })?;
//! writer.write_slot(2, b"hi")?;
//! let reader = SlateReader::open(&desc, 8, 64, 0)?;
//! match reader.read_slot(2)? {
//!     ReadResult::Slot { bytes, .. } => assert_eq!(&bytes[..], b"hi"),
//!     other => panic!("expected Slot, got {other:?}"),
//! }
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod error;
pub mod header;
pub mod namespace;
pub mod region;
pub mod slate;

pub use error::{Result, TesseraSlateError};
pub use namespace::NamespaceHandle;
pub use slate::{HeaderSnapshot, ReadResult, Slate, SlateConfig, SlateReader};
