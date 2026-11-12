#!/usr/bin/env bash
# QEStudio Agent Bundle 安装器（Debian 13 / Arch Linux，systemd）
#
# 用法：
#   sudo ./install.sh                 # 交互
#   sudo ./install.sh --yes [...]     # 非交互
#   ./install.sh --dry-run            # 只打印动作，不修改系统
#
# 安装内容：qe-agent + boot-display + agent-client（可选）+ systemd/env + 桌面入口。
set -euo pipefail

BUNDLE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="/usr/local/bin"
UNIT_DST="/etc/systemd/system/qe-agent.service"
ENV_DST="/etc/qe-agent.env"
DATA_DIR="/var/lib/qe-agent"
SHARE_DIR="/usr/share/qe-agent-client"
ICON_DIR="/usr/share/icons/hicolor/128x128/apps"
DESKTOP_DIR="/usr/share/applications"

BUNDLE_VERSION="$(cat "$BUNDLE_DIR/VERSION" 2>/dev/null || echo unknown)"
YES=0
DRY_RUN=0
UNINSTALL=0
INSTALL_GUI=1
INSTALL_DISPLAY=1
START=0
GUI_USER="${SUDO_USER:-}"
ALLOW_POWEROFF="1"
FAST_INTERVAL="10"
FULL_INTERVAL="300"
LIVE_PUSH="0"
LIVE_INTERVAL_MS="500"
ENROLL_LINK=""
KIOSK=1

usage() {
  cat <<USAGE
QEStudio Agent $BUNDLE_VERSION 安装器

  --yes                    非交互，使用默认/参数值
  --gui-user USER          安装桌面端并加入 qe-agent 组（默认 SUDO_USER）
  --no-gui                 不安装 agent-client
  --no-boot-display        不安装 boot-display
  --allow-poweroff         允许远程关机（默认）
  --no-allow-poweroff      禁止远程关机
  --fast-interval SECS     遥测 fast 间隔，默认 10
  --full-interval SECS     遥测 full 间隔，默认 300
  --live-push              开启毫秒实时中转（默认关闭）
  --no-live-push           只本地采样 + 快照批量提交（默认）
  --live-interval-ms MS    本地采样间隔，默认 500
  --enroll-link URL        安装后立即注册（一次性 enrollment link）
  --no-kiosk               不做 kiosk 化：不自动登录、不锁屏禁用、不禁止切 TTY
  --start                  安装后启动 qe-agent
  --dry-run                只打印
  --uninstall              卸载二进制/unit/env/桌面入口（保留数据与证书）
  -h, --help
USAGE
}

log() { printf '[qestudio-install] %s\n' "$*"; }
die() { printf '[qestudio-install] 错误：%s\n' "$*" >&2; exit 1; }
run() {
  if [[ "$DRY_RUN" -eq 1 ]]; then printf '[dry-run] %s\n' "$*"; else "$@"; fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --yes) YES=1; shift;;
    --gui-user) GUI_USER="${2:-}"; shift 2;;
    --no-gui) INSTALL_GUI=0; shift;;
    --no-boot-display) INSTALL_DISPLAY=0; shift;;
    --allow-poweroff) ALLOW_POWEROFF="1"; shift;;
    --no-allow-poweroff) ALLOW_POWEROFF="0"; shift;;
    --fast-interval) FAST_INTERVAL="${2:-}"; shift 2;;
    --full-interval) FULL_INTERVAL="${2:-}"; shift 2;;
    --live-push) LIVE_PUSH="1"; shift;;
    --no-live-push) LIVE_PUSH="0"; shift;;
    --live-interval-ms) LIVE_INTERVAL_MS="${2:-}"; shift 2;;
    --enroll-link) ENROLL_LINK="${2:-}"; shift 2;;
    --no-kiosk) KIOSK=0; shift;;
    --start) START=1; shift;;
    --dry-run) DRY_RUN=1; shift;;
    --uninstall) UNINSTALL=1; shift;;
    -h|--help) usage; exit 0;;
    *) echo "未知参数：$1" >&2; usage; exit 2;;
  esac
done

[[ "$DRY_RUN" -eq 1 || "${EUID}" -eq 0 ]] || die "需要 root：sudo ./install.sh"
command -v systemctl >/dev/null 2>&1 || die "仅支持 systemd Linux"
[[ -x "$BUNDLE_DIR/bin/qe-agent" ]] || die "bundle 缺少 bin/qe-agent"
[[ -f "$BUNDLE_DIR/share/qe-agent.service" ]] || die "bundle 缺少 systemd 模板"

