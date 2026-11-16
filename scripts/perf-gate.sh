#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_NET_OFFLINE=true
cargo perf-gate "$@"
cargo perf-paint
