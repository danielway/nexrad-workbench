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
| `state` | Centralized state tree (`AppState`) with sub-states for playback, visualization, layers, live mode, preferences, session stats |
| `nexrad` | Data acquisition (download, realtime streaming), Web Worker decoding, GPU rendering, IndexedDB caching |
| `ui` | Panel layout, timeline, canvas, playback controls, modals, keyboard shortcuts |
| `geo` | Map projection, camera system, geographic feature rendering (states, counties, cities), globe rendering |
| `data` | NEXRAD site definitions, storage key types, IndexedDB abstraction, record storage facade |

### Source Files

#### `state/`
| File | Purpose |
|------|---------|
| `mod.rs` | Root `AppState` definition, re-exports |
| `playback.rs` | Playback controls — timestamp, speed, loop mode, selection range |
| `radar_data.rs` | `RadarTimeline` — scan/sweep/radial timeline representation |
| `viz.rs` | Visualization state — product, palette, zoom, pan, render mode, view mode |
| `live_mode.rs` | Live streaming state machine and phase transitions |
| `layer.rs` | Geographic layer visibility toggles |
| `preferences.rs` | User preferences persistence (localStorage) |
| `settings.rs` | Storage quotas and eviction targets |
| `stats.rs` | Session metrics — download, ingest, and render timing |
| `theme.rs` | Dark/light theme mode |
| `url_state.rs` | URL parameter parsing for deep linking |
| `vcp.rs` | Volume Coverage Pattern definitions |

#### `nexrad/`
| File | Purpose |
|------|---------|
| `gpu_renderer.rs` | WebGL2 radar rendering with OKLab color space interpolation |
| `worker_api.rs` | Web Worker communication protocol (main thread side) |
| `decode_worker.rs` | Worker-side NEXRAD decoding and sweep rendering |
| `download.rs` | AWS S3 download pipeline with async channels |
| `archive_index.rs` | Archive file listing and caching |
| `realtime.rs` | Real-time chunk streaming pipeline |
| `record_decode.rs` | Archive2 record parsing |
| `types.rs` | `CachedScan`, `ScanMetadata` types |
| `cache_channel.rs` | IndexedDB metadata loading channel |
| `volume_ray_renderer.rs` | 3D volumetric ray-marching renderer |
| `globe_radar_renderer.rs` | Radar data projection onto 3D globe |

#### `ui/`
| File | Purpose |
|------|---------|
| `timeline.rs` | Zoomable timeline with data availability, ghost markers, scrubbing |
| `canvas.rs` | Central radar visualization canvas with geographic overlays |
| `playback_controls.rs` | Play/pause, speed, loop mode, step controls |
| `left_panel.rs` | Radar operations panel (VCP, elevation, scan info) |
| `right_panel.rs` | Product, palette, layers, processing, 3D options |
| `top_bar.rs` | Site context, status messages, mode selector |
| `bottom_panel.rs` | Playback and stats bottom dock |
| `colors.rs` | Color definitions for products and UI elements |
| `shortcuts.rs` | Keyboard shortcut handling and help overlay |
| `site_modal.rs` | Site selection modal |
| `stats_modal.rs` | Session statistics detail modal |
| `wipe_modal.rs` | Cache wipe confirmation modal |

#### `geo/`
| File | Purpose |
|------|---------|
| `camera.rs` | Map projection camera system (2D and globe modes) |
| `layer.rs` | Geographic feature types (states, counties, cities) |
| `renderer.rs` | Geographic feature rendering on 2D canvas |
| `projection.rs` | Map projection transformations |
| `globe_renderer.rs` | 3D globe rendering |
| `geo_line_renderer.rs` | Geographic line rendering primitives |
| `cities.rs` | Built-in US cities data (~300 cities) |

#### `data/`
| File | Purpose |
|------|---------|
| `sites.rs` | All NEXRAD site definitions (156+ sites) |
| `keys.rs` | Storage key types (`ScanKey`, `RecordKey`) |
| `indexeddb.rs` | IndexedDB browser storage abstraction |
| `facade.rs` | Record storage facade |

## Data Flow

### Archive Download
```
User selects site/date
  → DownloadChannel fetches AWS S3 listing
  → ArchiveIndex caches listing
  → User selects scan (or range)
  → Check IndexedDB cache
    → Hit: Return cached records
    → Miss: Download from S3, cache, return
  → Send to Web Worker for decoding
  → Decoded Volume inserted into VolumeRing
  → Build RenderSweep → GPU render to texture
```

### Real-time Streaming
```
Start live mode
  → RealtimeChannel spawns ChunkIterator
  → Chunks accumulate, cached to IndexedDB
  → Each chunk sent to Web Worker for decoding
  → Decoded Volume → VolumeRing → Render
  → Timeline updated, UI refreshed
```

### Playback/Scrubbing
```
Timeline position changes
  → RadarTimeline.find_recent_scan()
  → ScrubLoadChannel loads from IndexedDB
  → Send to Web Worker for decoding
  → Decoded Volume → VolumeRing → Invalidate texture → Re-render
```

