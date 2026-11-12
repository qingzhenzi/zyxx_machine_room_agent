#!/usr/bin/env bash
# 在「被控 Debian」上执行：装 agent 用户服务 + uinput 权限。
# 前提：二进制已放到 BIN（默认 /home/dev/lan_video），GStreamer runtime 已装。
#
# 用法：AGENT_TOKEN='...' ./scripts/setup-debian-agent.sh [BIN]
# 不传 AGENT_TOKEN 会生成一个；token 文件路径会打印，复制到控制端。
set -euo pipefail

BIN="${1:-/home/dev/lan_video}"
TOKEN="${AGENT_TOKEN:-$(head -c 24 /dev/urandom | base64 -w0 | tr '+/' '-_' | tr -d '=\n')}"
UID_NUM="$(id -u)"
mkdir -p "$HOME/.config/systemd/user" "$HOME/.local/state/lan_video"

cat > "$HOME/.config/systemd/user/lan-video-agent.service" <<UNIT
[Unit]
Description=lan_video agent: control API + screen capture
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Environment=XDG_RUNTIME_DIR=/run/user/$UID_NUM
Environment=DISPLAY=:0
Environment=WAYLAND_DISPLAY=
Environment=PULSE_SERVER=unix:/run/user/$UID_NUM/pulse/native
ExecStart=$BIN serve --bind 0.0.0.0:38080 --token $TOKEN
Restart=always
RestartSec=2

[Install]
WantedBy=graphical-session.target
UNIT

export XDG_RUNTIME_DIR="/run/user/$UID_NUM"
export DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$UID_NUM/bus"
systemctl --user daemon-reload
systemctl --user enable --now lan-video-agent.service

# 给当前用户开 /dev/uinput（uinput 注入需要）
if [ "$(id -u)" -ne 0 ] && [ -x "$(dirname "$0")/debian-uinput.sh" ]; then
  "$(dirname "$0")/debian-uinput.sh" || true
fi

printf '%s\n' "$TOKEN" > "$HOME/.config/lan_video-agent-token"
chmod 600 "$HOME/.config/lan_video-agent-token"
echo "agent token 文件: $HOME/.config/lan_video-agent-token"
echo "把它复制到控制端: scp $HOME/.config/lan_video-agent-token <controller>:\$HOME/.config/lan_video/debian-agent-token"
