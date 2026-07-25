# Architecture Review — 2026-07-25

A point-in-time review of the codebase on branch `functional-core-migration`
(~94k LOC Rust, 60+15 files in `src/ui/` + `src/nexrad/` alone), focused on
**clean APIs/interfaces, modularity, and clean abstractions**, and on locating
where accumulated iteration ("entropy") is concentrated.

Method: five parallel evidence-gathering audits, each grounded in `file:line`
citations, cross-checked against the intended design in
[ARCHITECTURE.md](../../ARCHITECTURE.md) and [CORE_SHELL.md](../CORE_SHELL.md).
The detailed audits are the appendices; this document is the synthesis and the
prioritized plan.

| Appendix | Scope |
|---|---|
| [audit-state-layers.md](audit-state-layers.md) | `src/state/`, `src/subsystem/`, `src/app/`, `src/core/`, `main.rs` |
| [audit-ui-shell.md](audit-ui-shell.md) | `src/ui/**` vs. the thin-shell standard |
| [audit-nexrad-pipeline.md](audit-nexrad-pipeline.md) | `src/nexrad/**`, `worker.js` |
| [audit-data-geo-support.md](audit-data-geo-support.md) | `src/data/`, `src/geo/`, `src/alerts/`, `src/mping/`, `src/net/` |
| [audit-dependency-matrix.md](audit-dependency-matrix.md) | Module dependency graph, cycles, visibility, test topology |

---

## Verdict

The codebase is in **substantially better shape than its iteration history
would suggest** — ~1,850 headless tests with the densest coverage exactly where
the standard demands it (the pure core), a real docs culture, and several
genuinely clean seams (worker decode spine, `data/` as a true leaf, the unified
retry policy, pure camera math). This is not a codebase that needs rescue.

The entropy is real, but it is **concentrated and has one dominant shape: five
concurrent half-finished migrations, each leaving two live conventions side by
side.** Half-finished is worse than not-started, because every reader (and
every future change) must guess which convention is authoritative:

1. The functional-core migration moved the *vocabulary* (Intent/Effect exist)
   but not the *mass* (the decision logic still lives untested in `src/app/`).
2. The projection/timing unification is at "Phase 0 wrapper" with **five
   overlapping owners** (~9,900 LOC).
3. The storage facade fronts only half the DB traffic (main-thread reads;
   worker writes bypass it).
4. The `Derived` view-model exists but covers ~3% of what panels read.
5. Modal/UI state consolidation stalled across **three homes** (`Chrome`,
   `AppState.datetime_picker`, `WorkbenchApp.modals`).

The single structural defect with the widest blast radius is **type placement**:
the domain data model (`Scan`/`Sweep`/`Radial`, `StormCellInfo`,
`RenderProcessing`, `SweepIdentity`, `AppCommand`) lives in `src/state/`, which
puts `state` at the center of every dependency cycle and **inverts the intended
layering** — `core` imports `state` 14×; the arrow `state → core` does not
exist at all. Fixing type placement is compiler-verified, needs no manual QA,
and unblocks nearly everything else.

## What is already strong (protect these)

- **Test discipline.** ~1,854 tests; `core` has the best density (~40/kLOC);
  every large logic file carries a real suite. The tested/untested split honors
  the standard with one exception (`src/app/`, below).
- **The worker decode spine.** `ingest_phases` primitives shared by archive and
  live paths; thin coordinator facades (`AcquisitionCoordinator` is 105 lines of
  pure delegation; `RenderCoordinator` delegates dedup to the pure core);
  structured worker errors with forward-compat (`#[serde(other)] Unknown`).
- **`data/` and `net/` are true leaves** — zero outward imports. `net::with_retry`
  genuinely wraps every outbound request (the one inline loop in streaming is a
  documented, justified exception).
- **`geo/camera.rs` is pure math** (2 egui value-type imports, zero GL/web_sys),
  well tested — the S4 carve-out claim holds.
