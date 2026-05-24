# Ambitious Architectural Improvements (Strategic Tier)

## Context

The **tactical tier** — the catalog in `docs/REFACTORING.md` — was about clarifying what's already there: typed protocols, named outcomes, registry patterns, aggregator structs. Six of those items have landed (C1, A2, D1 stage 1, E3, E4 dispatcher cleanup, E5); nine remain.

This plan covers the **strategic tier**: changes to *how the codebase is structured*, not just how its existing pieces are labeled. Each of the four initiatives below subsumes one or more remaining catalog items and goes further by re-drawing boundary or ownership lines. They're sized so that any one of them is a multi-PR project, but together they represent a coherent next phase.

## Where we stand

- Done or partial: C1, A2, D1 stage 1, E3, E4 dispatcher, E5
- Remaining catalog items: A1, A3, A4, B1, B2, E1, E2, F1, F2, D1 stage 2
- The initiatives below subsume those remaining items.

## Cross-cutting themes still in need of attention

1. **State ownership is still scattered.** `AppState` is a 45-field god struct; `WorkbenchApp` owns 11+ managers. The tactical refactors (aggregating modals, naming a `CommandOutcome`) helped readability but did not change the underlying *"shared mutable state accessible to anyone"* pattern. Adding a feature still means touching both structs and possibly several panels.

2. **Four async patterns coexist.** `wasm_bindgen_futures::spawn_local` for one-shots; polled channels for downloads/cache/render; `Rc<RefCell<Vec<T>>>` shared queues drained each frame (GPS, `pending_observations`); direct `async fn` exposed to JS in `worker_api`. Each was added pragmatically; together they make async surface hard to reason about. WASM async failures are silent (no panic, just stuck state), so subtle bugs hide.

3. **2D and 3D barely overlap.** `MapProjection` (`geo/projection.rs`) and `GlobeCamera` (`geo/camera.rs`, 889 lines) have parallel coordinate transformations with no shared interface. 2D camera state (`zoom`, `pan_offset`) lives on `viz_state`; 3D camera state lives on `viz_state.camera`. Every interaction handler branches on `ViewMode`.

4. **UI rendering is imperative and scattered.** Panels mutate `AppState` directly. Derived state (visible bounds, sweep azimuth, current sweep info) is recomputed per frame across multiple panels. Mobile/desktop is dispatched centrally now but visibility/conditional rendering still lives in each panel.

---

## S1. Subsystem Decomposition

*Subsumes catalog items **A1**, **A3**, **A4**.*

### The problem
The codebase has two god structures (`WorkbenchApp`, `AppState`) plus orphan managers (`alerts_manager`, `mping_manager`, `playback_manager`, `network_monitor`). The catalog's A1 (move managers into AppState), A3 (unify acquisition state), and A4 (nest AppState into sub-structs) each take one slice, but none of them changes the underlying pattern of *"shared mutable state any module can reach into."* They rearrange the chairs.

### The change
Define bounded subsystems, each owning a coherent slice of state + behavior, with a typed external API. Initial decomposition:

| Subsystem | Owns | Replaces |
|-----------|------|----------|
| `Acquisition` | download queue, download channel, archive index, cache loader, pending download | `AppState.acquisition` + `WorkbenchApp.{acquisition, persistence (URL-state piece)}` |
| `Render` | worker pool, GPU resources, render dedup, sweep cache, displayed-scan tracking | `WorkbenchApp.{render, playback_manager, gpu}` |
| `Timeline` | scans, sweeps, time bounds, shadow boundaries, scrub cache | `AppState.{radar_timeline, shadow_scan_boundaries}` + `WorkbenchApp.scrub_cache` |
| `Playback` | position, speed, mode, animation, view bounds, time model | `AppState.playback_state` |
| `Live` | realtime channel, observations, projector, live model, app mode | `AppState.{live_mode_state, live_radar_model, app_mode}` + `WorkbenchApp.streaming` |
| `Chrome` | UI flags, sidebar visibility, theme, modal opens, modal states, mobile chrome | `AppState.{site_modal_open, *_visible, ...}` + `WorkbenchApp.modals` |
| `Diagnostics` | alerts, mPING, dev mode, network monitor, session stats, GPS | `AppState.{alerts, mping, gps_state, dev_mode, ...}` + `WorkbenchApp.{alerts_manager, mping_manager, network_monitor}` |

