# Tessera Decision Ledger (ADR-Light)

Append-only ledger for decisions, deferrals, hypotheses, discoveries, incidents,
outages, and human corrections. Maintained per
[ADR-Light](https://github.com/Auspex-Aerie/ADRLight): one grep-able file, causal
edges (`triggered_by`, `supersedes`, `resolves`), status-line-only edits to past
entries.

---

## Format

Entry types: DEC (decision), DEF (deferral), HYP (hypothesis), DIS (discovery),
INC (incident), OUT (outage), BOT (human correction). Each type has its own
number space (DEC-001, DEF-001, ...). IDs are allocated by appending an entry
(or stub) to this file — nowhere else.

Format changes are recorded as dated blockquote notes in this section, never by
rewriting old entries.

---

## Decisions

### DEC-001: Add Slate as Tessera's snapshot primitive (graduate the Auspice board)
- `id`: DEC-001
- `date`: 2026-06-14
- `status`: accepted
- `triggered_by`: live observation — the Auspice metrics board was the one multi-process boundary still outside Tessera, plus the directive to put every fitting MP boundary on Tessera ("a package between them is fine")
- `decision`: Add `tessera-slate`, a fifth primitive: a bytes-only, seqlock-protected, latest-value snapshot slot table (overwrite-in-place, random-access by index, no history). It is the snapshot peer of Pool/Ring/Channel. The typed/manifest layer stays in Auspice (`auspice-board`) layered on top.
- `rationale`: No existing primitive fits snapshot semantics — Ring is a lossy *stream* with history and per-reader cursors, Pool is lease-backed one-shot handoff, Channel is a reliable FIFO. A snapshot reader wants the latest value, not a stream. Building it as a primitive (not a layer-2 atop Ring) keeps the dependency graph a clean DAG: services may compose primitives, primitives never depend on each other.
- `impact`: New crate `crates/tessera-slate` (shipped — 23 unit tests + 1 doctest, concurrent cross-mapping hammer, clippy-clean under `-D warnings`; commit `f564ad0`). Slate implements its **own** seqlock so primitives stay independent — Ring is untouched. README + workspace `Cargo.toml` updated. The Auspice-side rework (auspice-schema layout split + auspice-board reimplemented over tessera-slate) is the remaining implementation, tracked in the work tracker, not here.
- `docs_updated`: `crates/tessera-slate/*`, `README.md`, `Cargo.toml`
- `related`: DEF-001

---

### DEC-002: Public `Descriptor` constructor for cross-language IPC handoff
- `id`: DEC-002
- `date`: 2026-07-02
- `status`: accepted
- `triggered_by`: cross-language handoff — a Rust producer (the Ominari loader) writes batches into a Pool for a Python consumer, which must rebuild a `Descriptor` from a slot reference received over a Channel. `Descriptor` was constructible only via `Pool.write`; the `_descriptor_from_bytes` reconstruction path existed but was underscore-private (pickle-only).
- `decision`: Expose a public `#[new]` on the Python `Descriptor` — `Descriptor(slot_index, generation, lease_id_bytes, size_bytes)` — reusing the existing reconstruction path. A non-Python producer can now hand a slot reference over any byte channel and the consumer rebuilds the `Descriptor` to call `pool.read_payload()`.
- `rationale`: Cross-process handoff is a first-class Tessera use case, and bytes are language-neutral — the `Descriptor` is the one structured token that must cross. The logic already existed for pickle; exposing it publicly adds zero new risk (`Descriptor` is `frozen`/read-only after construction) and unblocks Rust→Python (and any non-Python producer → Python consumer).
- `impact`: `python/py-tessera-pool/src/lib.rs` — added `Descriptor.__new__`; verified round-trip in Python. Enables the Ominari loader's shared-memory batch handoff.
- `related`: DEF-002

---

## Deferrals

### DEF-001: Defer heterogeneous / per-group slot sizing in Slate
- `id`: DEF-001
- `date`: 2026-06-14
- `status`: active
- `triggered_by`: DEC-001 — Slate v0.1 uses a single uniform `slot_size_bytes` for every slot
- `decision`: Ship Slate v0.1 with **uniform** slot sizes. Heterogeneous (arbitrary per-slot) or per-group (Ring-style size classes) sizing is deferred. Callers that vary record size pad every slot to the maximum; Slate's per-slot `length` field absorbs the variance, so no caller-side padding code is needed and reads return only the written bytes.
- `rationale`: For the intended snapshot-board use case (similarly-sized records) the uniform-padding overhead is negligible against system RAM — tens to a few hundred KB, dominated by one large field. Per-group sizing is a *moderate* change (on-disk geometry table, per-entry offset precompute and validation, a bifurcated config) for a benefit the use case does not need, on a primitive whose value is minimalism. It adds **no correctness risk** (the seqlock and 8-byte alignment are unaffected). Crucially it is cleanly evolvable: `FORMAT_VERSION` + the 40 reserved `GlobalHeader` bytes make it a **non-breaking v0.2 addition**, so deferring costs nothing in future flexibility.
- `impact`: README carries an explicit "known limitation (planned for v0.2)" note next to the Slate component so consumers aren't surprised.
- `revisit_when`: a caller needs heterogeneous slot sizes, OR uniform-padding waste becomes non-negligible for a real workload (e.g. thousands of slots with widely uneven sizes). If revisited, implement **per-group size classes** (mirroring Ring's sections), not arbitrary per-slot — it matches how callers structure data and is the proven pattern.

---

### DEF-002: Audit + expand the cross-language constructor surface (tokens vs results)
- `id`: DEF-002
- `date`: 2026-07-02
- `status`: active
- `triggered_by`: DEC-002 — exposing `Descriptor.__new__` revealed the Python facades only partially support a non-Python producer reconstructing objects that cross a process boundary.
- `decision`: Defer a systematic pass that classifies every frozen `#[pyclass]` as either a **cross-process handoff token** (must have a public constructor so a non-Python producer can hand it over) or a **local read-result** (fine construct-only-internally). Do not blanket-add `#[new]`; expand deliberately, guided by that distinction. Current state: `Descriptor` (token) — done (DEC-002). `Lease` (pool) — identical frozen / private `_lease_from_bytes` / no-`#[new]` pattern, but **owner-side**, so no consumer needs it today. `Event`/`ReaderStats` (ring) and `Header`/`SlotRead` (slate) — frozen, but **local read-results** obtained from one's own `Reader`/`SlateReader`, never passed between processes → no constructor needed. Channel / Ring / Slate / Sink primary types already expose `#[new]`.
- `rationale`: The only actual cross-language blocker (`Descriptor`) is fixed. `Lease` is owner-side and read-results aren't tokens, so nothing is blocked now. A blanket expansion adds unused API surface and raises pickle-symmetry questions on types that never cross. The token-vs-result rule keeps the facade minimal while making the next addition mechanical.
- `revisit_when`: a non-Python **owner** needs to hand a `Lease` across processes (→ add `Lease.__new__` mirroring `_lease_from_bytes`), or any frozen type turns out to be passed between processes rather than read locally. Apply the rule then: cross-process tokens get public constructors (+ pickle); local results stay frozen-only.
- `related`: DEC-002
