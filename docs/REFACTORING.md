# Architectural & Code Quality Opportunities

## Context

NEXRAD Workbench is a mature WASM-based weather radar app where we have been doing steady incremental refactoring (recent commits: split `indexeddb.rs`, extracted `vcp_forecast_serialize`, centralized `ViewState` via `From<&AppState>`, decomposed `realtime.rs`). The codebase is well-organized, but several themes of accumulated debt are now visible: scattered ownership between `AppState` and out-of-state managers, sprawling command dispatch, untyped boundaries (worker protocol, errors), and load-bearing invariants enforced only by prose comments.

This document catalogs **medium and large opportunities** to clean up tech debt and make future iteration easier. Each item is sized so you can pick by appetite and tackle one as its own focused project. Small polish items are omitted.

> For the next-tier strategic plan that **subsumes most of the remaining items below** under four bigger structural initiatives (subsystem decomposition, async unification, UI layer tree, camera state machine), see [REFACTORING_STRATEGIC.md](REFACTORING_STRATEGIC.md).

## Cross-cutting theme: invariants in comments, not types

A recurring pattern across the codebase: load-bearing rules are documented in prose, with runtime panics or silent breakage as the only enforcement. Several opportunities below reduce to moving an invariant from a comment into the type system.

Concrete examples:
- **Worker postMessage protocol** — schema in prose, magic-string dispatch (`worker.js:7-34`, `decode_worker/receive.rs:45-92`)
- **IDB no-await-inside-readwrite** — the `WriteTransaction` non-Send pattern enforces it, but `with_chunk_accum_mut` re-opens a runtime escape hatch
- **IDB single-writer for `upsert_scan`** — comment-only invariant; concurrent callers can silently corrupt `scan_availability`
- **Camera mode field validity** — `GlobeCamera` mixes fields for three disjoint modes; nothing prevents cross-mode mutation
- **Update-loop stage ordering** — `WorkbenchApp::update` is a 21-stage choreography with five critical ordering invariants documented in comments

## Recommended priority order

1. **C1 Typed postMessage protocol** — best clarity-per-effort; eliminates a class of silent failures
2. **A2 AppCommand dispatch consistency** — small standalone win and prerequisite for further state refactors
3. **D1 Unified typed error model** — UX + ops improvement and makes future refactors safer
4. **E3 Modal state unification** + **E4 Mobile/desktop chrome unification** — clears a significant UI cleanup together
5. **A1 Consolidate `WorkbenchApp` managers into state** — the highest-leverage structural change but also the riskiest

---

## A. State & coordination cohesion

### A1. Consolidate `WorkbenchApp` managers into a coherent state-owned tick
- **Where**: `main.rs:128-183` (`WorkbenchApp` struct), `main.rs:602-723` (the `update` loop)
- **Pain**: `WorkbenchApp` owns 11+ semi-independent managers (`render`, `acquisition`, `streaming`, `persistence`, `playback_manager`, `alerts_manager`, `mping_manager`, `network_monitor`, three modal states, `scrub_cache`). The `update()` loop is a 21-stage sequence with five critical ordering invariants documented in comments — a single out-of-order refactor breaks the app.
- **Direction**: Move polled/ticked managers into `AppState` (or a struct it owns). Introduce `AppState::tick()` that encapsulates the per-frame sequence and returns a named result struct. State and the logic that mutates it become collocated; ordering invariants become checkable.
- **Size**: Large · **Risk**: High (stage as multiple PRs; do A4 first)

### A2. AppCommand dispatch consistency
- **Where**: `state/mod.rs:78-115` (18-variant enum), `app/command_dispatch.rs:18-144`
- **Pain**: Dispatch is inconsistent across variants: `PauseQueue` mutates state directly; `DownloadSelection` sets a flag consumed elsewhere; `StartLive` spawns an async task in dispatch itself. Adding a command requires edits in the enum, the dispatcher, possibly a flag, and a consumer. Hard to trace where the "real" logic lives.
- **Direction**: Pick one dispatch shape (e.g. direct handler returning a typed `CommandOutcome`). Group commands into handler families (download, live, queue, alerts) with consistent return types. Eliminate the dispatch→flag→consumer indirection.
- **Size**: Medium · **Risk**: Medium

