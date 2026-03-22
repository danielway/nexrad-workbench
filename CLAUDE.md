# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

NEXRAD Workbench is a browser-based NEXRAD (WSR-88D) weather radar visualization tool. Rust compiled to WebAssembly, runs entirely client-side with no backend. Uses eframe/egui for UI, WebGL2 via glow for GPU rendering.

## Build Commands

```bash
# Type-check (fastest feedback loop — no bundling)
cargo check

# Lint (CI enforces zero warnings)
cargo clippy -- -D warnings

# Format check
cargo fmt -- --check

# Dev server with hot reload (requires: cargo install --locked trunk)
trunk serve

# Production build → dist/
trunk build --release
```

There are no Rust unit tests to run for the main crate (tests exist only in `data/keys.rs` but require wasm-bindgen-test infrastructure). Pre-commit hooks via cargo-husky enforce `cargo fmt` and `cargo clippy`.

## Key Constraints

- **WASM-only target**: The default build target is `wasm32-unknown-unknown` (set in `.cargo/config.toml`). All code must compile for this target. Native stubs exist but are minimal.
- **Stable toolchain only**: No nightly, no `build-std`, no atomics. See `rust-toolchain.toml`.
- **No `await` inside IndexedDB readwrite transactions**: In WASM, IDB transactions auto-commit when the event loop yields. Read first in a separate readonly transaction, then write synchronously in readwrite before calling `.await`. See `src/data/indexeddb.rs`.
- **`globalThis` not `window`**: IDB and other browser APIs accessed via `js_sys::global()` / `js_sys::Reflect::get("indexedDB")` so the same code works in both main thread and Web Worker contexts.
- **Raw gate values on GPU**: NEXRAD gate values are raw u8/u16. Physical conversion (`physical = (raw - offset) / scale`) happens in the fragment shader. Values 0 (below threshold) and 1 (range folded) are sentinels checked via `v > 1.5`. This means bilinear interpolation works on raw values before conversion.

## Architecture

### Fat Worker Pattern

The main thread is a thin UI shell. All expensive operations run in a dedicated Web Worker (`worker.js`):
- bzip2 decompression, NEXRAD record decode, sweep extraction
- IndexedDB read/write
- Pre-computed sweep blob generation

Communication uses `postMessage` with Transferable ArrayBuffers (zero-copy). The main thread only uploads results to GPU textures and paints the UI.

### Data Pipeline

1. **Acquire**: AWS S3 download or real-time chunk streaming → raw bytes
2. **Ingest** (worker): Split records → decompress → decode → extract sweep blobs → store in IndexedDB
3. **Render** (worker): Read single pre-computed sweep blob from IDB → marshal for transfer
4. **Display** (main): Upload raw f32 data to GPU R32F texture → fragment shader does polar→Cartesian + color lookup

Sweep blobs are pre-computed during ingestion so that scrubbing and elevation changes have near-zero render latency (no decompression or decoding at render time).

### Module Layout

- `src/main.rs` — App entry, update loop, coordination manager orchestration
- `src/state/` — Centralized `AppState` with sub-states. UI actions emit `AppCommand` variants processed in the main loop.
- `src/nexrad/` — Data pipeline: acquisition (`download.rs`, `realtime.rs`), worker communication (`decode_worker/`, `worker_api/`), GPU rendering (`gpu_renderer/`), coordination managers (`acquisition_coordinator.rs`, `render_coordinator.rs`, `streaming_manager.rs`, `persistence_manager.rs`)
- `src/ui/` — egui panels, timeline (`timeline/`), canvas with overlays (`canvas_overlays/`), modals, shortcuts
- `src/geo/` — Map projection, camera (2D/globe), geographic feature rendering
- `src/data/` — Site definitions, storage key types (`ScanKey`, `SweepDataKey`), IndexedDB abstraction
- `worker.js` — ES module Web Worker dispatching postMessage to WASM exports
- `service-worker.js` — Cross-origin isolation headers (COOP/COEP) and network metrics

### Async Pattern

egui's update loop is synchronous. Async operations use channels polled each frame:

```rust
channel.start_operation(ctx.clone(), params);  // spawn async task
if let Some(result) = channel.try_recv() { … } // poll in update()
```

WASM async spawning: `wasm_bindgen_futures::spawn_local()`.

### Coordination Managers

Recent refactoring consolidated scattered fields into focused owners:
- `AcquisitionCoordinator` — download channel, archive index, cache loader, download queue
- `RenderCoordinator` — decode worker, render request deduplication via `last_render_params`
- `StreamingManager` — realtime + backfill channel lifecycle
- `PersistenceManager` — URL state pushing (throttled), preference saving

### Worker Protocol

Six message types (see `worker.js` header for full protocol):
`init`, `ingest`, `ingest_chunk`, `render`, `render_volume`, `render_live`

### IndexedDB Schema

Two primary stores: `sweep_data` (pre-computed sweep blobs, keyed `SITE|MS|ELEV|PRODUCT`) and `scan_index` (per-scan metadata, keyed `SITE|MS`). Legacy `records` and `record_index` stores exist but the primary render path uses sweep blobs.

## Timestamps

- `UnixMillis`: milliseconds since epoch (IndexedDB keys, `ScanKey`)
- `playback_position`: seconds since epoch (f64)
- Canvas/timeline use seconds; storage keys use milliseconds
- Convert: `playback_ts_ms = playback_position * 1000.0`
