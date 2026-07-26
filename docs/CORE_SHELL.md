# Architecture Standard: Functional Core, Thin Shell

**Status: binding standard for all new and changed code; existing code is being
migrated to it (roadmap below).** The one-paragraph rule lives in
[CLAUDE.md](../CLAUDE.md); this is the full reference — the contract, the seams in
this codebase, what counts as a violation, how to test, and the migration plan.

## Why

The UI is egui drawing to a WebGL canvas. That makes it expensive and unreliable
to verify *programmatically* — asserting "the right data is shown" or "this
interaction did the right thing" means a browser, a headless driver, and
screenshots. So we move **all behavior out of the UI** and behind a headless
interface that can be driven and asserted in plain unit tests. The GUI is then a
trivial projection: small enough that a human can validate "it draws the
view-model correctly" by eye, while every *decision* is covered by fast,
deterministic Rust tests.

## The rule

Three layers; only two things ever cross the boundary.

```
   user input ──▶ intent ──▶┌───────────────┐──▶ effect ──▶┌────────────┐
                            │ FUNCTIONAL    │              │ EFFECT     │──▶ IO
                            │ CORE          │◀── result ───│ RUNTIME    │◀──
   egui/canvas ◀── view-model│ (pure, all    │              │ (shell)    │
                            │  state+logic) │              └────────────┘
                            └───────────────┘
```

1. **Functional core — owns all state and all business logic, and is pure.**
   A decision is `(state, intent) -> (next state, effects)`: it mutates in-memory
   state and *returns a description of any I/O to perform*. It never does I/O
   itself, never touches egui/web-sys/GPU, and never blocks on async. It is
   unit-tested with no browser and no runtime.
2. **Effect runtime — the imperative shell for I/O.** It executes the `Effect`
   values the core returns (IndexedDB, HTTP, Web Worker dispatch, GPU upload,
   localStorage, geolocation, URL/history, timers) and feeds results back into the
   core as new intents. Effects are *described* by the core and *performed* here,
   so the deciding logic is testable without performing them.
3. **UI shell — egui panels, canvas painting, input.** It renders the **view-model**
   the core produces and translates user input into **intents**. No business
   logic, no state mutation, no I/O.

**The contract: intents in, view-model out — nothing else crosses.** Every feature
is validated by `construct core → send intents → assert (view-model, effects,
state)`.

## The seams in this codebase

We are not starting from scratch — three of the four seams already exist; the
effect boundary is the new piece.

| Seam | Role | Today (2026-07) |
|---|---|---|
| `Intent` ([intent.rs](../src/core/intent.rs)) | **Intents in** | Defined in the core (the old `AppCommand` name is gone). UI code pushes intents via `AppState::push_command`; the main loop drains and dispatches them. Remaining gap: the interactive panels' direct `&mut` writes (the deferred P5 batch below). Goal: `Intent` becomes the *only* way the UI changes anything. |
| `subsystem::Derived` ([derived.rs](../src/subsystem/derived.rs)) | **View-model out** | Exists but minimal — a small per-frame cache; panels still read subsystem internals directly at scale. `DiagnosticsVm` is the one complete per-panel view-model (the pattern to propagate). Goal: a *complete* per-panel view-model the panels read exclusively. |
| The subsystems + `AppState` | **State owners** | Exist (S1) and are thin/clean. The decision mass that used to live in `src/app/*` has been extracted into pure core reducers (see the status note below); `src/app/*` is now assemble → reduce → execute shells. |
| `Effect` ([effect.rs](../src/core/effect.rs)) | **Effects out** | Exists; executed by the shell runtime [`WorkbenchApp::apply_effects`](../src/app/effects.rs). Carries the simple cross-cutting effects (URL push, prefs save, geolocation). Heavy per-decision effects use local action enums — the `resolve_prev_sweep -> PrevSweepAction` idiom ([playback_manager.rs](../src/core/playback_manager.rs)) — now generalized into the Env/Slices/Actions reducer pattern (see ARCHITECTURE.md). |

## What counts as a violation

In `src/ui/**` (panels, canvas, canvas_interaction, overlays, modals, shortcuts,
mobile), or any egui/web-sys/GPU code:

