#!/usr/bin/env bash
# QEStudio 机器端一键安装：qe-agent（daemon）+ boot-display（展示层）+ systemd。
#
# 用法（workspace 根目录，root）：
#   cargo build -p qe-agent --release                 # 先构建 daemon（脚本不会自动 build）
#   sudo bash scripts/install-agent.sh [选项]
#
# 选项：
#   --qe-agent-bin PATH      指定 qe-agent 二进制（默认 target/release/qe-agent）
#   --boot-display-bin PATH  指定 boot-display 二进制（默认取 apps/boot-display 预构建产物）
#   --no-boot-display        跳过展示层安装
#   --gui-bin PATH           指定 agent-client GUI 二进制（默认 apps/agent-client/target/release/qe-agent-client）
#   --gui-user USER          为该用户安装登录自启并加入 qe-agent 组（默认 SUDO_USER）
#   --no-gui                 跳过桌面客户端安装
#   --start                  安装完成后 systemctl enable --now qe-agent
#   -h, --help               显示帮助
#
# 可重复执行：已存在的 /etc/qe-agent.env 不会被覆盖，仅幂等补充/更新
# QE_BOOT_DISPLAY_BIN；不会 cargo clean / 删除任何数据。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
UNIT_SRC="${REPO_ROOT}/apps/qe-agent/deploy/qe-agent.service.example"
ENV_SRC="${REPO_ROOT}/apps/qe-agent/deploy/qe-agent.env.example"
UNIT_DST="/etc/systemd/system/qe-agent.service"
ENV_DST="/etc/qe-agent.env"
BIN_DIR="/usr/local/bin"
DISPLAY_RELEASE="${REPO_ROOT}/apps/boot-display/src-tauri/target/release/boot-display"
DISPLAY_DEBUG="${REPO_ROOT}/apps/boot-display/src-tauri/target/debug/boot-display"

QE_AGENT_BIN="${QE_AGENT_BIN:-${REPO_ROOT}/target/release/qe-agent}"
BOOT_DISPLAY_BIN="${BOOT_DISPLAY_BIN:-}"
GUI_BIN="${QE_AGENT_CLIENT_BIN:-${REPO_ROOT}/apps/agent-client/src-tauri/target/release/qe-agent-client}"
GUI_USER="${GUI_USER:-${SUDO_USER:-}}"
INSTALL_DISPLAY=1
INSTALL_GUI=1
START_AFTER_INSTALL=0

usage() {
    cat <<'EOF'
用法：sudo bash scripts/install-agent.sh [选项]

  --qe-agent-bin PATH      qe-agent 二进制路径（默认 target/release/qe-agent）
  --boot-display-bin PATH  boot-display 二进制路径（默认优先 release，其次 debug）
  --no-boot-display        跳过 boot-display 安装
  --gui-bin PATH           agent-client GUI 二进制（默认 apps/agent-client/target/release/qe-agent-client）
  --gui-user USER          安装该用户的登录自启并加入 qe-agent 组（默认 SUDO_USER）
  --no-gui                 跳过桌面客户端安装
  --start                  安装完成后 systemctl enable --now qe-agent
  -h, --help               显示本帮助

首次使用前先在 workspace 根目录构建：cargo build -p qe-agent --release
展示层构建（可选）：pnpm --dir apps/boot-display tauri build
EOF
}

