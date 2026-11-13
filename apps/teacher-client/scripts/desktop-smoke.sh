#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINARY="$ROOT/src-tauri/target/debug/qe-agent-client"
LOG="$(mktemp -t qe-agent-desktop-smoke.XXXXXX.log)"

if [[ ! -x "$BINARY" ]]; then
  echo "Missing $BINARY; run: pnpm tauri build --debug --no-bundle" >&2
  exit 1
fi

if [[ -z "${DISPLAY:-}" ]]; then
  echo "DISPLAY is unset; desktop smoke needs an X11 session (or xvfb-run)." >&2
  exit 1
fi

DISPLAY="$DISPLAY" WEBKIT_DISABLE_COMPOSITING_MODE=1 GDK_BACKEND=x11 \
  "$BINARY" >"$LOG" 2>&1 &
PID=$!

cleanup() {
  kill "$PID" 2>/dev/null || true
  wait "$PID" 2>/dev/null || true
  rm -f "$LOG"
}
trap cleanup EXIT

sleep 4

if ! kill -0 "$PID" 2>/dev/null; then
  echo "QEStudio Agent exited during startup:" >&2
  cat "$LOG" >&2
  exit 1
fi

# The agent must start hidden: only the tray icon is visible, no app window.
if command -v xdotool >/dev/null 2>&1; then
  visible=$( { DISPLAY="$DISPLAY" xdotool search --onlyvisible --name 'QEStudio Agent' 2>/dev/null || true; } | wc -l)
  if [[ "$visible" -ne 0 ]]; then
    echo "expected a hidden startup, but $visible window(s) are visible" >&2
    cat "$LOG" >&2
    exit 1
  fi
fi

if grep -qiE 'panicked|error while running tauri application' "$LOG"; then
  echo "QEStudio Agent logged a fatal error:" >&2
  cat "$LOG" >&2
  exit 1
fi

echo "Desktop smoke passed: QEStudio Agent started hidden (tray-only mode) and stayed alive."