- ❌ Business logic, math, or state derivation (sweep matching, geometry,
  thresholds, formatting decisions). → move to the core; expose the result on the
  view-model.
- ❌ Mutating `AppState`/subsystem state directly. → emit an `Intent`.
- ❌ Reading `AppState`/subsystem internals to compute what to show. → read the
  view-model (`Derived`); add a field if it's missing.
- ❌ Performing I/O (fetch, IDB, `postMessage`, GPU upload, `localStorage`,
  geolocation, history). → return an `Effect` from the core.

In the core: ❌ any `egui::`, `web_sys::`, `js_sys::`, GPU, or `.await`-on-I/O in
decision logic. Pure in, pure out; effects are values.

### What is explicitly NOT a violation

Two patterns look like violations at a glance and are sanctioned. Both were
decided deliberately (2026-07, arch-health); treat them as settled rather than
as debt to burn down.

**1. A render leaf reading `AppState` to paint.** The view-model exists to carry
*derivations*, and every derivation belongs in the core (`core::panels`,
`DiagnosticsVm`, `core::canvas`). A panel that reads a field and draws it has no
decision to test, so wrapping that read in a per-panel struct buys ceremony, not
safety — the compiler already finds every site when state changes shape. Grow a
view-model when a panel *computes*; not when it merely displays. Mutation is a
different matter and stays forbidden.

**2. Direct-manipulation gestures calling a subsystem command method.** Intents
are drained at the top of `update()`, so an intent emitted while painting is
applied on the *next* frame. That is invisible for a button press and wrong for a
drag: scrub/jog gestures (`Live::detach_playhead` and friends) would trail the
cursor by a frame. Those gestures call the subsystem method synchronously — but
the *decision* still lives in the core (`core::transport::detach_playhead`), and
the subsystem method only executes what the core returned. The rule is therefore
"the decision is pure and testable", not "the call is deferred".

## How to test (the recipe)

The whole point. A feature test never touches egui or a browser (sketch — the
real seams are the per-decision reducers and `reduce` functions in `src/core/`):

```rust
let mut core = Core::new(/* fixture state */);
let effects = core.handle(Intent::SeekTo(ts));          // send an intent
assert_eq!(core.view_model().displayed_frame_ts, ts);   // assert the projection
assert!(effects.contains(&Effect::RenderSweep { .. })); // assert the I/O it asked for
// the effect is NOT executed — we assert the *decision*, not the side effect
```

For effect *results*, feed them back as intents (`Effect::Fetch` → runtime →
`Intent::FetchCompleted(..)`) and assert the next state. The egui layer gets
only shallow visual QA, because by construction it is a 1:1 projection of
`view_model()`.

## The rendering & GPU carve-out

Drawing radar is inherently shell work, but the *decisions* around it are core:

- **Core (pure, tested):** which sweep matches the playhead (the 0–2 rule),
  `value_at_polar`, storm-cell detection, camera/projection math
  (`geo_to_screen`/`screen_to_geo` round-trips), overlay visibility, what the
  view-model says to draw.
- **Shell (effects / paint):** uploading raw gate values to the GPU texture
  (an `Effect`), the fragment-shader pass, and egui painting of the view-model.

So the view-model carries *what to render* (sweep handle/params, camera, overlay
set, computed cells); the shell turns that into texture uploads and draw calls.
The pure geometry and matching are core and fully testable; only the pixel-pushing
needs eyes.

## Migration roadmap

Grounded in the 2026-06-14 coupling audit. Build on the existing seams,
lowest-risk-first, behavior-preserving; each phase establishes a concrete headless
test seam. Effort S/M/L = relative size. **Status: P0–P4 + P6 complete; P5 partial
(pure logic extracted, broad mutation→intent rewrite QA-gated).** Decisions log,
per-phase commit hashes, and the consolidated MANUAL-QA checklist:
[CORE_SHELL_MIGRATION_LOG.md](CORE_SHELL_MIGRATION_LOG.md).

**Status update (2026-07, branch `arch-health`).** The 2026-07 architecture
review (docs/arch-review-2026-07/) Phases A+B completed the non-UI half of this
migration:

