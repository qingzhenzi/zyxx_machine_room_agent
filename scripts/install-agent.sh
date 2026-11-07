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
INSTALL_DISPLAY=1
START_AFTER_INSTALL=0

usage() {
    cat <<'EOF'
用法：sudo bash scripts/install-agent.sh [选项]

  --qe-agent-bin PATH      qe-agent 二进制路径（默认 target/release/qe-agent）
  --boot-display-bin PATH  boot-display 二进制路径（默认优先 release，其次 debug）
  --no-boot-display        跳过 boot-display 安装
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
EOF
