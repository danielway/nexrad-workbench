# NEXRAD Workbench Architecture

A WebAssembly-based NEXRAD weather radar visualization application built with Rust and egui.

## Module Structure

```
src/
├── main.rs              # Application entry, event loop, channel orchestration
├── state/               # Application state management
├── nexrad/              # NEXRAD data pipeline (download, cache, render)
├── ui/                  # egui panels and rendering
├── geo/                 # Geographic projections and layer rendering
├── storage/             # IndexedDB abstraction
├── file_ops.rs          # Async file picker
└── data/                # Static data (site definitions)
```

### Module Responsibilities

| Module | Purpose |
|--------|---------|
| `state` | Centralized state tree (`AppState`) with sub-states for playback, visualization, layers, live mode |
| `nexrad` | Data acquisition, caching, volume management, texture rendering |
| `ui` | Panel layout, timeline, canvas, user interaction |
| `geo` | Map projection, geographic feature rendering (states, counties) |
| `storage` | `KeyValueStore` trait with IndexedDB implementation |

## Data Flow

### Archive Download
```
User selects site/date
  → DownloadChannel fetches AWS S3 listing
  → ArchiveIndex caches listing
  → User selects scan
  → Check NexradCache (IndexedDB)
    → Hit: Return CachedScan
    → Miss: Download from S3, cache, return
  → Decode via nexrad crate → Volume
  → Insert into VolumeRing
  → Build RenderSweep → Render to texture
```

### Real-time Streaming
```
Start live mode
  → RealtimeChannel spawns ChunkIterator
  → Chunks accumulate until volume complete
  → Decode → VolumeRing → Cache → Render
  → Timeline updated, UI refreshed
```

### Playback/Scrubbing
```
Timeline position changes
  → RadarTimeline.find_recent_scan()
  → ScrubLoadChannel loads from cache
  → Decode → VolumeRing → Invalidate texture
```

## Key Types

### Data Types
| Type | Description |
|------|-------------|
| `ScanKey` | Unique identifier: `{site_id}_{timestamp}` |
| `CachedScan` | Full scan data with metadata, stored in IndexedDB |
| `ScanMetadata` | Lightweight (~100 bytes) for fast timeline queries |
| `Volume` | Decoded radar volume from `nexrad` crate |

### Rendering Types
| Type | Description |
|------|-------------|
| `VolumeRing` | Circular buffer of 2-3 decoded volumes |
| `RenderSweep` | Dynamic sweep built from best radials across volumes |
| `RadarTextureCache` | egui texture storage with content-based invalidation |

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
| `DownloadChannel` | AWS S3 file downloads with progress |
| `CacheLoadChannel` | IndexedDB metadata loading |
| `ScrubLoadChannel` | On-demand scan loading for timeline |
| `RealtimeChannel` | Live streaming from AWS |
| `FilePickerChannel` | Async file dialog |

### Platform-Specific Spawning
- **WASM**: `wasm_bindgen_futures::spawn_local()`
- **Native**: `std::thread::spawn()` + `pollster::block_on()`

## Caching Strategy

### Three-Layer Cache

1. **IndexedDB** (persistent, WASM only)
   - `nexrad-scans`: Full scan data (1-5 MB each)
   - `scan-metadata`: Lightweight metadata for timeline
   - `file-cache`: Uploaded files

2. **VolumeRing** (memory)
   - 2-3 most recent decoded volumes
   - FIFO eviction
   - Cleared on site change

3. **RadarTextureCache** (GPU)
   - egui TextureHandle storage
   - Content-signature-based invalidation

### Cache Key Format
```
{site_id}_{unix_timestamp}
Example: KDMX_1704067200
```

## State Management

Single `AppState` struct passed by mutable reference through the application.

```rust
AppState {
    upload_state: UploadState,
    playback_state: PlaybackState,      // timestamp, speed, selection
    radar_timeline: RadarTimeline,      // scans with sweeps
    viz_state: VizState,                // site, zoom, pan, product
    layer_state: LayerState,            // geo layer visibility
    live_mode_state: LiveModeState,     // streaming state machine
    session_stats: SessionStats,        // metrics
    // ... coordination flags
}
```

### Coordination Flags
State changes are coordinated via boolean flags checked each frame:
- `timeline_needs_refresh`
- `clear_cache_requested`
- `download_selection_requested`
- `start_live_requested`

## UI Layout

```
┌──────────────────────────────────────────────────────────┐
│ Top Bar: Status messages                                 │
├──────────┬─────────────────────────────┬─────────────────┤
│ Left     │ Canvas                      │ Right Panel     │
│ Panel    │ (Radar + Geo)               │ • Product       │
│ (Data    │                             │ • Palette       │
│  Source) │                             │ • Layers        │
├──────────┴─────────────────────────────┴─────────────────┤
│ Bottom: Timeline | Playback Controls | Stats            │
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
- Async spawning mechanism
- Web-specific APIs (js-sys, web-sys)

## Dependencies

| Crate | Purpose |
|-------|---------|
| `eframe`/`egui` | UI framework |
| `nexrad` | NEXRAD decoding and rendering |
| `nexrad-data` | AWS S3 data access |
| `web-sys`/`js-sys` | Browser API bindings |
| `wasm-bindgen` | Rust-JS interop |

## Build

```bash
# Development (uses default wasm32 target)
cargo check

# Release build
cargo build --profile release-wasm
```
