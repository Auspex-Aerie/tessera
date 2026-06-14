# tessera-slate

Seqlock-protected **latest-value snapshot slot table** over POSIX shared
memory — the snapshot member of the Tessera primitive set.

A writer overwrites slot *N* in place; readers poll slot *N* for its
current value. Unlike [`tessera-ring`](../tessera-ring) (a lossy *stream*
with history and per-reader cursors), Slate keeps no history — a reader
always converges to the latest written bytes. Unlike
[`tessera-pool`](../tessera-pool) (lease-backed handoff), slots are
statically addressed and rewritten continuously with no acquire/release
lifecycle.

- **One writer per slot**, many lock-free readers.
- Per-slot seqlock with bounded retry: torn reads are transient and
  self-correcting (`ReadResult::Torn` → retry next poll).
- **Bytes-only** boundary: payloads are caller-owned bytes. Any typing,
  field layout, or presence tracking lives above Slate.

```rust
use tessera_slate::{Slate, SlateConfig, SlateReader, ReadResult};

let writer = Slate::open(SlateConfig {
    description: "my-app/metrics".into(),
    slot_count: 64,
    slot_size_bytes: 256,
    schema_hash: 0,        // caller-defined layout hash; 0 = no schema
    is_owner: true,
    force_recreate: false,
})?;
writer.write_slot(3, b"latest state for slot 3")?;

let reader = SlateReader::open("my-app/metrics", 64, 256, 0)?;
if let ReadResult::Slot { bytes, .. } = reader.read_slot(3)? {
    assert_eq!(&bytes[..], b"latest state for slot 3");
}
# Ok::<(), tessera_slate::TesseraSlateError>(())
```

## Independence

Slate is a **primitive** — a peer of Pool / Ring / Channel, not a layer-2
service (that is Sink, which composes Pool + Channel). Its seqlock is
implemented independently from Ring's; primitives do not depend on one
another, so a shared seqlock crate (which would be a public surface
coupling two primitives) is deliberately avoided.

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE).
