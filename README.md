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