log() { printf '[install-agent] %s\n' "$*"; }
die() {
    printf '[install-agent] 错误：%s\n' "$*" >&2
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --qe-agent-bin)
            [[ $# -ge 2 ]] || die "--qe-agent-bin 缺少参数"
            QE_AGENT_BIN="$2"
            shift 2
            ;;
        --boot-display-bin)
            [[ $# -ge 2 ]] || die "--boot-display-bin 缺少参数"
            BOOT_DISPLAY_BIN="$2"
            shift 2
            ;;
        --no-boot-display)
            INSTALL_DISPLAY=0
            shift
            ;;
        --gui-bin)
            [[ $# -ge 2 ]] || { printf '错误：--gui-bin 缺少参数\n' >&2; exit 2; }
            GUI_BIN="$2"
            shift 2
            ;;
        --gui-user)
            [[ $# -ge 2 ]] || { printf '错误：--gui-user 缺少参数\n' >&2; exit 2; }
            GUI_USER="$2"
            shift 2
            ;;
        --no-gui)
            INSTALL_GUI=0
            shift
            ;;
        --start)
            START_AFTER_INSTALL=1
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "未知参数：$1" >&2
            usage
            exit 2
            ;;
    esac
done

[[ "${EUID}" -eq 0 ]] || die "需要 root：sudo bash scripts/install-agent.sh [选项]"
command -v systemctl >/dev/null 2>&1 || die "未找到 systemctl（本脚本仅支持 systemd Linux）"

# ---- 1. qe-agent 二进制 ----
[[ -f "${UNIT_SRC}" ]] || die "缺少 systemd 单元模板：${UNIT_SRC}"
[[ -f "${ENV_SRC}" ]] || die "缺少 env 模板：${ENV_SRC}"
if [[ ! -x "${QE_AGENT_BIN}" ]]; then
    die "找不到 qe-agent 可执行文件：${QE_AGENT_BIN}
      先构建：cargo build -p qe-agent --release（或用 --qe-agent-bin 指定路径）"
fi
install -d -m 0755 "${BIN_DIR}"
install -m 0755 "${QE_AGENT_BIN}" "${BIN_DIR}/qe-agent"
log "已安装 ${BIN_DIR}/qe-agent（来源：${QE_AGENT_BIN}）"

# ---- 2. qe-agent 系统组（IPC socket 0660 属组）----
if ! getent group qe-agent >/dev/null 2>&1; then
    groupadd --system qe-agent
    log "已创建系统组 qe-agent（GUI 用户需加入：usermod -aG qe-agent <user>）"
fi

# ---- 3. systemd 单元 ----
install -m 0644 "${UNIT_SRC}" "${UNIT_DST}"
log "已安装 ${UNIT_DST}"

# ---- 4. /etc/qe-agent.env（保留已有内容）----
if [[ -f "${ENV_DST}" ]]; then
    log "保留已有 ${ENV_DST}（仅按需补充 QE_BOOT_DISPLAY_BIN）"
else
    install -m 0600 "${ENV_SRC}" "${ENV_DST}"
    log "已安装 ${ENV_DST} 模板（请填写 DEVICE_PKI_MTLS_URL / DEVICE_SLOT / DEVICE_DATA_DIR）"
fi

# ---- 5. boot-display（尽力安装；缺失只提示构建命令，不影响 daemon）----
if [[ "${INSTALL_DISPLAY}" -eq 1 ]]; then
    DISPLAY_SRC=""
    if [[ -n "${BOOT_DISPLAY_BIN}" ]]; then
        [[ -x "${BOOT_DISPLAY_BIN}" ]] || die "--boot-display-bin 不可执行：${BOOT_DISPLAY_BIN}"
        DISPLAY_SRC="${BOOT_DISPLAY_BIN}"
    elif [[ -x "${DISPLAY_RELEASE}" ]]; then
        DISPLAY_SRC="${DISPLAY_RELEASE}"
    elif [[ -x "${DISPLAY_DEBUG}" ]]; then
        DISPLAY_SRC="${DISPLAY_DEBUG}"
        log "未找到 release 版 boot-display，回退使用 debug 产物（演示可用，正式环境建议 release）"
    fi

    if [[ -n "${DISPLAY_SRC}" ]]; then
        install -m 0755 "${DISPLAY_SRC}" "${BIN_DIR}/boot-display"
        log "已安装 ${BIN_DIR}/boot-display（来源：${DISPLAY_SRC}）"
        if grep -q '^QE_BOOT_DISPLAY_BIN=' "${ENV_DST}" 2>/dev/null; then
            sed -i "s|^QE_BOOT_DISPLAY_BIN=.*|QE_BOOT_DISPLAY_BIN=${BIN_DIR}/boot-display|" "${ENV_DST}"
        else
            {
                printf '\n# 展示层（boot-display）可执行文件；agent 启动时 spawn + supervise\n'
                printf 'QE_BOOT_DISPLAY_BIN=%s\n' "${BIN_DIR}/boot-display"
            } >>"${ENV_DST}"
        fi
        log "已设置 QE_BOOT_DISPLAY_BIN=${BIN_DIR}/boot-display"
    else
        log "未找到预构建 boot-display，跳过展示层安装（不影响 daemon；display 阶段会 skipped）。"
        log "构建：pnpm --dir apps/boot-display tauri build"
        log "产物：apps/boot-display/src-tauri/target/release/boot-display；构建后重跑本脚本即可。"
    fi
fi

# ---- 6. agent-client 桌面客户端（尽力安装；二进制缺失只提示构建命令）----
if [[ "${INSTALL_GUI}" -eq 1 ]]; then
    if [[ -x "${GUI_BIN}" ]]; then
        install -m 0755 "${GUI_BIN}" "${BIN_DIR}/qe-agent-client"
        ICON_DIR="/usr/share/icons/hicolor/128x128/apps"
        DESKTOP_DIR="/usr/share/applications"
        install -d -m 0755 "${ICON_DIR}" "${DESKTOP_DIR}"
        install -m 0644 "${REPO_ROOT}/apps/agent-client/src-tauri/icons/128x128.png" \
            "${ICON_DIR}/qe-agent-client.png"
        install -m 0644 "${REPO_ROOT}/apps/agent-client/deploy/qe-agent-client.desktop" \
            "${DESKTOP_DIR}/qe-agent-client.desktop"
        log "已安装 ${BIN_DIR}/qe-agent-client 与桌面菜单项"
        if command -v update-desktop-database >/dev/null 2>&1; then
            update-desktop-database "${DESKTOP_DIR}" >/dev/null 2>&1 || true
        fi
        if command -v gtk-update-icon-cache >/dev/null 2>&1; then
            gtk-update-icon-cache -q /usr/share/icons/hicolor >/dev/null 2>&1 || true
        fi
        if [[ -n "${GUI_USER}" ]] && id "${GUI_USER}" >/dev/null 2>&1; then
            GUI_HOME="$(getent passwd "${GUI_USER}" | cut -d: -f6)"
            if [[ -n "${GUI_HOME}" && -d "${GUI_HOME}" ]]; then
                install -d -m 0755 -o "${GUI_USER}" -g "$(id -gn "${GUI_USER}")" \
                    "${GUI_HOME}/.config/autostart"
                sed "s|@EXEC@|${BIN_DIR}/qe-agent-client|g; s|@ICON@|${ICON_DIR}/qe-agent-client.png|g" \
                    "${REPO_ROOT}/apps/agent-client/deploy/qe-agent-client-autostart.desktop" \
                    >"${GUI_HOME}/.config/autostart/qe-agent-client.desktop"
                chown "${GUI_USER}:$(id -gn "${GUI_USER}")" \
                    "${GUI_HOME}/.config/autostart/qe-agent-client.desktop"
                chmod 0644 "${GUI_HOME}/.config/autostart/qe-agent-client.desktop"
                log "已为 ${GUI_USER} 安装登录自启：${GUI_HOME}/.config/autostart/qe-agent-client.desktop"
            fi
            if getent group qe-agent >/dev/null 2>&1 &&
                ! id -nG "${GUI_USER}" | tr ' ' '\n' | grep -qx qe-agent; then
                usermod -aG qe-agent "${GUI_USER}"
                log "已把 ${GUI_USER} 加入 qe-agent 组（需注销/重新登录生效）"
            fi
        else
            log "未指定 --gui-user，跳过用户级登录自启/组授权（桌面用户可手动加入 qe-agent 组）"
        fi
    else
        log "未找到 agent-client 二进制：${GUI_BIN}"
        log "构建：cd apps/agent-client && pnpm tauri build --debug（或用 --gui-bin 指定）"
        log "构建后重跑本脚本即可安装 GUI 与登录自启。"
    fi
fi

systemctl daemon-reload
log "已执行 systemctl daemon-reload"

if [[ "${START_AFTER_INSTALL}" -eq 1 ]]; then
    systemctl enable --now qe-agent
    log "已 systemctl enable --now qe-agent"
fi

cat <<'EOF'

下一步：
  1. sudoedit /etc/qe-agent.env          # 填 DEVICE_PKI_MTLS_URL / DEVICE_SLOT / DEVICE_DATA_DIR
  2. sudo qe-agent cli enroll '<注册链接>' --trust
  3. sudo qe-agent cli status            # 自检凭证与宿主能力
  4. sudo systemctl enable --now qe-agent   # 或重跑本脚本加 --start
  5. curl --unix-socket /run/qe-agent/agent.sock http://localhost/v1/status
  6. 桌面客户端：应用菜单「QEStudio Agent」；或 /usr/local/bin/qe-agent-client
     首次登录后托盘常驻；默认隐藏窗口，点托盘图标显示。
EOF
