# NEXRAD Workbench Architecture

A WebAssembly-based NEXRAD weather radar visualization application built with Rust and egui.

## Module Structure

```
src/
├── main.rs              # Application entry, event loop, channel orchestration
├── state/               # Application state management
├── nexrad/              # NEXRAD data pipeline (download, decode, cache, render)
├── ui/                  # egui panels and rendering
├── geo/                 # Geographic projections and layer rendering
└── data/                # Static data (site definitions), IndexedDB storage, keys
```

### Module Responsibilities

| Module | Purpose |
|--------|---------|
| `state` | Centralized state tree (`AppState`) with sub-states for playback, visualization, layers, live mode, acquisition, preferences, session stats |
| `nexrad` | Data acquisition (download, realtime streaming), Web Worker operations, GPU rendering, 3D globe/volume rendering, coordination managers |
| `ui` | Panel layout, timeline, canvas with overlays, playback controls, modals, keyboard shortcuts |
| `geo` | Map projection, camera system, geographic feature rendering (states, counties, cities), globe rendering |
| `data` | NEXRAD site definitions, storage key types, IndexedDB abstraction, record storage facade |

### Source Files

#### `state/`
| File | Purpose |
|------|---------|
| `mod.rs` | Root `AppState` definition, `AppCommand` enum, re-exports |
| `playback.rs` | Playback controls — timestamp, speed, loop mode, selection range |
| `playback_manager.rs` | Sweep cache, previous-sweep resolution, animation helpers |
| `radar_data.rs` | `RadarTimeline` — scan/sweep/radial timeline representation |
| `viz.rs` | Visualization state — product, palette, zoom, pan, render mode, view mode |
| `live_mode.rs` | Live streaming state machine (`LivePhase`) and phase transitions |
| `acquisition.rs` | Unified acquisition tracking — operation queue, status, network correlation |
| `layer.rs` | Geographic layer visibility toggles |
| `preferences.rs` | User preferences persistence (localStorage) |
| `saved_events.rs` | User-saved weather event bookmarks (localStorage) |
| `settings.rs` | Storage quotas and eviction targets |
| `stats.rs` | Session metrics — download, ingest, and render timing |
| `theme.rs` | Dark/light theme mode |
| `url_state.rs` | URL parameter parsing for deep linking |
| `vcp.rs` | Volume Coverage Pattern definitions |

#### `nexrad/`

Directory modules (split into sub-files):

| Directory | Sub-files | Purpose |
|-----------|-----------|---------|
| `gpu_renderer/` | `mod.rs`, `shaders.rs`, `textures.rs`, `inspect.rs` | WebGL2 radar rendering with OKLab color interpolation, polar→Cartesian shader, LUT textures, CPU-side value lookups |
| `decode_worker/` | `mod.rs`, `send.rs`, `receive.rs`, `types.rs` | Web Worker lifecycle, message send/receive, typed payloads, result polling |
| `worker_api/` | `mod.rs`, `ingest.rs`, `render.rs`, `render_live.rs` | WASM exports called from worker.js — ingest, render, live render implementations |

Single-file modules:

| File | Purpose |
|------|---------|
| `acquisition_coordinator.rs` | Owns download pipeline, archive index, cache load channel, download queue |
| `render_coordinator.rs` | Owns decode worker, request deduplication via `last_render_params` |
| `streaming_manager.rs` | Live streaming and backfill lifecycle, unified polling API |
| `persistence_manager.rs` | URL state pushing (throttled ~1/sec) and preference saving |
| `network_monitor.rs` | Service worker network metric collection and aggregate stats |
| `download.rs` | AWS S3 download pipeline with async channels and progress tracking |
| `archive_index.rs` | Archive file listing and caching |
| `realtime.rs` | Real-time chunk streaming pipeline |
| `record_decode.rs` | Archive2 record parsing and sweep data extraction |
| `ingest_phases.rs` | Core decode pipeline: decompress, VCP extract, radial grouping, sweep blob generation |
| `render_request.rs` | Render parameter types for request deduplication |
| `types.rs` | `CachedScan`, `ScanMetadata` types |
| `cache_channel.rs` | IndexedDB metadata loading channel |
| `color_table.rs` | Product color scales and value ranges |
| `download_queue.rs` | Serial download queue state machine |
| `globe_radar_renderer.rs` | Radar data projection onto 3D globe surface |
| `volume_ray_renderer.rs` | 3D volumetric ray-marching renderer |

#### `ui/`

Directory modules:

| Directory | Sub-files | Purpose |
|-----------|-----------|---------|
| `timeline/` | `mod.rs`, `ruler.rs`, `scan_track.rs`, `sweep_track.rs`, `interaction.rs`, `overlays.rs`, `tooltips.rs` | Zoomable timeline with time ruler, scan/sweep tracks, scrubbing, download ghosts, saved event markers |
| `canvas_overlays/` | `mod.rs`, `color_scale.rs`, `compass.rs`, `globe.rs`, `info.rs`, `sites.rs`, `sweep.rs` | Visual overlays drawn on top of the radar canvas |

