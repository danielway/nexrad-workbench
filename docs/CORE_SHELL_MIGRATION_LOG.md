# Functional-Core / Thin-Shell Migration — Decisions Log

Running log of decisions, assumptions, and resolutions made during the autonomous
migration to the functional-core / thin-shell standard (see
[CORE_SHELL.md](CORE_SHELL.md)). Branch: `functional-core-migration`.

Each entry: the ambiguity or choice, the option taken, and the rationale. The
consolidated manual-QA checklist and final report live at the end of this file
once the migration is complete.

## Conventions adopted

- **`core` module location.** A new `src/core/` module holds the contract
  vocabulary (`Intent`, `Effect`) and the pure decision functions the shell
  executes. Pre-existing pure decision fns in `src/state/**` stay where they are
  (moving 500+ tests and their call sites is pure churn and risk); `core`
  re-exports them so there is one canonical "this is the core" surface, matching
  the roadmap's "pure `derive_*` fns re-homed under the core" with a re-export
  rather than a physical move where a move buys nothing.
- **Behavior preservation is paramount.** Every numeric default, clamp, ordering,
  and observable behavior is preserved exactly. Where a decision is extracted into
  a pure fn, the shell call site is reduced to: build inputs → call core → execute
  returned effects, with the same values as before.

## Decisions

### D0 — Baseline
- Branched `functional-core-migration` off `simplify-user-interface` @ `09241a3`.
- Verified green baseline before any change: `cargo check` clean,
  `cargo clippy --bin nexrad-workbench -- -D warnings` clean,
  `cargo test --bin nexrad-workbench` = 510 passed / 0 failed.

### D1 — P0 contract types
- **Module name `core`.** Declared `mod core;` in `main.rs`. Rust's std `core`
  crate is referenced only via leading-`::` paths (derive macros) and one
  qualified `i_overlay::core::` import, so a crate-level `core` module does not
  collide. Chose `core` over `app_core`/`fcs` because the docs and standard call
  it "the core" — clarity wins.
- **`Intent` = alias of `AppCommand`** (`pub use crate::state::AppCommand as Intent`)
  rather than a rename. Renaming 200+ call sites is churn with no behavior value;
  the alias establishes the contract name now and the type grows into the superset
  as P5 folds UI mutations into it.
- **`Effect` is the simple cross-cutting effect type, not a monolith.** The
  codebase already returns effects-as-data via local enums (`PrevSweepAction`,
  `DesiredDisplay`, `QueueAction`). The roadmap says "imitate PrevSweepAction."
  So `core::Effect` carries only effects that share one executor (URL push,
  prefs save, geolocation later); heavy per-decision effects (GPU buffer uploads,
  worker dispatch) keep their own local action enums at the granularity that fits
  a buffer/`postMessage`. This is the lowest-risk, most idiomatic reading and
  avoids an Effect variant that has to carry `Vec<f32>` + `egui::Context`.
- **Derive additions (behavior-neutral):** `ViewState` gained `Debug, Clone,
  PartialEq`; `UserPreferences` gained `Debug`; `InterpolationMode` gained
  `Debug` — all so `Effect` can carry them and be `assert_eq!`-able in headless
  tests. Pure data enums/structs; no logic change.
- **`#[allow(unused_imports)]` on the `core` re-exports** until P1 consumes them
  (bin crates flag `pub use` as unused). Same staging idiom as the existing
  `#[allow(dead_code)]` on `PrevSweepAction`.

### D2 — P1 effect boundary + injectable clock
- **Wall-clock throttle, not monotonic.** The old throttle used
  `web_time::Instant` (monotonic). The roadmap says "extend the existing
  `FrameNow` seam," and `FrameNow` is wall-clock (Unix seconds from `Date::now`).
  Switched the throttle to compare injected wall-clock seconds. Behavior is
  observably identical for a ~1/sec gate; the only divergence is if the system
  clock jumps backward by >1s mid-session (extremely rare, and self-corrects next
  push). This is the cost of making the decision clock-injectable/testable, which
  the standard requires. Seeded `last_url_push_secs` with the construction-time
  wall clock so the first push still waits a throttle window (preserving the old
  `Instant::now()` seed semantics).
