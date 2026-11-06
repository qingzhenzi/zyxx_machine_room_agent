#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINARY="$ROOT/src-tauri/target/debug/boot-display"
LOG="$(mktemp -t qe-boot-display-smoke.XXXXXX.log)"

if [[ ! -x "$BINARY" ]]; then
  echo "Missing $BINARY; run: cargo build --manifest-path src-tauri/Cargo.toml" >&2
  exit 1
fi

RUNNER=()
if [[ -z "${DISPLAY:-}" ]]; then
  command -v xvfb-run >/dev/null || { echo "No DISPLAY and no xvfb-run" >&2; exit 1; }
  RUNNER=(xvfb-run -a)
fi

cleanup() {
  kill "$PID" 2>/dev/null || true
  wait "$PID" 2>/dev/null || true
  rm -f "$LOG"
}
trap cleanup EXIT

"${RUNNER[@]}" env WEBKIT_DISABLE_COMPOSITING_MODE=1 GDK_BACKEND=x11 \
  "$BINARY" >"$LOG" 2>&1 &
PID=$!

sleep 4

if ! kill -0 "$PID" 2>/dev/null; then
  echo "boot-display exited early:" >&2
  cat "$LOG" >&2
  exit 1
fi

echo "boot-display smoke ok (pid $PID, still running after 4s)"
