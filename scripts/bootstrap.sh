#!/usr/bin/env bash
# Local / devcontainer setup for nexrad-workbench.
# Keep the wasm-bindgen-cli pin in lockstep with .github/workflows/ci.yml.
set -euo pipefail

# Must match Cargo.lock's wasm-bindgen version and CI's install step.
WASM_BINDGEN_CLI_VERSION="${WASM_BINDGEN_CLI_VERSION:-0.2.127}"

echo "==> Rust toolchain (stable + wasm32 + clippy/rustfmt)"
rustup show active-toolchain >/dev/null 2>&1 || rustup toolchain install stable
rustup target add wasm32-unknown-unknown
rustup component add clippy rustfmt

echo "==> wasm-bindgen-cli ${WASM_BINDGEN_CLI_VERSION} (pinned; must match Cargo.lock)"
if ! command -v wasm-bindgen-test-runner >/dev/null 2>&1 \
  || ! wasm-bindgen-test-runner --version 2>/dev/null | grep -q "${WASM_BINDGEN_CLI_VERSION}"; then
  cargo install wasm-bindgen-cli --locked --version "${WASM_BINDGEN_CLI_VERSION}" --force
else
  echo "    already installed: $(wasm-bindgen-test-runner --version 2>/dev/null || true)"
fi

echo "==> cargo-audit (Dependabot-class Rust advisories)"
if ! command -v cargo-audit >/dev/null 2>&1; then
  cargo install cargo-audit --locked
else
  echo "    already installed: $(cargo audit --version 2>/dev/null || true)"
fi

if ! command -v node >/dev/null 2>&1; then
  echo "ERROR: node.js is required for cargo test --bin nexrad-workbench" >&2
  echo "       Install Node LTS (devcontainer feature, nvm, or apt)." >&2
  exit 1
fi
echo "==> node $(node --version)"

if command -v ssh-keyscan >/dev/null 2>&1; then
  mkdir -p "${HOME}/.ssh"
  touch "${HOME}/.ssh/known_hosts"
  if ! grep -q 'github.com' "${HOME}/.ssh/known_hosts" 2>/dev/null; then
    echo "==> adding github.com to known_hosts"
    ssh-keyscan -t ed25519,rsa github.com >> "${HOME}/.ssh/known_hosts" 2>/dev/null || true
  fi
fi

if command -v gh >/dev/null 2>&1; then
  gh auth setup-git 2>/dev/null || true
fi

echo "==> done. Try: cargo check && cargo test --bin nexrad-workbench"