### A3. Unify acquisition state ownership
- **Where**: `state/mod.rs:253-254` (`AppState::acquisition`), `nexrad/acquisition_coordinator.rs:26-56`, `main.rs:142`
- **Pain**: `AppState::acquisition` (queue/ops tracking) and `WorkbenchApp::acquisition` (download channels, archive index, data facade) are two separate objects that must be kept in sync. Worker-result handling threads through both; easy to miss an update.
- **Direction**: Fold the coordinator's channel/index state into `AcquisitionState`, or move `AcquisitionState` into the coordinator. One owner, one source of truth. Expose a read-only view to UI.
- **Size**: Medium · **Risk**: Medium

### A4. Decompose `AppState` into nested sub-states
- **Where**: `state/mod.rs:117-305` (~45 public fields)
- **Pain**: `AppState` mixes playback, visualization, layers, live, alerts, mPING, GPS, UI toggles, modal flags, network stats, dev mode, mobile chrome flags, render cache. Panels touch ten or more fields at a time; tests must construct the whole struct even to exercise a subsystem.
- **Direction**: The sub-modules already exist (`state/playback.rs`, `state/viz.rs`, etc.) — promote them into composed sub-structs on `AppState` (e.g. `state.ui.left_sidebar_visible`, `state.system.dev_mode`). Keep deep access ergonomic via `pub` fields. Useful prerequisite for A1.
- **Size**: Medium · **Risk**: Low (mechanical; mostly field-access path updates)

---

## B. Async/threading patterns

### B1. Replace `Rc<RefCell<>>` in `RealtimeChannel` with structured channels
- **Where**: `nexrad/realtime/mod.rs:184-305`, `nexrad/realtime/streaming.rs` (1260 lines)
- **Pain**: Streaming state shared via `Rc<RefCell<RealtimeState>>` with three separate coordination mechanisms — `stop_requested` bool, `pending_observations` Vec, `filter_epoch` u64. Runtime borrow-check with comment-only invariants. The filter-epoch wake-up is non-obvious without reading the sleep loop.
- **Direction**: Use an async-channel crate (`async-channel` or `futures::channel::mpsc`) for results. Convert control signals (stop, filter-changed) into events on a single control channel. Move the thread-safety invariants from comments into the type system.
- **Size**: Large · **Risk**: Medium (async is subtle; exercise backfill + live + filter-change interactions thoroughly)

### B2. Type-enforced single-writer for `upsert_scan`
- **Where**: `data/indexeddb/mod.rs:323-406`
- **Pain**: `upsert_scan` does read-then-write across two IDB transactions; correctness depends on callers serializing writes per `ScanKey`. Today the `CHUNK_ACCUM` thread-local happens to enforce this for the ingest path, but a future concurrent-async refactor could silently corrupt `scan_availability` merges.
- **Direction**: Either (a) introduce a `SingleWriterGuard<ScanKey>` RAII token that the API requires across the RMW, or (b) restructure to do the read inside the readwrite transaction via a read-then-write callback. Option (b) is simpler but loses the deliberate split.
- **Size**: Medium · **Risk**: High (data integrity)

---

## C. Worker boundary & protocol

### C1. Typed postMessage protocol
- **Where**: `worker.js:7-34` (prose-comment schema), `nexrad/decode_worker/receive.rs:45-92` (magic-string dispatch)
- **Pain**: Six message types matched via string literals on a dynamic `JsValue`. Unknown types silently log + drop. Envelope parsing failures return `None` and leak the pending-context entry (memory-leak hazard). Sender/receiver divergence is invisible at compile time.
- **Direction**: Define a `WorkerMessage` enum on the Rust side with serde tagged-union deserialization (`#[serde(tag = "type")]`). Keep one source of truth for type strings shared between Rust and JS. Treat envelope-parse failures as hard errors so context leaks become loud.
- **Size**: Medium · **Risk**: Medium (touches every worker message path)

---

## D. Error handling & observability

### D1. Unified typed error model
- **Where**: `state/mod.rs` (`worker_init_error: Option<String>`), `state/alerts.rs` (`last_error: Option<String>`), `worker.js:73-75 etc.` (stringify-everything error path), `nexrad/national_mosaic.rs:45-53`, `nexrad/download.rs:18-28`
- **Pain**: Errors are surfaced inconsistently — some as banner state, some only logged, some as channel outcomes with opaque `String`. The worker stringifies all errors at the JS boundary, losing taxonomy (quota vs transient vs not-found vs invalid-data). UI can't decide whether to prompt the user, retry silently, or offer recovery.
- **Direction**: Define a `WorkerErrorKind` enum with structured JSON serialization across the worker boundary; classify caught exceptions in `worker.js` into kinds. On the app side, define an `AppError` taxonomy and an `ErrorContext` collector on `AppState` that all reporters push to (with severity, timestamp, source operation id). UI surfaces from a single source.
- **Size**: Medium · **Risk**: Medium (wide surface; stage worker-boundary first, then app-side consolidation)

