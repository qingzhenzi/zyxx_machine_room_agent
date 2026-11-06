#!/usr/bin/env bash
# 安装 qe-agent daemon（Linux，需要 root）。
#
# 用法：
#   cargo build -p qe-agent --release
#   sudo bash crates/agent/deploy/install.sh [target/release/qe-agent]
#
# 只做幂等安装：复制二进制、创建 qe-agent 组、安装 systemd 单元与 /etc/qe-agent.env
# 模板（已存在则保留），不自动 enable/start、不修改业务数据。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BINARY="${1:-${SCRIPT_DIR}/../../../target/release/qe-agent}"
UNIT_SRC="${SCRIPT_DIR}/qe-agent.service.example"
ENV_SRC="${SCRIPT_DIR}/qe-agent.env.example"

if [[ "${EUID}" -ne 0 ]]; then
    echo "需要 root：sudo bash $0 [binary]" >&2
    exit 1
fi
if [[ ! -x "${BINARY}" ]]; then
    echo "找不到可执行文件：${BINARY}" >&2
    echo "先在 workspace 根目录执行：cargo build -p qe-agent --release" >&2
    exit 1
fi
for file in "${UNIT_SRC}" "${ENV_SRC}"; do
    if [[ ! -f "${file}" ]]; then
        echo "缺少安装文件：${file}" >&2
        exit 1
    fi
done

install -d -m 0755 /usr/local/bin
install -m 0755 "${BINARY}" /usr/local/bin/qe-agent

if ! getent group qe-agent >/dev/null 2>&1; then
    groupadd --system qe-agent
    echo "已创建系统组 qe-agent（GUI 用户需加入：usermod -aG qe-agent <user>）"
fi

install -m 0644 "${UNIT_SRC}" /etc/systemd/system/qe-agent.service

if [[ -f /etc/qe-agent.env ]]; then
    echo "保留已有 /etc/qe-agent.env"
else
    install -m 0600 "${ENV_SRC}" /etc/qe-agent.env
    echo "已安装 /etc/qe-agent.env 模板（请填写 DEVICE_PKI_MTLS_URL / DEVICE_SLOT / DEVICE_DATA_DIR）"
fi

systemctl daemon-reload
echo
echo "下一步："
echo "  1. sudoedit /etc/qe-agent.env"
echo "  2. sudo /usr/local/bin/qe-agent enroll '<注册链接>' --trust"
echo "  3. sudo /usr/local/bin/qe-agent status"
echo "  4. sudo systemctl enable --now qe-agent"
echo "  5. curl --unix-socket /run/qe-agent/agent.sock http://localhost/v1/status"
