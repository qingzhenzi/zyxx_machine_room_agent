# lan_video

Linux 局域网低延迟音视频传输 + 远程控制（KDE Wayland 免授权采集 / uinput 注入 / 鉴权 / 单向剪贴板 / 文件传输）。

当前形态：**主机端 agent + CLI + HTTP/WebSocket API**，可被 Tauri/Vue 前端或运维脚本调用；不带 GUI。

## 功能

| 能力 | 状态 |
|---|---|
| **X11 采集 / 单窗口（`ximagesrc xid`）— 推荐主线** | ✅ 稳定 |
| **VNC 采集（QEMU/libvirt 虚拟机画面，无需宿主机桌面）** | ✅ 已实机验证 |
| KDE Wayland 采集（KWin screencast，无 portal 弹窗） | ⚠️ 实验性（Debian KWin 实测 0 帧，见 `docs/wayland-decision.md`） |
| Windows 采集（WGC/DXGI）/ 窗口 `--window-handle` / `windows` 枚举 | 🧪 代码完成，**未实机验证** |
| 视频编码：VAAPI / NVENC / MF / x264 | ✅（本机 VAAPI） |
| 音频：系统声音 / 麦克风，Opus 10ms | ✅ |
| A/V 同步（rtpbin + RTCP SR/NTP） | ✅ 实验特性 |
| 远程键鼠控制（Linux uinput） | ✅ 跨机验证 |
| 会话鉴权（control token + 24h TTL） | ✅ |
| 媒体加密（SRTP AES-128-ICM + HMAC-SHA1，默认开启） | ✅ 抓包无 H.264/Opus 明文 |
| 单向剪贴板（控制端 → 被控端，Klipper） | ✅ |
| 文件上传/列表/下载（token + 大小/类型限制） | ✅ |
| 原生 viewer（GStreamer + SDL2，键鼠/剪贴板/拖拽上传） | ✅ Linux（`--features viewer`） |
| UDP RTP（默认，需控制机放行 UDP 入站） | ✅ |
| RTP-over-TCP（反向 UDP 不可达时 fallback） | ✅ |
| Debian 被控端 agent / uinput / 一键连接 | ✅（当前用 Plasma X11） |
| 会话强杀 / STOP-START 管理 | ✅ |
| Windows 输入注入（SendInput）+ 剪贴板 | 🧪 代码完成，**未实机验证** |

图例：✅ 已实机验证（含跨机）｜🧪 代码完成但未实机验证｜⚠️ 实验性

## 架构

```
观看/控制端                         被控/发送端
  WebSocket (control)  ───────────▶  lan_video agent (serve)
  HTTP (files)         ───────────▶    ├─ SessionManager
  UDP/RTP (video/audio) ◀─────────────┤   ├─ gst-launch: capture→VAAPI→RTP
                                      │   └─ uinput-inject helper
                                      └─ API/控制面
```

> **采集后端**：主线推荐 **X11**（稳定、可控）；Wayland/KWin 实验性（Debian 实测 0 帧，
> 单独立项）。被控端需运行 Plasma X11 会话。详见 `docs/wayland-decision.md`。
>
> ⚠️ **发送端本身就是 Wayland 会话时**：`ximagesrc` 抓到的是 XWayland 的 `DISPLAY=:0`，
> 通常能出画面，但纯 Wayland 合成层（如直接合成上屏的窗口、部分覆盖层）可能缺失或不同步。
> 若发现画面内容不全，请改用 Plasma X11 会话登录，或走 KWin/PipeWire 采集。
> 验证当前会话：`echo $XDG_SESSION_TYPE`（`wayland` 即命中此情形）。

## 构建（Linux）

```bash
# 依赖（Arch 示例）
sudo pacman -S gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad \
  gst-plugins-ugly gst-plugin-pipewire gst-plugin-va pipewire \
  wayland wayland-protocols pkgconf gcc sdl2

# 只做发送/接收/agent（不依赖 SDL2）
cargo build --release

# 需要原生 viewer 时
cargo build --release --features viewer
./target/release/lan_video --help
```