---

## E. UI structure

### E1. Canvas computation/rendering split + UI-cache layer
- **Where**: `ui/canvas.rs:72-346` (`render_canvas_with_geo`), `ui/left_panel.rs:76-150` (`query_radar_state_at_timestamp`), `ui/canvas.rs:536-598` (`compute_sweep_line_azimuth`, `compute_gpu_sweep_state`)
- **Pain**: Per-frame derivation (visible bounds, sweep state, sweep-line azimuth, current scan/sweep lookup) is interleaved with rendering and recomputed across panels each frame. The 2D and 3D paths duplicate setup. Mirrors the consolidation already done with `ViewState`.
- **Direction**: Add a `FrameDerived` struct populated once per frame at the top of `update()`, before any UI render. Panels and overlays read from it. Centralizes derivation, makes invalidation explicit, removes duplicated timeline walks.
- **Size**: Medium · **Risk**: Medium (discipline required around what's cached vs. recomputed)

### E2. Overlay layering registry
- **Where**: `ui/canvas.rs:64-340` (flat sequence of overlay calls), `ui/canvas_overlays/mod.rs`
- **Pain**: Overlays drawn as an imperative sequence with implicit z-order. The 2D and 3D paths each call `draw_color_scale` and `draw_overlay_info` separately. No way to reason about reordering, per-overlay toggles, or 2D/3D variation without reading the whole function.
- **Direction**: Define an `Overlay` trait with `fn draw(&self, ctx: &OverlayContext)`. Assemble a Vec of overlays once. Z-order, visibility gating, and 2D/3D variation become declarative. Imperative escape hatch remains available for genuine edge cases.
- **Size**: Medium · **Risk**: Low

### E3. Modal state pattern unification
- **Where**: `ui/event_modal.rs:9-30` (`EventModalState`), `ui/site_modal.rs` (`SiteModalState`), `ui/mping_modal.rs` (`MpingModalState`); also held outside `AppState` in `main.rs:152-172`
- **Pain**: Three modal state structs follow similar shapes but with copy-paste boilerplate (init flag, form fields, validation). They live outside `AppState` by convention to dodge `Clone + Default`, but threading them through render functions is inconsistent (`render_site_modal(ctx, state, modal)` vs `handle_shortcuts(ctx, state)`). Modal state doesn't reset on site change — one-frame lag at dismissal.
- **Direction**: Define a `ModalState` trait with `reset()`, `validate()`, `apply()`. Pick one placement rule (either fold into `AppState` with `#[serde(skip)]` on non-serializable fields, or keep external behind a single `ModalStates` aggregator passed alongside `AppState`) and apply it uniformly.
- **Size**: Medium · **Risk**: Low

### E4. Mobile/desktop chrome unification
- **Where**: `ui/bottom_panel.rs:10-22`, `ui/left_panel.rs:32-34`, `ui/right_panel.rs`, `ui/top_bar.rs` (early `if state.is_mobile { return; }` guards), `ui/mobile/mod.rs` (mobile chrome)
- **Pain**: Mobile and desktop paths use scattered `is_mobile` early-returns. Mobile chrome reimplements playback controls, timeline interactions, and settings panels in parallel. Risk of silent divergence when shared state changes.
- **Direction**: Define a `LayoutProvider` trait covering chrome rendering methods (`render_top_chrome`, `render_bottom_chrome`, `render_left_sidebar`, etc.). Implement `MobileLayout` and `DesktopLayout`. Instantiate one per frame. Shared subcomponents (timeline scrubber, product selector) become composable widgets used by both.
- **Size**: Medium · **Risk**: Medium

### E5. Shortcuts registry
- **Where**: `ui/shortcuts.rs` (494 lines: ~150 lines of static shortcut definitions + ~340 lines of polling/dispatch)
- **Pain**: Shortcut definitions are already static data, but dispatch is an imperative match/if tree calling `ctx.input()` 50+ times. Adding a shortcut requires four touch points. The help overlay can't introspect actual handlers — it's purely decorative. No way to unit-test handler logic.
- **Direction**: Build a `ShortcutRegistry` mapping `(key, modifiers)` to `(name, AppCommand | handler fn)`. Drive both dispatch (single loop) and the help overlay (read from registry). Pure data + lookup.
- **Size**: Medium · **Risk**: Low

---

## F. Geo / camera

### F1. Camera state machine via enum
- **Where**: `geo/camera.rs:56-102` (15+ public fields for three disjoint modes), `geo/camera.rs:157-234` (matrix computation)
- **Pain**: `GlobeCamera` has fields for `PlanetOrbit`, `SiteOrbit`, and `FreeLook` mixed into one flat struct. Any code can mutate any field in any mode. View-matrix methods are mode-aware but can't enforce invariants. State leaks across mode switches (e.g. `free_pos` retained when switching to orbit).
- **Direction**: `enum CameraState { PlanetOrbit(PlanetOrbitState), SiteOrbit(SiteOrbitState), FreeLook(FreeLookState) }`. Control methods dispatch on the variant. Mode transitions become explicit constructors. Field validity becomes type-enforced.
- **Size**: Large · **Risk**: High (deeply used; needs thorough mode-transition testing)

### F2. Projection trait
- **Where**: `geo/projection.rs` (2D `MapProjection`), `geo/camera.rs` (3D `GlobeCamera`), `ui/canvas_interaction.rs:24-135` (per-mode branching)
- **Pain**: 2D and 3D coordinate systems each have `geo_to_screen` / `screen_to_geo` methods, but they're not interchangeable — every interaction handler branches on `ViewMode`. Forces duplicated code paths in callers.
- **Direction**: Define a `Projection` trait with `geo_to_screen` (returns `Option<Pos2>` to handle off-globe) and `screen_to_geo`. Implement for both. Interaction code works generically over `&dyn Projection`.
- **Size**: Medium · **Risk**: Medium (careful handling of optionality — flat map always projects; globe can miss)

---

## Verification

Each catalog item is independently shippable as its own PR. Per-item verification flow:

1. `cargo check` and `cargo clippy -- -D warnings` clean
2. `cargo test --bin nexrad-workbench` (pure-Rust logic tests)
3. `CHROMEDRIVER=/usr/bin/chromedriver cargo test --test idb` for any IDB-touching change (B2, parts of A1/A3)
4. `trunk serve` and exercise the affected flow in a browser:
   - **State refactors (A*, D1)**: boot → ingest a scan → scrub → switch elevations → enter live mode → exit live → refresh URL
   - **Worker protocol (C1)**: same as above; verify no console errors and that intentional bad messages fail loudly
   - **Async (B*)**: exercise the realtime/streaming path with filter changes mid-stream and a forced stop
   - **UI (E*, F*)**: exercise the affected panels in both desktop and mobile viewports; for F1, exercise all three camera modes plus transitions

## How to use this catalog

- Each Medium item is roughly a 1–3-day focused effort; each Large is a multi-PR project.
- Items in the same theme can be stacked productively:
  - **A4 → A1** (decompose state before consolidating ownership)
  - **E3 → E4** (settle modal pattern before unifying mobile chrome)
  - **A2 → A1** (settle command dispatch before reorganizing the tick loop)
  - **C1 → D1** (typed messages first, then typed errors riding the same envelope)
- Pick one to dig into as a separate planning conversation.

## Status

| ID | Title | Status |
|----|-------|--------|
| C1 | Typed postMessage protocol | done |
| A2 | AppCommand dispatch consistency | done |
| D1 | Unified typed error model | partial (worker boundary + ErrorContext + Worker/Download/Alerts/mPING reporters all wired; UI consumer remains) |
| E3 | Modal state pattern unification | done |
| E4 | Mobile/desktop chrome unification | partial (single-dispatcher cleanup done; LayoutProvider trait deferred) |
| A1 | Consolidate `WorkbenchApp` managers | not started |
| A3 | Unify acquisition state ownership | not started |
| A4 | Decompose `AppState` into nested sub-states | not started |
| B1 | Replace `Rc<RefCell<>>` in `RealtimeChannel` | not started |
| B2 | Type-enforced single-writer for `upsert_scan` | not started |
| E1 | Canvas computation/rendering split | not started |
| E2 | Overlay layering registry | not started |
| E5 | Shortcuts registry | done |
| F1 | Camera state machine via enum | not started |
| F2 | Projection trait | not started |
