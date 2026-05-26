# tessera-sink

Atomic-write worker pool to disk. The first *composite service* in
Tessera, built on the three primitives:

- [`tessera-pool`](../tessera-pool/) — zero-copy payload handoff (chunked).
- [`tessera-channel`](../tessera-channel/) — control plane (owner → worker)
  and ack plane (worker → owner).

N worker subprocesses stream chunks to a temp file and atomically
rename into place on commit, with BLAKE3 integrity verification.

**Status**: v0.0.1, Stage 4d in progress. Error, config, and region-name
derivation land first; the message codec, worker run loop, and owner
state machine land in follow-up commits.

See the [workspace README](../../README.md) and
[`docs/concept_landscape.md`](../../docs/concept_landscape.md) for the
design context.