- **`decide_persist` returns `PersistDecision`, not bare `Vec<Effect>`.** The
  roadmap sketches `-> Vec<Effect>`; I return `{ effects, last_url_push_secs?,
  saved_preferences? }`. Reason: the throttle marker and prefs snapshot are the
  decision's "next state" — folding them into the return keeps the bookkeeping
  pure and directly testable ("unchanged prefs emit no `SavePreferences` and
  advance nothing"), instead of having the manager re-derive them by inspecting
  the effects. `decision.effects` still *is* the `Vec<Effect>`.
- **Effect runtime lives at `src/app/effects.rs`** as `impl WorkbenchApp`
  (`apply_effects`/`apply_effect`). One exhaustive match so a new variant forces
  a shell decision. This is the "effect runtime / imperative shell" box from the
  standard's diagram.

### D3 — P2 Diagnostics reference slice
- **`DiagnosticsIntent` sub-enum wrapped by `AppCommand::Diagnostics(..)`**, not
  flat variants. Folded the existing flat `OpenAlert`/`CloseAlert`/`RefreshAlerts`
  into it (roadmap: "folding the existing alert AppCommands in"). `CloseAlert` had
  no emit site, so it was dropped (its behavior split into `ClearAlertSelection` +
  `CloseAlertList`). `DiagnosticsIntent` lives in `core::diagnostics`; `AppCommand`
  (in `state`) references it — module cycles are fine in Rust.
- **`reduce(DiagnosticsStateMut, intent) -> Vec<Effect>`**, a borrow-bundle of the
  state it touches (`alerts`, `mping`, `gps`, and `gps_layer_active` = the
  `layer_state.geo.gps_location` toggle the GPS-failure path auto-clears). One
  total reducer; the only effect is `StartGeolocation`. Everything else
  (`refresh_requested`, `invalidate_requested`, selections) is pure state
  mutation, not an Effect — those flags are read by the manager ticks, not I/O.
- **`ShowAlertOnMap(id)` is a separate top-level `AppCommand`, not a
  `DiagnosticsIntent`.** "Show on map" mutates `viz_state`/camera (P5/carve-out
  territory), which `DiagnosticsStateMut` doesn't carry. Handled in the shell
  (`command_dispatch`) via the pure `compute_alert_focus(alert) -> AlertFocus`,
  keeping the *decision* (which layer, which centroid) tested while the camera
  write stays shell. Calls the already-verified `camera.center_on` (S4 carve-out).
- **`GpsState`/channel stays out of the core.** `Effect::StartGeolocation` carries
  no payload; the shell pulls `gps.result_sender()` + `ctx` when executing. GPS
  *results* (the async drain) are fed back through the same `reduce` as
  `GpsResolved`/`GpsFailed` intents, applied inline in `update()` (not queued) to
  preserve same-frame display.
- **`DiagnosticsVm` is an owned snapshot** (clones the severity-sorted visible
  alerts), built once per frame from `derived.visible_bounds` and threaded via a
  new `LayoutCtx.diagnostics_vm` field. The chip keeps its own `visible_bounds`
  None-check (3D globe → render nothing) — distinct from "bounds present, no
  alerts". Only the genuine UI-side computation (`AlertsState::visible_in`) was
  VM-ified; trivially-projected overlay reads were left direct (pure projection,
  not logic).
- **`alerts::types` made `pub(crate)`** (was private) so the cross-module
  `core::diagnostics` tests can build `Alert`/`AlertGeometry`/`Ring` fixtures.
  Chose this over crate-wide re-exports, which would read as unused in non-test
  builds and trip the clippy gate.
- **One-frame lag is now uniform for diagnostics modal/selection interactions**
  (they go through the command queue, drained next frame) — same as the
  pre-existing `OpenAlert` path; imperceptible and consistent.

### D4 — P3 canvas decision extraction
- **Pure functions, not a monolithic `RadarFrameVM`.** The roadmap sketches
  `core::render_view_model(...) -> RadarFrameVM`. I extracted the *decisions* as
  pure functions in `core::canvas` and left `ui::canvas`'s paint orchestration
  in place, rather than restructuring the hot paint path around one VM struct.
  Reason: P3 is the highest-regression-risk phase and I cannot run the egui/GL
  app, so I minimized churn to the paint sequence. The decisions are all
  individually pure + tested, which is what the standard actually requires; a
  later cosmetic bundling into a struct is cheap if wanted.
- **`value_at_polar` decoupled from the GL object via `PolarSweepMeta`.** The pure
  lookup takes plain slices + a metadata struct; `RadarGpuRenderer` builds the
  meta from its `SweepState` and passes its CPU shadow buffers. Behavior copied
  line-for-line (sentinel `raw <= 1.0`, `scale == 0.0` passthrough, sparse vs
  evenly-spaced azimuth indexing for current vs prev), then covered by tests.
- **`find_nearest_azimuth_index` moved** from `gpu_renderer` (where it was a
  private fn used only by the inspector) into `core::canvas` — core must not
  depend on the renderer, so the pure helper belongs in core and the renderer
  calls it from there.
- **Behavior-preservation note for QA:** `select_gpu_sweep` only computes
  `sweep_bounds` when `live_range.is_none() && animation` (was nested in the
  original `else if`), so the timeline lookup is skipped in exactly the same cases.
  `between_sweeps` reads the *post-update* cache (matches original ordering).

### D5 — P4 GPU & worker effects as data
- **No monolithic `decide_render(...) -> Vec<Effect>`.** Much of the render path
  was *already* effect-as-data via local action enums: `PrevSweepAction`
  (prev-sweep texture) and `DesiredDisplay` (main sweep) are exactly the
  "imitate PrevSweepAction" pattern, and the GPU upload/clear + worker dispatch
  are the shell executing them. Folding those into one `Effect` enum would force
  `Vec<f32>` gate buffers + `egui::Context` + worker handles into variants — the
  anti-pattern D1 explicitly avoided. So P4 = close the *remaining inline*
  decisions, not rewrite the working ones.
- **Extracted the prefetch-next-sweep decision** (`decide_prefetch_next_elevation`)
  out of `advance_playback` — genuinely tangled timeline logic, now pure + tested.
  The shell wrapper preserves the original's lazy future-scan lookup (only when
  the current sweep is last in its scan) so no extra timeline walk happens.
