# Debian KDE 测试机接入记录

## 机器信息

| 项 | 值 |
|---|---|
| 系统 | Debian GNU/Linux 13 (trixie), kernel 6.12.113+deb13-amd64 |
| 桌面 | KDE Plasma 6；**当前为 Plasma X11**（控制场景；Wayland KWin 6.3 screencast 实测不出帧） |
| CPU/GPU | Intel Alder Lake，UHD Graphics 770，`/dev/dri/renderD128` |
| 音频 | PipeWire 1.4.2；默认 sink 是 `auto_null`（无物理输出） |
| 网络 | `192.168.8.115/24`，到开发机 RTT ≈ 1ms |
| 登录 | `dev` 用户，sudo 可提权 |

## 已做的配置

- 通过 SSH（`dev@192.168.8.115`）接入；
- 安装了 GStreamer 运行/工具链：
  `gstreamer1.0-tools`、`gstreamer1.0-vaapi`、`gstreamer1.0-pipewire`
  （base/good/bad/ugly/libav 之前已有）；
- 部署二进制到 `/home/dev/lan_video`（release，约 4.9MB）；
- 未安装 Rust/开发头（`rustc`、`wayland-scanner` 都没有），所以 Debian 侧目前只做**接收端**。

## 推送方式（本地编译 → scp）

```bash
# 本机（开发机）：
./scripts/deploy-debian.sh dev@192.168.8.115

# 它只做三件事：本机 cargo build --release、scp 二进制、远程 --version 验证。
# 不需要在 Debian 上装 Rust/开发头。
```

Debian 只需要运行时的 GStreamer（一次性）：

```bash
# 在 Debian 上执行（需要 sudo）
./scripts/debian-setup.sh
```

> 二进制本身不携带 GStreamer 运行时/插件，所以 Debian 侧必须有 `gst-launch-1.0` 和
> `rtph264depay/opusdec/decodebin` 等插件；这些已用 `scripts/debian-setup.sh` 装好。

## 接收端用法

```bash
export XDG_RUNTIME_DIR=/run/user/1001
export WAYLAND_DISPLAY=wayland-0
export PULSE_SERVER=unix:/run/user/1001/pulse/native

# 视频（Wayland 窗口）
/home/dev/lan_video receive --port 5000 --sink waylandsink
# 或带音频
/home/dev/lan_video receive --port 5000 --audio-port 5002 \
  --sink waylandsink --audio-sink autoaudiosink
```

> `receive` 会自动给 `waylandsink` 补 `videoconvert`；`autovideosink` / `glimagesink` 直接吃 NV12。

## 已验证

- 发送端（开发机 KWin 采集，1280x720@30）→ Debian：
  - `gst-launch` 在 Debian 上解出 `video/x-h264`（1280x720）；
  - `autovideosink` 与 `waylandsink` 都能出画面；远程截图确认窗口里是开发机桌面；
  - 音频 Opus 管线正常启动（但 Debian 默认 sink 是 `auto_null`，听不到声音）。
- 网络 RTT ≈ 1ms，UDP 直连无防火墙拦截。

## 持久全屏显示（当前已开启）

本机发送端（systemd --user）：

```bash
systemctl --user status lan-video-sender.service
# 停止：systemctl --user stop lan-video-sender.service
```

- 服务：`~/.config/systemd/user/lan-video-sender.service`
- 内容：本机 KWin 采集 1920x1080@30、20Mbps、带系统声音，目标 `192.168.8.115`
- `Restart=always`，登录后随 graphical-session 自启

Debian 接收端（systemd --user）：

```bash
# 在 Debian 上
~/bin/lan-video-display status
~/bin/lan-video-display stop     # 关闭投屏
~/bin/lan-video-display start    # 重新打开
```

- 服务：`~/.config/systemd/user/lan-video-display.service`
- 当前显示 sink：`autovideosink`（X11 窗口；不再用 waylandsink）
- 桌面快捷方式：`~/桌面/关闭投屏 (lan_video).desktop`、`打开投屏 (lan_video).desktop`
- 应用菜单里也有同样的两个条目
- `Restart=on-failure`；手动 stop 后不会自动重开

**关闭联动**（已实现并验证）：

- 本机额外跑一个反向 SSH 隧道服务 `lan-video-tunnel.service`：
  `ssh -N -R 127.0.0.1:38099:127.0.0.1:38099 dev@192.168.8.115`
- 发送端 `run` 带 `--manage-port 38099`，接受 `STOP` / `START`；
- Debian 的 `~/bin/lan-video-display stop` = 停接收端 + 发 `STOP`（发送端销毁会话，但 agent 进程保留）；
- `start` = 启接收端 + 发 `START`（发送端新建会话，重新出画面）；
- 已验证：stop 后本机 gst/helper 消失、Debian 全屏消失；start 后两端进程恢复、画面恢复。

> 隧道断开时，stop 只关接收端，发送端会继续发流；恢复隧道后再 stop/start 即可。
> 本机发送端彻底停止：`systemctl --user stop lan-video-sender.service`。

## 远程控制（已跨机验证）

- 用 `scripts/debian-uinput.sh` 给 dev 用户开 `/dev/uinput`（已执行，`/dev/uinput` 对 dev 可写）；
- 本机作为 viewer 连接 Debian agent 的 control WebSocket：
  `ws://192.168.8.115:38086/v1/sessions/<id>/control?token=<control_token>`；
- Debian 上成功创建 `lan-video virtual input` 虚拟键鼠设备；
- 单向剪贴板写入 Debian Klipper 成功（`getClipboardContents` 返回测试文本）；
- 文件上传/下载接口已实现（本机 128KB 往返验证），Debian 走同一 HTTP 接口。

## 限制 / 下一步

1. **Debian 侧 VAAPI 编码不可用**：`gst-inspect-1.0 vah264enc` 不存在（`vah264dec` 可用）。
   要把 Debian 当发送端，需要装 Intel VAAPI 驱动（`intel-media-va-driver` / `vainfo`）并验证，
   或者用 `x264enc` 软编。
2. **Debian 没有 Rust/开发头**：要跑本项目的 KWin 采集，需要装 Rust 工具链、
   `wayland-client`/`wayland-scanner` 开发包；或者交叉编译。
3. **音频输出**：先接物理声卡/HDMI，或把 PipeWire default sink 指到可用设备。
4. **延迟**：目前只验证了链路；玻璃到玻璃仍需相机法（一块屏显示毫秒时钟，另一块显示流）。
5. **A/V 同步**：`--sync` 管线已通，但 Debian 侧还没实测音画偏差。
