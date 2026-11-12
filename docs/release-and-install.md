# QEStudio 客户端发布与自动部署（方案）

> 状态：设计稿（2026-11-12）。当前仓库**还没有统一 release 产物**；
> 本文定义三件套 `qe-agent + agent-client + boot-display` 的版本发布、打包、
> 新机器安装（UI / CLI）与升级方式。

## 1. 当前真实状态

| 项 | 现状 |
| --- | --- |
| 版本号 | `Cargo.toml` / `tauri.conf.json` 都是 `0.1.0`，手动改 |
| qe-agent release 产物 | 未提交；本机 target 里可能有，但不是发布件 |
| boot-display release 产物 | 未提交 |
| agent-client 产物 | 本机有 `.deb`（`QEStudio Agent_0.1.0_amd64.deb`） |
| 安装方式 | `scripts/install-agent.sh` 从仓库路径安装，要求源码树 + 先构建 |
| 新机器流程 | 拷仓库 → 装 Rust/Node/系统依赖 → build → sudo 脚本，不可交付 |
| Tauri updater | endpoint 是占位 `https://updates.qestudio.example/latest.json`，没有签名/发布 |
| CI | 只有 `room_node/.github/workflows/ci.yml`；room_agent / boot-display / agent-client 无发布流水线 |

结论：**目前没有“版本发布”机制，只有本地构建 + 源码安装脚本。**

## 2. 发布物设计

每次发布一个 Linux x86_64 自包含 bundle：

```text
qestudio-agent-<version>-linux-amd64/
  VERSION
  manifest.json            # 版本、构建时间、git commit、sha256
  SHA256SUMS
  bin/
    qe-agent
    boot-display
    qe-agent-client        # 或 .deb/AppImage，见下
  share/
    agent-setup.sh
    qe-agent.service
    qe-agent.env.example
    qe-agent-client.desktop
    icons/qe-agent-client.png
  install.sh               # CLI 交互安装器
  README.md
```

版本建议：

- 三个二进制共用一个版本号（`v0.2.0`），避免“客户端 0.2 / agent 0.1”错配；
- `manifest.json` 记录 `git_sha`、`build_time`、`rustc`、`node`；
- qe-agent 的 `--version` 和 agent-client 的版本都从同一 `VERSION` 注入；
- Tauri updater 的 `latest.json` 指向 bundle，而不是单个二进制。

## 3. 构建流水线（GitHub / Gitea 通用）

在 Ubuntu 24.04 runner（Docker）上：

1. `apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf build-essential pkg-config`
2. `cargo build --release -p qe-agent`
3. `pnpm --dir apps/boot-display tauri build --no-bundle`
4. `pnpm --dir apps/agent-client tauri build`（产出 `.deb` / AppImage）
5. 调用 `scripts/package-release.sh` 收集产物、写 manifest、生成 `SHA256SUMS`
6. 上传到 Release / 对象存储；生成 updater `latest.json` 并用 minisign 私钥签名
7. 标签 `v*` 触发，产物与 tag 绑定

CI 要避免写死 GHCR/GitHub-only：

- registry 用 `REGISTRY` / `REGISTRY_USER` / `REGISTRY_TOKEN` secret；
- 不依赖 `GITHUB_TOKEN` 的 `packages: write`（Gitea 不支持 job token 发自己的 package）；
- 构建产物用 `actions/upload-artifact`（Gitea 也支持 artifacts），发布到外部存储更稳。

## 4. 新机器安装：CLI

```bash
tar xf qestudio-agent-0.2.0-linux-amd64.tar.gz
cd qestudio-agent-0.2.0-linux-amd64
sudo ./install.sh
```

`install.sh` 交互项：

```text
1. 安装目录？            [/usr/local/bin]
2. GUI 用户？            [SUDO_USER / 当前用户 / 跳过]
3. 允许远程关机？        [Y/n]      → QE_AGENT_ALLOW_POWEROFF
4. 遥测 fast 间隔？      [10s]      → ROOM_AGENT_FAST_INTERVAL_SECS
5. 遥测 full 间隔？      [300s]     → ROOM_AGENT_FULL_INTERVAL_SECS
6. 实时中转？            [y/N]      → ROOM_AGENT_LIVE_PUSH
7. 本地采样间隔？        [500ms]    → ROOM_AGENT_LIVE_INTERVAL_MILLIS
8. 注册链接？            [可留空，稍后 GUI 注册]
9. 安装 boot-display？   [Y/n]
10. 安装 agent-client？  [Y/n]
```

