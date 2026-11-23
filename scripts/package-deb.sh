#!/usr/bin/env bash
# Builds target/debian/orbit-tape_<version>-1_amd64.deb with cargo-deb.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo-deb >/dev/null 2>&1; then
    cargo install --locked cargo-deb
fi

cargo build --release --locked -p orbit-tape -p orbit-tape-indicator-engine --bins
cargo deb --no-build --locked "$@"
