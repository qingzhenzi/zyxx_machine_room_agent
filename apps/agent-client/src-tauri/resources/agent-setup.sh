#!/usr/bin/env bash
# 由 agent-client「初始化」页通过 pkexec 调用的 root 安装助手。
# 只做本机 daemon 安装/服务控制；GUI 本身不落系统目录。
#
# 用法（root）：
#   agent-setup.sh install --agent-bin /path/qe-agent [--gui-user NAME] [--start]
#       [--allow-poweroff|--no-allow-poweroff] [--fast-interval SECS] [--full-interval SECS]
#       [--live-interval-ms MS] [--live-push|--no-live-push]
#   agent-setup.sh enroll --link <注册链接>
#   agent-setup.sh start|stop|restart|status
#   agent-setup.sh uninstall [--purge-data]
# 非 root 只允许 status / --dry-run。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="/usr/local/bin"
UNIT_DST="/etc/systemd/system/qe-agent.service"
ENV_DST="/etc/qe-agent.env"
DATA_DIR="/var/lib/qe-agent"
UNIT_SRC="${SCRIPT_DIR}/qe-agent.service"
ENV_SRC="${SCRIPT_DIR}/qe-agent.env.example"

ACTION="${1:-status}"
shift || true

AGENT_BIN=""
GUI_USER=""
START=0
PURGE_DATA=0
DRY_RUN=0
ENROLL_LINK=""
ALLOW_POWEROFF=""
FAST_INTERVAL=""
FULL_INTERVAL=""
LIVE_INTERVAL_MS=""
LIVE_PUSH=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --agent-bin) AGENT_BIN="${2:-}"; shift 2;;
    --gui-user) GUI_USER="${2:-}"; shift 2;;
    --start) START=1; shift;;
    --purge-data) PURGE_DATA=1; shift;;
    --link) ENROLL_LINK="${2:-}"; shift 2;;
    --allow-poweroff) ALLOW_POWEROFF=1; shift;;
    --no-allow-poweroff) ALLOW_POWEROFF=0; shift;;
    --fast-interval) FAST_INTERVAL="${2:-}"; shift 2;;
    --full-interval) FULL_INTERVAL="${2:-}"; shift 2;;
    --live-interval-ms) LIVE_INTERVAL_MS="${2:-}"; shift 2;;
    --live-push) LIVE_PUSH=1; shift;;
    --no-live-push) LIVE_PUSH=0; shift;;
    --dry-run) DRY_RUN=1; shift;;
    *) echo "未知参数：$1" >&2; exit 2;;
  esac
done

run() {
  if [[ "${DRY_RUN}" -eq 1 ]]; then
    printf '[dry-run] %s\n' "$*"
  else
    "$@"
  fi
}

# 写入/覆盖 /etc/qe-agent.env 的单个 key（保留其它配置）。
set_env_value() {
  local key="$1" value="$2"
  if [[ "${DRY_RUN}" -eq 1 ]]; then
    printf '[dry-run] set %s=%s in %s\n' "${key}" "${value}" "${ENV_DST}"
    return 0
  fi
  python3 - "${ENV_DST}" "${key}" "${value}" <<'PYEOF'
import pathlib, sys
path, key, value = sys.argv[1:4]
lines = pathlib.Path(path).read_text().splitlines()
out, seen = [], False
for line in lines:
    if line.startswith(key + '='):
        out.append(f'{key}={value}'); seen = True
    else:
        out.append(line)
if not seen:
    out.append(f'{key}={value}')
pathlib.Path(path).write_text('\n'.join(out) + '\n')
PYEOF
}

need_root() {
  if [[ "${DRY_RUN}" -eq 1 ]]; then return 0; fi
  [[ "${EUID}" -eq 0 ]] || { echo "此操作需要 root（请通过 pkexec/sudo 调用）" >&2; exit 1; }
}