- **Type rehoming done** — the domain data model lives below everything in
  [`core/domain/`](../src/core/domain/) (`Scan`/`Sweep`/`Radial`/`RadarTimeline`,
  playback time model, viz types, `UserPreferences`, errors, feeds, telemetry,
  worker outcomes). `state → core` is the arrow now; `core → state` is gone.
- **`Intent` is defined in the core** ([`core/intent.rs`](../src/core/intent.rs));
  the `AppCommand` name no longer exists anywhere.
- **The `src/app/` decision mass moved into pure reducers** — the completion of
  B1: [`core::worker_ingest`](../src/core/worker_ingest.rs),
  [`core::worker_decoded`](../src/core/worker_decoded.rs),
  [`core::render_loop`](../src/core/render_loop.rs) (advance-playback), and the
  acquisition pump reducers in [`core::acquisition`](../src/core/acquisition.rs).
  `src/app/*` files are now assemble → reduce → execute shells. The reducer shape
  (Env/Slices/Actions) is documented in ARCHITECTURE.md.
- **Projection + timing consolidated in the core** (B2) —
  [`core/projection/`](../src/core/projection/) (`ProjectionEngine` sole owner,
  `projector.rs` demoted to its private kernel) and
  [`core/timing/`](../src/core/timing/) (the upstream fork, moved intact).
- **`playback_manager` moved into the core**
  ([`core/playback_manager.rs`](../src/core/playback_manager.rs)) so the
  render-loop reducer can compose it.
- **The layering is now enforced** by the build-time ratchet
  (`tools/arch_check.rs`, run from `build.rs`) — see ARCHITECTURE.md
  "Architecture Enforcement".

**Still deferred (the one QA-gated UI batch — review Phase C):** P5's broad
`&mut` → intent rewrite for the interactive panels, the `LayoutCtx` →
`&ViewModel` + intent-sink reshape, and moving `site_modal`'s I/O (geocode
fetch, geolocation) behind effects. These remain pending exactly as documented
in the P5 entry below and CORE_SHELL_MIGRATION_LOG.md §4.

- **P0 — Contract types (S). ✅ DONE.** Introduced `Intent` (initially an alias
  of the then-`AppCommand`; since 2026-07 the definition itself lives in
  [`core/intent.rs`](../src/core/intent.rs) and the alias is gone) and an
  `Effect` enum, modeled on `PrevSweepAction`, in the new `src/core/` module.
  No behavior change. The vocabulary exists; heavy per-decision effects keep
  their own local action enums (the `PrevSweepAction` idiom), `core::Effect`
  carries simple cross-cutting effects.
- **P1 — Effect boundary + injectable clock (S→M). ✅ DONE.** Established the
  effect boundary: the core returns `Vec<Effect>` and the shell's effect runtime
  ([`WorkbenchApp::apply_effects`](../src/app/effects.rs)) executes them.
  `persist_if_due` is now a thin shell over the pure
  [`core::decide_persist`](../src/core/persist.rs) `-> PersistDecision { effects,
  tracking }`; the throttle clock is injected as `FrameNow` wall-clock seconds
  (replacing the monotonic `Instant`). 6 headless tests cover the throttle gate,
  boundary, and prefs change-detection. Other effect categories (GPU/worker/IDB/
  geolocation) are introduced by the phase that needs them rather than all here.
- **P2 — Reference slice: Diagnostics overlays (M). ✅ DONE.** Migrated alerts +
  mPING + GPS end-to-end, proving the full `intent → core → view-model → shell`
  loop. New [`core::diagnostics`](../src/core/diagnostics.rs): pure
  `select_alert_at` (hit-test + severity-rank tie-break) and `compute_alert_focus`;
  a `DiagnosticsIntent` + pure `reduce(state, intent) -> Vec<Effect>`; and a
  `DiagnosticsVm` (severity-sorted visible alerts) the chip + list modal render.
  `Effect::StartGeolocation` joins the effect runtime. The canvas, both alert
  modals, the mPING modal, the right panel, the top bar (desktop + mobile), and
  the site modal now emit intents instead of mutating overlay state. The GPS
  result drain routes through the same reducer (auto-off-on-failure stays a tested
  rule). 14 headless tests (overlapping warning+advisory → highest rank, class
  gating, tie-break, mPING gating, GPS enable→effect / fail→auto-off, key
  save/clear). Deferred: VM-ifying trivially-projected overlay reads (pure
  projection, no logic) — left direct.
