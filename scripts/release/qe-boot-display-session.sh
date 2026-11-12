#!/bin/sh
# 在已有图形会话内启动 boot-display（root systemd 直接 spawn 会没有 DISPLAY/WAYLAND_DISPLAY）。
# 由安装器放到 /usr/local/bin/qe-boot-display-session，并让 agent 的
# QE_BOOT_DISPLAY_BIN 指向它。
set -eu
BIN="/usr/local/bin/boot-display"
[ -x "$BIN" ] || exit 0

# 找第一个有 Wayland socket 的普通用户（UID >= 1000）
RUNTIME=""
for dir in /run/user/*; do
  [ -d "$dir" ] || continue
  uid="$(basename "$dir")"
  [ "$uid" -ge 1000 ] 2>/dev/null || continue
  socket="$(ls "$dir"/wayland-* 2>/dev/null | grep -v '\.lock$' | head -1 || true)"
  [ -n "$socket" ] || continue
  RUNTIME="$dir"
  WAYLAND="$(basename "$socket")"
  USER_NAME="$(stat -c %U "$dir" 2>/dev/null || true)"
  [ -n "$USER_NAME" ] || continue
  UID_NUM="$(stat -c %u "$dir")"
  GID_NUM="$(stat -c %g "$dir")"
  HOME_DIR="$(getent passwd "$USER_NAME" | cut -d: -f6)"
  # setpriv 直接 exec 成目标用户，PID 就是 boot-display 本身；
  # runuser 会多 fork 一层，agent kill 掉 runuser 后 boot-display 会变孤儿。
  if command -v setpriv >/dev/null 2>&1; then
    exec setpriv --reuid="$UID_NUM" --regid="$GID_NUM" --init-groups env \
      HOME="$HOME_DIR" XDG_RUNTIME_DIR="$RUNTIME" \
      WAYLAND_DISPLAY="$WAYLAND" GDK_BACKEND=wayland XDG_SESSION_TYPE=wayland \
      DBUS_SESSION_BUS_ADDRESS="unix:path=$RUNTIME/bus" "$BIN" "$@"
  fi
  exec /usr/sbin/runuser -u "$USER_NAME" -- env \
    HOME="$HOME_DIR" XDG_RUNTIME_DIR="$RUNTIME" \
    WAYLAND_DISPLAY="$WAYLAND" GDK_BACKEND=wayland XDG_SESSION_TYPE=wayland \
    DBUS_SESSION_BUS_ADDRESS="unix:path=$RUNTIME/bus" "$BIN" "$@"
done

# 没有 Wayland 时尝试 X11（需要可用的 XAUTHORITY；常见桌面会自动设置）
for disp in /tmp/.X11-unix/X*; do
  [ -S "$disp" ] || continue
  num="${disp##*X}"
  user="$(stat -c %U /tmp/.X11-unix/X"$num" 2>/dev/null || true)"
  [ -n "$user" ] || continue
  home="$(getent passwd "$user" | cut -d: -f6)"
  [ -f "$home/.Xauthority" ] || continue
  exec /usr/sbin/runuser -u "$user" -- env \
    DISPLAY=":$num" XAUTHORITY="$home/.Xauthority" GDK_BACKEND=x11 "$BIN" "$@"
done

# 没有任何图形会话：安静退出，不 crash-loop；安装器通常不会在这种机器上启用
# QE_BOOT_DISPLAY_BIN 指向本脚本，这里只作为兜底。
exit 0
