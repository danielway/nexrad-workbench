#!/bin/bash
# Cloud-session setup: wasm32 target, trunk, pinned wasm-bindgen-cli, warm build.
# Local dev uses scripts/bootstrap.sh instead.
set -euo pipefail

if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

cd "${CLAUDE_PROJECT_DIR:-$(dirname "$0")/../..}"

TRUNK_VERSION="0.21.14"
# Must match Cargo.lock (see scripts/bootstrap.sh).
WASM_BINDGEN_VERSION="$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p')"
BIN="$HOME/.local/bin"
mkdir -p "$BIN"
export PATH="$BIN:$PATH"

rustup target add wasm32-unknown-unknown >/dev/null
rustup component add clippy rustfmt >/dev/null

if ! trunk --version 2>/dev/null | grep -q "$TRUNK_VERSION"; then
  curl -sSfL "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-x86_64-unknown-linux-gnu.tar.gz" \
    | tar xz -C "$BIN"
fi

# Prebuilt release instead of `cargo install` (minutes faster).
if ! wasm-bindgen-test-runner --version 2>/dev/null | grep -q "$WASM_BINDGEN_VERSION"; then
  name="wasm-bindgen-${WASM_BINDGEN_VERSION}-x86_64-unknown-linux-musl"
  curl -sSfL "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/${WASM_BINDGEN_VERSION}/${name}.tar.gz" \
    | tar xz -C "$BIN" --strip-components=1 \
      "${name}/wasm-bindgen" "${name}/wasm-bindgen-test-runner"
fi

echo "export PATH=\"$BIN:\$PATH\"" >> "${CLAUDE_ENV_FILE:-/dev/null}"

# Warm the target/ cache so `trunk serve`, clippy and tests start incremental.
# Best-effort: a new stable lint or a broken build must not block session start.
trunk build || true
cargo clippy --all-targets || true
cargo test --bin nexrad-workbench --no-run || true
