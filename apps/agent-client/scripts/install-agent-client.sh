#!/usr/bin/env bash
# QEStudio Agent 桌面客户端安装（用户级，无需 root）：
#   - 安装 GUI 二进制到 ~/.local/bin
#   - 安装图标与 .desktop（应用菜单）
#   - 安装登录自启 .desktop（默认开启，--no-autostart 关闭）
#   - 检查 qe-agent 组权限；--add-group 时执行 sudo usermod -aG qe-agent <user>
#
# 用法：
#   bash scripts/install-agent-client.sh [选项]
# 可选：
#   --gui-bin PATH     指定二进制（默认 target/release/qe-agent-client，其次 debug）
#   --prefix DIR       安装前缀（默认 $HOME/.local）
#   --no-autostart     不安装登录自启
#   --add-group        自动把当前用户加入 qe-agent 组（需要 sudo；加入后需重新登录）
#   --dry-run          只打印将执行的动作
#   -h, --help
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
PREFIX="${HOME}/.local"
GUI_BIN=""
AUTOSTART=1
ADD_GROUP=0
DRY_RUN=0

usage() { sed -n '2,20p' "$0"; }
log() { printf '[install-agent-client] %s\n' "$*"; }
die() { printf '[install-agent-client] 错误：%s\n' "$*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --gui-bin) [[ $# -ge 2 ]] || die "--gui-bin 缺参数"; GUI_BIN="$2"; shift 2;;
    --prefix) [[ $# -ge 2 ]] || die "--prefix 缺参数"; PREFIX="$2"; shift 2;;
    --no-autostart) AUTOSTART=0; shift;;
    --add-group) ADD_GROUP=1; shift;;
    --dry-run) DRY_RUN=1; shift;;
    -h|--help) usage; exit 0;;
    *) die "未知参数：$1（--help 查看用法）";;
  esac
done

[[ "${EUID}" -ne 0 ]] || die "请用普通桌面用户运行（安装到用户目录）；组权限步骤会提示 sudo"

if [[ -z "${GUI_BIN}" ]]; then
  for candidate in \
    "${APP_DIR}/src-tauri/target/release/qe-agent-client" \
    "${APP_DIR}/src-tauri/target/debug/qe-agent-client"; do
    if [[ -x "${candidate}" ]]; then GUI_BIN="${candidate}"; break; fi
  done
fi
[[ -n "${GUI_BIN}" && -x "${GUI_BIN}" ]] || die "找不到 GUI 二进制；先构建：cd ${APP_DIR} && pnpm tauri build --debug（或用 --gui-bin 指定）"

BIN_DST="${PREFIX}/bin/qe-agent-client"
ICON_DIR="${PREFIX}/share/icons/hicolor/128x128/apps"
ICON_DST="${ICON_DIR}/qe-agent-client.png"
DESKTOP_DIR="${PREFIX}/share/applications"
DESKTOP_DST="${DESKTOP_DIR}/qe-agent-client.desktop"
RESOURCE_DIR="${PREFIX}/share/qe-agent-client"
AUTOSTART_DIR="${XDG_CONFIG_HOME:-${HOME}/.config}/autostart"
AUTOSTART_DST="${AUTOSTART_DIR}/qe-agent-client.desktop"
ICON_SRC="${APP_DIR}/src-tauri/icons/128x128.png"
[[ -f "${ICON_SRC}" ]] || die "缺少图标：${ICON_SRC}"

run() {
  if [[ "${DRY_RUN}" -eq 1 ]]; then
    printf '[dry-run] %s\n' "$*"
  else
    "$@"
  fi
}

log "GUI 二进制：${GUI_BIN}"
log "安装前缀：${PREFIX}"
run install -d -m 0755 "${PREFIX}/bin" "${ICON_DIR}" "${DESKTOP_DIR}" "${RESOURCE_DIR}"
run install -m 0755 "${GUI_BIN}" "${BIN_DST}"
run install -m 0644 "${ICON_SRC}" "${ICON_DST}"
# 应用内“安装/初始化”使用的 root 助手与 systemd/env 模板。
run install -m 0755 "${APP_DIR}/src-tauri/resources/agent-setup.sh" "${RESOURCE_DIR}/agent-setup.sh"
run install -m 0644 "${APP_DIR}/src-tauri/resources/qe-agent.service" "${RESOURCE_DIR}/qe-agent.service"
run install -m 0644 "${APP_DIR}/src-tauri/resources/qe-agent.env.example" "${RESOURCE_DIR}/qe-agent.env.example"

# 应用菜单条目（Icon 用绝对路径，避免 hicolor 缓存未更新时找不到图标）
install -d -m 0755 "${DESKTOP_DIR}" 2>/dev/null || true
tmp_desktop="$(mktemp)"
trap 'rm -f "${tmp_desktop}"' EXIT
sed "s|^Exec=.*|Exec=${BIN_DST}|; s|^Icon=.*|Icon=${ICON_DST}|" \
  "${APP_DIR}/deploy/qe-agent-client.desktop" >"${tmp_desktop}"
run install -m 0644 "${tmp_desktop}" "${DESKTOP_DST}"

if [[ "${AUTOSTART}" -eq 1 ]]; then
  run install -d -m 0755 "${AUTOSTART_DIR}"
  sed "s|@EXEC@|${BIN_DST}|g; s|@ICON@|${ICON_DST}|g" \
    "${APP_DIR}/deploy/qe-agent-client-autostart.desktop" >"${tmp_desktop}"
  run install -m 0644 "${tmp_desktop}" "${AUTOSTART_DST}"
  log "已安装登录自启：${AUTOSTART_DST}"
else
  log "跳过登录自启（--no-autostart）"
fi

# 应用菜单/图标缓存（尽力而为，失败不致命）
if [[ "${DRY_RUN}" -eq 0 ]] && command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "${DESKTOP_DIR}" >/dev/null 2>&1 || true
fi
if [[ "${DRY_RUN}" -eq 0 ]] && command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q "${PREFIX}/share/icons/hicolor" >/dev/null 2>&1 || true
fi

# IPC socket 属组：/run/qe-agent/agent.sock 权限 0660，属组 qe-agent。
if getent group qe-agent >/dev/null 2>&1; then
  if id -nG "${USER}" 2>/dev/null | tr ' ' '\n' | grep -qx "qe-agent"; then
    log "当前用户已在 qe-agent 组（可访问本机 IPC）"
  else
    log "当前用户不在 qe-agent 组，桌面端会显示“权限不足/未连接”。"
    log "修复：sudo usermod -aG qe-agent ${USER}   （之后注销/重新登录）"
    if [[ "${ADD_GROUP}" -eq 1 ]]; then
      run sudo usermod -aG qe-agent "${USER}"
      log "已加入组；请注销/重新登录后生效"
    fi
  fi
else
  log "尚未创建 qe-agent 组（先安装 daemon：sudo bash room_agent/scripts/install-agent.sh --start）"
fi

if [[ "${DRY_RUN}" -eq 0 ]] && command -v desktop-file-validate >/dev/null 2>&1; then
  desktop-file-validate "${DESKTOP_DST}" >/dev/null || log "警告：desktop-file-validate 未通过（可忽略或手工修正）"
fi

log "完成。可从应用菜单启动「QEStudio Agent」，或直接：${BIN_DST}"
log "程序默认隐藏启动、托盘常驻；点击托盘图标显示窗口。"