Each subsystem:
- Lives in its own module with a `State` struct, a `Coordinator`/`Manager` type, and `tick`/`render` hooks
- Exposes a read-only `View` (or `&self` projection) for cross-subsystem reads
- Receives commands via the existing `AppCommand` system, possibly namespaced (`AppCommand::Acquisition(AcqCmd::PauseQueue)`)
- Owns its async surface end-to-end — worker handles, channel ends, callback queues

Cross-subsystem coordination uses:
- The existing `AppCommand` queue (handler families per catalog A2, already shipped)
- A new typed `Event` channel for "subsystem X changed Y" (e.g. `TimelineEvent::ScansChanged`) that other subsystems subscribe to instead of polling AppState
- Explicit cross-subsystem calls when read-only access suffices (e.g. `render.request_for(timeline.current_scan())`)

The root `WorkbenchApp` shrinks to a thin coordinator holding subsystems + the egui context. The 21-stage `update()` loop becomes a sequence of subsystem ticks plus the render pass.

### Why it's ambitious
- Touches nearly every file (every `state.foo` read across the codebase becomes `subsystem.foo` or `view.foo`)
- Forces hard ownership decisions (who owns `displayed_scan_timestamp`? `request_repaint` semantics across subsystems?)
- Changes the mental model from "shared state everyone touches" to "bounded contexts that communicate explicitly"

### Effort & risk
- **6–10 PRs over multiple weeks.** Stage by subsystem; start with whichever has the messiest current ownership (`Acquisition` is the worst — `AppState.acquisition` and `WorkbenchApp.acquisition` are two objects that must stay in sync).
- **High risk.** Wrong subsystem boundaries cause churn.

### Migration approach
- For each subsystem, first PR introduces the subsystem module as a *thin wrapper* around current state — no behavior change. Subsequent PRs move state ownership in.
- Keep `AppState` as the explicit registry of subsystems during migration; remove only once all consumers are on the new APIs.
- One subsystem per PR; pre-commit hook and full manual flow per PR (boot, ingest, scrub, live, change site, refresh).

### Critical files
- `src/main.rs` (the orchestration); `src/state/mod.rs` (the god struct)
- `src/state/{acquisition.rs, playback.rs, live_mode.rs, alerts.rs, mping.rs, gps.rs, ...}` — these existing sub-modules become the seeds of each subsystem
- `src/app/{command_dispatch.rs, worker_results.rs, download.rs, render_loop.rs, frame_setup.rs}` — handlers move into subsystems
- `src/nexrad/{acquisition_coordinator.rs, render_coordinator.rs, persistence_manager.rs, realtime/}` — already-existing coordinators get absorbed

---

## S2. Unified Async / Effect Model

*Subsumes catalog item **B1**; absorbs **B2**.*

### The problem
Four distinct async patterns coexist:

| Pattern | Where | Why it was added |
|---------|-------|------------------|
| `spawn_local(async move { … })` | command dispatch, eviction, geolocation | One-shot fire-and-forget |
| Polled `Channel<T>` with `try_recv` | `DownloadChannel`, `CacheLoadChannel`, `RealtimeChannel` results | egui's sync update loop pulls from async tasks |
| `Rc<RefCell<Vec<T>>>` shared queue | `GpsState.results`, `RealtimeState.pending_observations` | Allow JS callbacks / async tasks to push without &mut state |
| Direct `async fn` exposed to JS | `worker_api::{worker_ingest, worker_render, …}` | Worker's WASM entry points |

