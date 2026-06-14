# Strategic Refactors — Remaining Work

This is the **strategic tier**: changes to *how the codebase is structured*,
not just how its pieces are labeled. An earlier tactical pass handled
readability (typed worker protocols, named command outcomes, a shortcuts
registry, aggregated modal state, unified mobile/desktop chrome). The four
strategic initiatives below each re-draw a boundary or ownership line; any one is
a multi-PR project.

**Two are done (S1, S2), two remain (S3, S4) plus one small S2 follow-up.** The
completed work is summarized briefly at the end; this doc now leads with what's
left.

## Status at a glance

| ID | Title | Status |
|----|-------|--------|
| S1 | Subsystem Decomposition | ✅ **done** — `AppState` shrank from ~45 to ~22 fields; `WorkbenchApp` owns 7 bounded subsystems (Acquisition, Diagnostics, Render, Live, Timeline, Playback, Chrome). |
| S2 | Unified Async / Effect Model | ✅ **done**, one small follow-up — see *S2 remnant* below. |
| S3 | UI Layer Tree | ⬜ **not started** — substantial design work. |
| S4 | Camera + Projection State Machine | 🟦 **partial** — the `Projection` trait + `GlobeProjection` adapter landed; the `GlobeCamera` → `Camera` enum split has not. |

---

## S2 remnant — CHUNK_ACCUM compile-time safety

The bulk of S2 shipped (typed channels everywhere, `UpsertScanGuard` RAII for the
single-writer requirement, `RealtimeChannel` fully migrated to typed
results/observations/control channels with an `Rc<Cell<bool>>` active flag and no
shared `RealtimeState`).

One piece remains: `CHUNK_ACCUM` in
[`src/nexrad/worker_api/ingest.rs`](../src/nexrad/worker_api/ingest.rs) is still a
per-worker thread-local accessed through runtime-checked helpers
(`with_chunk_accum` / `with_chunk_accum_mut` / `set_chunk_accum`). The re-entrance
invariant ("don't hold the accumulator borrow across `.await`") is enforced at
runtime, not by the type system.

**The change:** replace the thread-local + runtime check with a scope-local typed
token that statically forbids holding the accumulator across an `.await`. Small,
isolated; add tests for the re-entrance scenarios.

---

## S3. UI Layer Tree (not started)

### The problem
UI dispatch is imperative and scattered:
- `main.rs` calls panels in a specific order with mobile/desktop branching.
- Each panel has its own visibility checks (sidebar visible, advanced mode, …).
- Modal overlays are separate render calls at the tail of `update()`.
- Canvas overlays (`ui/canvas_overlays/`) are a flat imperative sequence with
  implicit z-order.
- Derived state (visible bounds, sweep azimuth, current sweep info) is recomputed
  per frame across multiple panels.

No single place captures the whole layout. **Symptom:** 53 functions currently
silence `clippy::too_many_arguments` (up from 40 when this plan was written —
threading grew as S1 landed). A declarative layer tree would address this by
giving each rendered surface its own bound context.

### The change
Two complementary parts.

**Part A: Per-frame `Derived` snapshot.** Populated once at the top of `update()`,
before any UI render. Holds visible bounds, current scan/sweep info, sweep-line
azimuth, camera-settled flag, etc. Panels and overlays read from `&Derived`
instead of recomputing.

**Part B: Typed layer tree.** Replace the imperative panel + overlay + modal
sequence with a declarative tree:

```rust
enum Layer {
    Panel  { region: PanelRegion, visible: VisibilityFn, render: RenderFn },
    Overlay{ z_order: i32, render: RenderFn },
    Modal  { z_order: i32, visible: VisibilityFn, render: RenderFn },
    Group(Vec<Layer>),
}
```

Mobile and desktop layouts become tree builders
(`build_desktop_layout(state) -> LayerTree`, `build_mobile_layout(...)`); a single
renderer walks the tree. Modal stacking becomes z-order, not order-of-call.
Conditional visibility moves into the builders, removing per-panel guards.