- **Docs-as-decisions culture.** CORE_SHELL.md + the migration log honestly
  record what was deferred and why. Zero TODO/FIXME markers in code — debt is
  parked in prose, which is the right place. (The flip side: some docs have
  drifted; see F6.)
- **Model-citizen examples already exist in-tree** for every pattern this review
  recommends: `alerts_modal.rs` (all-intent UI), `DiagnosticsVm` (complete
  per-panel view-model), `TimelineFrame` (read-only frame projection),
  `PrevSweepAction` (effect-as-data), `handle_diagnostics_intent` (pure reduce →
  effects). The migration doesn't need invention, only propagation.

---

## Load-bearing findings

### F1. The layering is inverted at the root — `core` sits above `state`

Intended: `core (pure base) ← state ← subsystem ← app`, with `ui` reading
projections. Measured (see [audit-dependency-matrix.md](audit-dependency-matrix.md)):

- `core → state`: **14 edges**. `core::Intent` *is* `state::AppCommand`
  (`src/core/intent.rs:13`); core functions take `state::radar_data::{Scan,Sweep}`,
  `state::{AppState, AlertsState, …}` as inputs. `state → core`: **0 edges**.
- A genuine 3-cycle exists through the "pure core":
  `core → state → nexrad → core`.
- `state` is the crate's entropy nexus: imported by 8 of 11 modules (fan-in 100),
  and itself imports `nexrad` (17), `data` (21), `geo` (4), even `ui` (2:
  `ui::LocationResult` into `state/gps.rs`, `ui::colors::mode` into
  `state/app_mode.rs`).
- Root cause is **~10 misplaced types**, not intrinsic coupling:
  `Scan/Sweep/Radial` (forces core→state and nexrad→state),
  `StormCellInfo` (produced by `nexrad::detection`, housed in state),
  `RenderProcessing` (consumed by three nexrad renderers), `SweepIdentity`,
  `AppCommand`, `LocationResult` (defined in a UI modal, stored by state),
  `ViewMode`/`GeoLayerVisibility` (imported upward by `geo`).

`core` **is** pure in the I/O sense (zero production web_sys/js_sys — verified),
but it is not *independent*: it cannot be reasoned about, or one day extracted,
without dragging `state` along.

### F2. The decision mass still lives in the one untested module — `src/app/`

`src/app/` is 3,883 LOC of `impl WorkbenchApp` orchestration and is the **only
module with zero tests**. It is logic-bearing, not paint:

- `app/worker_results.rs` (1,056 LOC): `handle_chunk_ingested_outcome` is a
  **410-line** method interleaving live-engine mutation, GPU uploads, and render
  dispatch.
- `app/render_loop.rs`: `advance_playback` (270 lines) decides sweep matching,
  elevation snapping, prefetch — inline with GPU texture calls.
- `app/acquisition_intent.rs` computes prefetch windows *and* fires
  `fetch_listing` inline.

Meanwhile the `Effect` runtime carries **3 variants** against roughly **a dozen
real side-effect categories**; ~25 inline side-effect call sites in `app/`
bypass it. `command_dispatch.rs` is a router into imperative methods that do
I/O directly (`spawn_local` ×3 in the dispatcher itself); only the diagnostics
path does pure-reduce → effects. The local action enums (`PrevSweepAction`,
`DesiredDisplay`) are effect-*shaped* but still *executed* inline — effect
values without an effect runtime.

This is precisely the deferred half of P5/P6, and it is the real coverage gap:
the biggest untested files are decision files, not shell files.

### F3. The UI seam is *inconsistent*, which is worse than uniformly deferred

The thin-shell standard holds on the render leaves (all of `canvas_overlays/`,
timeline paint modules, `left_panel`, `alerts_modal` — 34 files clean). On the
interactive spine it is not just unmigrated but **split mid-action**
(see [audit-ui-shell.md](audit-ui-shell.md)):

- "Go live" exists in **three conventions**: `now_edge.rs` emits
  `AppCommand::StartLive` *and in the same function* directly mutates
  `playback.state` + `live.exit_live()`; `transport.rs` does the whole
  play/pause/live-detach decision as direct mutation; `playback_controls.rs`
  uses intents.