Real consequences:
- `RealtimeChannel` uses `Rc<RefCell<RealtimeState>>` with three coordination mechanisms (`stop_requested` bool, `pending_observations` Vec, `filter_epoch` u64). The filter-epoch wakeup is non-obvious without reading the sleep loop.
- Pending-context leaks (the C1 fix) were possible because polled channels silently drop unknown messages.
- The IDB no-await-inside-readwrite invariant is enforced by runtime panics and prose comments (catalog item B2).
- `upsert_scan`'s single-writer requirement (B2) is comment-only.

### The change
Pick **one** cross-task communication model and migrate every async surface to it. Recommended:
- **`async-channel`** (or `futures::channel::mpsc`) for all cross-task communication that the egui loop polls
- **`spawn_local`** for one-shot fire-and-forget (unchanged)
- **No `Rc<RefCell<Vec<T>>>` for cross-task state** — replace with channels
- **An explicit `Effect` boundary** for IDB / HTTP / worker dispatch, so the pattern is consistent and mockable

Concrete cleanups, in priority order:
1. `RealtimeChannel`'s `Rc<RefCell<RealtimeState>>` → typed result channel + control-message channel (filter change, stop)
2. `GpsState::results: Rc<RefCell<Vec<>>>` → `mpsc::Receiver<LocationResult>` consumed in the right subsystem
3. `CHUNK_ACCUM` thread-local + `with_chunk_accum_mut` (runtime-checked re-entrance) → scope-local typed token that statically forbids holding across `.await`
4. `upsert_scan` single-writer requirement → `SingleWriterGuard<ScanKey>` RAII token the API requires

Document the chosen async model in `CLAUDE.md` so future patterns conform.

### Why it's ambitious
- Async is the most subtle correctness surface in the app; WASM async failures are silent
- The current four-pattern coexistence is the source of the trickiest bugs we've already fixed (silent leaks, filter-epoch hack, IDB invariants)
- Unifying eliminates the *class* rather than chasing instances

### Effort & risk
- **4–6 PRs.** Best done after S1 lands the relevant subsystem, so each migration happens in one place.
- **High risk.** Async bugs hide. Mitigations:
  - Migrate one surface at a time; keep other patterns until everything is converted
  - Strong code review per PR
  - Add browser-driven tests (likely under `tests/`) for realtime backfill + filter-change + abrupt stop
  - Add tests for `CHUNK_ACCUM` re-entrance scenarios

### Critical files
- `src/nexrad/realtime/{mod.rs, streaming.rs}` — biggest payoff and the model
- `src/state/gps.rs` — smallest, do as a warm-up
- `src/nexrad/worker_api/ingest.rs` (`CHUNK_ACCUM`) — for B2-style type safety
- `src/data/indexeddb/{mod.rs, helpers.rs, logic.rs}` — `upsert_scan` and `SingleWriterGuard`

---

## S3. UI Layer Tree

*Subsumes catalog items **E1**, **E2**, and the deferred LayoutProvider work from **E4**.*

### The problem
UI dispatch is imperative and scattered across multiple layers:
- `main.rs` calls panels in a specific order with mobile/desktop branching
- Each panel has its own visibility checks (sidebar visible, advanced mode, etc.)
- Modal overlays are 8 separate render calls at the bottom of `update()`
- Canvas overlays (`ui/canvas_overlays/`) are a flat imperative sequence with implicit z-order
- Derived state (visible bounds, sweep azimuth, current sweep info) is recomputed per frame across multiple panels (catalog item E1)

Result: the rule for "what should render when" is split between `main.rs` decisions, per-panel guards, and per-overlay implicit ordering. No single place captures the whole layout.

### The change
Two complementary parts.

**Part A: Per-frame `Derived` snapshot.** Populated once at the top of `update()`, before any UI render. Holds visible bounds, current scan/sweep info, sweep-line azimuth, camera-settled flag, etc. Panels and overlays read from `&Derived` instead of recomputing. This is catalog item E1 done properly.

