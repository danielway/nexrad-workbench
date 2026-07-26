# Architecture Health Program — 2026-07

The execution record for the remediation plan in
[docs/arch-review-2026-07/README.md](../arch-review-2026-07/README.md). The
review diagnosed the entropy as **five concurrent half-finished migrations**,
each leaving two live conventions side by side; this program closed them, on
branch `arch-health` (31 commits off the review commit).

The review's own meta-rule drove the sequencing: *finish or kill* — a migration
is done only when the old convention is deleted and the doc is updated. Where
finishing wasn't the right call, the decision is written down as settled
(see [CORE_SHELL.md](../CORE_SHELL.md) → "What is explicitly NOT a violation")
rather than left as a sixth half-migration.

## Result

| Measure | Before | After |
|---|---|---|
| Headless tests | 1,831 | 1,931 |
| Layering violations (build-enforced) | 9 inherited edges | **0** |
| `src/app/` largest handler | 413 lines, untested | 78-line shell over a tested reducer |
| `src/app/` test coverage | zero | reducers tested in core |
| Decorative `pub` | ~926 `pub fn` | crate-wide `unreachable_pub`, ~1,195 demoted |
| `#[allow(dead_code)]` | 88, mostly bare | 44, each narrow + reasoned |
| Modules importing `state` | 8 of 11 | domain vocabulary lives in `core` |

Every commit passed the same gate: `cargo check` (zero warnings),
`cargo clippy -- -D warnings`, `cargo fmt`, and the full test suite.

## The anti-entropy mechanism

`tools/arch_check.rs`, run from `build.rs` on every `cargo check`, scans each
`crate::<module>` reference (comments stripped) and fails the build on any edge
that isn't in `ALLOWED`. Violations being burned down go in `GRANDFATHERED` with
a reason — and the build **also fails when a grandfathered edge stops
occurring**, forcing its row to be deleted. The table can only shrink.

This was built first, deliberately: it converted every subsequent fix into a
permanent one. It fired on schedule at each burn-down, and **`GRANDFATHERED` is
now empty** — the dependency graph matches the documented layering exactly, and
a regression is a failed build rather than a review comment.

## Phase A — compiler-verified structural repair

- **A1 — type rehoming.** The domain vocabulary moved out of `state` into
  `core/domain/` (viz, radar model, playback time model, forecast, prefs,
  errors, feed containers, ops, volume roster, telemetry, worker outcomes).
  `Intent` is now *defined* in `core` — `AppCommand` was renamed crate-wide, no
  alias left. Shell-coupled halves (localStorage load/save, wall-clock capture)
  stayed behind as extension impls on the core types. Killed the edges
  `core→state`, `core→subsystem`, `geo→state`, `state→ui`, `alerts→state`,
  `mping→state`.
- **A3 — `nexrad/` regrouped** into `acquisition/`, `live/`, `decode/`,
  `render/` (plus the existing `detection/`). `persistence_manager` moved to
  `app/`; `network_monitor` split into pure records (`core/domain/telemetry`)
  and the browser listener (`subsystem/`). Killed `nexrad→state`.
- **A4 — surface diet.** `#![warn(unreachable_pub)]` crate-wide (with `data/`
  exempted, since its `pub` surface is the lib facade `tests/idb.rs` links
  against). Then a dead-code audit: every `allow(dead_code)` was removed, the
  compiler consulted, and each finding either deleted (~30 items, −241 LOC),
  moved under `#[cfg(test)]` (27 helpers), or kept with a narrow allow and a
  written reason.
- **A5 — docs resynced** against the tree (see below).

## Phase B — decision mass into the core

- **B3 — `data/keys.rs`** (1,833 LOC behind the name "key types") split into
  `keys` / `blob_format` / `vcp_timing` / `live_anchor`.