# 交互（只有 TTY 且未 --yes 才询问）
if [[ "$YES" -eq 0 && "$DRY_RUN" -eq 0 && -t 0 ]]; then
  echo "== QEStudio Agent $BUNDLE_VERSION 安装 =="
  read -r -p "GUI 用户（留空跳过桌面端）[$GUI_USER]: " answer; GUI_USER="${answer:-$GUI_USER}"
  read -r -p "允许远程关机？[Y/n]: " answer
  [[ "$answer" == n || "$answer" == N ]] && ALLOW_POWEROFF=0
  read -r -p "遥测 fast 间隔（秒）[$FAST_INTERVAL]: " answer; FAST_INTERVAL="${answer:-$FAST_INTERVAL}"
  read -r -p "遥测 full 间隔（秒）[$FULL_INTERVAL]: " answer; FULL_INTERVAL="${answer:-$FULL_INTERVAL}"
  read -r -p "开启实时中转？[y/N]: " answer
  [[ "$answer" == y || "$answer" == Y ]] && LIVE_PUSH=1
  read -r -p "本地采样间隔（毫秒）[$LIVE_INTERVAL_MS]: " answer; LIVE_INTERVAL_MS="${answer:-$LIVE_INTERVAL_MS}"
  read -r -p "注册链接（可留空）: " answer; ENROLL_LINK="${answer:-$ENROLL_LINK}"
  read -r -p "安装 boot-display？[Y/n]: " answer
  [[ "$answer" == n || "$answer" == N ]] && INSTALL_DISPLAY=0
  read -r -p "安装 agent-client 桌面端？[Y/n]: " answer
  [[ "$answer" == n || "$answer" == N ]] && INSTALL_GUI=0
  read -r -p "安装后启动服务？[Y/n]: " answer
  [[ "$answer" != n && "$answer" != N ]] && START=1
fi

# 参数范围
[[ "$FAST_INTERVAL" =~ ^[0-9]+$ ]] && (( FAST_INTERVAL >= 1 && FAST_INTERVAL <= 60 )) || die "fast 间隔必须 1-60"
[[ "$FULL_INTERVAL" =~ ^[0-9]+$ ]] && (( FULL_INTERVAL >= 60 && FULL_INTERVAL <= 3600 )) || die "full 间隔必须 60-3600"
(( FULL_INTERVAL >= FAST_INTERVAL )) || die "full 不能小于 fast"
[[ "$LIVE_INTERVAL_MS" =~ ^[0-9]+$ ]] && (( LIVE_INTERVAL_MS >= 0 && LIVE_INTERVAL_MS <= 5000 )) || die "live 间隔必须 0-5000ms"

set_env() {
  local key="$1" value="$2"
  if [[ "$DRY_RUN" -eq 1 ]]; then
    printf '[dry-run] set %s=%s in %s\n' "$key" "$value" "$ENV_DST"
    return 0
  fi
  [[ -f "$ENV_DST" ]] || install -m 0600 "$BUNDLE_DIR/share/qe-agent.env.example" "$ENV_DST"
  local tmp
  tmp="$(mktemp)"
  awk -v key="$key" -v value="$value" '
    index($0, key "=") == 1 { print key "=" value; found=1; next }
    { print }
    END { if (!found) print key "=" value }
  ' "$ENV_DST" > "$tmp"
  cat "$tmp" > "$ENV_DST"
  rm -f "$tmp"
}

# 从 meta.json 取一个字符串字段（不依赖 python3，最小 Debian 也可能没有）。
json_field() {
  local field="$1" file="$2"
  sed -n "s/.*\"$field\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" "$file" | head -1
}

