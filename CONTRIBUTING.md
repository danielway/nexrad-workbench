# Contributing

Thanks for taking a look. This page covers building and running the app, the
checks CI runs, and where to read more.

## Set up

You need stable Rust and Node.js. The Node.js requirement is for the test runner.

```bash
# wasm32 target, clippy/rustfmt, the pinned wasm-bindgen-cli, cargo-audit
bash scripts/bootstrap.sh

# the dev server and bundler
cargo install --locked trunk
```

`wasm-bindgen-cli` has to be exactly the version in `Cargo.lock`. The
bootstrap script and CI both pin it. A plain `cargo install` gets the latest
version and breaks the tests.

## Run it

```bash
trunk serve            # http://127.0.0.1:8080 with hot reload
trunk build --release  # production bundle in dist/
```

The default target is `wasm32-unknown-unknown` (set in `.cargo/config.toml`),
so `cargo check` and `cargo clippy` check the browser build.

## Checks

CI runs all of these, and a pre-commit hook runs the first three:

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test --bin nexrad-workbench    # headless logic tests, run in Node via wasm-bindgen-test
CHROMEDRIVER=/usr/bin/chromedriver cargo test --test idb   # IndexedDB tests in headless Chromium
cargo audit
```

`cargo check` also runs an architecture check (`tools/arch_check.rs`) that
fails the build when a module imports a layer it shouldn't.

## How the code is organized

The code is split into a functional core and a thin shell. Decisions live in
pure functions under `src/core/` and are unit-tested there. The egui UI only
renders a view-model and emits intents, and every side effect (network,
IndexedDB, workers, GPU) is a value the core returns and the shell carries out.
If you can't test a change without a browser, it's probably in the wrong
layer. [docs/CORE_SHELL.md](docs/CORE_SHELL.md) explains the rule and how to
test against it.

Start with [ARCHITECTURE.md](ARCHITECTURE.md) for the map of modules and the
data pipeline. [docs/README.md](docs/README.md) lists the deeper references.

## Deployment

Every push to `main` builds and deploys to GitHub Pages at
<https://danielway.github.io/nexrad-workbench/>. Each pull request gets a
preview at `pr-preview/pr-<number>/` on the same site.
