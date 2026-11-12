#!/usr/bin/env bash
# 本机连接一台 Debian 被控端：创建 TCP 会话 + 打开 viewer。
#
# 用法：./scripts/connect-debian.sh <debian-host> [port] [capture]
#   capture: x11（默认，Plasma X11）或 kwin（Wayland，需目标 KWin 能出帧）
# 环境变量：
#   LAN_VIDEO_AGENT_TOKEN_FILE  默认为 ~/.config/lan_video/debian-agent-token
#   LAN_VIDEO_BIN               默认为仓库 target/release/lan_video
set -euo pipefail

HOST="${1:?用法: $0 <debian-host> [port] [capture]}"
PORT="${2:-6000}"
CAPTURE="${3:-x11}"
TRANSPORT="${LAN_VIDEO_TRANSPORT:-udp}"
PROFILE="${LAN_VIDEO_PROFILE:-quality}"
AUDIO_PORT="${LAN_VIDEO_AUDIO_PORT:-6002}"
JITTER_MS="${LAN_VIDEO_JITTER_MS:-15}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${LAN_VIDEO_BIN:-$ROOT/target/release/lan_video}"
TOKEN_FILE="${LAN_VIDEO_AGENT_TOKEN_FILE:-$HOME/.config/lan_video/debian-agent-token}"

[ -x "$BIN" ] || { echo "找不到 viewer 二进制：$BIN（先 cargo build --release --features viewer）" >&2; exit 1; }
[ -r "$TOKEN_FILE" ] || { echo "找不到 agent token：$TOKEN_FILE" >&2; exit 1; }
API_TOKEN="$(cat "$TOKEN_FILE")"

case "$PROFILE" in
  latency) WIDTH=1280; HEIGHT=720; FPS=60; KBPS=8000 ;;
  quality|*) WIDTH=1920; HEIGHT=1080; FPS=30; KBPS=20000 ;;
esac

if [ "$TRANSPORT" = "tcp" ]; then
  TARGET="0.0.0.0"
  TCP_JSON=true
  VIEW_TRANSPORT=(--tcp --host "$HOST")
else
  # 本机从哪个源地址去 Debian，就用它作为 Debian 发 UDP/RTP 的目标
  TARGET="$(ip route get "$HOST" 2>/dev/null | awk '{for(i=1;i<=NF;i++) if($i=="src"){print $(i+1); exit}}')"
  TARGET="${TARGET:-192.168.8.231}"
  TCP_JSON=false
  VIEW_TRANSPORT=()
fi
BODY=$(printf '{"target":"%s","port":%s,"capture":{"type":"%s"},"encoder":"x264","width":%s,"height":%s,"fps":%s,"bitrate_kbps":%s,"tcp":%s,"audio_port":%s,"audio_source":"system"}' "$TARGET" "$PORT" "$CAPTURE" "$WIDTH" "$HEIGHT" "$FPS" "$KBPS" "$TCP_JSON" "$AUDIO_PORT")
if ! RAW="$(curl -sS --max-time 15 -w '\n%{http_code}' -X POST "http://$HOST:38080/v1/sessions" \
  -H "Authorization: Bearer $API_TOKEN" -H 'Content-Type: application/json' -d "$BODY")"; then
  echo "agent API 不可达：http://$HOST:38080" >&2
  exit 1
fi
HTTP_CODE="${RAW##*$'\n'}"
RESP="${RAW%$'\n'*}"
if [ "$HTTP_CODE" != "200" ] && [ "$HTTP_CODE" != "201" ]; then
  echo "创建会话失败 HTTP $HTTP_CODE：$RESP" >&2
  exit 1
fi
# 用一次 python 解析，失败时只打印响应，不抛 traceback
PARSED="$(printf '%s' "$RESP" | python3 -c 'import sys,json
try:
 d=json.load(sys.stdin); s=d.get("session",{}); print(s.get("id","")); print(s.get("control_token","")); print(s.get("media_secret","") or "")
except Exception:
 pass' 2>/dev/null || true)"
SID="$(printf '%s\n' "$PARSED" | sed -n '1p')"
CT="$(printf '%s\n' "$PARSED" | sed -n '2p')"
MS="$(printf '%s\n' "$PARSED" | sed -n '3p')"
if [ -z "$SID" ] || [ -z "$CT" ]; then
  echo "创建会话失败：$RESP" >&2
  exit 1
fi

cleanup() {
  curl -sS --max-time 5 -X DELETE "http://$HOST:38080/v1/sessions/$SID" \
    -H "Authorization: Bearer $API_TOKEN" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

echo "session=$SID  capture=$CAPTURE  $TRANSPORT  ${WIDTH}x${HEIGHT}@${FPS} ${KBPS}kbps  target=$TARGET"
VIEW_SECRET=()
if [ -n "$MS" ]; then
  VIEW_SECRET=(--media-secret "$MS")
  echo "media=SRTP (encrypted)"
else
  echo "media=plaintext"
fi
"$BIN" view "${VIEW_TRANSPORT[@]}" "${VIEW_SECRET[@]}" --port "$PORT" --audio-port "$AUDIO_PORT" --jitter-ms "$JITTER_MS" \
  --agent "http://$HOST:38080" --session "$SID" --token "$CT"