构建依赖：`wayland-scanner`、`wayland-client` 开发头、`cc`、`pkg-config`。
Windows 接收端：装 GStreamer MSVC 运行时即可，不需要 Rust/开发头（详见 `docs/windows.md`）。

## 快速开始

```bash
# 探测
lan_video capabilities
lan_video devices          # 系统声/麦克风
lan_video windows          # KDE 窗口 UUID（或 X11 xid）

# 启动 agent（控制面）
lan_video serve --bind 0.0.0.0:38080

# 创建会话（token / media_secret 在响应里）
curl -X POST http://127.0.0.1:38080/v1/sessions -H 'Content-Type: application/json' \
  -d '{"target":"192.168.8.115","port":5000,"audio_port":5002,
       "capture":{"type":"x11"},"audio_source":"system",
       "encoder":"x264","width":1920,"height":1080,"fps":30,"bitrate_kbps":20000}'

# 接收端（被投屏机器）
lan_video receive --port 5000 --audio-port 5002 --sink waylandsink

# 原生 viewer（看 + 控，Linux）
lan_video view --port 5000 --audio-port 5002 \
  --agent http://192.168.8.115:38080 \
  --session <id> --token <control_token> --media-secret <media_secret>
```

控制：

```
ws://<agent>:38080/v1/sessions/{id}/control?token=<control_token>
```

二进制协议见 `docs/control.md`。

## 采集虚拟机画面（VNC）

无头虚拟机没有窗口可抓，`x11`/`kwin` 都够不到。`capture=vnc` 用 GStreamer 的
`rfbsrc`（本身就是个 VNC 客户端）直连 QEMU 的图形控制台，不依赖宿主机有桌面会话。

```bash
# 拿端口（qe-agent 封装好了；原生等价 virsh vncdisplay <name>）
qe-agent vm vnc debian12          # → 127.0.0.1:5900

# 把虚拟机画面接进 lan_video 会话
lan_video run --target 192.168.8.115 --port 5000 \
  --capture vnc --vnc-host 127.0.0.1 --vnc-port 5900 \
  --encoder vah264 --width 1280 --height 800 --bitrate 4000

# 带密码的（XML 里 graphics passwd='...'）
lan_video run ... --capture vnc --vnc-host 10.0.0.5 --vnc-port 5900 --vnc-password 'xxx'
```

几个要记住的点：

- **VNC 是变化驱动**：画面静止时没有新帧，接收端停在最后一帧。这是预期行为，
  也是它能做到极低带宽的原因——有人操作（鼠标、滚动、打字）时才产生流量。
  因此管线不声明固定帧率，`--fps` 对 VNC 源只影响 `key-int-max`。
- **`--width/--height` 填虚拟机自己的分辨率**。填大了只会加黑边（`add-borders`），
  不会变清晰；填小了会掉细节。
- 链路是 `RGBA → I420 → H264`，比直接看 VNC 多了一次编解码。要最低延迟就用下面的
  直连方式；要**多路分发、录制、统一鉴权、复用控制通道**才值得走 lan_video。
- **`capture=vnc` 只解决"看"**。控制 WebSocket 走的是宿主机 uinput，无头虚拟机没有
  窗口焦点，注入不进去。要"看 + 控"必须走 VNC 协议本身（RFB 自带
  `KeyEvent`/`PointerEvent`），也就是下面这种直连方式。

### 只想知道怎么看到画面

```bash
qe-agent vm vnc debian12      # 打印端口 + 现成的 SSH 隧道命令
vncviewer 127.0.0.1:5900      # 或 Remmina / TigerVNC
```

浏览器端要接进管理前端，需要 noVNC（`RFB` over WebSocket）并在中间放一个
WebSocket→TCP 的代理，本仓库不含这部分。

## 持久投屏（本机 → Debian）

本机（发送端）systemd user 服务：

