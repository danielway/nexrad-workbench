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

### Record-Based Storage (v4 Schema)

The new storage architecture stores individual compressed **records** rather than
full scans, enabling efficient partial storage, time-based queries, and deduplication.

#### IndexedDB Schema (v4)

```
nexrad-workbench (v4)
├── records_v4       - Raw bzip2-compressed record blobs
│   Key: "SITE|SCAN_START_MS|RECORD_ID"
│   Value: ArrayBuffer (raw bytes)
│
├── record_index_v4  - Per-record metadata
│   Key: "SITE|SCAN_START_MS|RECORD_ID"
│   Value: { key, record_time, size_bytes, has_vcp, stored_at }
│
└── scan_index_v4    - Per-scan metadata
    Key: "SITE|SCAN_START_MS"
    Value: { scan, has_vcp, expected_records, present_records, ... }
```

#### Key Types

| Type | Format | Example |
|------|--------|---------|
| `ScanKey` | `SITE\|SCAN_START_MS` | `KDMX\|1700000000000` |
| `RecordKey` | `SITE\|SCAN_START_MS\|RECORD_ID` | `KDMX\|1700000000000\|12` |

#### Scan Completeness States

| State | Description |
|-------|-------------|
| `Missing` | No records present |
| `PartialNoVcp` | Some records, no VCP metadata |
| `PartialWithVcp` | Some records with VCP (can determine expected count) |
| `Complete` | All expected records present |

#### Record Splitting

Archive2 files contain concatenated bzip2 blocks. Each block is stored as a separate
record, identified by sequence number (0-based). Record 0 typically contains VCP/LDM
metadata needed to interpret the scan structure.

### Three-Layer Cache

1. **IndexedDB** (persistent, WASM only)
   - v4: Record-based storage (see above)
   - Legacy v3: `nexrad-scans`, `scan-metadata` (lazy migration)
   - `file-cache`: Uploaded files

2. **VolumeRing** (memory)
   - 2-3 most recent decoded volumes
   - FIFO eviction
   - Cleared on site change

3. **RadarTextureCache** (GPU)
   - egui TextureHandle storage
   - Content-signature-based invalidation

### Current Integration (Dual-Write)

Downloads and live streams write to both caches:
1. **v3 (legacy)**: Full scan blobs for immediate compatibility
2. **v4 (records)**: Split into bzip2 records via `split_archive2_into_records()`

Timeline/scrub still uses v3 cache (`CacheLoadChannel`, `ScrubLoadChannel`).

### Future Migration

Full migration to v4-only requires:
1. Update `CacheLoadChannel` to query `scan_index_v4`
2. Update `ScrubLoadChannel` to reassemble from `records_v4`
3. Remove v3 writes from download pipeline
4. Add lazy migration for existing v3 data

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
