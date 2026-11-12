#!/usr/bin/env bash
# Debian 侧控制「全屏投屏」服务。
# 用法：./scripts/debian-display.sh {start|stop|restart|status}
set -euo pipefail
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=$XDG_RUNTIME_DIR/bus}"
case "${1:-status}" in
  start)   systemctl --user start lan-video-display.service ;;
  stop)    systemctl --user stop lan-video-display.service ;;
  restart) systemctl --user restart lan-video-display.service ;;
  status)  systemctl --user --no-pager status lan-video-display.service ;;
  *) echo "usage: $0 {start|stop|restart|status}"; exit 2 ;;
esac