```ini
# ~/.config/systemd/user/lan-video-sender.service
[Unit]
Description=lan_video sender
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Environment=XDG_RUNTIME_DIR=/run/user/1000
Environment=DISPLAY=:0
Environment=WAYLAND_DISPLAY=
ExecStart=/path/to/lan_video run --target 192.168.8.115 --port 5000 --audio-port 5002 \
  --audio-source system --manage-port 38099 --capture x11 --encoder x264 \
  --width 1920 --height 1080 --fps 30 --bitrate 20000
Restart=always
RestartSec=3

[Install]
WantedBy=graphical-session.target
```

Debian（接收端）全屏服务：

```ini
# ~/.config/systemd/user/lan-video-display.service
[Service]
Environment=XDG_RUNTIME_DIR=/run/user/1001
Environment=WAYLAND_DISPLAY=wayland-0
Environment=DISPLAY=:1
ExecStart=/home/dev/bin/lan-video-receive   # 内部执行 receive --sink "waylandsink fullscreen=true"
Restart=on-failure
WantedBy=graphical-session.target
```

关闭/重开（Debian 桌面有快捷方式，也可命令行）：

```bash
~/bin/lan-video-display stop    # 关接收端 + 通过反向 SSH 隧道给发送端发 STOP
~/bin/lan-video-display start   # 开接收端 + 发 START
```

反向隧道（本机 → Debian，供 STOP/START 从 Debian 到本机 38099）：

```ini
# ~/.config/systemd/user/lan-video-tunnel.service
[Service]
ExecStart=/usr/bin/ssh -N -o ExitOnForwardFailure=yes -R 127.0.0.1:38099:127.0.0.1:38099 dev@192.168.8.115
Restart=always
WantedBy=default.target
```

## API

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/v1/health` | 健康检查 |
| GET | `/v1/capabilities` | 编码器/采集源/IP |
| GET | `/v1/audio-devices` | 音频设备 |
| GET | `/v1/windows` | 窗口（kwin/x11） |
| GET/POST | `/v1/sessions` | 列表/创建 |
| DELETE | `/v1/sessions/{id}` | 强杀 |
| GET | `/v1/sessions/{id}/control` | 控制 WebSocket（token） |
| POST/GET | `/v1/sessions/{id}/files` | 上传/列表（token） |
| GET | `/v1/sessions/{id}/files/{name}` | 下载（token） |

## 延迟

- 控制 WebSocket：本机 p50 ≈ 0.11ms；本机→Debian（LAN）p50 ≈ 0.83ms、p95 ≈ 1.0ms（已开 TCP_NODELAY）。
- 视频：采集+VAAPI 编码+UDP；本机 1080p 实测编码端约 35–40fps（680M 瓶颈），建议 1080p30 / 20Mbps。
- 玻璃到玻璃延迟需相机法实测（`docs/low-latency.md`）。

## 安全

- 控制/文件有 per-session token（24h TTL，强杀后失效）；
- **媒体流默认 SRTP 加密**（AES-128-ICM + HMAC-SHA1-80，密钥每次会话随机生成）；
- **但 SRTP 密钥派生是简化实现**：`srtp_master_key()` 只取 secret 中的十六进制字符、
  截断或补零到 30 字节，不是 KDF。因此**必须用高熵随机 secret**（默认行为即是），
  切勿手填短口令（如 `--media-secret abc123` 会派生出 `abc1230000…`，密钥空间可枚举）。
  严格场景应换成 DTLS-SRTP 握手协商，尚未实现。
- 媒体加密只保护内容机密性/完整性，**不隐藏端口存在性**；面向不可信网络请叠加 VLAN/防火墙/WireGuard；
- 被控端 `/dev/uinput` 权限通过 `scripts/debian-uinput.sh` 配置。

## 文档

- `docs/low-latency.md`：采集/编码/传输、KWin 权限、A/V 同步、延迟测量
- `docs/debian-control.md`：控制其他 Debian（TCP/agent/一键连接）
- `docs/viewer.md`：原生 viewer 用法/快捷键
- `docs/control.md`：控制协议、鉴权、剪贴板、文件
- `docs/debian-test.md`：Debian KDE 接入与持久投屏
- `docs/windows.md`：Windows 接收/发送端
- `docs/research/rust-stack.md`：Rust crate 调研
