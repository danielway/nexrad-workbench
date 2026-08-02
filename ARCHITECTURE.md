# NEXRAD Workbench Architecture

A WebAssembly-based NEXRAD weather radar visualization application built with Rust and egui.

This is the canonical engineering map of the codebase. For a **diagram-first
overview** — structural relationships plus runtime sequence/flow diagrams — see
the visual companion [ARCHITECTURE_DIAGRAMS.md](docs/ARCHITECTURE_DIAGRAMS.md).
For depth on a specific
subsystem, follow the dedicated references: [RENDERING.md](docs/RENDERING.md)
(GPU shader pipeline + 3D), [STREAMING.md](docs/STREAMING.md) (real-time
sequencing/timing), [TIMING.md](docs/TIMING.md) (the three time categories), and
[INDEXEDDB.md](docs/INDEXEDDB.md) (cache layer + schema). Product/UX intent lives
in [PRODUCT.md](docs/PRODUCT.md); the binding architecture standard + migration
roadmap in [CORE_SHELL.md](docs/CORE_SHELL.md); the agent/build guide in
[CLAUDE.md](CLAUDE.md).

How the current structure came to be: the 2026-07
[architecture review](docs/arch-review-2026-07/README.md) diagnosed the accreted
entropy, and the [architecture-health program](docs/arch-health-2026-07/README.md)
records what was done about it — including the decisions taken *not* to migrate
something, so they read as settled rather than unfinished.

## Module Structure

```
src/
├── main.rs              # WorkbenchApp, eframe update loop, frame orchestration
├── lib.rs               # Thin lib facade exposing `data` for tests/idb.rs
├── core/                # Headless functional core: domain types, Intent/Effect,
│                        #   pure decision fns + reducers, projection engine, timing
├── state/               # AppState root container + shell-side impls for core types
├── subsystem/           # Bounded state owners (Acquisition, Render, Live, …)
├── app/                 # Imperative shell: assemble → reduce → execute over the
│                        #   core reducers; the Effect runtime
├── nexrad/              # Data pipeline: acquisition/, live/, decode/, render/,
│                        #   detection/
├── ui/                  # egui panels, canvas, overlays, mobile chrome (thin shell)
├── geo/                 # Camera, map projection, geographic feature rendering
├── data/                # Leaf: sites, VCPs, keys, blob format, IndexedDB, quota
├── alerts/              # NWS active-alerts polling feed
├── mping/               # mPING storm-report polling feed
└── net/                 # Shared HTTP retry policy
```

