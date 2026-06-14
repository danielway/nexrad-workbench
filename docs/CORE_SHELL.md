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
test seam. Effort S/M/L = relative size. **Status: not started.**

- **P0 — Contract types (S).** Introduce `Intent` (superset of today's
  `AppCommand`) and an `Effect` enum, modeled on `PrevSweepAction`. No behavior
  change. Seam: the vocabulary exists; pure `derive_*` fns re-homed under the core.
- **P1 — Effect boundary + injectable clock (S→M).** Define how the core returns
  effects (a `Vec<Effect>` / sink) covering GPU upload, worker dispatch, IDB,
  localStorage, URL, geolocation, timers; make the persistence throttle clock
  injectable (extend the existing `FrameNow` seam). Convert `persist_if_due` →
  `decide_persist(state, now) -> Vec<Effect>`. Seam: persistence/preference-save
  decisions assertable headlessly. Low risk (non-visual).
- **P2 — Reference slice: Diagnostics overlays (M).** Migrate alerts + mPING + GPS
  end-to-end (details below) to prove the `intent → core → view-model → shell`
  loop on a low-risk, high-QA-value feature off the render hot path. Seam:
  `core + click/toggle intents → assert selected alert / VM`.
- **P3 — Canvas decision extraction (M→L).** Move sweep matching
  (`compute_gpu_sweep_state`), sweep-line azimuth, cutout math, and data-probe
  polar/value computation out of `src/ui/canvas.rs` into a pure
  `core::render_view_model(...) -> RadarFrameVM`; decouple `value_at_polar` from the
  GL object. Seam: sweep-matching / between-sweeps / probe values unit-tested.
  Risk: medium-high (hot path) — gate behind manual canvas QA.
- **P4 — GPU & worker effects as data (L).** Convert `sync_prev_sweep_texture`,
  `request_render_if_needed`, prefetch, and clear-display in
  `src/app/render_loop.rs` into pure `decide_render(...) -> Vec<Effect>` (extend
  `PrevSweepAction` to the main sweep + worker dispatch); the shell executes
  `UploadSweep` / `DispatchRender` / `ClearGpu`. Seam: the whole render-decision
  loop assertable. Risk: high (perceived-latency path, dedup correctness).
- **P5 — Intent-ize remaining UI + widen the view-model (L).** Replace direct
  `&mut state` mutation in `right_panel` / `left_panel` / `canvas_interaction` /
  `shortcuts` with intents; replace direct subsystem reads with per-panel VM
  structs. `LayoutCtx` shrinks from `&mut`-everything to `&ViewModel` + an intent
  sink. Seam: every panel feature is `send intent → assert VM`. Risk: high (broad)
  — file-by-file, each behind its own commit + manual QA.
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