非交互：

```bash
sudo ./install.sh --yes \
  --gui-user "$USER" \
  --allow-poweroff \
  --fast-interval 10 --full-interval 300 \
  --no-live-push --live-interval-ms 500 \
  --enroll-link 'https://<control>:4440/enroll?token=...'
```

安装动作：

1. 创建 `qe-agent` 组；
2. 安装 `qe-agent` / `boot-display` / `qe-agent-client`；
3. 安装 systemd unit + `/etc/qe-agent.env`（已存在则只更新选中的 key）；
4. 安装 desktop/icon/autostart；
5. 可选：`qe-agent cli enroll` 注册；
6. `systemctl enable --now qe-agent`；
7. 打印“注销重登”提示（组权限）。

## 5. 新机器安装：UI

两条 UI 路径：

- **推荐**：bundle 内 `install.sh --ui` 先完成系统安装，再以 GUI 用户启动 agent-client；
  agent-client 启动初始化向导负责环境检查/注册（用户不接触终端）。
- 只给 AppImage：用户双击 AppImage，初始化向导用内嵌的 qe-agent/boot-display 资源安装；
  这要求 agent-client bundle 增加 `resources/bin/` 并扩展 `agent_setup_install` 支持
  “从 bundle 安装 boot-display 和 GUI 自身”。

UI 交互流程：

```text
首次启动 agent-client
  → 环境检查（systemd/pkexec/agent 是否存在）
  → 安装 daemon（pkexec）
  → 注册设备（粘贴注册链接）
  → 安装 boot-display（可选）
  → 保存默认配置 → 进入状态页
```

## 6. 自动部署到“新机器”

目标机器是 systemd Linux（Ubuntu/Debian/CachyOS 等）：

1. 制作 U 盘 / PXE 镜像，预置 bundle；
2. 装机后执行 `sudo ./install.sh --yes ...`；
3. 或把安装命令放进首次启动脚本（cloud-init / systemd first-boot）；
4. 注册链接可以预生成一次性 token，安装时自动 enroll；
5. 已经用系统镜像批量克隆的机器，建议 **不把证书烧进镜像**，首次启动用一次性 token 注册，
   避免所有机器共用同一设备身份。

## 7. 升级与回滚

- `qe-agent` 二进制替换 + `systemctl restart`；
- agent-client 用 Tauri updater（minisign 签名）或 `.deb` 升级；
- boot-display 随 agent bundle 替换；
- 回滚：保留上一版 bundle；`/etc/qe-agent.env` 与设备证书不动；
- 升级前自动备份 `/etc/qe-agent.env`、当前二进制；失败可恢复。

## 8. Gitea Actions 兼容性结论

Gitea Actions 设计上兼容 GitHub Actions，但不是“所有 GitHub workflow 都能直接跑”：

- workflow 要放 `.gitea/workflows/`（Gitea 不会自动读 `.github/workflows`）；
- runner 需要 gitea-runner（Docker 推荐），`runs-on` 标签要映射；
- `actions/checkout`、rust-toolchain、pnpm/action-setup、setup-node、buildx 通常可用；
- **GHCR / `GITHUB_TOKEN` 包发布不兼容**：改用 Gitea registry + PAT；
- `permissions` 语义不同，`id-token`/`pages`/`checks` 等 GitHub scopes 不支持；
- package registry 的 job token 不能发布本仓库包，需个人访问令牌；
- Windows/macOS runner、部分 GitHub-only action、problem matchers/annotations 不可用；
- 网络需要能下载 GitHub actions（或配置 `DEFAULT_ACTIONS_URL` 指向镜像）。

对我们仓库：CI 的 test/frontend/integration 三段基本可迁移；image 段（GHCR）必须改。
如果目标是 GitHub/Gitea 双跑，建议把发布 job 做成“只构建产物 + 上传 artifact”，
registry 相关步骤用变量和可跳过条件隔离。

## 9. 需要补的代码（优先级）

1. `scripts/package-release.sh`：收集三件套 + manifest + checksums；
2. bundle 内 `install.sh`：CLI 交互/`--yes`，写 env/注册/启动；
3. agent-client `resources/bin/` 打包与 setup 扩展（纯 AppImage 安装路径）；
4. CI release workflow（GitHub + `.gitea/workflows` 副本）；
5. updater 签名密钥与 `latest.json`；
6. 版本注入：统一 `VERSION`，不再三处手写 0.1.0。