### Why it's ambitious / effort
- Reorganizes UI dispatch + derivation in one move; enables unit tests against
  the layer tree without running egui; forces mobile and desktop into one model
  with two configs.
- **3–5 PRs.** Order: `Derived` struct first (lowest risk), then `Overlay` tree,
  then panel + modal tree.
- **Medium risk** — primarily organizational; egui still renders. Visual
  regressions possible if z-order shifts. Mitigate with per-PR `trunk serve`
  walkthroughs in both viewports.

### Critical files
- `src/main.rs` (update loop + render-pass tail)
- `src/ui/canvas.rs` (`render_canvas_with_geo` and the overlay calls)
- `src/ui/canvas_overlays/mod.rs` and submodules
- `src/ui/{left_panel, right_panel, bottom_panel, top_bar}.rs` (extract visibility
  predicates)
- `src/ui/mobile/{mod.rs, tabs.rs, top_bar.rs}` (becomes a tree builder)

---

## S4. Camera + Projection State Machine (partial)

### Done
The `Projection` trait ([`src/geo/projection.rs`](../src/geo/projection.rs)) and
its `GlobeProjection` adapter ([`src/geo/camera.rs`](../src/geo/camera.rs))
landed, so UI interaction handlers and overlays can work generically on a
projection.

### The problem that remains
- `GlobeCamera` ([`src/geo/camera.rs`](../src/geo/camera.rs), now **989 lines**,
  up from 889) is one struct with 15+ public fields covering three disjoint modes
  (PlanetOrbit, SiteOrbit, FreeLook). Any code can mutate any field in any mode,
  and transitions leak state (e.g. `free_pos` retained across a switch to orbit).
- 2D camera state (`zoom`, `pan_offset`) lives on `viz_state`, not on the camera
  struct at all. There is no `Camera` type that owns both 2D and 3D state.
- Every interaction handler in `canvas_interaction.rs` still branches on
  `ViewMode`.

### The change
Split `GlobeCamera` into a single camera state machine, with `ViewMode` becoming a
derived view of the variant rather than an independent toggle to keep in sync:

```rust
enum Camera {
    Flat2D(Flat2DState),       // zoom, pan_offset
    PlanetOrbit(PlanetOrbitState),
    SiteOrbit(SiteOrbitState),
    FreeLook(FreeLookState),
}
```