Single-file modules:

| File | Purpose |
|------|---------|
| `canvas.rs` | Central radar visualization canvas with geographic layers |
| `canvas_inspector.rs` | Hover tooltip (lat/lon, value), crosshair, distance measurement, storm cells |
| `canvas_interaction.rs` | Pan/zoom/click input handling for 2D and globe views |
| `playback_controls.rs` | Play/pause, speed, loop mode, step controls |
| `left_panel.rs` | Radar operations panel (VCP, elevation, scan info) |
| `right_panel.rs` | Product, palette, layers, processing, 3D options |
| `top_bar.rs` | Site context, status messages, mode selector |
| `bottom_panel.rs` | Playback controls, stats, and acquisition drawer toggle |
| `acquisition_drawer.rs` | Expandable drawer showing download queue and network activity |
| `network_panel.rs` | Network request log with aggregate statistics |
| `colors.rs` | Color definitions for products and UI elements |
| `shortcuts.rs` | Keyboard shortcut handling and help overlay |
| `site_modal.rs` | Site selection modal |
| `stats_modal.rs` | Session statistics detail modal |
| `event_modal.rs` | Saved event create/edit/delete modal |
| `wipe_modal.rs` | Cache wipe confirmation modal |
| `modal_helper.rs` | Shared backdrop pattern for modal overlays |

#### `geo/`
| File | Purpose |
|------|---------|
| `camera.rs` | Map projection camera system (2D flat, SiteOrbit, PlanetOrbit, FreeLook) |
| `layer.rs` | Geographic feature types (states, counties, cities) |
| `renderer.rs` | Geographic feature rendering on 2D canvas |
| `projection.rs` | Map projection transformations |
| `globe_renderer.rs` | 3D globe sphere rendering |
| `geo_line_renderer.rs` | Geographic line rendering on the 3D globe |
| `cities.rs` | Built-in US cities data (~300 cities) |

#### `data/`
| File | Purpose |
|------|---------|
| `sites.rs` | All NEXRAD site definitions (156+ sites) |
| `keys.rs` | Storage key types (`ScanKey`, `RecordKey`, `SweepDataKey`, `SweepMeta`, `ExtractedVcp`) |
| `indexeddb.rs` | IndexedDB browser storage abstraction |
| `facade.rs` | Record storage facade |

### JavaScript / HTML

| File | Purpose |
|------|---------|
| `worker.js` | ES module Web Worker — dispatches `postMessage` commands to WASM exports |
| `service-worker.js` | Cross-origin isolation headers (COOP/COEP) and network metric collection |
| `index.html` | WASM entry point with Trunk build directives, service worker registration |
| `build.rs` | Build script for compile-time asset preparation |

## Data Flow

### Archive Download (Primary Pipeline)
```
User selects site/date
  → AcquisitionCoordinator fetches AWS S3 listing
  → ArchiveIndex caches listing
  → User selects scan (or range queued in DownloadQueueManager)
  → Worker ingest: split records → decompress → decode → extract sweep blobs
  → Store pre-computed sweep blobs + metadata in IndexedDB
  → Return sweep metadata to main thread
  → RenderCoordinator sends worker.render(scan_key, elevation, product)
  → Worker reads single sweep blob from IDB → marshals for transfer
  → Main thread uploads raw f32 data to GPU R32F texture
  → Fragment shader: polar→Cartesian + raw→physical conversion + LUT color
```

### Real-time Streaming
```
Start live mode
  → StreamingManager spawns RealtimeChannel (chunk iterator)
  → Each chunk → worker.ingest_chunk → decode + accumulate radials
  → Completed elevations → sweep blobs stored to IDB
  → Partial elevations → worker.render_live (reads in-memory accumulator)
  → GPU texture updated per chunk → sweep line extrapolated between chunks
  → Timeline updated, UI refreshed
```

### Playback / Scrubbing
```
Timeline position changes
  → RadarTimeline.find_recent_scan(timestamp)
  → RenderCoordinator detects param change vs last_render_params
  → Worker.render(scan_key, elevation, product)
  → Reads pre-computed sweep blob from IDB (near-zero decode cost)
  → GPU texture upload → immediate re-render
```

### Elevation / Product Change
```
User changes elevation or product
  → RenderCoordinator detects param change
  → Worker.render(same scan_key, new elevation/product)
  → Same flow as scrubbing
```

## Key Types

### Data Types
| Type | Description |
|------|-------------|
| `ScanKey` | Unique identifier: `SITE\|SCAN_START_MS` |
| `RecordKey` | Record identifier: `SITE\|SCAN_START_MS\|RECORD_ID` |
| `SweepDataKey` | Pre-computed sweep: `SITE\|SCAN_START_MS\|ELEV_NUM\|PRODUCT` |
| `SweepMeta` | Lightweight sweep metadata (time span, elevation, azimuth) |
| `ExtractedVcp` | VCP pattern data extracted from Message Type 5 |
| `ScanMetadata` | Lightweight (~100 bytes) scan metadata for fast timeline queries |

