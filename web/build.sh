#!/usr/bin/env bash
# Build the browser receiver: compiles crates/courierqr-wasm to WebAssembly
# and drops it next to the web app as web/courierqr.wasm.
#
#   ./web/build.sh            # needs: rustup target add wasm32-unknown-unknown
#   ./web/build.sh --serve    # ...then serve web/ on http://localhost:8080
#
# No rustup target available (offline/sandboxed toolchain) but the std
# sources are? Set CQ_BUILD_STD=1 to compile std for wasm yourself.
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET=wasm32-unknown-unknown
if [[ "${CQ_BUILD_STD:-0}" == "1" ]]; then
  RUSTC_BOOTSTRAP=1 cargo build --release -p courierqr-wasm --target "$TARGET" -Zbuild-std=std,panic_abort
else
  cargo build --release -p courierqr-wasm --target "$TARGET"
fi

cp "target/$TARGET/release/courierqr_wasm.wasm" web/courierqr.wasm
echo "web/courierqr.wasm: $(wc -c < web/courierqr.wasm) bytes, sha256 $(sha256sum web/courierqr.wasm | cut -c1-16)…"

if [[ "${1:-}" == "--serve" ]]; then
  echo "serving web/ at http://localhost:8080 (camera works on localhost; phones need HTTPS)"
  cd web && exec python3 -m http.server 8080 --bind 127.0.0.1
fi
