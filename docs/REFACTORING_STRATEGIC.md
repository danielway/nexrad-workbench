# Strategic Refactors — Status & Future Candidates

The **strategic tier** covered changes to *how the codebase is structured*, not
just how its pieces are labeled — each initiative re-drew a boundary or ownership
line. **All four (S1–S4) are now complete.** This document records what each
delivered and the forward-looking candidates to reassess enhancements from.

## Status

| ID | Title | Status |
|----|-------|--------|
| S1 | Subsystem Decomposition | ✅ done |
| S2 | Unified Async / Effect Model | ✅ done |
| S3 | UI Layer Tree | ✅ done |
| S4 | Camera + Projection State Machine | ✅ done |

---

## What each initiative delivered

### S1. Subsystem Decomposition
Replaced the two god structures (`WorkbenchApp`, a ~45-field `AppState`) plus
orphan managers with bounded subsystems, each owning a coherent slice of state +
behavior behind a typed API: **Acquisition, Render, Timeline, Playback, Live,
Chrome, Diagnostics**. `WorkbenchApp` is now a thin coordinator; the long
`update()` loop is a sequence of subsystem ticks plus the render pass.
Cross-subsystem coordination goes through the existing `AppCommand` queue and
explicit read-only projections. `AppState` shrank from ~45 to ~22 fields.

### S2. Unified Async / Effect Model
Collapsed the four coexisting async patterns toward typed channels:
- `GpsState` + `SiteModalState` location queues moved from `Rc<RefCell<Vec<T>>>`
  to `mpsc`.
- `upsert_scan`'s single-writer requirement is enforced by an `UpsertScanGuard`
  RAII token rather than a prose comment.
- `RealtimeChannel` fully migrated: results / observations / control are typed
  channels, the active flag is `Rc<Cell<bool>>`, and there is no shared
  `RealtimeState` (see [STREAMING.md](STREAMING.md) §1).
- The `CHUNK_ACCUM` worker accumulator was evaluated for a scope-local token and
  deliberately kept as the synchronous-`FnOnce` accessor API: the accumulator
  must persist across independent worker entry points (`worker_ingest_chunk`
  mutates it; `worker_render_live` reads it), so it has to live in worker-global
  storage; the closure API already type-enforces the no-borrow-across-`.await`
  invariant, and re-entrance can't occur because no accessor closure re-enters
  the helpers. The contract is now pinned by `accum_tests`. The rationale lives at
  the thread-local in `src/nexrad/worker_api/ingest.rs`.

### S3. UI Layer Tree
Two parts, both shipped:
- **Per-frame `Derived` snapshot** ([`src/subsystem/derived.rs`](../src/subsystem/derived.rs)),
  materialized once at the top of `update()` and read by panels/overlays instead
  of recomputing visible bounds / sweep info per panel.
- **Declarative layer tree.** Every chrome panel and modal implements the `Layer`
  trait and is dispatched by z-order through `render_layout` from desktop/mobile
  layout slices, with visibility predicates absorbing the old per-panel guards
  ([`src/ui/layout.rs`](../src/ui/layout.rs)). Canvas corner-chrome overlays have
  the parallel `Overlay` trait + z-ordered registry
  ([`src/ui/canvas_overlays/mod.rs`](../src/ui/canvas_overlays/mod.rs)). Both
  registries debug-check their z-order invariants and have unit tests.

### S4. Camera + Projection State Machine
Replaced the single 15-field `GlobeCamera` struct (three disjoint 3D modes), the
separately-stored 2D `zoom`/`pan_offset`, and the independent `ViewMode`/
`CameraMode` toggles with one `Camera` enum
([`src/geo/camera.rs`](../src/geo/camera.rs)):

```rust
enum Camera {
    Flat2D(Flat2D),            // 2D pan/zoom + carried 3D site seed
    PlanetOrbit(PlanetOrbitState),
    SiteOrbit(SiteOrbitState),
    FreeLook(FreeLookState),
}
```

Each variant owns only the fields valid in its mode, so invalid-field-in-wrong-
mode access and cross-mode state leaks (e.g. `free_pos` surviving a switch to
orbit) are impossible by construction. Mode changes are explicit `switch_to_*`
transitions that carry the shared `Globe3DCommon` forward and preserve the
historical enter/leave-FreeLook math verbatim. `ViewMode` is now **derived** from
the active variant — the camera is the single source of truth. The `Projection`
trait (landed earlier) plus per-variant matrix dispatch keep `view_projection` /
`camera_world_pos` byte-identical to the old struct; GPU renderers and overlays
take `&Camera`; URL share-links round-trip via `UrlCameraSnapshot`.

---

## Verification

The whole tier is verified by `cargo check`, `cargo clippy -- -D warnings`
(zero warnings), `cargo fmt -- --check`, and the wasm unit suite
(`cargo test --bin nexrad-workbench`, **510 tests passing**), all on
`wasm32-unknown-unknown`. The browser-only `tests/idb.rs` suite compiles but runs
in CI only (needs chromedriver).

**S4 still needs live QA in the browser** — camera math/output can't be exercised
without it. Check: every transition pair among 2D / Site Orbit / Planet Orbit /
Free Look (keys 1–4, top-bar pills in Basic *and* Advanced, the `T` toggle);
pan/zoom/orbit/free-look interactions; click-to-inspect in 2D and 3D; the compass
in orbit modes (neutral in Free Look); URL share-links restoring per mode; and
mobile force-to-2D on resize.

---

## Future enhancement candidates

The strategic tier is closed; these are the deliberately-out-of-scope items to
weigh next, roughly by leverage:

1. **Pure Core / Effect Shell** — separate pure state-transition logic from side
   effects so most logic gets browser-free unit tests. The most transformative
   refactor; was too speculative before S1+S2, now viable. A tier of its own.
2. **Schema-first worker IPC** — generate the Rust types *and* `worker.js`
   dispatch from one schema. Incremental on the existing typed protocol.
3. **`too_many_arguments` reduction** — ~53 functions still carry the
   `#[allow(clippy::too_many_arguments)]` suppression. They're diffuse across
   `gpu_renderer`, `decode_worker`, `geo`, `streaming`, `timeline`, and `canvas`
   — many are inherently param-heavy render/worker signatures, not dispatch
   threading (which S3 already fixed via `LayoutCtx`). Worth bundling into context
   structs *only where it genuinely reads better*, file by file — not a blanket
   sweep.
4. **Data-flow canvas overlays → declarative registry** — S3 made the corner-
   *chrome* overlays declarative; the data-flow overlays (radar texture, alert
   polygons, site markers, sweep line, mosaic) are still drawn imperatively in
   `canvas.rs`. Their order is pipeline-inherent (radar under features under
   annotations), not accidental, so this is lower-value and carries visual-
   regression risk — do it only if a real need arises.
5. **`streaming.rs` decomposition** — split the 1850-line live-streaming loop
   ([`src/nexrad/realtime/streaming.rs`](../src/nexrad/realtime/streaming.rs))
   into focused submodules. This was the projection refactor's stretch item "P6";
   it was de-scoped to here because the projection refactor's actual goals
   (entropy reduction, collection-domain estimates) shipped, and a behavior-
   preserving split of the live-stream core would reopen live-stream QA for purely
   organizational gain. Do it when that file next needs substantial change.

These were intentionally not pursued — performance (the renderer is already fast)
and new features were also out of scope.
