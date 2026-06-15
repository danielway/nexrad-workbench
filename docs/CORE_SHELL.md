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

| Seam | Role | Today (per the 2026-06-14 coupling audit) |
|---|---|---|
| `AppCommand` | **Intents in** | Exists but thin — ~20 variants, mostly *effect* triggers (`ClearCache`, `StartLive`, `FetchScan`); most ordinary state changes bypass it via direct `&mut`. Goal: it (as a renamed/superset `Intent`) becomes the *only* way the UI changes anything. |
| `subsystem::Derived` ([derived.rs](../src/subsystem/derived.rs)) | **View-model out** | Exists but minimal — a 4-field per-frame cache, read ~14×; panels read subsystem internals directly at scale. Goal: a *complete* per-panel view-model the panels read exclusively. |
| The 7 subsystems + `AppState` | **State owners** | Exist (S1) and are thin/clean. The decision+effect *tangle* lives mostly in `src/app/*` (the `update()` orchestration `impl WorkbenchApp` methods). Goal: that logic becomes pure `(state, intent) -> effects`. |
| `Effect` (an enum the core returns) | **Effects out** | **New.** Replaces inline IDB/HTTP/worker/GPU/localStorage/etc. calls in decision logic with described values an effect runtime executes. The pattern already exists in one place — `resolve_prev_sweep -> PrevSweepAction` ([playback_manager.rs](../src/state/playback_manager.rs)) — **imitate it.** |

## What counts as a violation

In `src/ui/**` (panels, canvas, canvas_interaction, overlays, modals, shortcuts,
mobile), or any egui/web-sys/GPU code:

- ❌ Business logic, math, or state derivation (sweep matching, geometry,
  thresholds, formatting decisions). → move to the core; expose the result on the
  view-model.
- ❌ Mutating `AppState`/subsystem state directly. → emit an `AppCommand`.
- ❌ Reading `AppState`/subsystem internals to compute what to show. → read the
  view-model (`Derived`); add a field if it's missing.
- ❌ Performing I/O (fetch, IDB, `postMessage`, GPU upload, `localStorage`,
  geolocation, history). → return an `Effect` from the core.

In the core: ❌ any `egui::`, `web_sys::`, `js_sys::`, GPU, or `.await`-on-I/O in
decision logic. Pure in, pure out; effects are values.

## How to test (the recipe)

The whole point. A feature test never touches egui or a browser:

```rust
let mut core = Core::new(/* fixture state */);
let effects = core.handle(AppCommand::SeekTo(ts));      // send an intent
assert_eq!(core.view_model().displayed_frame_ts, ts);   // assert the projection
assert!(effects.contains(&Effect::RenderSweep { .. })); // assert the I/O it asked for
// the effect is NOT executed — we assert the *decision*, not the side effect
```

For effect *results*, feed them back as intents (`Effect::Fetch` → runtime →
`AppCommand::FetchCompleted(..)`) and assert the next state. The egui layer gets
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
test seam. Effort S/M/L = relative size. **Status: P0 complete; in progress.**
(Running decisions log: [CORE_SHELL_MIGRATION_LOG.md](CORE_SHELL_MIGRATION_LOG.md).)

- **P0 — Contract types (S). ✅ DONE.** Introduced `Intent` (alias of today's
  `AppCommand`, to grow into the superset) and an `Effect` enum, modeled on
  `PrevSweepAction`, in the new `src/core/` module. No behavior change. The
  vocabulary exists; heavy per-decision effects keep their own local action
  enums (the `PrevSweepAction` idiom), `core::Effect` carries simple
  cross-cutting effects.
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
  ([`PrevSweepAction`](../src/state/playback_manager.rs) for the prev-sweep
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
- **P6 — Acquisition & live orchestration (L, last/optional).** Move the
  prefetch / selection / listing pumps (`src/app/*`) into pure decide→effect; only
  extract already-pure pieces from `streaming.rs` (a full split stays de-scoped —
  it reopens live QA). Seam: prefetch / selection-gate decisions assertable.

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
- **`streaming.rs` (~1850 lines) — deferred** (a split reopens live QA; see related
  cleanups).

### Honest note on current structure
There are three overlapping "state" layers today: `src/state/` (data + already-pure
decision fns, well covered — ~533 pure tests), `src/subsystem/` (7 thin owners),
and `src/app/` (the `update()` orchestration, where most of the untested
decision+effect tangle lives). The migration consolidates decision logic —
especially out of `src/app/*` — into the pure core.

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
- **`streaming.rs` decomposition** — split the ~1850-line live-streaming loop
  (`src/nexrad/realtime/streaming.rs`) into focused submodules; behavior-preserving
  but reopens live-stream QA, so do it when that file next needs substantial change.