- **P3 — Canvas decision extraction (M→L). ✅ DONE.** Moved the canvas decision
  math into the pure [`core::canvas`](../src/core/canvas.rs): sweep-line azimuth
  interpolation (`sweep_line_azimuth`), the live/archive sweep selection +
  between-sweeps + cache rules (`select_gpu_sweep` / `next_sweep_cache` /
  `between_sweeps`), the data-probe `value_at_polar` lookup (`value_at_polar_current`
  / `_prev`, `collection_time_*`, `find_nearest_azimuth_index` moved out of
  `gpu_renderer`), `polar_in_prev_region`, and the geometry (`geo_to_polar`,
  `cutout_lon_range_deg`). `value_at_polar` is decoupled from the GL object — the
  renderer now binds the pure lookup to its CPU shadow buffers via
  `PolarSweepMeta`. 13 headless tests. GPU paint stays shell (carve-out). Risk:
  hot path — **gate behind manual canvas QA** (sweep animation, hover probe).
  Deferred: a single bundled `RadarFrameVM` struct — extracted as pure functions
  instead, to keep the paint orchestration untouched (lower regression risk with
  no runnable QA).
- **P4 — GPU & worker effects as data (L). ✅ DONE.** The render path's
  effect-as-data was largely already realized via local action enums
  ([`PrevSweepAction`](../src/core/playback_manager.rs) for the prev-sweep
  texture, [`DesiredDisplay`] for the main sweep) — that *is* the PrevSweepAction
  idiom the standard endorses, at a granularity that suits buffer uploads /
  `postMessage` (heavy payloads stay out of a monolithic `Effect`, per D1). P4
  closed the two remaining inline decisions in [`core::render`](../src/core/render.rs):
  the prefetch-next-sweep target (`decide_prefetch_next_elevation`, lifted out of
  `advance_playback`) and the request **dedup gate** (`should_dispatch`, now used
  by `RenderCoordinator` for both single + volume renders). 6 headless tests
  (prefetch boundary/next-in-scan/future-scan/skip-current; dedup
  suppress/pass). GPU upload + worker dispatch stay shell (carve-out). Risk: high
  (perceived-latency, dedup) — **gate behind manual QA** (scrub, elevation/product
  switch, play-through sweep boundaries, 3D volume).
