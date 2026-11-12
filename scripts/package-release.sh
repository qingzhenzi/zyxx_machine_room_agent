#!/usr/bin/env bash
# 收集 qe-agent + boot-display + agent-client，生成可分发 release bundle。
#
# 用法（workspace 根目录，先构建好 release 产物）：
#   VERSION=0.2.0 bash scripts/package-release.sh
#
# 可覆盖：
#   QE_AGENT_BIN=... BOOT_DISPLAY_BIN=... AGENT_CLIENT_BIN=...
#   OUT_DIR=dist
#
# 产物：
#   dist/qestudio-agent-<version>-linux-x86_64/
#   dist/qestudio-agent-<version>-linux-x86_64.tar.gz
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
VERSION="${VERSION:-0.1.0}"
ARCH="$(uname -m)"
OUT_DIR="${OUT_DIR:-${REPO_ROOT}/dist}"
NAME="qestudio-agent-${VERSION}-linux-${ARCH}"
STAGE="${OUT_DIR}/${NAME}"

QE_AGENT_BIN="${QE_AGENT_BIN:-${REPO_ROOT}/target/release/qe-agent}"
BOOT_DISPLAY_BIN="${BOOT_DISPLAY_BIN:-${REPO_ROOT}/apps/boot-display/src-tauri/target/release/boot-display}"
AGENT_CLIENT_BIN="${AGENT_CLIENT_BIN:-${REPO_ROOT}/apps/agent-client/src-tauri/target/release/qe-agent-client}"

log() { printf '[package-release] %s\n' "$*"; }
die() { printf '[package-release] 错误：%s\n' "$*" >&2; exit 1; }

[[ -x "$QE_AGENT_BIN" ]] || die "缺少 qe-agent release 产物：$QE_AGENT_BIN
  构建：cargo build --release -p qe-agent"
[[ -x "$BOOT_DISPLAY_BIN" ]] || die "缺少 boot-display release 产物：$BOOT_DISPLAY_BIN
  构建：pnpm --dir apps/boot-display tauri build --no-bundle"
[[ -x "$AGENT_CLIENT_BIN" ]] || die "缺少 agent-client release 产物：$AGENT_CLIENT_BIN
  构建：pnpm --dir apps/agent-client tauri build --no-bundle"

rm -rf "$STAGE"
mkdir -p "$STAGE"/{bin,share/icons}
install -m 0755 "$QE_AGENT_BIN" "$STAGE/bin/qe-agent"
install -m 0755 "$BOOT_DISPLAY_BIN" "$STAGE/bin/boot-display"
install -m 0755 "$AGENT_CLIENT_BIN" "$STAGE/bin/qe-agent-client"

CLIENT_RES="${REPO_ROOT}/apps/agent-client/src-tauri/resources"
install -m 0755 "$CLIENT_RES/agent-setup.sh" "$STAGE/share/agent-setup.sh"
install -m 0644 "$CLIENT_RES/qe-agent.service" "$STAGE/share/qe-agent.service"
install -m 0644 "$CLIENT_RES/qe-agent.env.example" "$STAGE/share/qe-agent.env.example"
install -m 0644 "${REPO_ROOT}/apps/agent-client/deploy/qe-agent-client.desktop" "$STAGE/share/qe-agent-client.desktop"
install -m 0644 "${REPO_ROOT}/apps/agent-client/src-tauri/icons/128x128.png" "$STAGE/share/icons/qe-agent-client.png"
install -m 0755 "${SCRIPT_DIR}/release/install.sh" "$STAGE/install.sh"
install -m 0755 "${SCRIPT_DIR}/release/qe-boot-display-session.sh" "$STAGE/share/qe-boot-display-session.sh"
printf '%s\n' "$VERSION" > "$STAGE/VERSION"
cp "${REPO_ROOT}/docs/release-and-install.md" "$STAGE/README.md" 2>/dev/null || true

# manifest + checksums
python3 - "$STAGE" "$VERSION" <<'PY'
import hashlib, json, pathlib, subprocess, sys, datetime
stage, version = pathlib.Path(sys.argv[1]), sys.argv[2]
def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for chunk in iter(lambda: f.read(1024*1024), b''):
            h.update(chunk)
    return h.hexdigest()
files = {}
for path in sorted(stage.rglob('*')):
    if path.is_file() and path.name not in {'manifest.json', 'SHA256SUMS'}:
        files[str(path.relative_to(stage))] = {'size': path.stat().st_size, 'sha256': sha(path)}
try:
    git_sha = subprocess.check_output(['git', '-C', str(stage), 'rev-parse', 'HEAD'], text=True).strip()
except Exception:
    try:
        git_sha = subprocess.check_output(['git', '-C', str(stage.parent.parent), 'rev-parse', 'HEAD'], text=True).strip()
    except Exception:
        git_sha = 'unknown'
manifest = {
    'product': 'QEStudio Agent',
    'version': version,
    'platform': 'linux-x86_64' if sys.maxsize > 2**32 else 'linux',
    'git_sha': git_sha,
    'build_time': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'files': files,
}
(stage / 'manifest.json').write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + '\n')
PY
(cd "$STAGE" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS)

TARBALL="${OUT_DIR}/${NAME}.tar.gz"
tar -C "$OUT_DIR" -czf "$TARBALL" "$NAME"
log "bundle 目录：$STAGE"
log "压缩包：$TARBALL"
log "大小：$(du -h "$TARBALL" | cut -f1)"
log "sha256：$(sha256sum "$TARBALL" | cut -d' ' -f1)"