- **B2 — the projection/timing concern** had five overlapping owners. The whole
  pure layer moved into `core`: `timing/` (upstream fork moved intact),
  `StreamingPlan`, `StreamingFilter`, the projection engine and its vocabulary,
  with `Projector` demoted to a private kernel of the engine. The one edge
  keeping `timing/` in `nexrad` was severed by inverting a `StreamingFilter`
  parameter into a predicate. Killed `core→nexrad`.
- **B1 — the `src/app/` decision mass**, previously the only untested module,
  became thin shells over tested core reducers, all following one
  Env/Slices/Actions shape (exemplar: `core::worker_ingest`):

  | Handler | Before | After |
  |---|---|---|
  | `handle_chunk_ingested_outcome` | 413 lines | 78-line shell + `core::worker_ingest` |
  | `advance_playback` | 271 lines | 52-line shell + `core::render_loop` |
  | acquisition pumps | 696 LOC, 0 tests | `core::acquisition` owns the decisions |
  | decoded / live-decoded | 348 lines | `core::worker_decoded` |

  `playback_manager` moved to `core` wholesale along the way.

## Phase C — the QA-gated UI batch

Bundled into one manual pass, because manual QA is this project's scarce
resource.

- **C1 — seam unification.** The review's sharpest finding was that the same
  action had different implementations in different files. Go-live/transport
  existed in three conventions; `ui/transport.rs` was deleted and its logic (and
  its tests — which existed *inside a UI file*, proving it was misplaced) now
  live in `core::transport`. The canvas click handler, the eight layer
  checkboxes (collapsed onto one `SetGeoLayer` intent), and the three competing
  intent-emission channels were all unified. Legitimate two-way egui bindings
  (26 widgets) are now explicitly marked so they read as sanctioned.
- **C2 — I/O out of the shell.** Site-modal geolocation and zip geocoding moved
  behind `Effect::LocateForSite` / `Effect::GeocodeZip` (with zip validation
  extracted as a tested pure decision); the external link became
  `Effect::OpenUrl`. `src/ui/` now contains exactly one browser call — a CSS
  safe-area-inset read, which is layout measurement, not I/O. This removed the
  last grandfathered edge.
- **C3 — decided, not deferred.** The blanket per-panel view-model migration was
  **killed** with rationale (see CORE_SHELL.md); the compiler-verified half was
  done instead — `LayoutCtx::diagnostics` narrowed to `&`, proving no layer
  mutates it, with every surviving `&mut` documenting the mutation that keeps it.

### One behavioral fix

Clicking "stop live" reset the state machine but never stopped the worker: the
app kept downloading and storing chunks indefinitely, with no live indicator to
reveal it. (Distinct from *detaching*, which intentionally keeps a **visible**
background stream.) An explicit stop now tears the channel down, matching the
error and site-change paths. This is the only intentional behavior change in the
program.

## Phase D — opportunistic cleanups taken

- The byte-identical `err_text` duplicated across both feed modules → `net`.
- Five hand-rolled copies of local/UTC timestamp splitting → one `TimeParts`
  primitive in `ui::time_format`.
- `DataFacade` renamed **`MainThreadStore`**: it never fronted the worker's write
  path, and the name implied it did. The two sanctioned storage entry points are
  now documented on the type.

Still open (unchanged from the review, and genuinely optional): schema-first
worker IPC, `streaming.rs` decomposition, generators for the hand-transcribed
sites/cities tables, `too_many_arguments` bundling.

## Documentation

ARCHITECTURE.md was rewritten against the tree, with per-file inventories
replaced by module-level responsibilities (file tables rot fastest) plus new
sections on the ratchet and the reducer pattern. INDEXEDDB.md's write contract
was replaced with the `upsert_scan` + `UpsertScanGuard` reality. TIMING.md and
STREAMING.md were corrected — including a silent drift nobody had caught: the
real-time scan key is now parsed from the Start chunk's volume header, not
derived from upload time minus median lag.

## Manual QA

The consolidated checklist — this program's items merged with the still-pending
functional-core migration pass — is in [QA_CHECKLIST.md](QA_CHECKLIST.md). It is
the only human touchpoint; everything else in this program is verified by the
compiler and the headless suite.