Mode transitions become explicit `Camera::switch_to_*` methods that construct the
new variant from the old (preserving what makes sense, dropping what doesn't).
Field validity becomes type-enforced, eliminating "ViewMode branch missing
somewhere" bugs.

### Effort
- **2–4 PRs.** Order: extract `Flat2DState` from `viz_state`; introduce the
  `Camera` enum; migrate call sites onto it.
- **High risk** — camera is touched by every interaction handler and overlay.
  Manual UI testing of every camera transition is required (no automated coverage
  today).

### Critical files
- `src/geo/camera.rs` (the bulk of the work)
- `src/geo/projection.rs` (the 2D side)
- `src/ui/canvas_interaction.rs` (branches on `ViewMode`)
- `src/state/viz.rs` (where 2D camera state lives)
- `src/ui/shortcuts.rs` (camera mode switching via 1–4 keys — registry exists,
  just repoint at new types)

---

## Recommended sequence for what's left

1. **S2 remnant** (CHUNK_ACCUM) — small, isolated, do anytime.
2. **S3 (UI Layer Tree)** and **S4 (Camera enum split)** are independent of each
   other and can proceed in parallel. S4 is more isolated; S3 has the larger
   blast radius but the clearer payoff (the `too_many_arguments` count keeps
   climbing).

Each initiative is multiple PRs. If appetite is lower than a full initiative, the
smaller changes within each can be done incrementally.

## Verification per PR

1. `cargo check` and `cargo clippy -- -D warnings` clean.
2. `cargo test --bin nexrad-workbench` — pure-Rust logic suite.
3. `CHROMEDRIVER=/usr/bin/chromedriver cargo test --test idb` for any IDB-touching
   change (the S2 remnant touches `worker_api/ingest.rs`).
4. `trunk serve` + manual flow:
   - **S2 remnant**: ingest under quota pressure; realtime ingest.
   - **S3**: every panel + modal in desktop and mobile viewports; modal stacking
     order; sidebar visibility transitions.
   - **S4**: all four camera modes plus every transition pair; pan/zoom/orbit/
     free-look; click-to-inspect in both 2D and 3D.

## Out of scope (recorded, not planned)

- **Pure Core / Effect Shell** — separate pure state-transition logic from side
  effects to enable browser-free unit tests for most logic. The truly
  transformative refactor; viable now that S1+S2 are done, but a tier beyond this.
- **Schema-first worker IPC** — generate Rust types + `worker.js` dispatch from
  one schema. Incremental on the existing typed protocol; not strategic-sized.
- **Performance** — the renderer is already fast.
- **New features** — the app's functionality is set.

---

## Completed work (history)

### S1. Subsystem Decomposition ✅
Replaced the two god structures + orphan managers with bounded subsystems, each
owning a coherent slice of state + behavior behind a typed API:

| Subsystem | Owns |
|-----------|------|
| `Acquisition` | download queue/channel, archive index, cache loader, pending download |
| `Render` | worker pool, GPU resources, render dedup, sweep cache, displayed-scan tracking |
| `Timeline` | scans, sweeps, time bounds, shadow boundaries, scrub cache |
| `Playback` | position, speed, mode, animation, view bounds, time model |
| `Live` | realtime channel, observations, projector, live model, app mode |
| `Chrome` | UI flags, sidebar visibility, theme, modal opens/states, mobile chrome |
| `Diagnostics` | alerts, mPING, dev mode, network monitor, session stats, GPS |

`WorkbenchApp` became a thin coordinator; the long `update()` loop became a
sequence of subsystem ticks plus the render pass. Cross-subsystem coordination
goes through the existing `AppCommand` queue and explicit read-only projections.

### S2. Unified Async / Effect Model ✅ (one remnant)
Collapsed the four coexisting async patterns toward typed channels:
- `GpsState` + `SiteModalState` location queues moved from `Rc<RefCell<Vec<T>>>`
  to `mpsc`.
- `upsert_scan`'s single-writer requirement is enforced by an `UpsertScanGuard`
  RAII token rather than a prose comment.
- `RealtimeChannel` fully migrated: results / observations / control are typed
  channels, the active flag is `Rc<Cell<bool>>`, and there is no shared
  `RealtimeState`. (See [STREAMING.md](STREAMING.md) §1.)

The one remaining piece is the CHUNK_ACCUM compile-time token described at the top.

---

## Cross-cutting themes (the "why")

These motivated the whole tier; S1/S2 addressed the first two, S3/S4 address the
rest:

1. **State ownership was scattered** — a god `AppState` + many managers anyone
   could reach into. *(Addressed by S1.)*
2. **Four async patterns coexisted** — `spawn_local`, polled channels,
   `Rc<RefCell<Vec<T>>>` shared queues, and direct `async fn` in `worker_api`;
   WASM async failures are silent, so subtle bugs hid. *(Addressed by S2, modulo
   the CHUNK_ACCUM remnant.)*
3. **2D and 3D barely overlap** — parallel coordinate transforms, camera state
   split across `viz_state` and the camera struct, every handler branching on
   `ViewMode`. *(S4 target.)*
4. **UI rendering is imperative and scattered** — panels mutate `AppState`
   directly; derived state recomputed per frame; visibility split across
   `main.rs`, per-panel guards, and per-overlay implicit ordering. *(S3 target.)*
