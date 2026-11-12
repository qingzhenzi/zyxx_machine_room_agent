#!/usr/bin/env bash
# 在本机编译 release，然后把二进制推送到目标 Linux 机器。
#
# 用法：./scripts/deploy-debian.sh [user@host]
# 默认：dev@192.168.8.115
set -euo pipefail

target="${1:-dev@192.168.8.115}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "==> building release on local machine"
cargo build --release --manifest-path "$root/Cargo.toml"

echo "==> pushing target/release/lan_video -> $target:~/lan_video"
scp "$root/target/release/lan_video" "$target:~/lan_video"

echo "==> verifying remote binary"
ssh "$target" 'chmod +x ~/lan_video && ~/lan_video --version && ~/lan_video capabilities | head -6'