- **Formalized the dedup gate** as `should_dispatch<T: PartialEq>(new, last)` used
  by `RenderCoordinator::request_render_for` and `request_volume_render`. The gate
  was already a pure `==`; extracting it gives the roadmap's named "dedup
  correctness" seam a unit test. The stateful dedup *cache* (`last_render`,
  `set_last_render` on prefetch) stays in the coordinator (shell), per the
  carve-out.

### D6 — P5 scope decision (partial)
- **What I did:** moved the pure panel derivations into `core::panels` and routed
  the panels through them — `query_radar_state_at_timestamp` (the biggest
  UI-resident derivation), `animation_frozen`, `archive_azimuth_from_progress`,
  `status_message_visibility`. Leaf helpers are unit-tested; the radar-state move
  is a behavior-identical relocation (its leaf helpers are the tested part —
  `Live` isn't headlessly constructible, so the whole function can't be unit-tested
  without a fixture, noted for the QA pass).
- **What I deferred and why:** the roadmap's full P5 — replace every `&mut state`
  mutation in the interactive panels with intents and reshape `LayoutCtx` into
  `&ViewModel` + an intent sink — is **deliberately not done blind.** Three
  reasons, all rooted in the project's own rules:
  1. *Behavior-preservation is paramount and I cannot run the egui/WebGL app.*
     Converting immediate `&mut` mutations to queued intents introduces one-frame
     lag and reorders effects; on an interactive hot path that is exactly the
     class of regression only manual QA catches. The roadmap itself gates P5
     "behind manual QA, file-by-file."
  2. *Several mutations must stay `&mut` anyway* (per the P5 coupling map):
     camera/WASD motion and `playback.advance(dt)` are frame-tier and
     high-frequency; two-way `ui.checkbox(&mut field)` bindings need wrapper
     widgets, not a blind find-replace.
  3. *The pattern is already proven.* P2 demonstrates the full `intent → reduce →
     effect → VM` loop end-to-end for a whole feature area; P5's remainder is
     mechanical application of it, which is safe to do incrementally alongside the
     human QA pass rather than in one un-verifiable sweep.
  The end-QA checklist calls out the panels whose mutation→intent conversion
  remains, so the follow-up is scoped.

### D7 — P6 acquisition decisions
- **Extracted the selection-fetch gate** (`decide_selection_gate -> SelectionGate
  {Arm, Confirm}`) — the roadmap's named "selection-gate decisions assertable"
  seam. The shell (`resolve_selection_fetch_gate`) maps `Arm`→arm the bulk pump,
  `Confirm`→open the modal; the `armed_at_secs`/target construction (needs `now`)
  stays shell.
- **Re-homed the already-pure acquisition fns** (`reactive_prefetch_allowed`,
  `dates_spanning`, `dates_in_range`) from `app::acquisition_intent` into
  `core::acquisition`, moving their tests with them and adding coverage for the
  date spans (single-day, midnight cross, interior-day enumeration, reversed
  range). This puts the acquisition core surface in one place.
- **Pump I/O sequencing stays shell.** The pumps (`pump_implicit_prefetch`,
  `pump_selection_fetch`, `pump_visible_listings`, `pump_lookback_backfill`)
  interleave decisions with listing fetches + queue enqueues + backoff bookkeeping.
  Their *pure* decisions are now in core/tested; folding the I/O dispatch itself
  into a `decide→Vec<Effect>` rewrite is the same un-QA-able ordering risk as P5's
  interactive surface (and the `download_queue` state machine — the real dispatch
  logic — is already pure + 30-test-covered). So P6 extracts the gates and leaves
  the I/O sequencing in the shell, consistent with the worker-dispatch carve-out.
- **`streaming.rs` untouched** — the carve-out explicitly defers its split.
