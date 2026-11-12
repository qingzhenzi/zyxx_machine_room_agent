# 发布与签名

## 版本

版本号必须同时存在于：

- `package.json`
- `src-tauri/tauri.conf.json`
- `src-tauri/Cargo.toml`

运行：

```sh
pnpm version:check
```

## 本地构建

```sh
pnpm check
pnpm check:rust                        # cargo fmt --check + clippy + test
pnpm tauri build --debug --no-bundle   # 快速验证
pnpm tauri build                       # 当前平台安装包
```

Linux 需要 WebKitGTK 4.1、librsvg、libayatana-appindicator 等系统依赖，CI 的
`.gitea/workflows/release.yml` 已列出。

本机已验证：

```text
QEStudio_0.1.0_amd64.deb (3.9 MB)
```

## 自动化产物

打 `v*` tag 或手动触发 `Client Release` 工作流：

- 构建 Linux `.deb`
- 上传 `qestudio-linux-deb` artifact

Windows / macOS 安装包需要各自 runner 和签名证书；`release.yml` 提供手动触发入口，当前 Linux `.deb` 已验证。

## 代码签名

当前产物**未签名**，只能用于内部测试。正式发布需要：

| 平台 | 需要 |
| --- | --- |
| Windows | Authenticode 证书 + `signtool` |
| macOS | Developer ID + notarization |
| Linux | 可选：仓库签名 / AppImage 签名 |

Tauri 构建可通过环境变量注入签名配置；不要把证书或私钥提交到仓库。

## 自动更新

已接入 `tauri-plugin-updater` + `tauri-plugin-process`，当前是**受控启用**状态：

- 公钥已写入 `src-tauri/tauri.conf.json` 的 `plugins.updater.pubkey`
- endpoint 为占位地址 `https://updates.qestudio.example/latest.json`，需要替换为真实更新源
- 前端按钮受 `VITE_UPDATER_ENABLED=true` 控制；未开启时点击会提示“更新源未配置”
- 开发密钥：`~/.config/qestudio/updater.key`；开发密码文件：`~/.config/qestudio/updater.password`（仅本机，**不要提交**）；生产前应轮换

### 生成签名构建

基础 `src-tauri/tauri.conf.json` **有意不开启** `createUpdaterArtifacts`：普通 `pnpm tauri build` 不需要签名 key，未设置 `TAURI_SIGNING_PRIVATE_KEY` 时也不会失败；只有下面的签名构建才通过 `--config` 覆盖开启。

```sh
TAURI_SIGNING_PRIVATE_KEY="$HOME/.config/qestudio/updater.key" \
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="<你的密码>" \
pnpm tauri build --bundles deb \
  --config '{"bundle":{"createUpdaterArtifacts":true}}'
```

### 更新清单

把 `.sig` 与安装包上传到更新源，然后生成 `latest.json`：

```sh
pnpm manifest:update \
  --version 0.2.0 \
  --url https://updates.example.com/QEStudio_0.2.0_amd64.deb \
  --signature-file src-tauri/target/release/bundle/deb/QEStudio_0.2.0_amd64.deb.sig \
  --notes "修复与改进" \
  --output latest.json
```

脚本会生成 Tauri updater 需要的 `version / notes / pub_date / platforms` 结构。

构建启用更新检查的客户端：

```sh
VITE_UPDATER_ENABLED=true pnpm tauri build
```

> 未配置真实 endpoint、签名产物和 `latest.json` 前，不要把 `VITE_UPDATER_ENABLED` 打开给生产用户。

## 本地假更新服务器

完整说明见 [`tools/update-server/README.md`](../tools/update-server/README.md)。

```sh
pnpm update:certs
pnpm update:serve
pnpm test:update-server
```

流程：

1. 用开发 key 构建带 updater 签名的 `.deb`
2. `pnpm update:publish` 写入 `latest.json` + `downloads/`
3. 用 `src-tauri/tauri.update-http.conf.json` 构建指向本地服务器的测试客户端
4. 打开设置 → 关于 → 检查更新

HTTPS 模式使用自签证书，启动时信任
`tools/update-server/certs/server.crt`；证书和私钥都在 `.gitignore` 中。

## macOS / Gitea

macOS 构建和自签选项见 [`docs/MACOS_GITEA.md`](MACOS_GITEA.md)。Gitea runner
label 由 workflow_dispatch 的 `runner` 输入配置，支持托管 `macos-14` 或自托管
`macos-arm64`。

## 发布检查清单

- [ ] `pnpm version:check`
- [ ] `pnpm check`
- [ ] `pnpm check:rust`
- [ ] `pnpm test:e2e`
- [ ] `pnpm tauri build --debug --no-bundle`
- [ ] 原生启动并登录
- [ ] 检查 Mock API 数据清理（清除 localStorage）
- [ ] 打 tag，确认 release artifact
- [ ] 记录已知限制（运行轮询、Mock 数据、未签名）
