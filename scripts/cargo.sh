#!/usr/bin/env bash
# Run cargo safely on the shared desktop. Use this instead of plain `cargo`.
#  - One cargo at a time machine-wide (all worktrees share one lock).
#  - All worktrees share the main checkout's target dir, so a new worktree
#    does not rebuild GPUI from scratch.
#  - The build runs in a systemd scope with a hard memory cap, so running out
#    of memory kills cargo instead of freezing the desktop.
set -euo pipefail
cd "$(dirname "$0")/.."

lock_dir=/tmp/retro-paint-cargo-lock
mkdir -p "$lock_dir"
main_checkout=$(cd "$(git rev-parse --git-common-dir)/.." && pwd)
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$main_checkout/target}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
mem_max="${RP_CARGO_MEM_MAX:-14G}"

run=(cargo "$@")
if command -v systemd-run >/dev/null && systemd-run --user --scope -q true 2>/dev/null; then
  run=(systemd-run --user --scope -q -p MemoryHigh=12G -p "MemoryMax=$mem_max" -p MemorySwapMax=0 -- "${run[@]}")
else
  echo "scripts/cargo.sh: systemd-run unavailable; refusing to run cargo without a memory cap" >&2
  exit 1
fi

status=0
flock "$lock_dir/retro-paint-cargo.lock" "${run[@]}" || status=$?
if [ "$status" -eq 137 ] || [ "$status" -eq 9 ]; then
  echo "scripts/cargo.sh: cargo was killed (memory cap $mem_max hit?). Do not raise the cap or bypass this script; report it." >&2
fi
exit "$status"