### Rendering Types
| Type | Description |
|------|-------------|
| `RadarGpuRenderer` | WebGL2 renderer: polar data texture + LUT + fragment shader |
| `GlobeRadarRenderer` | Radar projection onto 3D globe surface mesh |
| `VolumeRayRenderer` | 3D volumetric ray-marching through all elevations |
| `RenderRequest` | Parameters for deduplication (scan_key + elevation + product) |

### Coordination Types
| Type | Description |
|------|-------------|
| `AcquisitionCoordinator` | Owns download pipeline, archive index, cache load, download queue |
| `RenderCoordinator` | Owns decode worker, request deduplication, scan/elevation state |
| `StreamingManager` | Owns realtime + backfill channels, unified polling |
| `PersistenceManager` | URL state pushing (throttled) and preference saving |
| `NetworkMonitor` | Service worker metric listener, ring buffer, aggregate stats |

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

Heavy computation (bzip2 decompression, NEXRAD decoding, sweep extraction, IDB I/O) runs in a dedicated Web Worker (`worker.js`) to keep the UI thread responsive. Communication uses `postMessage` with Transferable ArrayBuffers for zero-copy data transfer.

| Operation | Direction | Purpose |
|-----------|-----------|---------|
| `init` | Main → Worker | Initialize with Trunk-generated WASM/JS URLs |
| `ingest` | Main → Worker | Full archive: split, decode, extract sweeps, store in IDB |
| `ingest_chunk` | Main → Worker | Real-time chunk: decode, accumulate, flush completed sweeps |
| `render` | Main → Worker | Read pre-computed sweep from IDB, marshal for GPU upload |
| `render_volume` | Main → Worker | Pack all elevations for 3D ray-marching |
| `render_live` | Main → Worker | Read partial sweep from in-memory accumulator (synchronous) |

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

```
nexrad-workbench
├── records           - Raw bzip2-compressed record blobs (legacy, kept for fallback)
│   Key: "SITE|SCAN_START_MS|RECORD_ID"
│   Value: ArrayBuffer (raw bytes)
│
├── record_index      - Per-record metadata
│   Key: "SITE|SCAN_START_MS|RECORD_ID"
│   Value: { key, record_time, size_bytes, has_vcp, stored_at }
│
├── scan_index        - Per-scan metadata
│   Key: "SITE|SCAN_START_MS"
│   Value: { scan, has_vcp, expected_records, present_records, ... }
│
└── sweep_data        - Pre-computed sweep blobs (primary render path)
    Key: "SITE|SCAN_START_MS|ELEV_NUM|PRODUCT"
    Value: ArrayBuffer (compact binary: azimuth count, gate count, metadata, raw gate values)
```

### Scan Completeness States

| State | Description |
|-------|-------------|
| `Missing` | No records present |
| `PartialNoVcp` | Some records, no VCP metadata |
| `PartialWithVcp` | Some records with VCP (can determine expected count) |
| `Complete` | All expected records present |

### Three-Layer Cache

1. **IndexedDB** (persistent, WASM only)
   - Pre-computed sweep blobs and record-level storage
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

Single `AppState` struct passed by mutable reference through the application.

```rust
AppState {
    playback_state: PlaybackState,       // timestamp, speed, selection, loop mode
    radar_timeline: RadarTimeline,       // scans with sweeps and radials
    viz_state: VizState,                 // site, zoom, pan, product, palette, render/view mode
    layer_state: LayerState,             // geographic layer visibility
    live_mode_state: LiveModeState,      // streaming state machine
    acquisition_state: AcquisitionState, // download queue, operation tracking
    session_stats: SessionStats,         // download/ingest/render metrics
    download_progress: DownloadProgress, // active download tracking
    storage_settings: StorageSettings,   // quota and eviction targets
    render_processing: RenderProcessing, // interpolation, smoothing options
    theme_mode: ThemeMode,               // dark/light/system
    // ... UI flags, coordination flags, tool state
}
```

### Command Pattern
State mutations from UI actions are expressed as `AppCommand` variants, processed
in the main update loop. This keeps the UI code declarative (emit commands) and
the mutation logic centralized.

### Coordination Flags
State changes are coordinated via boolean flags checked each frame:
- `timeline_needs_refresh`
- `clear_cache_requested`
- `download_selection_requested`
- `start_live_requested`
- `check_eviction_requested`

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

### Canvas Overlays (drawn in order)
1. Geographic layers (states, counties, highways, lakes, cities)
2. Radar texture (GPU-rendered polar data)
3. Range rings and radial lines
4. Sweep animation line and donut chart
5. NEXRAD site markers
6. Info overlay (top-left: site, time, elevation, age)
7. Color scale legend (right edge)
8. Inspector tooltip and crosshair (on hover)
9. Distance measurement line (when tool active)
10. Storm cell bounding boxes (when detected)
11. Compass rose (3D globe mode only)

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

# Check only (no bundle)
cargo check
```

Pre-commit hooks enforce `cargo fmt` and `cargo clippy -D warnings` via cargo-husky.