The dependency direction between these modules is **enforced at build time** —
see [Architecture enforcement](#architecture-enforcement) below.

### Module Responsibilities

Coarse, module-level map with a few anchor files each. Per-file inventories rot
fastest, so this table names *responsibilities*; find the file with `grep`.

| Module | Purpose | Anchors |
|--------|---------|---------|
| `core` | The pure, headless core. `core/domain/` holds the domain vocabulary shared by every layer (radar model `Scan`/`Sweep`/`Radial`/`RadarTimeline`/`ScanMetadata`, playback time model, viz types incl. `RenderProcessing`/`SweepIdentity`/`StormCellInfo`, `UserPreferences`, `ViewState`, errors, feed states, telemetry records, worker result types). `core` also owns the contract types (`Intent`, `Effect`), the pure decision modules (`persist`, `diagnostics`, `canvas`, `render`, `panels`, `acquisition`), the worker/frame **reducers** (`worker_ingest`, `worker_decoded`, `render_loop`), the live-mode state machine (`live_mode`, `live_radar_model`), playback sweep-cache logic (`playback_manager`), the timeline view model (`timeline_view`), streaming vocabulary (`streaming_plan`, `streaming_filter`), the projection engine (`projection/` — single owner of forward-looking radar timing; `projector.rs` is its private kernel), and the chunk-timing physics fork (`timing/`, upstream-contribution candidate). No I/O, no egui, no web_sys in decision logic. | `core/mod.rs`, `core/intent.rs`, `core/effect.rs`, `core/projection/engine.rs`, `core/timing/config.rs` |
| `state` | `AppState` root container (frame scratch, status, command queue, settings) plus focused state containers (`viz`, `acquisition`, `calendar`, `url_state`, `recency`, `render_cache`, `saved_events`, `settings`, `stats`, `theme`, `app_mode`, `layer`) and the **shell halves** of core types — the impls that touch the browser (`preferences` load/save, `frame_clock` capture, `playback` wall-clock constructors). | `state/mod.rs`, `state/viz.rs`, `state/url_state.rs` |
| `subsystem` | Bounded state owners with typed APIs, each owning a coherent slice of state + channels: `Acquisition`, `Render`, `Timeline`, `Playback`, `Live`, `Chrome`, `Diagnostics`, `NetworkMonitor`, plus the per-frame `Derived` view-model cache. | `subsystem/mod.rs` |
| `app` | The imperative shell over the core reducers: each frame-loop concern assembles read-only inputs, calls the pure core, then executes the described actions (`worker_results`, `render_loop`, `acquisition_intent`, `command_dispatch`, `live_mode`, `download`, `selection_download`, `frame_setup`). Also the `Effect` runtime (`effects.rs`) and the persistence shell (`persistence_manager.rs`). | `app/mod.rs`, `app/effects.rs` |
| `nexrad` | The data pipeline, grouped by phase: `acquisition/` (S3 download, download queue, archive index, cache-load channel, coordinator), `live/` (realtime streaming loop + channel, `StreamingState`), `decode/` (record decode, ingest phases, `worker_api` WASM exports, `decode_worker` pool), `render/` (GPU renderer, globe/volume renderers, color tables, national mosaic, `RenderCoordinator`), `detection/` (storm cells). | `nexrad/mod.rs`, `nexrad/live/realtime/streaming/`, `nexrad/decode/ingest_phases.rs` |
| `ui` | Desktop panel layout, timeline, canvas + overlays, playback controls, modals, keyboard shortcuts, mobile chrome and gestures. Thin render shell per the core/shell standard. | `ui/layout.rs`, `ui/canvas.rs`, `ui/timeline/` |
| `geo` | Camera system (2D flat + unified 3D orbit camera with fly-to transitions — pure math), map projection, geographic feature types + rendering (`ViewMode`, `GeoLayerVisibility` live here), 3D globe/line renderers, built-in cities. | `geo/camera.rs`, `geo/projection.rs` |
| `data` | True leaf. Static data (`sites`, `vcp`), storage key vocabulary (`keys`), the sweep-blob wire format (`blob_format`), VCP sweep-duration physics (`vcp_timing`), the live-volume anchor state machine (`live_anchor`), the IndexedDB store (`indexeddb/`), quota policy (`quota`), and the main-thread eviction facade (`facade`). | `data/mod.rs`, `data/indexeddb/mod.rs` |
| `alerts` / `mping` | Polling feed modules: API client, parse, async channel, manager (cadence/dedup/viewport policy). Their pure state containers live in `core/domain/feeds.rs`. | `alerts/manager.rs`, `mping/manager.rs` |
| `net` | Unified retry policy (`with_retry`, `Verdict`, per-context policies) applied to every outbound HTTP request. | `net/retry.rs` |

### JavaScript / HTML

| File | Purpose |
|------|---------|
| `worker.js` | ES module Web Worker — dispatches `postMessage` commands to WASM exports |
| `service-worker.js` | Cross-origin isolation headers (COOP/COEP) and network metric collection |
| `index.html` | WASM entry point with Trunk build directives, service worker registration |
| `build.rs` | Version stamping + invokes the architecture ratchet (`tools/arch_check.rs`) |

## Architecture Enforcement

The module dependency graph is enforced by a **build-time ratchet**:
[`tools/arch_check.rs`](tools/arch_check.rs), invoked from `build.rs`, runs on
every `cargo check`/`cargo build`. It scans every `crate::<module>` reference in
`src/` (comments stripped) and fails the build when a cross-module edge appears
that is neither:

- **ALLOWED** — the intended layering (leaves `net`/`data`/`geo` import nothing;
  `core` may use the data/geo vocabulary and pure feed types; `nexrad` sits on
  `core`+leaves; `state` on the domain modules; `subsystem` on `state` and
  below; `app` on everything except `ui`; `ui` may read every layer), nor
- **GRANDFATHERED** — a known violation being burned down. Each row carries a
  reason and a burn-down pointer. Currently one edge remains:
  `app → ui` (the geolocation effect executor lives in ui; moves in Phase C2).

Reading a failure: the panic lists the offending edge (`from -> to`) with up to
8 `file:line` sites. Fix the dependency direction, or — only with a real
architectural justification — add the edge to `ALLOWED`. **Never add to
GRANDFATHERED.** The ratchet also fails when a grandfathered edge no longer
occurs, forcing its row to be deleted — the table only shrinks.

## Data Flow

### Archive Download (Primary Pipeline)
```
User selects site/date
  → AcquisitionCoordinator fetches AWS S3 listing
  → ArchiveIndex caches listing
  → User selects scan (or range queued in the download queue)
  → Worker ingest: split records → decompress → decode → extract sweep blobs
  → upsert_scan: pre-computed sweep blobs + scan-index entry into IndexedDB
  → Return sweep metadata to main thread
  → RenderCoordinator sends worker.render(scan_key, elevation, product)
  → Worker reads single sweep blob from IDB → marshals for transfer
  → Main thread uploads raw f32 data to GPU R32F texture
  → Fragment shader: polar→Cartesian + raw→physical conversion + LUT color
```

### Real-time Streaming
```
Start live mode
  → subsystem::Live starts RealtimeChannel, which spawns streaming_loop
    (nexrad/live/realtime/streaming/)
  → Loop: predict next chunk poll time (core ProjectionEngine) → sleep → fetch
  → Each chunk → worker.ingest_chunk → decode + accumulate radials
  → Completed elevations → sweep blobs stored to IDB (upsert_scan)
  → Partial elevations → worker.render_live (reads in-memory accumulator)
  → Main thread: core::worker_ingest reducer applies the outcome, describes
    GPU/render/status actions the shell executes
  → GPU texture updated per chunk → sweep line extrapolated between chunks
```

### Playback / Scrubbing
```
Timeline position changes
  → core::render_loop::reduce_advance_playback (pure) decides the sweep to show
  → RenderCoordinator dedup gate (core::render::should_dispatch vs SweepIdentity)
  → Worker.render(scan_key, elevation, product)
  → Reads pre-computed sweep blob from IDB (near-zero decode cost)
  → GPU texture upload → immediate re-render
```

Elevation / product changes take the same path — the dedup gate sees the
changed parameter and dispatches a fresh worker render.

## The Reducer Pattern (Env / Slices / Actions)

The decision mass that used to live inline in `src/app/` methods is extracted
into pure **reducers** in `core`, all following one shape. Using
[`core::worker_ingest`](src/core/worker_ingest.rs) as the exemplar:

1. The shell (`app/worker_results.rs`) assembles a read-only **Env** snapshot
   (`ChunkIngestEnv`: is_live, site, product string, frame time, coordinator
   elevations, frame projection) — everything the decision reads but doesn't
   own.
2. It passes mutable **Slices** over the core-owned state
   (`ChunkIngestSlices`: live-mode state, projection engine, elevation
   selection, playback) — the reducer mutates these in memory only.
3. The reducer (`reduce_chunk_ingested`) returns an **Actions** struct
   (`ChunkIngestActions`) *describing* every side effect: channel records, the
   live render dispatch, GPU texture promotion, status text, intents to queue,
   render-request flags.
4. The shell executes the actions **in field order**.

The same pattern backs `core::worker_decoded` (decode outcomes → GPU upload
decisions), `core::render_loop` (`reduce_advance_playback` +
`decide_prefetch_and_caption`, two decision points separated by effects), and
`core::acquisition` (the prefetch/listing pump reducers). Reducers are
unit-tested headlessly; the shells are mechanical assemble→call→execute
wrappers.

## Key Types

### Data Types
| Type | Where | Description |
|------|-------|-------------|
| `ScanKey` | `data/keys.rs` | Unique identifier: `SITE\|SCAN_START_MS` |
| `SweepDataKey` | `data/keys.rs` | Pre-computed sweep: `SITE\|SCAN_START_MS\|ELEV_NUM\|PRODUCT` |
| `ScanIndexEntry` / `CachedSweep` | `data/keys.rs` | Per-scan IDB metadata: VCP plan + realized cached sweeps |
| `ScanHeader` / `ElevationUpload` | `data/keys.rs` | Inputs to the `upsert_scan` write contract |
| `PrecomputedSweep` | `data/blob_format.rs` | The 72-byte-header sweep blob wire format |
| `ExtractedVcp` | `data/vcp_timing.rs` | VCP pattern data extracted from Message Type 5 |
| `ScanMetadata` | `core/domain/radar.rs` | Lightweight scan metadata for fast timeline queries |
| `Scan` / `Sweep` / `Radial` / `RadarTimeline` | `core/domain/radar.rs` | The in-memory radar timeline model |

### Rendering Types
| Type | Where | Description |
|------|-------|-------------|
| `RadarGpuRenderer` | `nexrad/render/gpu_renderer/` | WebGL2 renderer: polar data texture + LUT + fragment shader |
| `GlobeRadarRenderer` | `nexrad/render/globe_radar_renderer.rs` | Radar projection onto 3D globe surface mesh |
| `VolumeRayRenderer` | `nexrad/render/volume_ray_renderer.rs` | 3D volumetric ray-marching through all elevations |
| `SweepIdentity` / `VolumeRenderRequest` | `core/domain/viz.rs`, `nexrad/render/render_request.rs` | Dedup identities checked by `core::render::should_dispatch` |

### Coordination Types
| Type | Where | Description |
|------|-------|-------------|
| `AcquisitionCoordinator` | `nexrad/acquisition/` | Owns download pipeline, archive index, cache load, download queue |
| `RenderCoordinator` | `nexrad/render/` | Owns decode worker dispatch; dedup via the pure `should_dispatch` gate |
| `RealtimeChannel` | `nexrad/live/realtime/` | Typed mailboxes + lifecycle of the live `streaming_loop` |
| `ProjectionEngine` | `core/projection/` | Single owner of forward-looking radar timing (plans, forecasts) |
| `PersistenceManager` | `app/persistence_manager.rs` | Shell over `core::decide_persist` — URL push throttle + prefs save |
| `NetworkMonitor` | `subsystem/network_monitor.rs` | Service worker metric listener (record types in `core/domain/telemetry.rs`) |

## Async Architecture

The application bridges async operations with egui's synchronous update loop using channel-based communication and per-frame polling.

### Channel Pattern
```rust
// Spawn async task
channel.start_operation(ctx.clone(), params);

// Poll each frame in update()
if let Some(result) = channel.try_recv() {
    handle_result(result);
}
```

### Web Worker

Heavy computation (bzip2 decompression, NEXRAD decoding, sweep extraction, IDB I/O) runs in a pool of Web Workers (`worker.js`) to keep the UI thread responsive. The pool is sized at startup (`default_pool_size`); commands are dispatched to the next available worker. Communication uses `postMessage` with Transferable ArrayBuffers for zero-copy data transfer.

| Operation | Direction | Purpose |
|-----------|-----------|---------|
| `init` | Main → Worker | Initialize with Trunk-generated WASM/JS URLs |
| `ingest` | Main → Worker | Full archive: split, decode, extract sweeps, store in IDB |
| `ingest_chunk` | Main → Worker | Real-time chunk: decode, accumulate, flush completed sweeps |
| `render` | Main → Worker | Read pre-computed sweep from IDB, marshal for GPU upload |
| `render_volume` | Main → Worker | Pack all elevations for 3D ray-marching |
| `render_live` | Main → Worker | Read partial sweep from in-memory accumulator (synchronous) |

The Rust side of this protocol lives in `nexrad/decode/worker_api/` (the
`#[wasm_bindgen]` exports worker.js calls) and `nexrad/decode/decode_worker/`
(the main-thread pool + typed send/receive).

### GPU Raw Decode Pipeline

Gate values are stored as raw u8/u16 in NEXRAD archives. The physical conversion
`physical = (raw - offset) / scale` happens in the GPU fragment shader, which means:
- Raw values 0 (below threshold) and 1 (range folded) are sentinel values
- The shader checks `v > 1.5` to identify valid data
- Bilinear interpolation and smoothing work correctly on raw values because the
  linear transform is invariant under interpolation
- GPU uniforms `u_offset` and `u_scale` are set per-frame

### Platform-Specific Spawning
- **WASM**: `wasm_bindgen_futures::spawn_local()`
- **Native**: `std::thread::spawn()` + `pollster::block_on()` (development only)

## Caching Strategy

### Pre-computed Sweep Storage

During ingestion, radials are grouped by elevation and product, then serialized as
compact sweep blobs and stored in IndexedDB. At render time, the worker reads a single
blob and marshals it for GPU upload — no decompression or decoding needed. This gives
near-zero render latency for scrubbing and elevation changes.

### IndexedDB Schema

Database `nexrad-workbench`, schema version 5, with three string-keyed object
stores: `sweeps` (pre-computed sweep blobs, the primary render path), `scan_index`
(per-scan metadata for fast timeline queries), and `scan_touches` (per-scan
last-access timestamps for LRU eviction, isolated so fire-and-forget touch bumps
don't race index writes). All writes go through the single `upsert_scan`
create-or-merge entry point. Schema upgrades are destructive.

The full store layout, payload byte formats, concurrency model, and key-range
query rules are documented in [INDEXEDDB.md](docs/INDEXEDDB.md) — the single
source of truth for the schema.

### Scan Completeness States

| State | Description |
|-------|-------------|
| `Missing` | No records present |
| `PartialNoVcp` | Some records, no VCP metadata |
| `PartialWithVcp` | Some records with VCP (can determine expected count) |
| `Complete` | All expected records present |

### Three-Layer Cache

1. **IndexedDB** (persistent, WASM only)
   - Pre-computed sweep blobs and per-scan metadata
   - Configurable quota with LRU eviction
   - Survives page reload

2. **GPU textures** (video memory)
   - R32F data texture (azimuths x gates) for current sweep
   - RGBA8 LUT texture for color mapping
   - Content-signature-based invalidation

3. **In-memory accumulator** (worker only, live mode)
   - `ChunkAccumulator` holds partial sweeps during real-time streaming
   - Flushed to IDB when elevations complete
   - Readable via `render_live` for immediate partial display

## State Management

`WorkbenchApp` (in `main.rs`) is a thin coordinator: it owns `AppState` plus the
bounded subsystems, and its `update()` runs the frame sequence — frame setup,
channel drains (via the core reducers), command dispatch, effect execution,
then UI layout.

- **`AppState`** (`state/mod.rs`) holds cross-cutting state: the per-frame
  clock (`frame_now`), viz/layer state, status message, session stats, storage
  settings, saved events, theme, mobile/width-tier resolution, the recent-error
  ring, and the **command queue** (`VecDeque<Intent>`).
- **Subsystems** (`subsystem/`) own their domains: `Acquisition` (download
  pipeline + operation tracking), `Render` (worker pool + scan/elevation
  tracking), `Timeline` (scan inventory), `Playback` (cursor/speed/mode),
  `Live` (streaming channel + live-mode state + per-frame derived models),
  `Chrome` (UI visibility flags + modal booleans), `Diagnostics` (alerts,
  mPING, GPS, network monitor), and `Derived` (the per-frame view-model cache).

### Intent Pattern
State mutations from UI actions are expressed as `Intent` variants
(`core/intent.rs` — defined in the core; there is no separate `AppCommand`
anymore). UI code pushes intents via `AppState::push_command`; the main update
loop drains and dispatches them (`app/command_dispatch.rs`). Decisions the
dispatch needs are pure core functions; the side effects they describe are
executed by the `Effect` runtime (`app/effects.rs`).

## UI Layout

```
┌──────────────────────────────────────────────────────────┐
│ Top Bar: Site context, status, mode indicators           │
├──────────┬─────────────────────────────┬─────────────────┤
│ Left     │ Canvas                      │ Right Panel     │
│ Panel    │ (Radar + Geographic layers  │ • Product       │
│ (Radar   │  + Overlays)                │ • Palette       │
│  Ops)    │                             │ • Layers        │
│          │                             │ • Processing    │
│          │                             │ • 3D Options    │
├──────────┴─────────────────────────────┴─────────────────┤
│ Acquisition Drawer (expandable: queue + network tabs)    │
├──────────────────────────────────────────────────────────┤
│ Bottom: Timeline | Playback Controls | Stats             │
└──────────────────────────────────────────────────────────┘
```

When `AppState::is_mobile` is true, `ui/mobile/` replaces this desktop layout
with dedicated mobile chrome (tabs, scrubber, gesture handling, auto-hide).

Canvas overlays live in `ui/canvas_overlays/` — one module per overlay
(national mosaic, alerts, mPING reports, GPS location, sweep line/donut, site
markers, info text, color scale, scale bar, compass, globe). `ui/canvas.rs`
owns the draw order and the radar texture pass.

## Platform Support

The codebase targets WASM primarily, with native stubs for development.

```toml
# .cargo/config.toml
[build]
target = "wasm32-unknown-unknown"
```

Conditional compilation via `#[cfg(target_arch = "wasm32")]` gates:
- IndexedDB storage
- Web Worker communication
- Async spawning mechanism
- Browser-specific APIs (js-sys, web-sys)

## Build

```bash
# Development server with hot reload
trunk serve

# Production build
trunk build --release

# Check only (no bundle) — also runs the architecture ratchet
cargo check
```

Pre-commit hooks enforce `cargo fmt`, `cargo clippy -D warnings`, and the fast
headless test suite (`cargo test --bin nexrad-workbench`) via cargo-husky.
`#![warn(unreachable_pub)]` is enabled crate-wide (the `data` module is
exempted as the lib facade consumed by `tests/idb.rs`).
