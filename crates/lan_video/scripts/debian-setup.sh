#!/usr/bin/env bash
# Debian 侧一次性运行依赖安装（需要 sudo）。
# 本项目的二进制只依赖运行时的 gst-launch-1.0 + 插件，不需要在本机编译 Rust。
#
# 用法：在 Debian 上执行：./scripts/debian-setup.sh
set -euo pipefail

sudo apt-get update
sudo apt-get install -y \
  gstreamer1.0-tools \
  gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-ugly \
  gstreamer1.0-libav \
  gstreamer1.0-vaapi \
  gstreamer1.0-pipewire

echo "==> verify"
gst-inspect-1.0 --version
for e in rtpbin rtph264depay opusdec decodebin autovideosink waylandsink pipewiresrc x264enc; do
  printf '%-16s %s\n' "$e" "$(gst-inspect-1.0 "$e" >/dev/null 2>&1 && echo yes || echo no)"
done
