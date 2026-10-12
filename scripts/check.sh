#!/usr/bin/env bash
# The merge gate: format, lint, test. Must pass before any feature merges to main.
# Runs cargo through scripts/cargo.sh (memory cap, one build at a time, shared target dir).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
scripts/cargo.sh clippy --workspace --all-targets -- -D warnings
scripts/cargo.sh test --workspace