- One canvas click handler: mPING/alert selection via intents, site selection
  and distance tool via direct mutation.
- One panel's layer list: GPS/mPING toggles emit intents; the eight sibling
  checkboxes write `state.layer_state.geo.*` directly.
- **Three intent-emission channels** coexist: `push_command`,
  `&mut Vec<AppCommand>` parameters, and a local-Vec-then-drain idiom.
- Real I/O in the shell: `site_modal.rs` runs browser geolocation + an HTTP
  geocoding fetch + the UI's only `spawn_local`; `top_bar.rs` opens URLs;
  `canvas.rs` reads `js_sys::Date` directly, bypassing the injected `FrameNow`
  clock the codebase already has.
- Totals: ~150 direct field-assignment sites + 24 `.set_*` calls + 64 `chrome.*`
  toggles vs. ~49 intent emissions; 18 `AppCommand` variants.

### F4. State ownership is split four ways; the view-model is 4 fields wide

- `AppState` has 34 fields, of which ~41% are per-frame scratch (`frame_now`,
  `is_dark`, `is_mobile`, `width_tier`), one-shot coordination flags
  (`selection_just_finalized`, `start_live_on_site_select`, …), or UI-local
  state (`datetime_picker` — a modal's six text buffers on the root struct).
- Transient UI state lives in **three homes**: `subsystem::Chrome` (13 booleans),
  `AppState.datetime_picker`, `WorkbenchApp.modals: ui::ModalStates`.
- `subsystem::Derived` (the "view-model out" seam) has 4 fields, read 14×;
  panels reach into raw internals **~460×** (`playback.state.*` 192×,
  `viz_state.*` 182×). Coverage of the intended contract: ~3%. The one complete
  view-model (`DiagnosticsVm`) proves the pattern works.
- `subsystem::Playback` is a 1-field newtype; every call site pays `.state.`
  indirection (192× in ui) for zero behavior.
- `main.rs::update()` hands out ~19 simultaneous `&mut` borrows via `LayoutCtx`
  (8 of 11 fields `&mut`); `render_canvas_with_geo` takes 10 positional params.

### F5. `src/nexrad/` is four modules wearing one trenchcoat, with a mid-flight projection rewrite

- The **projection/timing concern is fragmented across five owners** (~9,900
  LOC): pure `timing/` (upstream-fork, disciplined), `projector.rs` (kernel),
  `projection/` (engine wrapping the kernel, self-described "Phase 0,
  incremental"), `streaming_plan.rs`, `streaming_state.rs`. Three of these
  simultaneously "own" projection state. This is the largest unfinished
  migration by volume.
- `realtime/streaming.rs`: 2,044 lines with an **883-line `streaming_loop`**;
  the ~35 free functions around it already form clean seams (loop-state /
  acquire / backfill / poll / persist) — the split is pre-drawn.
- Misfits: `persistence_manager.rs` (a thin shell over `core::decide_persist`,
  zero NEXRAD coupling) and `network_monitor.rs` (diagnostics telemetry) do not
  belong in the data-pipeline module.
- The worker protocol is **triple-defined** (Rust send structs → `worker.js`
  string literals + hand-remapped fields → Rust receive structs). Rust-side
  tests pin the Rust literals; nothing ties `worker.js` to them — a JS-side
  rename fails only at runtime.
- Three GPU renderers share shaders partially but no lifecycle abstraction;
  constructor contracts differ (`Result` vs infallible); archive vs live render
  marshaling is copy-pasted and has silently diverged (`azimuth_spacing_deg`).

### F6. The canonical docs have drifted from the tree

ARCHITECTURE.md lists `streaming_manager.rs` and `realtime.rs` (neither exists),
omits the entire 4,025-LOC `projection/` module, and lists a removed `SweepMeta`
type. INDEXEDDB.md documents a `create_scan`/`put_scan` write contract that was
consolidated into `upsert_scan` (+ an undocumented `UpsertScanGuard`).
TIMING.md §3b disagrees with STREAMING.md on the live poll pad (400 ms vs the
actual 750 ms in `timing/config.rs:42`). For a repo whose docs are load-bearing
(they gate agent work), drift is expensive.

### Secondary inventory (smaller, real)

| Item | Evidence |
|---|---|
| `data/keys.rs` (1,800 LOC) has accreted the blob wire-format engine, VCP timing physics, and a live-volume anchor state machine behind the name "key types" | audit-data-geo-support §3 |
| `DataFacade` fronts only main-thread read/eviction; worker write + render-read paths use `IndexedDbStore` directly (justified, but the name oversells) | audit-data-geo-support §1 |
| `alerts/` vs `mping/`: `channel.rs` byte-identical, `err_text` byte-identical, `fetch_attempt` skeleton near-cloned (~120 removable lines); manager *policies* legitimately differ | audit-data-geo-support §4 |
| Time formatting hand-rolled in ≥4 places despite `ui/time_format.rs` existing | audit-ui-shell §8 |
| Visibility is decorative: 926 `pub fn` (state+nexrad export ~650 into a single-binary crate); 447 `pub` fields in `state/`; 87 `#[allow(dead_code)]`; 52 `too_many_arguments` | audit-dependency-matrix §4 |
| `geo::projection` (spatial) vs `nexrad::projection` (temporal) name collision — unrelated concepts sharing a name | audit-data-geo-support §5 |
| Static data: zones generated via `tools/build_zone_geometry.py`, but sites (207 entries) and cities (135) are hand-transcribed with no regeneration pipeline | audit-data-geo-support §6 |
| 51 unused underscore params (35 in `shortcuts.rs` alone) from grab-bag signatures | audit-ui-shell §9 |

---

## Recommendations

Sequenced by **verification cost**, because that is this project's scarcest
resource: manual QA passes are expensive (no headless UI runs), while the
compiler and the headless test suite are free. Phases A and B need no manual
QA at all; Phase C is deliberately batched into one QA pass.

### Phase A — Compiler-verified structural repair (highest leverage, lowest risk)

**A1. Rehome the misplaced types; make the dependency arrows match the doc.**
Move the domain data model below everything that consumes it — either into
`core` or a new `src/domain/` that `core`, `state`, and `nexrad` all sit on:
`radar_data::{Scan, Sweep, Radial}`, `SweepIdentity`, `RenderProcessing`,
`StormCellInfo`, and `AppCommand` (making `Intent` the definition, `AppCommand`
the alias — reversing today's direction). Move the strays down:
`LocationResult` out of `ui/site_modal.rs`, `ViewMode`/`GeoLayerVisibility`
into `geo`, and break `state/app_mode.rs`'s import of `ui::colors` (let the
shell map mode → color). Use temporary `pub use` re-exports at the old paths to
cap the diff, then burn them down. This one move erases `core→state`, most of
`nexrad↔state`, `state→ui`, and `geo→state`. Pure refactor; `cargo check` +
existing tests verify it.

**A2. Freeze the layering with an architecture test.**
A plain `#[test]` in the fast suite that scans `use crate::` lines per
top-level module against an allowed-edge table (the whole crate is one binary,
so this is a 50-line test, not tooling). Without this, Phase A's win erodes;
with it, every future violation is a red test at commit time. This is the
single best anti-entropy investment available.

**A3. Regroup `src/nexrad/` into intentional submodules and evict the misfits.**
`acquisition/` (download, download_queue, archive_index, cache_channel,
coordinator), `live/` (realtime, streaming_state/filter/plan), `decode/`
(record_decode, ingest_phases, worker_api, decode_worker), `render/`
(gpu_renderer, globe/volume renderers, color_table, national_mosaic,
render_coordinator/request), `projection/` (already), `analysis/` (detection).
Move `persistence_manager.rs` → `src/app/`, `network_monitor.rs` next to its
owner (diagnostics). `git mv` + path fixes; no behavior change.

**A4. Visibility diet + dead-surface purge.**
Default to private/`pub(super)`/`pub(crate)`; nothing external consumes the
~650 `pub fn` in state/nexrad. Then let the compiler surface what the 87
`#[allow(dead_code)]` are hiding and delete what is truly dead (keep the
documented `timing/` fork exemption). Shrinking the reachable surface is what
makes every later refactor cheaper.

**A5. Resync the docs (F6 list is the worklist).**
Fix ARCHITECTURE.md's ghost files and missing `projection/`, INDEXEDDB.md's
`upsert_scan` contract, TIMING.md's poll constants. Consider making
ARCHITECTURE.md's per-file tables coarser (module-level, not file-level) —
file inventories are the fastest-rotting doc form.

### Phase B — Move the decision mass into the core (headless tests verify; no UI churn)

**B1. Extract `src/app/`'s decisions into pure reducers** — the actual
completion of P5/P6's non-UI half. Priority order by risk×size:
`worker_results.rs` (its 410-line handler decomposes into
`(state, outcome) → {state updates, local action enums for GPU/worker}` with
the existing `PrevSweepAction` idiom), then `render_loop.rs::advance_playback`,
then the acquisition pumps. Target: `src/app/` files become
build-inputs → call core → execute effects, and the module leaves the
zero-test column. This directly attacks the largest untested logic mass.

**B2. Land the projection consolidation.**
Finish what `projection/mod.rs` Phase 0 started: one owner (`ProjectionEngine`),
`Projector` demoted to its internal kernel, ownership out of `StreamingState`,
and the six `#[allow(unused_imports)]` re-exports resolved. Every month this
sits half-done, live-mode changes pay a five-owner comprehension tax.

**B3. Split `data/keys.rs` along its four actual concerns** —
`keys` (key vocabulary), `blob_format` (the 72-byte wire format), `vcp_timing`
(sweep-duration physics), `live_anchor` (provisional→confirmed state machine).
Mechanical; tests move with the code.

### Phase C — One QA-gated UI batch (bundle everything needing eyes into a single manual pass)

**C1. Unify each split seam to one convention, flow by flow:** go-live (one
path — intent), transport, the right-panel layer toggles, the canvas click
handler. Collapse the three intent channels into `push_command`. Where a
two-way egui binding must stay `&mut` (sliders, text edits), mark it with a
standard comment so sanctioned bindings are distinguishable from violations.

**C2. Move `site_modal`'s I/O behind effects** (geocode fetch, geolocation —
the executor for `Effect::StartGeolocation` already exists), and route the
stray `js_sys::Date` reads through the injected clock.

**C3. Grow per-panel view-models on the `DiagnosticsVm` pattern**, one panel at
a time (left panel first — its derivation is already pure in `core::panels`),
shrinking `LayoutCtx`'s eight `&mut` fields as each panel converts.

All three are the changes the migration log correctly refused to do blind;
batching them against the already-pending manual-QA checklist
(CORE_SHELL_MIGRATION_LOG.md §2) amortizes the QA pass.

### Phase D — Opportunistic (do when touching the area; don't schedule)

- **Worker IPC single source of truth**: generate `worker.js`'s dispatch table
  (or at minimum a shared JSON constants file + a node-side test) so a JS
  rename can't drift silently.
- **`streaming.rs` decomposition** on its next substantial change — the seam
  map is in [audit-nexrad-pipeline.md](audit-nexrad-pipeline.md) §5.
- **Shared polling-feed skeleton** for alerts/mping (channel + `err_text` +
  request-building; leave the divergent cadence policies alone).
- **Time formatting** consolidated into `time_format.rs`.
- **Generators for sites/cities** matching the zones pipeline.
- **`DataFacade` honesty**: either extend it over the write path or rename it
  (`MainThreadStore`?) and document the two sanctioned entry points.
- Renderer lifecycle trait — optional; buys uniformity, not deduplication.

### The meta-rule

Adopt **finish-or-kill for migrations**: at most one convention-changing
migration in flight per layer, and "done" includes deleting the old convention
and updating the doc. The five half-states above are where nearly all of this
codebase's entropy lives; the discipline that produced the migration log is
exactly the discipline that can close them.
