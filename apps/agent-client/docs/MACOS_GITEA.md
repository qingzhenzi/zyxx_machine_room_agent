# macOS 构建与 Gitea Actions

`.gitea/workflows/release-macos.yml` 是手动触发的 macOS 发布工作流。它不是只绑定
GitHub；Gitea Actions 只要能提供 macOS runner 即可使用。

## Runner 配置

工作流用 `workflow_dispatch` 输入 `runner` 指定 runner label：

- GitHub 托管：`macos-14`
- Gitea 自托管：在 runner 上打 `macos-arm64` 等标签，触发时填入
- Intel 机器：`macos-13` 或自托管 x86_64 label

`runs-on` 直接使用该输入，所以不需要改 YAML 就能切换机器。

## 架构

`arch` 输入：

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`

Tauri updater 的平台键分别对应 `darwin-aarch64` 和 `darwin-x86_64`。

## Secrets

在 Gitea 仓库 Settings → Actions → Secrets 配置：

| Secret | 用途 |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | updater minisign 私钥内容 |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 私钥密码 |
| `APPLE_CERTIFICATE` | Developer ID `.p12` 的 base64 |
| `APPLE_CERTIFICATE_PASSWORD` | `.p12` 密码 |
| `APPLE_SIGNING_IDENTITY` | 例如 `Developer ID Application: ...` |
| `APPLE_ID` / `APPLE_PASSWORD` / `APPLE_TEAM_ID` | notarization |

不要把证书、私钥或密码提交到仓库。

## 自签选项

工作流输入 `self_signed=true` 时，会用
[`scripts/macos-selfsigned.sh`](../scripts/macos-selfsigned.sh) 给 `.app` 做本地
自签名。这个签名：

- 不经过 Apple notarization
- Gatekeeper 默认不信任
- 只适合本机/内部测试
- 需要时把生成的证书导入钥匙串并显式信任

正式分发仍需要 Developer ID + notarization。

## 产物

构建后上传：

- `.dmg`
- `.app.tar.gz`
- `.app.tar.gz.sig`

生成清单：

```sh
pnpm manifest:update \
  --version 0.1.1 \
  --url https://updates.example.com/QEStudio.app.tar.gz \
  --signature-file dist/macos/QEStudio.app.tar.gz.sig \
  --platform darwin-aarch64 \
  --output latest.json
```

再把 `latest.json` 和 `.app.tar.gz` 上传更新服务器。Gitea 的 artifact 上传只负责
构建产物；发布到 Release/更新服务器可以由 Gitea API、对象存储或部署 job 完成。

## 自签名更新服务器

Linux 本地可用 `src-tauri/tauri.update-http.conf.json` 指向
`http://127.0.0.1:8787/latest.json`；macOS 推荐用 HTTPS 配置并信任开发证书：

```sh
SSL_CERT_FILE="$PWD/tools/update-server/certs/server.crt" \
  open -a QEStudio
```

针对 `tauri-plugin-updater` 的 TLS 实现，若系统 trust store 不生效，建议使用
`security add-trusted-cert` 把 `tools/update-server/certs/server.crt` 加到
System keychain，再运行 HTTPS 更新测试。
