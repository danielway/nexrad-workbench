# NEXRAD Workbench

A browser-based workbench for visualizing and analyzing NEXRAD (WSR-88D) weather radar data. Built with Rust, compiled to WebAssembly, and runs entirely client-side with no backend services.

**Live site:** https://danielway.github.io/nexrad-workbench/

## Features

- **Archive browsing** — Browse and download historical radar data from AWS S3
- **Real-time streaming** — Stream live radar data as the radar produces it
- **Local file upload** — Open local Archive II files for offline analysis
- **Multiple radar products** — Reflectivity, Velocity, Spectrum Width, Differential Reflectivity, Correlation Coefficient, Differential Phase, Clutter Filter Power
- **Interactive timeline** — Zoomable timeline with data availability visualization, playback controls, and variable-speed animation
- **Geographic overlays** — State boundaries, county boundaries, and city labels
- **3D visualization** — Globe view with volumetric ray-marching renderer
- **Storm cell detection** — Automated detection with configurable thresholds
- **Measurement tools** — Inspector (lat/lon + data values) and distance measurement
- **Persistent caching** — IndexedDB-backed cache with configurable storage limits
- **Keyboard shortcuts** — Full shortcut set for power users (press `?` to view)
- **Dark and light themes** — Follows OS preference with manual override

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

The project automatically deploys to GitHub Pages on push to `main`. The CI pipeline:

1. Checks formatting (`cargo fmt`)
2. Lints with Clippy (`clippy -D warnings`)
3. Builds the WASM bundle with Trunk
4. Deploys to GitHub Pages

## Technology

| Category | Stack |
|----------|-------|
| Language | Rust 2021, compiled to WebAssembly |
| UI framework | eframe/egui 0.33 |
| Graphics | WebGL2 via glow 0.16 |
| NEXRAD data | `nexrad`, `nexrad-data`, `nexrad-decode`, `nexrad-model`, `nexrad-process`, `nexrad-render` crates |
| Browser APIs | wasm-bindgen, web-sys, js-sys |
| Build tool | Trunk |
| CI/CD | GitHub Actions |

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for technical details. See the [docs/](docs/) directory for product specifications.