**Part B: Typed layer tree.** Replace the imperative panel + overlay + modal sequence with a declarative tree:

```rust
enum Layer {
    Panel  { region: PanelRegion, visible: VisibilityFn, render: RenderFn },
    Overlay{ z_order: i32, render: RenderFn },
    Modal  { z_order: i32, visible: VisibilityFn, render: RenderFn },
    Group(Vec<Layer>),
}
```

Mobile and desktop layouts become tree builders (`build_desktop_layout(state) -> LayerTree`, `build_mobile_layout(state) -> LayerTree`). A single renderer walks the tree. Modal stacking becomes z-order, not order-of-call. Canvas overlay ordering becomes declarative (catalog item E2).

Conditional visibility (sidebar visible, advanced mode, modal-open flags) moves into the tree builders, removing the per-panel guards we still have.

### Why it's ambitious
- Reorganizes UI dispatch + derivation in one move
- Enables unit tests against the layer tree without running egui (e.g. "in mobile + advanced mode + alerts modal open, what layers are present and in what z-order?")
- Forces mobile and desktop into a single mental model with two configurations, not two parallel code paths

### Effort & risk
- **3–5 PRs.** Order: `Derived` struct first (lowest risk), then `Overlay`-layer tree (next-lowest), then panel + modal layer tree.
- **Medium risk.** Primarily organizational; egui still does the actual rendering. Visual regressions possible if z-order shifts.
- Mitigations: per-PR `trunk serve` walkthrough in both viewports; per-modal screenshot diff is hard to automate but a manual checklist works.

### Critical files
- `src/main.rs:617-714` (the update loop + render-pass tail)
- `src/ui/canvas.rs` (`render_canvas_with_geo` and the overlay calls)
- `src/ui/canvas_overlays/mod.rs` and its submodules
- `src/ui/{left_panel, right_panel, bottom_panel, top_bar}.rs` (visibility predicates extracted)
- `src/ui/mobile/{mod.rs, tabs.rs, top_bar.rs}` (becomes a tree builder)

---

## S4. Camera + Projection State Machine

*Subsumes catalog items **F1** and **F2**.*

### The problem
- `GlobeCamera` (`src/geo/camera.rs`, 889 lines) has 15+ public fields covering three disjoint modes (PlanetOrbit, SiteOrbit, FreeLook). Any code can mutate any field in any mode. Mode transitions leak state (e.g. `free_pos` retained across switch to orbit).
- 2D camera state (`zoom`, `pan_offset`) lives on `viz_state`, not on the camera struct at all. There is no `Camera` type that owns both 2D and 3D state.
- `MapProjection` (2D) and `GlobeCamera` (3D) each define `geo_to_screen` / `screen_to_geo`, but they are not interchangeable. Every interaction handler in `canvas_interaction.rs` branches on `ViewMode`.

### The change
Unify into a single camera state machine that also implements a shared projection trait.

```rust
enum Camera {
    Flat2D(Flat2DState),       // zoom, pan_offset
    PlanetOrbit(PlanetOrbitState),
    SiteOrbit(SiteOrbitState),
    FreeLook(FreeLookState),
}

trait Projection {
    fn geo_to_screen(&self, lat: f64, lon: f64) -> Option<Pos2>;
    fn screen_to_geo(&self, pos: Pos2) -> Option<(f64, f64)>;
}
impl Projection for Camera { /* dispatch on variant */ }
```