case "${ACTION}" in
  status)
    echo "unit: $( [[ -f "${UNIT_DST}" ]] && echo installed || echo missing )"
    if command -v systemctl >/dev/null 2>&1 && [[ "${DRY_RUN}" -eq 0 ]]; then
      echo "active: $(systemctl is-active qe-agent 2>/dev/null || true)"
      echo "enabled: $(systemctl is-enabled qe-agent 2>/dev/null || true)"
    else
      echo "active: unknown"; echo "enabled: unknown"
    fi
    echo "agent: $(command -v qe-agent 2>/dev/null || echo missing)"
    echo "env: $( [[ -f "${ENV_DST}" ]] && echo installed || echo missing )"
    ;;
  install)
    need_root
    [[ -n "${AGENT_BIN}" && -x "${AGENT_BIN}" ]] || { echo "--agent-bin 必须是可执行的 qe-agent 路径" >&2; exit 1; }
    [[ "$(basename "${AGENT_BIN}")" == "qe-agent" ]] || { echo "--agent-bin 的文件名必须是 qe-agent（即要安装的 daemon 二进制）" >&2; exit 1; }
    [[ -f "${UNIT_SRC}" ]] || { echo "缺少 systemd 模板：${UNIT_SRC}" >&2; exit 1; }
    [[ -f "${ENV_SRC}" ]] || { echo "缺少 env 模板：${ENV_SRC}" >&2; exit 1; }
    if [[ -n "${FAST_INTERVAL}" ]]; then
      [[ "${FAST_INTERVAL}" =~ ^[0-9]+$ ]] && (( FAST_INTERVAL >= 1 && FAST_INTERVAL <= 60 )) \
        || { echo "--fast-interval 必须是 1-60 秒的整数" >&2; exit 1; }
    fi
    if [[ -n "${LIVE_INTERVAL_MS}" ]]; then
      [[ "${LIVE_INTERVAL_MS}" =~ ^[0-9]+$ ]] && (( LIVE_INTERVAL_MS >= 0 && LIVE_INTERVAL_MS <= 5000 )) \
        || { echo "--live-interval-ms 必须是 0-5000 的整数（0 关闭）" >&2; exit 1; }
    fi
    if [[ -n "${FULL_INTERVAL}" ]]; then
      [[ "${FULL_INTERVAL}" =~ ^[0-9]+$ ]] && (( FULL_INTERVAL >= 60 && FULL_INTERVAL <= 3600 )) \
        || { echo "--full-interval 必须是 60-3600 秒的整数" >&2; exit 1; }
    fi
    if [[ -n "${FAST_INTERVAL}" && -n "${FULL_INTERVAL}" ]] && (( FULL_INTERVAL < FAST_INTERVAL )); then
      echo "--full-interval 不能小于 --fast-interval" >&2; exit 1
    fi
    if ! getent group qe-agent >/dev/null 2>&1; then
      run groupadd --system qe-agent
    fi
    run install -d -m 0755 "${BIN_DIR}"
    run install -m 0755 "${AGENT_BIN}" "${BIN_DIR}/qe-agent"
    run install -m 0644 "${UNIT_SRC}" "${UNIT_DST}"
    if [[ ! -f "${ENV_DST}" ]]; then
      run install -d -m 0750 -g qe-agent "${DATA_DIR}"
      run install -m 0600 "${ENV_SRC}" "${ENV_DST}"
    fi
    [[ -n "${ALLOW_POWEROFF}" ]] && set_env_value QE_AGENT_ALLOW_POWEROFF "${ALLOW_POWEROFF}"
    [[ -n "${FAST_INTERVAL}" ]] && set_env_value ROOM_AGENT_FAST_INTERVAL_SECS "${FAST_INTERVAL}"
    [[ -n "${FULL_INTERVAL}" ]] && set_env_value ROOM_AGENT_FULL_INTERVAL_SECS "${FULL_INTERVAL}"
    [[ -n "${LIVE_INTERVAL_MS}" ]] && set_env_value ROOM_AGENT_LIVE_INTERVAL_MILLIS "${LIVE_INTERVAL_MS}"
    [[ -n "${LIVE_PUSH}" ]] && set_env_value ROOM_AGENT_LIVE_PUSH "${LIVE_PUSH}"
    # 配置写入后若服务已在运行，重启一次让它读取新 env（首次安装时服务未启动）。
    if [[ -n "${ALLOW_POWEROFF}${FAST_INTERVAL}${FULL_INTERVAL}" ]] \
       && command -v systemctl >/dev/null 2>&1 \
       && [[ "$(systemctl is-active qe-agent 2>/dev/null || true)" == "active" ]]; then
      run systemctl restart qe-agent
    fi
    if [[ "${DRY_RUN}" -eq 0 ]]; then
      systemctl daemon-reload
    else
      echo "[dry-run] systemctl daemon-reload"
    fi
    if [[ "${START}" -eq 1 ]]; then
      if find "${DATA_DIR}/creds" -name cert.pem -print -quit 2>/dev/null | grep -q .; then
        run systemctl enable --now qe-agent
      else
        echo "尚未注册（无设备凭证），暂不启动 daemon；请执行 enroll 或稍后 start。"
      fi
    fi
    if [[ -n "${GUI_USER}" ]] && id "${GUI_USER}" >/dev/null 2>&1; then
      if ! id -nG "${GUI_USER}" | tr ' ' '\n' | grep -qx qe-agent; then
        run usermod -aG qe-agent "${GUI_USER}"
        echo "已把 ${GUI_USER} 加入 qe-agent 组（需注销/重新登录）"
      fi
    fi
    echo "安装完成：${BIN_DIR}/qe-agent，服务 $( [[ "${START}" -eq 1 ]] && echo '已启动' || echo '未启动（可稍后 start）' )"
    ;;
  enroll)
    need_root
    [[ -n "${ENROLL_LINK}" ]] || { echo "enroll 需要 --link <注册链接>" >&2; exit 1; }
    if ! command -v qe-agent >/dev/null 2>&1; then
      [[ -x "${BIN_DIR}/qe-agent" ]] || { echo "qe-agent 未安装，请先 install" >&2; exit 1; }
    fi
    # 未注册时服务会因缺凭证崩溃重启；先停掉，注册后再启动。
    if command -v systemctl >/dev/null 2>&1; then
      run systemctl stop qe-agent 2>/dev/null || true
    fi
    run install -d -m 0750 -g qe-agent "${DATA_DIR}"
    QE_BIN="$(command -v qe-agent 2>/dev/null || echo "${BIN_DIR}/qe-agent")"
    run "${QE_BIN}" cli enroll "${ENROLL_LINK}" --trust --data-dir "${DATA_DIR}"
    META="$(find "${DATA_DIR}/creds" -name meta.json -printf '%T@ %p\n' 2>/dev/null | sort -nr | head -1 | cut -d' ' -f2-)"
    [[ -n "${META}" ]] || { echo "注册成功但未找到凭证 meta.json" >&2; exit 1; }
    SLOT="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["device_slot"])' "${META}")"
    MTLS="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["mtls_endpoint"])' "${META}")"
    if [[ "${DRY_RUN}" -eq 0 ]]; then
      if [[ -f "${ENV_DST}" ]]; then
        python3 - "${ENV_DST}" "${MTLS}" "${SLOT}" <<'PYEOF'
