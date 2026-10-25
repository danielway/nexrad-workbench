# Architecture Standard: Functional Core, Thin Shell

**Status: binding standard for all new and changed code; existing code has been
migrated to it, apart from one deferred UI batch (see "Where the codebase
stands").** The one-paragraph rule lives in [CLAUDE.md](../CLAUDE.md); this is
the full reference — the contract, the seams in this codebase, what counts as a
violation, how to test, and where the migration stands.

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
| `Intent` ([intent.rs](../src/core/intent.rs)) | **Intents in** | Defined in the core (the old `AppCommand` name is gone). UI code pushes intents via `AppState::push_command`; the main loop drains and dispatches them. Remaining gap: the interactive panels' direct `&mut` writes (the deferred batch under "Where the codebase stands"). Goal: `Intent` becomes the *only* way the UI changes anything. |
| `subsystem::Derived` ([derived.rs](../src/subsystem/derived.rs)) | **View-model out** | Exists but minimal — a small per-frame cache; panels still read subsystem internals directly at scale. `DiagnosticsVm` and `core::activity::ActivityVm` are the complete per-panel view-models (the pattern to propagate). Goal: a *complete* per-panel view-model the panels read exclusively. |
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
decided deliberately (2026-07); treat them as settled rather than
as debt to burn down.

**1. A render leaf reading `AppState` to paint.** The view-model exists to carry
*derivations*, and every derivation belongs in the core (`core::panels`,
`DiagnosticsVm`, `core::canvas`). A panel that reads a field and draws it has no
decision to test, so wrapping that read in a per-panel struct buys ceremony, not
safety — the compiler already finds every site when state changes shape. Grow a
view-model when a panel *computes*; not when it merely displays. Mutation is a
different matter and stays forbidden.

**2. A render function taking many narrow parameters** (`#[allow(clippy::too_many_arguments)]`, 57 sites — 30 in `ui`, 21 in `nexrad`). Two different
things produce these, and both are deliberate. In `ui`, a layer unpacks
`LayoutCtx` into exactly the pieces it touches rather than passing the whole
bundle down — that is what makes the narrowed borrows meaningful and what lets
you see a function's real dependencies in its signature. In `nexrad`, the GPU
paint and worker-dispatch calls take genuinely distinct scalars (azimuths, gate
values, counts, ranges, offset, scale); bundling them into a struct relocates
the fields without removing them. Reach for a context struct when the arguments
are *cohesive* — not to satisfy the lint.

**3. Direct-manipulation gestures calling a subsystem command method.** Intents
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
  `value_at_polar`, storm-cell detection (the `storm_cells` leaf) and which
  cut it runs on, when, and which cached result is shown
  (`core::storm_cells`; the worker run is a described `DetectRequest`),
  camera/projection math
  (`geo_to_screen`/`screen_to_geo` round-trips), overlay visibility, what the
  view-model says to draw.
- **Shell (effects / paint):** uploading raw gate values to the GPU texture
  (an `Effect`), the fragment-shader pass, and egui painting of the view-model.

So the view-model carries *what to render* (sweep handle/params, camera, overlay
set, computed cells); the shell turns that into texture uploads and draw calls.
The pure geometry and matching are core and fully testable; only the pixel-pushing
needs eyes.

## Where the codebase stands

The migration to this standard is complete outside one deliberately deferred
UI batch. Its phase-by-phase record is in git history.

- **Done.** The domain model lives in [`core/domain/`](../src/core/domain/),
  and `Intent` is defined in [`core/intent.rs`](../src/core/intent.rs). The
  decision mass of `src/app/` moved into pure reducers
  ([`core::worker_ingest`](../src/core/worker_ingest.rs),
  [`core::worker_decoded`](../src/core/worker_decoded.rs),
  [`core::render_loop`](../src/core/render_loop.rs),
  [`core::acquisition`](../src/core/acquisition.rs)), so `src/app/*` files are
  assemble → reduce → execute shells. Projection and timing are consolidated in
  [`core/projection/`](../src/core/projection/) and
  [`core/timing/`](../src/core/timing/). Complete `intent → core → view-model →
  shell` slices exist for the diagnostics overlays
  ([`core::diagnostics`](../src/core/diagnostics.rs)), acquisition activity
  ([`core::activity`](../src/core/activity.rs)), the canvas decisions
  ([`core::canvas`](../src/core/canvas.rs)), the render dedup and prefetch
  ([`core::render`](../src/core/render.rs)), persistence
  ([`core::persist`](../src/core/persist.rs)), and the One Instrument surfaces
  (session view-model, transport, ladder, timeline, dialogs, phone). The
  layering is enforced by `tools/arch_check.rs`; see ARCHITECTURE.md
  "Architecture Enforcement".
- **Deferred.** The broad `&mut state` → intent rewrite for the interactive
  panels (transport, layer checkboxes, camera, text edit), and reshaping
  `LayoutCtx` into `&ViewModel` plus an intent sink. That half is mechanical
  but has one-frame-lag risk on an interactive hot path that can't be unit
  tested, and several mutations (camera/WASD, `playback.advance`, two-way
  checkbox bindings) must stay `&mut` regardless. Take it on only with manual
  QA in a real browser.

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
- **`LayoutCtx` still hands five subsystems as `&mut`** (`sites`, `live`,
  `playback`, `acquisition`, `chrome`, plus `modals` for widget buffers).
  `sites` is `&mut` only because the transport calls the UI makes (`Live::seek`,
  `Live::select_range`) can stop every site's stream channel; a saved event from
  another site goes through `Intent::SelectSite`. `diagnostics` was
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
deferred batch) and the carve-outs above.

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
