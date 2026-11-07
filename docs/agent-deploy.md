# 机器端部署（qe-agent + agent-client）

> 适用：Linux 房间主节点 / 子机（第一版 Linux-first）。涉及两个仓库：
> - `room_agent`（本仓库）：`qe-agent` daemon、boot-display 展示层、systemd 单元、IPC；
> - `agent-client`（`apps/agent-client`，独立 git 仓库）：Tauri 桌面 GUI（默认隐藏/托盘）。

## 1. 组成

```text
qe-agent（daemon，systemd 常驻，root）
  ├─ serve --role both
  │    ├─ node 角色：mTLS 下行 image.pull / agent config / transfer 上报
  │    └─ machine 角色：两档遥测 → mTLS /machine/report；GET /agent/config 拉频率
  └─ --ipc-socket /run/qe-agent/agent.sock  ← GUI 读状态/注册/日志
agent-client（Tauri GUI，登录用户，默认隐藏）
  └─ 托盘 + 本机 IPC（Unix socket 0660，组 qe-agent）
```

## 2. 服务端前置

1. device-pki 运行（4440 注册 / 4441 mTLS），CA 与证书就位；
2. qe-api 运行（管理端/机器总览），已执行迁移 0015–0017；
3. 管理端房间页给该机器签发注册链接（主设备用「注册 token」，子机用「添加子机」）。

## 3. 安装 daemon

```bash
cd /path/to/room_agent
cargo build -p qe-agent --release

# 幂等安装：qe-agent + systemd 单元 + /etc/qe-agent.env（+ 可选 boot-display 展示层）
sudo bash scripts/install-agent.sh
sudoedit /etc/qe-agent.env      # 填 DEVICE_PKI_MTLS_URL / DEVICE_SLOT / DEVICE_DATA_DIR
```

> 展示层：`scripts/install-agent.sh` 会自动检测
> `apps/boot-display/src-tauri/target/release/boot-display`（缺失回退 debug）并安装到
> `/usr/local/bin/boot-display`、写入 `QE_BOOT_DISPLAY_BIN`；没有预构建产物时只提示
> `pnpm --dir apps/boot-display tauri build`，daemon 照常启动（display 阶段 skipped）。

首次注册（一次性；注册后清掉链接）：

```bash
sudo qe-agent cli enroll "<管理端签发的注册链接>" --trust
sudo qe-agent cli status             # 自检：slot/serial/mTLS/证书
sudo systemctl enable --now qe-agent
journalctl -u qe-agent -f
```

验证：`curl --unix-socket /run/qe-agent/agent.sock http://localhost/v1/status`（root 或 qe-agent 组）。
`GET /v1/status` 里 `enrolled=true`，`roles=["node","machine"]`。

## 4. 安装 GUI（agent-client）

```bash
cd /path/to/agent-client
pnpm install --frozen-lockfile
pnpm tauri build --bundles deb   # 产物在 src-tauri/target/release/bundle/deb/
sudo dpkg -i src-tauri/target/release/bundle/deb/*.deb
```

- GUI 默认隐藏：启动只出托盘，不弹窗；关闭窗口=隐藏回托盘，退出走托盘菜单；
- 托盘 `Open Logs` 会唤起窗口并跳到日志页；
- GUI 通过 `QE_AGENT_IPC_SOCKET`（默认 `/run/qe-agent/agent.sock`）访问 daemon；
- 需要把桌面用户加入 `qe-agent` 组并重新登录：

```bash
sudo usermod -aG qe-agent "$USER"
```

- 登录自启：GUI 设置页打开「开机自启」（Tauri autostart），或手动放
  `~/.config/autostart/qe-agent-client.desktop`（`Exec=... --hidden`）。

## 5. 子机（只遥测）

子机不需要 node 角色：

```bash
sudo install -m 0755 target/release/qe-agent /usr/local/bin/
sudo qe-agent cli enroll "<子机注册链接>" --trust
子机注册 token 由管理端房间页「添加子机」或
`POST /api/v1/admin/rooms/{id}/devices/token` 签发，槽位自动为 `{room_code}-m{n}`；
注册后先等 5s 再首启，避免 device-pki 证书状态缓存尚未刷新导致一次握手失败。

```bash
# 子机单元同样用 qe-agent.service，但 ExecStart 改成 --role machine（或用一份 override）：
sudo systemctl edit qe-agent
# [Service]
# ExecStart=
# ExecStart=/usr/local/bin/qe-agent serve --role machine --ipc-socket /run/qe-agent/agent.sock
```

> 一个房间多台子机时，每台都用自己的注册链接（`{room_code}-m{n}` 槽位），
> 都关联到同一房间；管理端「房间详情 → 子机」可查看/新增/解绑。

## 6. 升级与回滚

```bash
# 升级 daemon
sudo systemctl stop qe-agent
sudo install -m 0755 target/release/qe-agent /usr/local/bin/qe-agent
sudo systemctl start qe-agent

# GUI 升级：重新 dpkg -i / 或走 Tauri updater（打包阶段配置 endpoint）
```

- 升级不删除 `/var/lib/qe-agent`（凭证/outbox/staging 保留）；
- 回滚二进制即可；数据库迁移只增不删（0015–0017 向后兼容旧二进制）。

## 7. 排障

| 现象 | 排查 |
| --- | --- |
| GUI 显示"未连接 daemon" | `systemctl status qe-agent`；socket 是否存在/权限；GUI 用户是否在 `qe-agent` 组 |
| `qe-agent cli status` 报凭证失败 | 注册是否完成、`DEVICE_DATA_DIR/creds/<slot>/` 是否存在且 0600 |
| 机器总览没有新快照 | `journalctl -u qe-agent`；`DEVICE_PKI_MTLS_URL` 可达；`REPORT_MODE` 是否 mtls |
| 频率不生效 | 管理端 `GET /admin/agent-configs/effective?device_slot=…`；agent 日志里的"已应用远端遥测间隔" |
| 注册后角色仍用旧证书 | 已知限制：enroll 后需 `systemctl restart qe-agent`（P4 计划热加载） |
| 刚注册就上报报 `HandshakeFailure` | device-pki 的证书状态缓存每 5s 全量刷新一次，新注册证书最多要等 5s 才可通过 mTLS 校验；等 5s 让 agent 重试即可（或 `systemctl restart qe-agent` 后观察重试）。持续失败则检查证书 CN 是否等于 `DEVICE_SLOT`、enrollment 是否 `issued`/`active` |
