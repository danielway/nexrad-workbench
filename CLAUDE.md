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

# Pure-Rust unit tests (runs on wasm32 in node via wasm-bindgen-test).
# The full headless suite: core decision logic + data-layer types + the
# IDB layer's pure decision functions (key range bounds, eviction order,
# throttle math, quota math, time-window filter, ScanIndexEntry accessors).
# Requires: `cargo install wasm-bindgen-cli --locked` and node.js installed.
cargo test --bin nexrad-workbench

# IDB orchestration tests (real IndexedDB in headless Chromium).
# Covers cross-store atomicity, the upsert_scan touch/merge contract,
# eviction order against a real DB, and full-entry round-tripping.
# Requires chromium + chromedriver installed locally.
CHROMEDRIVER=/usr/bin/chromedriver cargo test --test idb

# Dev server with hot reload (requires: cargo install --locked trunk)
trunk serve

# Production build → dist/
trunk build --release
```

Pre-commit hooks via cargo-husky run `cargo fmt`, `cargo clippy`, and
`cargo test --bin nexrad-workbench` (the fast logic suite — no browser
needed). The browser-driven `tests/idb.rs` suite runs in CI only and
requires Chromium + chromedriver.

## Commits

Always commit at natural milestones — a self-contained fix, a completed feature, or any coherent unit of work — proactively and without being asked. Do not batch changes until the end of a session, and do not wait for the user to prompt a commit. After `cargo check` and `cargo clippy -- -D warnings` are clean, commit. Match the repo's commit-message style (imperative subject, no trailing period; see `git log --oneline`). Never add `Co-Authored-By` lines. Only create new commits — never amend, never `--no-verify`.

## Key Constraints

- **WASM-only target**: The default build target is `wasm32-unknown-unknown` (set in `.cargo/config.toml`). All code must compile for this target. Native stubs exist but are minimal.
- **Stable toolchain only**: No nightly, no `build-std`, no atomics. See `rust-toolchain.toml`.
- **No `await` inside IndexedDB readwrite transactions**: In WASM, IDB transactions auto-commit when the event loop yields. Read first in a separate readonly transaction, then write synchronously in readwrite before calling `.await`. See `src/data/indexeddb/`.
- **Module edges are build-enforced**: `tools/arch_check.rs` (run from `build.rs` on every `cargo check`) fails the build on any cross-module dependency edge that isn't in its ALLOWED table. Fix the direction, or add to ALLOWED only with a real architectural reason. Never add to GRANDFATHERED.
- **`globalThis` not `window`**: IDB and other browser APIs accessed via `js_sys::global()` / `js_sys::Reflect::get("indexedDB")` so the same code works in both main thread and Web Worker contexts.
- **Raw gate values on GPU**: NEXRAD gate values are raw u8/u16. Physical conversion (`physical = (raw - offset) / scale`) happens in the fragment shader. Values 0 (below threshold) and 1 (range folded) are sentinels checked via `v > 1.5`. This means bilinear interpolation works on raw values before conversion.

## Architecture Standard — Functional Core, Thin Shell (MANDATORY)

All code follows a strict **functional core / imperative shell** split. This is the
binding rule for new and changed code, and the target of an in-progress migration
of existing code. Full pattern, seams, test recipe, and migration roadmap:
[CORE_SHELL.md](docs/CORE_SHELL.md).

- **The headless core owns all state and business logic.** Decision logic is pure
  — `(state, intent) -> (next state, effects)` — performs no I/O, and is
  unit-tested with no egui, no browser, no async.
- **Effects live behind a boundary.** Every side effect (IndexedDB, HTTP, Web
  Worker dispatch, GPU upload, localStorage, geolocation, URL/history, timers) is a
  described value the core returns and the shell executes — mockable, never inlined
  into decision logic.
- **The UI shell only renders a view-model and emits intents.** No business logic,
  no state mutation, no I/O anywhere under `src/ui/**` or in egui/canvas code.
  Input becomes an intent (`core::Intent`); rendering reads a view-model snapshot.
- **The contract is: intents in, view-model out — nothing else crosses.** Every
  feature is validated headlessly by `core + intents → assert view-model/state`;
  the egui layer is a trivial 1:1 projection you can eyeball.

Rule of thumb: put the logic and its tests in the core; the UI change must be a
thin projection. **If you can't test it without the browser, it's in the wrong layer.**

## Architecture

The full engineering map — module/file responsibilities, data flow, key types,
async architecture, caching, and UI layout — lives in
[ARCHITECTURE.md](ARCHITECTURE.md). Deep per-subsystem references:
[RENDERING.md](docs/RENDERING.md) (GPU shader pipeline + 3D),
[STREAMING.md](docs/STREAMING.md) (real-time sequencing/timing),
[TIMING.md](docs/TIMING.md) (the three time categories),
[INDEXEDDB.md](docs/INDEXEDDB.md) (cache layer + schema). Product/UX intent is in
[PRODUCT.md](docs/PRODUCT.md). The binding architecture standard (functional
core / thin shell) and its migration roadmap are in
[CORE_SHELL.md](docs/CORE_SHELL.md).

The essentials to hold in mind:

- **Fat Worker** — the main thread is a thin UI shell; all heavy work (bzip2
  decompress, NEXRAD decode, sweep extraction, IndexedDB I/O, sweep-blob
  generation) runs in a pool of Web Workers (`worker.js`). Communication is
  `postMessage` with Transferable ArrayBuffers (zero-copy). The worker protocol
  has six message types: `init`, `ingest`, `ingest_chunk`, `render`,
  `render_volume`, `render_live`.
- **Pipeline** — Acquire (S3 download or live chunk stream) → Ingest in the worker
  (split → decompress → decode → extract sweep blobs → store in IDB) → Render in
  the worker (read one pre-computed sweep blob from IDB) → Display on the main
  thread (upload raw f32 to a GPU R32F texture; the fragment shader does
  polar→Cartesian + raw→physical + color). Sweep blobs are pre-computed at ingest
  so scrubbing and elevation changes have near-zero render latency.
- **State** — `WorkbenchApp` is a thin coordinator over bounded subsystems
  (Acquisition, Render, Timeline, Playback, Live, Chrome, Diagnostics,
  NetworkMonitor — see ARCHITECTURE.md). UI actions emit `Intent` variants
  (defined in `src/core/intent.rs`) processed in the main loop; decisions live
  in pure `src/core/` reducers, and `src/app/` shells execute the described
  effects.
- **Async** — egui's update loop is synchronous; async tasks communicate via typed
  `futures_channel::mpsc` channels polled each frame, spawned with
  `wasm_bindgen_futures::spawn_local()`.

## Timestamps

- `UnixMillis`: milliseconds since epoch (IndexedDB keys, `ScanKey`)
- `playback_position`: seconds since epoch (f64)
- Canvas/timeline use seconds; storage keys use milliseconds
- Convert: `playback_ts_ms = playback_position * 1000.0`
