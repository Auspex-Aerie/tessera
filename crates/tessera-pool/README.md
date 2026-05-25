# tessera-pool

Non-lossy lease-backed shared-memory pool primitive. Fixed slots,
single-owner lifecycle, single-writer-lease, timeout reclaim with
slot-generation invalidation of stale handles.

**Status**: v0.0.1 — Rust core and PyO3 facade functional;
in-progress on Stage 4a of the upstream extraction plan. CI wiring +
docs polish + crates.io / PyPI publish land in Stage 5.

## What it does

- **Fixed slots in shared memory** — one SHM region per Pool, sized
  at construction. Layout is `[Header][SlotMeta × N][PayloadArea]`,
  documented in `src/header.rs`.
- **BLAKE3-derived namespace** — `Pool::new(config)` hashes the
  caller-supplied `description` and uses the digest both as the
  POSIX SHM segment name and as a cross-verification token in the
  header. Peers attaching with the same description automatically
  share the same region.
- **Single-owner lifecycle** — one process creates the region (owner),
  zero or more attachers read it. Owner restart attempts re-attach
  with epoch validation, falls back to unlink + recreate if the
  validation fails.
- **Single-writer-lease semantics** — only the owner mutates slot
  metadata. Attachers consume payload bytes by descriptor handoff;
  they cannot acquire / release / renew. This matches the
  intra-container Sink shape (owner producer + worker subprocesses)
  and avoids cross-process lease-table coordination.
- **Timeout-based reclaim with generation invalidation** — every
  slot carries a generation counter that bumps on `acquire` AND on
  `reclaim_stale`. Stale descriptors held by a slow worker that
  missed a reclaim fail validation rather than corrupting a
  re-leased slot. The owner can `renew(lease)` to refresh
  `acquired_at` during long operations.
- **POSIX SHM via the `shared_memory` crate** — works cross-container
  through Docker `ipc:` namespace sharing.

## Quick start

```rust
use std::time::Duration;
use tessera_pool::{Pool, PoolConfig};

let mut pool = Pool::new(PoolConfig {
    description: "my-app/training-batches".into(),
    slot_count: 8,
    slot_size_bytes: 64 * 1024 * 1024,
    is_owner: true,
    ttl_micros: 60_000_000,
})?;

let lease = pool.acquire(Duration::from_secs(1))?;
let descriptor = pool.write(&lease, &payload_bytes)?;
// hand `descriptor` to a worker (or in-process consumer) over a channel
let read_back = pool.read_payload(&descriptor)?;
pool.release(&lease)?;
# Ok::<(), tessera_pool::TesseraPoolError>(())
```

For Python ergonomics, install the
[`tessera-pool`](../../python/py-tessera-pool/) Python facade and use
`from tessera_pool import Pool` with the same API.

## Tests

`cargo test -p tessera-pool` — 29 tests covering header layout,
namespace derivation, region create / attach / unlink, the full
Pool state machine (acquire / write / read / release / renew /
reclaim_stale), and edge cases (oversized payload, double-write,
stale-handle rejection, attacher restrictions).

## Roadmap

- v0.1.0 (Stage 4a → Stage 5): publishing to crates.io with the
  current public surface.
- Future capabilities flagged for forward-compatible API design:
  typed slot views (Arrow / NumPy zero-copy), eviction-aware lease
  shapes, peer / multi-owner mode for symmetric replica deployments.
  None of these break the current `(slot_index, lease_id,
  generation)` descriptor model.