UI interaction handlers and overlays work generically on `&dyn Projection`. Mode transitions become explicit `Camera::switch_to_*` methods that construct the new variant from the old (preserving what makes sense, dropping what doesn't). Field validity becomes type-enforced.

`ViewMode` (currently `Flat2D` | `Globe3D`) becomes a derived view of the `Camera` variant, not an independent toggle that has to stay in sync.

### Why it's ambitious
- Eliminates a class of "ViewMode branch missing somewhere" bugs
- Unifies 2D and 3D, making future work (e.g. a hybrid mode, additional projections) feasible
- Camera is one of the few subsystems with substantial geometry, so getting it right pays compounding dividends

### Effort & risk
- **2–4 PRs.** Order: extract `Flat2DState` from `viz_state` first; then introduce `Camera` enum; then `Projection` trait; finally migrate call sites.
- **High risk.** Camera is touched by every interaction handler + every overlay. Manual UI testing of every camera transition is required (no automated coverage today).

### Critical files
- `src/geo/camera.rs` (889 lines — the bulk of the work)
- `src/geo/projection.rs` (the 2D side)
- `src/ui/canvas_interaction.rs` (branches on `ViewMode`)
- `src/state/viz.rs` (where 2D camera state lives)
- `src/ui/shortcuts.rs` (camera mode switching via 1-4 keys — registry already exists, just point at new types)

---

## Recommended sequence

1. **S1 (Subsystem Decomposition)** — sets ownership boundaries that S2 and S3 build on. Stage by subsystem; start with whichever has the messiest current ownership (likely `Acquisition` or `Live`).
2. **S2 (Async Unification)** — within each subsystem, clean up its async surface. Doing this *after* S1 means each subsystem owns its async, so the cleanup happens in one place per subsystem.
3. **S3 (UI Layer Tree) and S4 (Camera State Machine) in parallel** — both are independent of S1/S2 once subsystem boundaries are set. S4 is more isolated; S3 is best done after S1 since subsystems become the natural providers of "what to render."

If appetite is lower than "do all four," the catalog items each subsumes can be done individually and incrementally instead. The strategic plan above is the maximum-leverage version.

## Verification per initiative

Each initiative is multiple PRs; standard per-PR checks:

1. `cargo check` and `cargo clippy -- -D warnings` clean
2. `cargo test --bin nexrad-workbench` — pure-Rust logic suite (will grow as new subsystem-level tests are added)
3. `CHROMEDRIVER=/usr/bin/chromedriver cargo test --test idb` for any IDB-touching changes (S1 Acquisition/Render, S2 RealtimeChannel/CHUNK_ACCUM/upsert_scan)
4. `trunk serve` and full manual flow:
   - **S1**: boot, ingest a scan, scrub, switch elevations, enter live, exit live, change site, refresh URL — each transition exercises subsystem boundaries
   - **S2**: realtime with filter changes mid-stream + abrupt stops; GPS location grant + deny; ingest under quota pressure
   - **S3**: every panel + modal in both desktop and mobile viewports; modal stacking order; sidebar visibility transitions
   - **S4**: all four camera modes plus every transition pair; pan/zoom/orbit/free-look interactions; click-to-inspect in both 2D and 3D

## What this plan does *not* cover

- **Pure Core / Effect Shell** — the truly transformative refactor (separate pure state-transition logic from side effects, enable browser-free unit tests for most logic). Too speculative without first doing S1+S2; becomes viable after.
- **Schema-first worker IPC** — would generate both Rust types and `worker.js` dispatch from one schema. Incremental on C1; not ambitious enough for this tier.
- **Performance optimization** — the renderer is already fast.
- **New features** — the app's functionality is set.
- **Catalog items that *don't* fit a strategic initiative**: D1 stage 2 (app-side `ErrorContext` aggregation) is small enough to do standalone whenever appetite arises; it doesn't need a strategic envelope.

## Status

| ID | Title | Status |
|----|-------|--------|
| S1 | Subsystem Decomposition | in progress (Acquisition + full Diagnostics extracted; Render/Timeline/Playback/Live/Chrome remain) |
| S2 | Unified Async / Effect Model | not started |
| S3 | UI Layer Tree | not started |
| S4 | Camera + Projection State Machine | not started |