# 教室机器 kiosk 化：自动登录、禁锁屏、禁止切 TTY；boot-display 才能从服务加载就占屏。
configure_kiosk() {
  [[ "$KIOSK" -eq 1 ]] || { log "已禁用 kiosk 化（--no-kiosk）"; return 0; }
  [[ -n "$GUI_USER" ]] && id "$GUI_USER" >/dev/null 2>&1 || {
    log "未指定有效 GUI 用户，跳过 kiosk 自动登录/锁屏配置"
    return 0
  }

  # 1) 禁止 Ctrl+Alt+F2..F6 自动拉起 getty（保留 SDDM 的 tty1）
  local logind_dir="/etc/systemd/logind.conf.d"
  run install -d -m 0755 "$logind_dir"
  if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "[dry-run] write $logind_dir/90-qestudio-kiosk.conf (NAutoVTs=0)"
  else
    cat >"$logind_dir/90-qestudio-kiosk.conf" <<'EOF'
[Login]
NAutoVTs=0
EOF
    for n in 2 3 4 5 6; do
      systemctl mask "getty@tty${n}.service" >/dev/null 2>&1 || true
    done
    systemctl restart systemd-logind 2>/dev/null || true
  fi

  # 2) SDDM 自动登录到 GUI 用户（没有 SDDM 就跳过）
  if command -v sddm >/dev/null 2>&1; then
    local session="plasma.desktop"
    if [[ ! -f "/usr/share/wayland-sessions/${session}" && -f /usr/share/xsessions/plasma.desktop ]]; then
      session="plasma"
    fi
    run install -d -m 0755 /etc/sddm.conf.d
    if [[ "$DRY_RUN" -eq 1 ]]; then
      echo "[dry-run] write /etc/sddm.conf.d/90-qestudio-kiosk.conf (Autologin User=$GUI_USER)"
    else
      cat >/etc/sddm.conf.d/90-qestudio-kiosk.conf <<EOF
[Autologin]
User=${GUI_USER}
Session=${session}
Relogin=false
EOF
    fi
    log "已配置 SDDM 自动登录：${GUI_USER} / ${session}"
  fi

  # 3) 关闭 Plasma 锁屏（否则 boot-display 退出后会被锁屏盖住）
  local gui_home
  gui_home="$(getent passwd "$GUI_USER" | cut -d: -f6)"
  if [[ -n "$gui_home" && -d "$gui_home" ]]; then
    run install -d -m 0700 "$gui_home/.config"
    if [[ "$DRY_RUN" -eq 1 ]]; then
      echo "[dry-run] write $gui_home/.config/kscreenlockerrc"
    else
      cat >"$gui_home/.config/kscreenlockerrc" <<'EOF'
[Daemon]
Autolock=false
LockOnStart=false
EOF
      chown -R "$GUI_USER":"$(id -gn "$GUI_USER")" "$gui_home/.config/kscreenlockerrc"
    fi
    log "已关闭 ${GUI_USER} 的 Plasma 自动锁屏"
  fi
}

if [[ "$UNINSTALL" -eq 1 ]]; then
  log "卸载 QEStudio Agent（保留 $DATA_DIR 与设备证书）"
  run systemctl disable --now qe-agent 2>/dev/null || true
  for f in qe-agent boot-display qe-agent-client; do run rm -f "$BIN_DIR/$f"; done
  run rm -f "$UNIT_DST" /etc/systemd/system/qe-agent.service "$DESKTOP_DIR/qe-agent-client.desktop"
  run rm -rf "$SHARE_DIR"
  run systemctl daemon-reload
  log "已卸载（env=$ENV_DST 与 $DATA_DIR 保留，手动删除）"
  exit 0
fi

# 依赖提示（不自动联网安装；目标机需有 libvirt WebKitGTK 等）
check_binary_libs() {
  local file="$1" label="$2"
  [[ -x "$file" ]] || return 0
  local missing
  missing="$(ldd "$file" 2>/dev/null | grep 'not found' || true)"
  if [[ -n "$missing" ]]; then
    log "警告：$label 缺少共享库："
    printf '%s\n' "$missing"
    if [[ -f /etc/os-release ]]; then
      local distro_like
      distro_like="$(sed -n 's/^ID_LIKE=//p' /etc/os-release | tr -d '"' | head -1)"
      [[ -n "$distro_like" ]] || distro_like="$(sed -n 's/^ID=//p' /etc/os-release | tr -d '"' | head -1)"
      case "$distro_like" in
        *debian*|*ubuntu*)
          log "Debian/Ubuntu：sudo apt install libvirt0 libwebkit2gtk-4.1-0 libgtk-3-0 libayatana-appindicator3-1 librsvg2-2";;
        *arch*)
          log "Arch：sudo pacman -S libvirt webkit2gtk-4.1 gtk3 libayatana-appindicator librsvg";;
      esac
    fi
  fi
}
check_binary_libs "$BUNDLE_DIR/bin/qe-agent" "qe-agent"
[[ "$INSTALL_DISPLAY" -eq 1 ]] && check_binary_libs "$BUNDLE_DIR/bin/boot-display" "boot-display"
[[ "$INSTALL_GUI" -eq 1 ]] && check_binary_libs "$BUNDLE_DIR/bin/qe-agent-client" "agent-client"

# 组
if ! getent group qe-agent >/dev/null 2>&1; then
  run groupadd --system qe-agent
