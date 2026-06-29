# RFC: Sink-Worker Packaging & Distribution

| | |
|---|---|
| **Status** | DRAFT — for review |
| **Date** | 2026-06-28 |
| **Tracking** | #32 |
| **Supersedes input** | the deployment/packaging critique (internal) |
| **Decision record** | a `DEC` entry in `docs/decision_log.md` once accepted |

> This is a proposal for review cycles, not a finished plan. Sections 6–9 are
> where the real choices live; please push back there. Open questions are
> collected in §10 and the spike in Step 1 gates the rest.

---

## 1. Summary

`tessera-sink` is the only Tessera component that needs a **separate executable**
— `tessera-sink-worker` — running as an OS subprocess. Today that binary is not
distributed with anything: a user who runs `pip install tessera-sink` (or
`cargo add tessera-sink`) gets the library but **no worker**, and must build and
place the binary themselves. This RFC proposes bundling the worker into the
Python wheel so `pip install tessera-sink` yields a runnable Sink with **zero
manual build steps**, backed by reproducible CI-built wheels and an
install-from-source fallback.

This is the last substantial item gating the v0.1 publish (#24): we should not
ship an immutable `0.1.0` to crates.io / PyPI with a broken install story.

## 2. Problem statement

Sink composes Pool + Channel plus **N worker subprocesses** that stream chunks
to disk and atomically rename on commit. The workers are a distinct
`tessera-sink-worker` bin crate, launched via `Command::new(bin)` (see
`crates/tessera-sink/src/spawn.rs`).

The owner finds that binary through `resolve_worker_bin`, a four-tier probe:

1. explicit `SinkConfig::worker_bin_path` (hard error if set-but-missing),
2. the `TESSERA_SINK_WORKER_BIN` env var,
3. a sibling of `current_exe()`,
4. the bare name `tessera-sink-worker` resolved via `PATH` at spawn time.

Every tier assumes the operator has **already produced the binary somewhere**.
There is no tier that "just works" after a package install, because:

- The `tessera-sink` **Python wheel ships only the `_native` extension module**
  (the PyO3 facade). The worker binary is never put in the wheel.
- Every Sink example reimplements the same hack: a `_worker_bin()` that looks in
  `target/{debug,release}/` and aborts with "run `cargo build -p
  tessera-sink-worker` first."
- The bundled Sink tests build the worker via `cargo build` and point
  `worker_bin_path` at `target/debug/tessera-sink-worker`.

Net: **`pip install tessera-sink` does not produce a working Sink.** This was the
central finding of the packaging critique, and it is the one cluster of issues
that blocks a clean public release.

## 3. Goals / non-goals

**Goals**
- `pip install tessera-sink` → a runnable Sink on supported platforms, with **no
  manual `cargo build`** and **no env var** required.
- The bundled worker is discovered **by default** (the common path needs no
  configuration).
- Wheels are **reproducible and CI-verified**: a wheel-install + Sink
  integration test runs in CI *without* a dev tree (the critique's "can we even
  produce a working Sink install?" becomes a first-class check).
- Preserve the existing explicit overrides (`worker_bin_path`,
  `TESSERA_SINK_WORKER_BIN`) for power users and development.
- Preserve the locked multiprocess worker model and the argv contract — this RFC
  changes **distribution**, not runtime behavior.

**Non-goals**
- Changing the worker's argv/IPC contract or the Sink runtime model.
- crates.io publishing mechanics and the auspice path-dep cutover — that is #24.
  This RFC covers the install surface + the worker binary; #24 publishes it.
- Full Windows/macOS parity for v0.1 (platform scope is an open question, §10).

## 4. Current state (grounded)

- **Build backend:** maturin, one extension module per wheel
  (`[tool.maturin] module-name = "tessera_sink._native"`). Wheels are already
  platform-specific (the compiled ext), so adding a platform-specific binary is
  consistent with what we ship.
- **Worker crate:** `crates/tessera-sink-worker` — a normal cargo bin, built
  with `cargo build -p tessera-sink-worker`.
- **Discovery:** the four-tier `resolve_worker_bin` above; tier 3
  (sibling-of-`current_exe()`) is the one a packaged layout could satisfy, but
  nothing installs the binary next to anything today.
- **CI band-aid (already landed):** the `py-tessera-sink` CI job runs
  `cargo build -p tessera-sink-worker` so the integration tests run instead of
  skipping. That proves the *code* works end-to-end; it does **not** prove a
  *packaged install* works — the worker is built into `target/`, not the wheel.

## 5. Constraints

- maturin is the backend; its mechanism for bundling a **prebuilt, platform-
  specific** binary into a wheel must be confirmed (Step 1 spike).
- pyo3 is pinned at 0.22 (separate, deliberate; out of scope here).
- Primitives stay independent; **no silent fallbacks** — a missing worker must
  fail loud with the tried paths (the current behavior).
- Whatever we add must not regress the explicit-override semantics (tier 1 is a
  hard error if the path is set-but-missing — keep that).

## 6. Options for shipping the worker

| Option | Mechanism | Pros | Cons | Verdict |
|---|---|---|---|---|
| **A. Bundle worker in the wheel as an installed script** | Build the worker for the target platform; include it in the wheel's scripts/data so `pip install` drops it into the venv's `bin/`/`Scripts/` | Default path "just works"; lands next to the interpreter where a discovery tier can find it; no env var | Must build the worker per-platform during the wheel build; exact maturin mechanism needs confirming | **Recommended default** |
| **B. Console-script entry point** | A `[project.scripts]` entry | pip-native `bin/` entry | A console script is a *Python* wrapper; exposing a Rust binary still requires the binary to be bundled — composes with A, doesn't replace it | Fold into A if useful |
| **C. Companion wheel** | Ship `tessera-sink-worker` as its own wheel whose payload is the binary | Clean separation of concerns | Two artifacts to version + coordinate; more moving parts for the same result | Reject for v0.1 (revisit if A is infeasible) |
| **D. Build-on-install (sdist + cargo)** | sdist builds the worker from source at install time | No prebuilt binary needed | Requires a Rust toolchain on the user's machine; slow; brittle | **Keep as the no-wheel fallback only** |
| **E. Status quo** | Manual `cargo build` + env/PATH | — | `pip install` doesn't work | Keep tiers 1–4 as power-user overrides |

**Recommendation:** **A** as the default (bundle the worker so a plain
`pip install` works), **keep tiers 1–4** as explicit overrides, and ship an
**sdist that can build from source (D)** for platforms without a prebuilt wheel.

## 7. Recommended design

1. The `tessera-sink` wheel build also produces `tessera-sink-worker` for the
   wheel's target platform (release).
2. The worker is included in the wheel at a location `pip install` lands in the
   environment's executable dir (venv `bin/` on POSIX).
3. `resolve_worker_bin` gains a discovery tier for "next to the active Python
   interpreter / installed data location," inserted **after** the explicit
   override + env tiers and **before** the bare-PATH last resort, so:
   - a `worker_bin_path` / env override still wins (and still hard-errors if
     set-but-missing),
   - a pip-installed worker is found automatically,
   - the bare-PATH fallback remains for hand-rolled setups.
   The Python facade's default `Sink(...)` (no `worker_bin_path`) then works
   out of the box.
4. The examples' `target/{debug,release}` hack is replaced by a single portable
   helper (or simply by relying on the now-working default discovery).
5. sdist includes the Rust sources so a source build is possible where no wheel
   exists.

## 8. Implementation plan — enumerated steps

> Each step lists what it does, files touched, and how it's validated. Step 1 is
> a gate: its finding may revise Steps 2–3.

**Step 1 — Spike: confirm the maturin bundling mechanism.**
Determine the reliable way maturin includes a prebuilt, platform-specific binary
in a wheel such that `pip install` places it in the env's `bin/` (candidates:
`[tool.maturin] include` into the wheel's `*.data/scripts/` dir; a build hook;
or `[project.scripts]` shim). *Files:* none (investigation). *Validation:* a
throwaway wheel built locally that, on `pip install` into a fresh venv, drops an
executable into `.venv/bin/`. Output: a one-paragraph finding + chosen
mechanism appended to this RFC.

**Step 2 — Produce the worker during the wheel build.**
Wire `cargo build -p tessera-sink-worker --release` (for the wheel's target) into
the build so the binary exists when the wheel is assembled. *Files:*
`python/py-tessera-sink/pyproject.toml`, possibly a small build script / CI step.
*Validation:* `maturin build` yields a wheel containing the worker.

**Step 3 — Place the worker at a discoverable install location.**
Include the built worker in the wheel so install lands it in the env executable
dir. *Files:* `pyproject.toml` (`[tool.maturin]`). *Validation:* `pip install`
the wheel into a fresh venv → `tessera-sink-worker` present in `.venv/bin/`.

**Step 4 — Add the discovery tier + make the default Sink work.**
Insert the "next-to-interpreter / installed-data" tier in `resolve_worker_bin`
(order per §7); update the Python facade so a default `Sink()` resolves the
bundled worker. Keep tier-1 hard-error semantics. *Files:*
`crates/tessera-sink/src/spawn.rs`, `python/py-tessera-sink/src/lib.rs`, tests.
*Validation:* a Sink constructed with **no** `worker_bin_path`, from a
pip-installed wheel, submits and commits a file. New unit test for the tier
order.

**Step 5 — Replace the examples' hack.**
Swap the `target/{debug,release}` `_worker_bin()` helper for the default
discovery (or one portable helper). *Files:* `examples/sink_*`. *Validation:*
examples run from a pip-installed wheel with no dev tree.

**Step 6 — sdist + from-source fallback.**
Ensure an sdist builds the worker from source where no wheel exists. *Files:*
packaging config. *Validation:* `pip install` from the sdist in a clean
container with a Rust toolchain.

**Step 7 — CI: manylinux wheels + packaged integration test.**
Build wheels via manylinux (cibuildwheel or maturin's manylinux support); add a
job that installs the **wheel** and runs the Sink integration test **without** a
separate `cargo build` (proving the bundled worker). Add an sdist build. *Files:*
`.github/workflows/`. *Validation:* the packaged integration job is green; it is
added to the required checks on `main`.

**Step 8 — Docs + decision record.**
Add an "Installing & running Sink" section; refresh the README "Release
Readiness" table; record a `DEC` entry in `docs/decision_log.md`. *Files:*
README, docs, ledger.

**Step 9 — Platform scope decision.**
Decide and document the supported matrix for v0.1 (proposal: Linux/manylinux
first; macOS/Windows best-effort or deferred). *Files:* this RFC + README.

## 9. Testing & CI plan

- **Packaged integration test (new, required):** fresh venv → `pip install`
  the built wheel → construct `Sink()` with no overrides → submit + commit →
  assert the file lands with correct bytes. No dev tree, no `cargo build`.
- **manylinux matrix** for the wheels; **sdist** build job.
- Keep the existing in-tree integration tests (worker built via cargo) for fast
  developer feedback.

## 10. Risks & open questions

- **maturin binary bundling (Step 1 gate):** the whole default path (Option A)
  rests on maturin being able to ship the binary into the wheel's exec dir. If
  it can't cleanly, fall back to Option C (companion wheel).
- **Cross-platform worker build:** building the Rust bin under manylinux (and
  later macOS/Windows) adds toolchain surface to CI.
- **Wheel size:** bundling a binary grows the wheel; acceptable, worth noting.
- **Discovery precedence:** the new tier must not change the meaning of existing
  overrides; the hard-error-on-missing-explicit-path behavior must survive.
- **Platform scope:** Linux-first for v0.1? (§9, Step 9.)
- **Relationship to #24:** this RFC delivers the *install story*; #24 *publishes*
  it. They should land in that order.

## 11. Review checklist

- [ ] Is the problem framing accurate and complete?
- [ ] Is Option A the right default (vs. a companion wheel)?
- [ ] Is the proposed `resolve_worker_bin` tier order acceptable?
- [ ] Is Linux-first an acceptable v0.1 scope?
- [ ] Is the CI plan (packaged integration test + manylinux + sdist) sufficient?
- [ ] Anything missing from the enumerated steps?

**Reviewers:** _(add)_