## Key Types

### Data Types
| Type | Description |
|------|-------------|
| `ScanKey` | Unique identifier: `SITE\|SCAN_START_MS` |
| `RecordKey` | Record identifier: `SITE\|SCAN_START_MS\|RECORD_ID` |
| `CachedScan` | Full scan data with metadata, stored in IndexedDB |
| `ScanMetadata` | Lightweight (~100 bytes) for fast timeline queries |
| `Volume` | Decoded radar volume from `nexrad` crate |

### Rendering Types
| Type | Description |
|------|-------------|
| `VolumeRing` | Circular buffer of 2-3 decoded volumes |
| `RenderSweep` | Dynamic sweep built from best radials across volumes |
| `GpuRadarRenderer` | WebGL2 renderer with OKLab color interpolation |
| `VolumeRayRenderer` | 3D volumetric ray-marching renderer |
| `GlobeRadarRenderer` | Radar projection onto 3D globe surface |

## Async Architecture

The application bridges async operations with egui's synchronous update loop using channel-based communication.

### Channel Pattern
```rust
// Spawn async task
channel.start_operation(ctx.clone(), params);

// Poll each frame in update()
if let Some(result) = channel.try_recv() {
    handle_result(result);
}
```

### Channels
| Channel | Purpose |
|---------|---------|
| `DownloadChannel` | AWS S3 file downloads with progress tracking |
| `CacheLoadChannel` | IndexedDB metadata loading at startup |
| `ScrubLoadChannel` | On-demand scan loading for timeline scrubbing |
| `RealtimeChannel` | Live chunk streaming from AWS |

### Web Worker

Heavy computation (bzip2 decompression, NEXRAD decoding, sweep rendering) runs in a dedicated Web Worker (`worker.js`) to keep the UI thread responsive. Communication uses `postMessage` with a typed protocol:

| Operation | Direction | Purpose |
|-----------|-----------|---------|
| `init` | Main → Worker | Initialize the worker with WASM module |
| `ingest` | Main → Worker | Decode raw records into a Volume |
| `render` | Main → Worker | Render a sweep to pixel buffer |

### Platform-Specific Spawning
- **WASM**: `wasm_bindgen_futures::spawn_local()`
- **Native**: `std::thread::spawn()` + `pollster::block_on()` (development only)

## Caching Strategy

### Record-Based Storage

Individual bzip2-compressed records are stored rather than full scans, enabling partial storage, time-based queries, and deduplication.

#### IndexedDB Schema

```
nexrad-workbench
├── records           - Raw bzip2-compressed record blobs
│   Key: "SITE|SCAN_START_MS|RECORD_ID"
│   Value: ArrayBuffer (raw bytes)
│
├── record_index      - Per-record metadata
│   Key: "SITE|SCAN_START_MS|RECORD_ID"
│   Value: { key, record_time, size_bytes, has_vcp, stored_at }
│
└── scan_index        - Per-scan metadata
    Key: "SITE|SCAN_START_MS"
    Value: { scan, has_vcp, expected_records, present_records, ... }
```

#### Scan Completeness States

| State | Description |
|-------|-------------|
| `Missing` | No records present |
| `PartialNoVcp` | Some records, no VCP metadata |
| `PartialWithVcp` | Some records with VCP (can determine expected count) |
| `Complete` | All expected records present |

### Three-Layer Cache

1. **IndexedDB** (persistent, WASM only)
   - Record-based storage with configurable quota
   - LRU eviction when storage limits are reached
   - Survives page reload

2. **VolumeRing** (memory)
   - 2-3 most recent decoded volumes
   - FIFO eviction
   - Cleared on site change

3. **GPU textures** (video memory)
   - egui TextureHandle storage
   - Content-signature-based invalidation

## State Management

Single `AppState` struct passed by mutable reference through the application.

```rust
AppState {
    playback_state: PlaybackState,       // timestamp, speed, selection, loop mode
    radar_timeline: RadarTimeline,       // scans with sweeps and radials
    viz_state: VizState,                 // site, zoom, pan, product, palette, render/view mode
    layer_state: LayerState,             // geographic layer visibility
    live_mode_state: LiveModeState,      // streaming state machine
    session_stats: SessionStats,         // download/ingest/render metrics
    download_progress: DownloadProgress, // active download tracking
    storage_settings: StorageSettings,   // quota and eviction targets
    render_processing: RenderProcessing, // interpolation, smoothing options
    theme_mode: ThemeMode,               // dark/light/system
    // ... UI flags, coordination flags, tool state
}
```

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
│ Panel    │ (Radar + Geographic layers) │ • Product       │
│ (Radar   │                             │ • Palette       │
│  Ops)    │                             │ • Layers        │
│          │                             │ • Processing    │
│          │                             │ • 3D Options    │
├──────────┴─────────────────────────────┴─────────────────┤
│ Bottom: Timeline | Playback Controls | Stats             │
└──────────────────────────────────────────────────────────┘
```

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