fi
run install -d -m 0755 "$BIN_DIR"
run install -d -m 0750 -g qe-agent "$DATA_DIR"

# 二进制
run install -m 0755 "$BUNDLE_DIR/bin/qe-agent" "$BIN_DIR/qe-agent"
if [[ "$INSTALL_DISPLAY" -eq 1 && -x "$BUNDLE_DIR/bin/boot-display" ]]; then
  run install -m 0755 "$BUNDLE_DIR/bin/boot-display" "$BIN_DIR/boot-display"
  if [[ -x "$BUNDLE_DIR/share/qe-boot-display-session.sh" ]]; then
    run install -m 0755 "$BUNDLE_DIR/share/qe-boot-display-session.sh" "$BIN_DIR/qe-boot-display-session"
  fi
fi

# systemd + env
run install -m 0644 "$BUNDLE_DIR/share/qe-agent.service" "$UNIT_DST"
[[ -f "$ENV_DST" ]] || run install -m 0600 "$BUNDLE_DIR/share/qe-agent.env.example" "$ENV_DST"
set_env QE_AGENT_ALLOW_POWEROFF "$ALLOW_POWEROFF"
set_env ROOM_AGENT_FAST_INTERVAL_SECS "$FAST_INTERVAL"
set_env ROOM_AGENT_FULL_INTERVAL_SECS "$FULL_INTERVAL"
set_env ROOM_AGENT_LIVE_PUSH "$LIVE_PUSH"
set_env ROOM_AGENT_LIVE_INTERVAL_MILLIS "$LIVE_INTERVAL_MS"
if [[ "$INSTALL_DISPLAY" -eq 1 && -x "$BUNDLE_DIR/bin/boot-display" ]]; then
  # 只在检测到图形会话时启用，避免 root systemd 下 GTK 无 DISPLAY 崩溃循环。
  if compgen -G '/run/user/*/wayland-*' >/dev/null 2>&1 || [[ -S /tmp/.X11-unix/X0 ]]; then
    set_env QE_BOOT_DISPLAY_BIN "$BIN_DIR/qe-boot-display-session"
    set_env QE_BOOT_DISPLAY_EXIT_AFTER_BOOT 1
  else
    log "未检测到图形会话，跳过 boot-display 自启"
  fi
fi

# 教室机器 kiosk：自动登录、禁锁屏、禁切 TTY
configure_kiosk

run systemctl daemon-reload

# GUI
if [[ "$INSTALL_GUI" -eq 1 ]]; then
  if [[ -x "$BUNDLE_DIR/bin/qe-agent-client" ]]; then
    run install -m 0755 "$BUNDLE_DIR/bin/qe-agent-client" "$BIN_DIR/qe-agent-client"
    run install -d -m 0755 "$SHARE_DIR" "$ICON_DIR" "$DESKTOP_DIR"
    run install -m 0755 "$BUNDLE_DIR/share/agent-setup.sh" "$SHARE_DIR/agent-setup.sh"
    run install -m 0644 "$BUNDLE_DIR/share/qe-agent.service" "$SHARE_DIR/qe-agent.service"
    run install -m 0644 "$BUNDLE_DIR/share/qe-agent.env.example" "$SHARE_DIR/qe-agent.env.example"
    run install -m 0644 "$BUNDLE_DIR/share/icons/qe-agent-client.png" "$ICON_DIR/qe-agent-client.png"
    sed "s|@EXEC@|$BIN_DIR/qe-agent-client|g; s|@ICON@|$ICON_DIR/qe-agent-client.png|g" \
      "$BUNDLE_DIR/share/qe-agent-client.desktop" > /tmp/qe-agent-client.desktop
    run install -m 0644 /tmp/qe-agent-client.desktop "$DESKTOP_DIR/qe-agent-client.desktop"
    rm -f /tmp/qe-agent-client.desktop
    command -v update-desktop-database >/dev/null 2>&1 && run update-desktop-database "$DESKTOP_DIR" || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && run gtk-update-icon-cache -q /usr/share/icons/hicolor || true
    if [[ -n "$GUI_USER" ]] && id "$GUI_USER" >/dev/null 2>&1; then
      if ! id -nG "$GUI_USER" | tr ' ' '\n' | grep -qx qe-agent; then
        run usermod -aG qe-agent "$GUI_USER"
        log "已把 $GUI_USER 加入 qe-agent 组（需注销/重新登录）"
      fi
      GUI_HOME="$(getent passwd "$GUI_USER" | cut -d: -f6)"
      if [[ -n "$GUI_HOME" && -d "$GUI_HOME" ]]; then
        AUTOSTART="$GUI_HOME/.config/autostart"
        run install -d -m 0755 -o "$GUI_USER" -g "$(id -gn "$GUI_USER")" "$AUTOSTART"
        printf '[Desktop Entry]\nType=Application\nName=QEStudio Agent\nExec=%s/qe-agent-client\nIcon=%s/qe-agent-client.png\nX-GNOME-Autostart-enabled=true\n' \
          "$BIN_DIR" "$ICON_DIR" > /tmp/qe-agent-client-autostart.desktop
        run install -m 0644 -o "$GUI_USER" -g "$(id -gn "$GUI_USER")" \
          /tmp/qe-agent-client-autostart.desktop "$AUTOSTART/qe-agent-client.desktop"
        rm -f /tmp/qe-agent-client-autostart.desktop
      fi
    fi
    log "已安装 agent-client 到 $BIN_DIR/qe-agent-client"
  else
    log "bundle 未包含 agent-client，跳过 GUI"
  fi
