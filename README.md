# NEXRAD Workbench

A web-based workbench for visualizing NEXRAD weather radar data. Built with
Rust and compiled to WebAssembly for browser execution.

## Prerequisites

```bash
rustup target add wasm32-unknown-unknown
cargo install --locked trunk
```

## Local Development

Start the development server:

```bash
trunk serve
```

This opens a local server at `http://127.0.0.1:8080` with hot reloading.

## Build

Build for production:

```bash
trunk build --release
```

Output is written to the `dist/` directory.

## Deployment

The project automatically deploys to GitHub Pages on push to `main`. The workflow:

1. Checks formatting and lints with Clippy
2. Builds the WASM bundle with Trunk
3. Deploys to GitHub Pages

Visit the deployed site at: `https://danielway.github.io/nexrad-workbench/`

## UI Layout

The application shell provides a multi-panel layout for radar data visualization:

```
┌─────────────────────────────────────────────────────────────────┐
│ NEXRAD Workbench │ Status │          [Mode Selector]            │  Top Bar
├──────────────────┼────────────────────────────┼──────────────────┤
│                  │                            │                  │
│  Data Source     │                            │  Controls        │
│  Panel           │     Radar Canvas           │                  │
│                  │                            │  • Product       │
│  • Upload File   │     (Visualization)        │  • Palette       │
│  • Archive       │                            │  • Layers        │
│  • Realtime      │                            │  • Processing    │
│                  │                            │  • 3D/Volumetric │
│                  │                            │                  │
├──────────────────┴────────────────────────────┴──────────────────┤
│  [Play] ──●───────────── [0/100] │ Speed: 1x │ Mode: Radial     │  Bottom Panel
└─────────────────────────────────────────────────────────────────┘
```

### Panels

- **Top Bar**: App title, status message, and data source mode selector (Upload File, Archive Browser, Realtime Stream)
- **Left Panel**: Data source controls that change based on selected mode
- **Central Canvas**: Radar visualization area with overlay info (site, time, elevation)
- **Right Panel**: Collapsible sections for product, palette, layers, processing, and 3D options
- **Bottom Panel**: Playback controls including timeline, play/pause, speed, and mode selection

## State Architecture

The application uses a hierarchical state model organized into logical groupings:

```
AppState
├── data_source_mode    # Active mode (Upload/Archive/Realtime)
├── upload_state        # File selection state
├── archive_state       # AWS archive browser state
├── realtime_state      # Realtime stream connection state
├── playback_state      # Timeline and playback controls
├── viz_state           # Visualization (texture, zoom, pan, product, palette)
├── layer_state         # Layer visibility toggles
├── processing_state    # Processing options (smoothing, dealiasing)
└── status_message      # Current status text
```

### Module Organization

```
src/
├── main.rs             # Entry points and app orchestration
├── renderer.rs         # Placeholder image generation
├── state/
│   ├── mod.rs          # AppState root
│   ├── data_source.rs  # DataSourceMode, UploadState, ArchiveState, RealtimeState
│   ├── playback.rs     # PlaybackState, PlaybackSpeed, PlaybackMode
│   ├── viz.rs          # VizState, RadarProduct, ColorPalette
│   ├── layer.rs        # LayerState
│   └── processing.rs   # ProcessingState
└── ui/
    ├── mod.rs          # UI module exports
    ├── top_bar.rs      # Top bar rendering
    ├── left_panel.rs   # Data source panel (mode-dependent)
    ├── right_panel.rs  # Controls panel
    ├── bottom_panel.rs # Playback controls
    └── canvas.rs       # Radar visualization canvas
```

## Future Integration

This shell is designed to integrate with NEXRAD decoding and rendering:

### Decoding Integration
- `upload_state.file_data` will hold raw file bytes for decoding
- `archive_state` will connect to AWS S3 to list and fetch archive files
- `realtime_state` will manage WebSocket connections to streaming sources
- Decoded data will be stored in new state fields and fed to the renderer

### Rendering Integration
- `renderer.rs` currently generates placeholder images
- Future: Replace with actual radar data rendering using decoded radials
- `viz_state.texture` will be updated with rendered frames
- Zoom/pan state is already captured for interactive canvas control

### Playback Integration
- `playback_state` captures frame index and speed settings
- Future: Timer-based animation will update `current_frame`
- Frames will be rendered on demand or pre-cached

### Processing Integration
- `processing_state` captures user preferences
- Future: Apply smoothing/dealiasing algorithms during rendering