import pathlib, sys
path, mtls, slot = sys.argv[1:4]
lines = []
seen = set()
for line in pathlib.Path(path).read_text().splitlines():
    key = line.split('=', 1)[0].strip() if '=' in line else ''
    if key == 'DEVICE_PKI_MTLS_URL':
        lines.append(f'DEVICE_PKI_MTLS_URL={mtls}'); seen.add(key); continue
    if key == 'DEVICE_SLOT':
        lines.append(f'DEVICE_SLOT={slot}'); seen.add(key); continue
    if key == 'DEVICE_DATA_DIR':
        lines.append('DEVICE_DATA_DIR=/var/lib/qe-agent'); seen.add(key); continue
    lines.append(line)
if 'DEVICE_PKI_MTLS_URL' not in seen:
    lines.append(f'DEVICE_PKI_MTLS_URL={mtls}')
if 'DEVICE_SLOT' not in seen:
    lines.append(f'DEVICE_SLOT={slot}')
if 'DEVICE_DATA_DIR' not in seen:
    lines.append('DEVICE_DATA_DIR=/var/lib/qe-agent')
pathlib.Path(path).write_text('\n'.join(lines) + '\n')
PYEOF
      else
        cat >"${ENV_DST}" <<EOF
DEVICE_PKI_MTLS_URL=${MTLS}
DEVICE_SLOT=${SLOT}
DEVICE_DATA_DIR=/var/lib/qe-agent
EOF
        chmod 0600 "${ENV_DST}"
      fi
      systemctl daemon-reload
      run systemctl enable --now qe-agent
    fi
    echo "注册完成：slot=${SLOT} mtls=${MTLS}"
    echo "请重新打开 agent-client（或点重新检测）"
    ;;
  start|stop|restart)
    need_root
    if command -v systemctl >/dev/null 2>&1; then
      run systemctl "${ACTION}" qe-agent
    else
      echo "未找到 systemctl" >&2; exit 1
    fi
    ;;
  uninstall)
    need_root
    if command -v systemctl >/dev/null 2>&1; then
      run systemctl disable --now qe-agent 2>/dev/null || true
    fi
    run rm -f "${UNIT_DST}" "${BIN_DIR}/qe-agent"
    [[ "${PURGE_DATA}" -eq 1 ]] && run rm -rf "${DATA_DIR}" "${ENV_DST}"
    if [[ "${DRY_RUN}" -eq 0 ]]; then systemctl daemon-reload; fi
    echo "已卸载 daemon（GUI 保持）"
    ;;
  *)
    echo "未知操作：${ACTION}" >&2; exit 2;;
esac