fi

# libvirt-guests：避免 host 关机时被无法 ACPI 响应的 VM 卡住（真机踩过）
if [[ -f /etc/default/libvirt-guests ]] && command -v virsh >/dev/null 2>&1; then
  if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "[dry-run] tune /etc/default/libvirt-guests: PARALLEL_SHUTDOWN=1 SHUTDOWN_TIMEOUT=30"
  else
    cp -a /etc/default/libvirt-guests /etc/default/libvirt-guests.qe-backup 2>/dev/null || true
    for kv in "ON_SHUTDOWN=shutdown" "PARALLEL_SHUTDOWN=1" "SHUTDOWN_TIMEOUT=30"; do
      k="${kv%%=*}"; v="${kv#*=}"
      if grep -q "^${k}=" /etc/default/libvirt-guests; then
        sed -i "s|^${k}=.*|${k}=${v}|" /etc/default/libvirt-guests
      else
        echo "${k}=${v}" >> /etc/default/libvirt-guests
      fi
    done
    systemctl daemon-reload 2>/dev/null || true
  fi
fi

# 注册（可选）
if [[ -n "$ENROLL_LINK" ]]; then
  run systemctl stop qe-agent 2>/dev/null || true
  run "$BIN_DIR/qe-agent" cli enroll "$ENROLL_LINK" --trust --data-dir "$DATA_DIR"
  if [[ "$DRY_RUN" -eq 0 ]]; then
    META="$(find "$DATA_DIR/creds" -name meta.json -printf '%T@ %p\n' 2>/dev/null | sort -nr | head -1 | cut -d' ' -f2-)"
    [[ -n "$META" ]] || die "注册成功但找不到 meta.json"
    SLOT="$(json_field device_slot "$META")"
    MTLS="$(json_field mtls_endpoint "$META")"
    [[ -n "$SLOT" && -n "$MTLS" ]] || die "解析 $META 的 device_slot/mtls_endpoint 失败"
    set_env DEVICE_SLOT "$SLOT"
    set_env DEVICE_PKI_MTLS_URL "$MTLS"
    set_env DEVICE_DATA_DIR "$DATA_DIR"
  fi
  START=1
fi

if [[ "$START" -eq 1 ]]; then
  if [[ "$DRY_RUN" -eq 0 ]] && ! find "$DATA_DIR/creds" -name cert.pem -print -quit 2>/dev/null | grep -q .; then
    log "未注册（无设备凭证），服务不启动；请用 agent-client 或 enroll 完成注册"
  else
    run systemctl enable --now qe-agent
  fi
fi

cat <<EOF2

安装完成：qe-agent $BUNDLE_VERSION
  daemon      : $BIN_DIR/qe-agent
  display     : $([[ -x "$BIN_DIR/boot-display" ]] && echo "$BIN_DIR/boot-display" || echo '未安装')
  agent-client: $([[ -x "$BIN_DIR/qe-agent-client" ]] && echo "$BIN_DIR/qe-agent-client" || echo '未安装')
  env         : $ENV_DST
  data        : $DATA_DIR

下一步：
  1) 桌面用户注销/重新登录（qe-agent 组权限）
  2) 启动 agent-client：应用菜单「QEStudio Agent」或 $BIN_DIR/qe-agent-client
     首次启动会执行初始化；粘贴管理端注册链接即可
  3) CLI：systemctl status qe-agent --no-pager
          journalctl -u qe-agent -n 50
EOF2
