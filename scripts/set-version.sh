#!/usr/bin/env bash
# 同步三个组件的版本号（release CI / 手动发版）
# 用法：bash scripts/set-version.sh 0.2.0
set -euo pipefail
VERSION="${1:-}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || {
  echo "用法：$0 <x.y.z>" >&2; exit 2;
}
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Cargo workspace 版本
sed -i -E "0,/^version = \"[^\"]+\"/s//version = \"${VERSION}\"/" "$REPO/Cargo.toml"

python3 - "$VERSION" "$REPO" <<'PY'
import json, pathlib, re, sys
version, repo = sys.argv[1], pathlib.Path(sys.argv[2])
for rel in [
    'apps/boot-display/src-tauri/Cargo.toml',
    'apps/boot-display/src-tauri/tauri.conf.json',
    'apps/boot-display/package.json',
    'apps/agent-client/src-tauri/Cargo.toml',
    'apps/agent-client/src-tauri/tauri.conf.json',
    'apps/agent-client/package.json',
]:
    path = repo / rel
    if not path.exists():
        continue
    text = path.read_text()
    if path.suffix == '.json':
        data = json.loads(text)
        data['version'] = version
        path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + '\n')
    else:
        text = re.sub(r'(?m)^version = "[^"]+"', f'version = "{version}"', text, count=1)
        path.write_text(text)
print(f'version -> {version}')
PY
