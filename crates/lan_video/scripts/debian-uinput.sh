#!/usr/bin/env bash
# 让当前用户可写 /dev/uinput（Linux 被控端需要）。
# 用法：在需要被远程控制的机器上执行本脚本（需要 sudo）。
set -euo pipefail

rule='KERNEL=="uinput", GROUP="input", MODE="0660", TAG+="uaccess"'
echo "$rule" | sudo tee /etc/udev/rules.d/99-uinput.rules >/dev/null
sudo udevadm control --reload-rules
sudo udevadm trigger /dev/uinput || true
sudo setfacl -m "u:$(id -un):rw" /dev/uinput || true

ls -l /dev/uinput
if [ -w /dev/uinput ]; then
  echo "uinput: OK（当前用户可写）"
else
  echo "uinput: 仍不可写，检查 udev/seat 或把用户加入 input 组" >&2
  exit 1
fi
