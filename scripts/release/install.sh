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
GRUB_PASSWORD=""
BOOT_DISPLAY_DEBUG_PASSWORD="QEBootDebug2026!"

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
  --grub-password PASS     设置 GRUB 超级用户密码（默认随机生成并保存到 /root/qe-grub-password）
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
    --grub-password) GRUB_PASSWORD="${2:-}"; shift 2;;
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

# 给 GRUB 设置超级用户密码：隐藏菜单后，按 Esc/Shift 也只能到密码提示，
# 不能编辑启动项/进 recovery。密码哈希由 grub-mkpasswd-pbkdf2 生成。
configure_grub_password() {
  [[ -f /etc/default/grub ]] || return 0
  command -v grub-mkpasswd-pbkdf2 >/dev/null 2>&1 || {
    log "警告：缺少 grub-mkpasswd-pbkdf2，跳过 GRUB 密码"
    return 0
  }
  local pass="$GRUB_PASSWORD"
  if [[ -z "$pass" ]]; then
    pass="$(openssl rand -base64 18 2>/dev/null | tr -dc 'A-Za-z0-9' | head -c 20)"
    [[ -n "$pass" ]] || pass="$(head -c 18 /dev/urandom | base64 | tr -dc 'A-Za-z0-9' | head -c 20)"
  fi
  if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "[dry-run] set GRUB superuser password (hash via grub-mkpasswd-pbkdf2)"
    return 0
  fi
  local hash
  hash="$(printf '%s\n%s\n' "$pass" "$pass" | grub-mkpasswd-pbkdf2 2>/dev/null | grep -o 'grub\.pbkdf2\.[^ ]*' | head -1)"
  if [[ -z "$hash" ]]; then
    log "警告：生成 GRUB 密码哈希失败，跳过"
    return 0
  fi
  # 备份不能放在 /etc/grub.d 并被 chmod +x：grub-mkconfig 会执行所有可执行脚本，
  # 之前 40_custom.qe-backup 被当成脚本执行，导致密码块重复/清理失败。
  install -d -m 0700 /root/qe-grub-backups
  # 确保 GRUB 自动启动项是 --unrestricted：superusers 只保护编辑/命令行，
  # 正常启动不需要密码（否则会卡在 GRUB 密码提示）。
  for f in /etc/grub.d/10_linux /etc/grub.d/20_linux_xen; do
    [[ -f "$f" ]] || continue
    cp -a "$f" "/root/qe-grub-backups/$(basename "$f").pre-qe" 2>/dev/null || true
    sed -i "s/menuentry '/menuentry --unrestricted '/g; s/submenu '/submenu --unrestricted '/g" "$f"
  done
  local custom="/etc/grub.d/40_custom"
  cp -a "$custom" /root/qe-grub-backups/40_custom.pre-qe 2>/dev/null || true
  # 清掉旧版本误留在 grub.d 内的可执行备份
  for old in /etc/grub.d/*.qe-backup*; do
    [[ -e "$old" ]] || continue
    chmod -x "$old" 2>/dev/null || true
    mv "$old" /root/qe-grub-backups/ 2>/dev/null || true
  done
  # 去掉本项目旧块，避免重装重复
  if grep -q "BEGIN QESTUDIO GRUB PASSWORD" "$custom" 2>/dev/null; then
    sed -i '/# BEGIN QESTUDIO GRUB PASSWORD/,/# END QESTUDIO GRUB PASSWORD/d' "$custom"
  fi
  {
    echo ""
    echo "# BEGIN QESTUDIO GRUB PASSWORD"
    echo 'set superusers="qe-root"'
    echo "password_pbkdf2 qe-root ${hash}"
    echo "# END QESTUDIO GRUB PASSWORD"
  } >>"$custom"
  chmod 0755 "$custom"
  printf '%s\n' "$pass" >/root/qe-grub-password
  chmod 0600 /root/qe-grub-password
  log "已设置 GRUB 超级用户密码；密码保存在 /root/qe-grub-password (0600)"
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
    # Xorg 侧（SDDM 的 X server）禁止 VT 切换。
    install -d -m 0755 /etc/X11/xorg.conf.d
    cat >/etc/X11/xorg.conf.d/99-qestudio-no-vtswitch.conf <<'XEOF'
Section "ServerFlags"
    Option "DontVTSwitch" "true"
EndSection
XEOF
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

  # 2.5) 隐藏 GRUB 菜单：单系统教室机不需要 5 秒启动菜单
  if [[ -f /etc/default/grub ]]; then
    if [[ "$DRY_RUN" -eq 1 ]]; then
      echo "[dry-run] harden /etc/default/grub (TIMEOUT=0, TIMEOUT_STYLE=hidden, DISABLE_OS_PROBER, DISABLE_RECOVERY)"
    else
      cp -a /etc/default/grub /etc/default/grub.qe-backup 2>/dev/null || true
      for kv in "GRUB_TIMEOUT=0" "GRUB_TIMEOUT_STYLE=hidden" "GRUB_RECORDFAIL_TIMEOUT=0" "GRUB_DISABLE_OS_PROBER=true" "GRUB_DISABLE_RECOVERY=true"; do
        k="${kv%%=*}"; v="${kv#*=}"
        if grep -q "^${k}=" /etc/default/grub; then
          sed -i "s|^${k}=.*|${k}=${v}|" /etc/default/grub
        else
          echo "${k}=${v}" >> /etc/default/grub
        fi
      done
      # 去掉 UEFI Firmware Settings 菜单项（BIOS 仍可按 F10 进入）。
      if [[ -x /etc/grub.d/30_uefi-firmware ]]; then
        chmod -x /etc/grub.d/30_uefi-firmware 2>/dev/null || true
      fi
      if command -v update-grub >/dev/null 2>&1; then
        update-grub >/tmp/qe-grub.log 2>&1 || log "警告：update-grub 失败，见 /tmp/qe-grub.log"
      elif command -v grub-mkconfig >/dev/null 2>&1 && [[ -d /boot/grub ]]; then
        grub-mkconfig -o /boot/grub/grub.cfg >/tmp/qe-grub.log 2>&1 || log "警告：grub-mkconfig 失败，见 /tmp/qe-grub.log"
      fi
      log "已隐藏 GRUB 菜单（备份 /etc/default/grub.qe-backup）"
    fi
  fi
  # GRUB 密码默认不启用：superusers 会让所有启动项默认需要认证，配置错误会卡启动。
  # 只有显式 --grub-password 才启用；后续应加 --unrestricted 并先在 VM 验证。
  if [[ -n "$GRUB_PASSWORD" ]]; then
    configure_grub_password
  else
    log "未启用 GRUB 密码（如需锁定 Esc/Shift，请显式 --grub-password，且先在测试机验证）"
  fi

  # 2.8) KDE Wayland：用 KWin 脚本强制 boot-display 置顶/全屏/不进任务栏，
  # 并禁用常用 KWin 快捷键（Alt+F4 / Alt+F3 / Alt+Tab 等）。
  if command -v kwin_wayland >/dev/null 2>&1 || command -v kwin_x11 >/dev/null 2>&1; then
    local gui_home_kde
    gui_home_kde="$(getent passwd "$GUI_USER" | cut -d: -f6)"
    if [[ -n "$gui_home_kde" && -d "$gui_home_kde" ]]; then
      local script_dir="$gui_home_kde/.local/share/kwin/scripts/qe-kiosk"
      if [[ "$DRY_RUN" -eq 1 ]]; then
        echo "[dry-run] install KWin script qe-kiosk for $GUI_USER"
      else
        run install -d -m 0755 "$script_dir/contents/code"
        cat >"$script_dir/metadata.json" <<'KJSON'
{
  "KPackageStructure": "KWin/Script",
  "KPlugin": {
    "Id": "qe-kiosk",
    "Name": "QEStudio Kiosk",
    "Description": "Keep the QEStudio boot display above other windows",
    "Version": "1.0",
    "License": "MIT",
    "EnabledByDefault": true
  },
  "X-Plasma-API": "javascript",
  "X-Plasma-MainScript": "code/main.js"
}
KJSON
        cat >"$script_dir/contents/code/main.js" <<'KJS'
var bootWindow = null;
function isBootDisplay(client) {
  if (!client) return false;
  var caption = client.caption || "";
  var resource = (client.resourceClass || "").toString();
  return caption.indexOf("QEStudio Boot Display") >= 0 ||
         resource === "org.qestudio.boot-display" ||
         resource === "qe-boot-display" ||
         resource === "boot-display";
}
function allWindows() {
  return workspace.windowList ? workspace.windowList() : (workspace.clientList ? workspace.clientList() : []);
}
function enforce(client) {
  if (!client) return;
  client.keepAbove = true;
  client.fullScreen = true;
  client.skipTaskbar = true;
  client.noBorder = true;
  client.minimized = false;
}
function bootAlive() {
  if (!bootWindow) return false;
  return allWindows().indexOf(bootWindow) >= 0;
}
function minimizeOthers() {
  if (!bootAlive()) { bootWindow = null; return; }
  var windows = allWindows();
  for (var i = 0; i < windows.length; i++) {
    var w = windows[i];
    if (w !== bootWindow && w !== null) {
      w.minimized = true;
    }
  }
}
function onAdded(client) {
  if (bootWindow && !bootAlive()) bootWindow = null;
  if (isBootDisplay(client)) {
    bootWindow = client;
    enforce(client);
    minimizeOthers();
    return;
  }
  // 展示层存在期间，任何新应用/通知窗口都立即最小化，不能盖到上面
  if (bootWindow) client.minimized = true;
}
function onActivated(client) {
  if (!bootWindow) return;
  if (client === bootWindow) { enforce(client); return; }
  // 不允许切换到其他应用；把焦点还给 boot-display
  client.minimized = true;
  workspace.activeWindow = bootWindow;
  enforce(bootWindow);
}
workspace.windowAdded.connect(onAdded);
workspace.windowActivated.connect(onActivated);
var windows = allWindows();
for (var i = 0; i < windows.length; i++) {
  if (isBootDisplay(windows[i])) {
    bootWindow = windows[i];
    enforce(bootWindow);
    minimizeOthers();
  }
}
KJS
        chown -R "$GUI_USER":"$(id -gn "$GUI_USER")" "$gui_home_kde/.local/share/kwin"
        # 启用脚本：直接写 kwinrc，避免依赖 DBus/当前会话
        local kwinrc="$gui_home_kde/.config/kwinrc"
        run install -d -m 0700 "$gui_home_kde/.config"
        if grep -q '^\[Plugins\]' "$kwinrc" 2>/dev/null; then
          if grep -q '^qe-kioskEnabled=' "$kwinrc" 2>/dev/null; then
            sed -i 's/^qe-kioskEnabled=.*/qe-kioskEnabled=true/' "$kwinrc"
          else
            sed -i '/^\[Plugins\]/a qe-kioskEnabled=true' "$kwinrc"
          fi
        else
          printf '
[Plugins]
qe-kioskEnabled=true
' >>"$kwinrc"
        fi
        chown "$GUI_USER":"$(id -gn "$GUI_USER")" "$kwinrc"

        # 禁用会切走/关掉展示层的 KDE 全局快捷键（下次登录生效）
        if command -v kwriteconfig6 >/dev/null 2>&1; then
          disable_shortcut() {
            local group="$1" key="$2"
            run su -s /bin/sh "$GUI_USER" -c "kwriteconfig6 --file kglobalshortcutsrc --group '$group' --key '$key' 'none,none,none'" 2>/dev/null || true
          }
          for key in "Window Close" "Window Operations Menu" "Window Minimize" "Window Maximize" "Walk Through Windows" "Walk Through Windows (Reverse)" "Walk Through Windows of Current Application" "Show Desktop" "Activate Window Demanding Attention"; do
            disable_shortcut kwin "$key"
          done
          for key in "Log Out" "Lock Session" "Halt Without Confirmation" "Reboot" "Shut Down"; do
            disable_shortcut ksmserver "$key"
          done
          for key in "activate application launcher" "manage activities" "clipboard_action" "cycle-panels"; do
            disable_shortcut plasmashell "$key"
          done
          for n in 1 2 3 4 5 6 7 8 9; do disable_shortcut plasmashell "activate task manager entry ${n}"; done
          log "已禁用 KDE 关闭/切换窗口/注销/启动器快捷键（下次登录生效）"
        fi
        log "已安装 KWin qe-kiosk 脚本：boot-display 强制置顶/全屏"
      fi
    fi
  fi

  # 2.9) Plasma 面板自动隐藏 + 勿扰：否则面板/通知会盖在 boot-display 上
  local gui_home_panel
  gui_home_panel="$(getent passwd "$GUI_USER" | cut -d: -f6)"
  if [[ -n "$gui_home_panel" && -d "$gui_home_panel" ]]; then
    local plasmashellrc="$gui_home_panel/.config/plasmashellrc"
    if [[ "$DRY_RUN" -eq 1 ]]; then
      echo "[dry-run] set Plasma panelVisibility=1 and DoNotDisturb=1 for $GUI_USER"
    else
      run install -d -m 0700 "$gui_home_panel/.config"
      if [[ -f "$plasmashellrc" ]] && grep -q '^panelVisibility=' "$plasmashellrc"; then
        sed -i 's/^panelVisibility=.*/panelVisibility=1/' "$plasmashellrc"
      else
        printf '\npanelVisibility=1\n' >>"$plasmashellrc"
      fi
      chown "$GUI_USER":"$(id -gn "$GUI_USER")" "$plasmashellrc"
      local notifyrc="$gui_home_panel/.config/plasmanotifyrc"
      if [[ -f "$notifyrc" ]] && grep -q '^DoNotDisturb=' "$notifyrc"; then
        sed -i 's/^DoNotDisturb=.*/DoNotDisturb=true/' "$notifyrc"
      else
        printf '\n[Notifications]\nDoNotDisturb=true\n' >>"$notifyrc"
      fi
      chown "$GUI_USER":"$(id -gn "$GUI_USER")" "$notifyrc"
    fi
    log "已配置 Plasma 面板自动隐藏/勿扰（下次登录生效）"
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
    set_env QE_BOOT_DISPLAY_DEBUG_PASSWORD "${BOOT_DISPLAY_DEBUG_PASSWORD:-QEBootDebug2026!}"
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