- **P5 — Intent-ize remaining UI + widen the view-model (L). ◐ PARTIAL (pure
  logic extracted; broad mutation→intent rewrite deferred).** Extracted the
  panels' read-only *derivations and decision math* into the pure
  [`core::panels`](../src/core/panels.rs) (tested): the left panel's whole
  `query_radar_state_at_timestamp` derivation (the map's #1 left-panel violation)
  now lives in the core; the high-speed `animation_frozen` freeze rule, the
  archive sweep azimuth, and the top-bar status-message auto-dismiss/fade
  (`status_message_visibility`) are pure functions the panels call. 3 headless
  tests. **Deferred (QA-gated):** the broad `&mut state` → intent conversion for
  the *interactive* surface (transport, layer checkboxes, camera, text-edit) and
  the `LayoutCtx` → `&ViewModel` + intent-sink reshape. Rationale (see log D6):
  that half is mechanical but one-frame-lag-risky on an interactive hot path that
  can't be unit-tested *or* run here; the gotchas show several mutations
  (camera/WASD, `playback.advance`, two-way checkbox bindings) must stay `&mut`
  regardless; and the phase is explicitly "behind manual QA." The intent pattern
  itself is already proven end-to-end by P2. Doing a blind broad rewrite would
  risk the behavior-preservation guarantee with no way to verify.
- **P6 — Acquisition & live orchestration (L, last/optional). ✅ DONE (pure
  decisions extracted; pump I/O + streaming.rs stay shell).** Consolidated the
  acquisition *decisions* into [`core::acquisition`](../src/core/acquisition.rs):
  the selection-fetch duration gate (`decide_selection_gate -> SelectionGate`,
  the roadmap's named "selection-gate" seam, lifted out of
  `resolve_selection_fetch_gate`), plus the already-pure `reactive_prefetch_allowed`
  and the `dates_spanning` / `dates_in_range` span utilities re-homed from
  `app::acquisition_intent`. 6 headless tests (prefetch policy, gate arm/confirm
  boundary, single/midnight/interior date spans). The download-queue state
  machine (`nexrad::download_queue`) was already pure + tested and stays put.
  *(Superseded 2026-07: the pump I/O sequencing that P6 left in the shell has
  since been extracted too — the pump reducers now live in
  [`core::acquisition`](../src/core/acquisition.rs) and
  `app/acquisition_intent.rs` is a thin execute-in-field-order shell.)*
  `streaming.rs` stays de-scoped per the carve-out.

**Reference slice (do first): Diagnostics overlays — NWS alerts + mPING + GPS.**
Cleanest existing seams, off the render hot path, high QA value. Steps: define
`DiagnosticsIntent` (folding the existing alert `AppCommand`s in); write pure
`reduce(state, intent) -> (state, Vec<Effect>)` with the alert hit-test +
severity-rank tie-break moved into a pure `select_alert(...)`; produce a
`DiagnosticsVM` the panel + overlays read exclusively; replace the panel's direct
layer-toggle / `start_geolocation` and the canvas's direct `selected_report_id`
writes with intents. Tests: overlapping warning+advisory click → highest-rank
selected; mPING toggle gating; GPS toggle → `Effect::StartGeolocation`.

### Carve-outs (where the standard bends)
- **GPU paint is irreducibly shell.** The standard governs *what to upload / which
  sweep / which params* (→ view-model + `Effect`), not the GL calls. The executor
  is covered by manual canvas QA, not unit tests.
- **Camera / projection math is already pure (S4)** — route transitions through
  intents, but don't rewrite the verified matrix math.
- **`value_at_polar` is coupled to the GL object** — extract the CPU sweep buffers
  from the GL handle (in P3) only when probe logic needs coverage.
- **Worker dispatch is async + stateful (dedup)** — model the decision as pure
  (`should_dispatch(params, last) -> Option<RenderRequest>`); keep the queue and
  dispatch in the shell.
- **The live streaming loop** was decomposed in 2026-07 into
  `src/nexrad/live/realtime/streaming/` (nine focused submodules); the loop
  function itself keeps its await ordering intact, since the timing is
  load-bearing and unverifiable headlessly.
- **`LayoutCtx` still hands four subsystems as `&mut`** (`live`, `playback`,
  `acquisition`, `chrome`, plus `modals` for widget buffers). `diagnostics` was
  narrowed to `&` in 2026-07 as a compiler-checked proof that no layer mutates
  it; each surviving `&mut` carries an inline doc naming the mutation that keeps
  it, so the next narrowing attempt starts with the answer. `chrome` is the
  largest (~60 sites) and is arguably UI-local state that landed in a subsystem.

### Honest note on current structure
As of 2026-07 the layers match the intent: `src/core/` owns the domain types and
the decision logic (the densest test coverage), `src/state/` is a slim container
layer plus the shell halves of core types (localStorage/clock impls),
`src/subsystem/` holds the thin bounded owners, and `src/app/` is
assemble → reduce → execute shells over the core reducers plus the `Effect`
runtime. What remains shell-heavy by choice is the interactive UI surface (the
deferred P5 batch) and the carve-outs above.

## Related deferred cleanups

Minor, independent of the standard (carried over from the retired strategic doc;
recoverable from git history if expanded):

- **Schema-first worker IPC** — generate the Rust types *and* `worker.js` dispatch
  from one schema so the two can't drift. (Complements the effect boundary for the
  worker.)
- **`too_many_arguments` reduction** — ~53 `#[allow(...)]` sites, diffuse across
  `gpu_renderer`/`decode_worker`/`geo`/`streaming`/`timeline`/`canvas`; bundle into
  context structs only where it genuinely reads better.
- **Declarative data-flow canvas overlays** — extend the S3 `Overlay` registry
  (corner chrome) to the data-flow overlays in `canvas.rs`; low value, pipeline-
  inherent order, real visual-regression risk — do only if that code is reworked.
- ~~**`streaming.rs` decomposition**~~ — done (2026-07); see the carve-out above.
