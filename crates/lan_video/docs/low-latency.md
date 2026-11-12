# 低延迟视频传输：选型与当前实现

## 1. 选型结论

**RTP over UDP + GStreamer 硬件编码 + 最小抖动缓冲**，是局域网、原生客户端下延迟最低的路线（20–60ms）。

| 方案 | 延迟 | 结论 |
|---|---|---|
| **RTP/UDP**（本实现） | 20–60ms | ✅ 默认，媒体面只做这一条 |
| WebRTC | 50–150ms | 后续兼容层（浏览器/Tauri 观看） |
| SRT | 50–150ms | 弱网/WiFi 备选，不进默认路径 |
| QUIC DATAGRAM | — | 控制面可以，媒体暂不用 |
| KCP | — | ❌ 视频不用；可靠重传会拖垮实时性 |
| TCP/RTMP | 高 | ❌ 队头阻塞 |

多流模型：一路会话 = 一个独立 `gst-launch-1.0` 进程 = 一条 RTP 流（后续扩展为多 track/SSRC）；每路会话独立 pid、独立日志、可被管理 API 强杀。

## 2. 快速开始

```bash
cargo build

# 1) 探测能力（编码器/采集源/网卡）
./target/debug/lan_video capabilities

# 2) 看管线（不启动）
./target/debug/lan_video pipeline --target 192.168.1.20 --port 5000 \
  --capture test --encoder vah264 --width 1920 --height 1080 --fps 60 --bitrate 12000

# 3) 前台跑一路：KDE Wayland 免授权采集 + 系统声音（Ctrl-C 结束）
./target/debug/lan_video run --target 192.168.1.20 --port 5000 --audio-port 5002 \
  --capture kwin --encoder vah264 --width 1920 --height 1080 --fps 60 --bitrate 12000

# 4) 接收端（另一台机器 / 另一终端）
./target/debug/lan_video receive --port 5000 --audio-port 5002 --sink autovideosink

# 5) 网络基线（两台机器）
#    B: ./target/debug/lan_video probe --listen --bind 0.0.0.0:40000
#    A: ./target/debug/lan_video probe --peer <B-IP>:40000 --count 500 --interval-ms 5
```

`capture` 取值：

| 值 | 说明 |
|---|---|
| `test` | 合成画面，用来验证链路 |
| `kwin` | **KDE KWin screencast，免授权弹窗**；`--kwin-window "{uuid}"` 抓单窗口，不传抓整屏 |
| `pipewire` | 已有 PipeWire node：`--pipewire-path <id>` 或 `--pipewire-fd <fd>`（portal） |
| `x11` | X11 会话；`--x11-window <xid>` 抓单窗口（Wayland 下只抓 XWayland） |
| `wgc` / `dxgi` | Windows 发送端；`wgc` 支持 `--window-handle <hwnd>`（实验，见 [windows.md](windows.md)） |

> Windows 作为**接收端**完全可用：可直接用 GStreamer 收，或编译本项目跑 `lan_video.exe receive`；
> 详见 [docs/windows.md](windows.md)。

窗口采集（KDE）：

```bash
# 列出窗口，拿 uuid
./target/debug/lan_video windows --json

# 只抓某个窗口
./target/debug/lan_video run --target <peer> --port 5000 \
  --capture kwin --kwin-window "{f2e3f374-0647-4669-ab7d-653ad15d3234}" ...

# X11 窗口：windows 命令在非 KDE 会话会回退到 xdotool
./target/debug/lan_video run --target <peer> --port 5000 \
  --capture x11 --x11-window 0x15f0c16 ...

# Windows 窗口（实验）：PowerShell 拿 MainWindowHandle
# Get-Process | ? {$_.MainWindowTitle} | Select MainWindowHandle,MainWindowTitle
lan_video.exe run --target <peer> --port 5000 \
  --capture wgc --window-handle 0x00000000000A1B2C ...
```

音频通道（Linux/Windows 统一语义）：

```bash
# 系统声音（默认）
--audio-port 5002 --audio-source system
# 麦克风
--audio-port 5002 --audio-source mic
# 指定设备（优先于 audio-source）
./target/debug/lan_video devices      # 列设备 id
--audio-port 5002 --audio-device alsa_input.xxx
```

### KWin 采集的权限机制

KWin 只对满足条件的客户端暴露 `zkde_screencast_unstable_v1`：客户端的可执行文件必须有 desktop 文件声明
`X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1`。`lan-video` 首次启动 `kwin-capture` 时会自动生成：

```ini
# ~/.local/share/applications/lan-video-kwin.desktop
[Desktop Entry]
Type=Application
Name=lan-video KWin screencast permission
Exec=/绝对路径/lan_video
X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1
NoDisplay=true
```

并执行 `kbuildsycoca6 --noincremental` 刷新 KDE service 缓存。之后 KWin 静默放行，无 portal 弹窗。
注意：**不要用 root 跑采集**；root 客户端的 uid/会话不同，KWin 不会授权。

## 3. 控制 API

```bash
./target/debug/lan_video serve --bind 127.0.0.1:38080
# 可选：--token <token>，之后请求带 Authorization: Bearer <token>
```

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/v1/health` | 健康检查 + 会话数 |
| GET | `/v1/capabilities` | 编码器/采集源/IP |
| GET | `/v1/audio-devices` | 音频设备（system/mic + id） |
| GET | `/v1/windows` | 可采集窗口（KDE，含 uuid/title/app） |
| GET | `/v1/sessions` | 会话列表 + 状态 + pid |
| POST | `/v1/sessions` | 创建会话（body 为管线配置） |
| DELETE | `/v1/sessions/{id}` | **强制终止**（helper + gst 一起 SIGINT→SIGKILL） |
| GET | `/v1/sessions/{id}/control` | 控制 WebSocket（需 `control_token`） |
| POST | `/v1/sessions/{id}/files` | 上传文件（需 token） |
| GET | `/v1/sessions/{id}/files` | 文件列表（需 token） |
| GET | `/v1/sessions/{id}/files/{name}` | 下载文件（需 token） |

```bash
# 创建 KWin 采集会话
curl -s -X POST http://127.0.0.1:38080/v1/sessions \
  -H 'Content-Type: application/json' \
  -d '{"target":"192.168.1.20","port":5000,
       "capture":{"type":"kwin","window":"{f2e3f374-0647-4669-ab7d-653ad15d3234}"},
       "audio_port":5002,"audio_source":"mic",
       "encoder":"vah264","width":1920,"height":1080,"fps":60,"bitrate_kbps":12000}'

# 列表 / 强杀
curl -s http://127.0.0.1:38080/v1/sessions
curl -s -X DELETE http://127.0.0.1:38080/v1/sessions/<id>
```

## 4. 管线与低延迟参数

KWin 采集（实际生成，2560x1440源缩到1080p）：

```
pipewiresrc path=<kwin-node>
  ! video/x-raw                        # pipewiresrc 必须显式 caps
  ! vapostproc                         # GPU 缩放 + 上传
  ! video/x-raw,width=1920,height=1080,format=NV12
  ! queue max-size-buffers=1 leaky=downstream
  ! vah264enc rate-control=cbr bitrate=12000 key-int-max=120
  ! h264parse config-interval=1
  ! rtph264pay pt=96
  ! udpsink host=<target> port=5000 sync=false
```

测试源（无采集授权，用于链路验证）：

```
videotestsrc is-live=true pattern=ball
  ! video/x-raw,width=1920,height=1080,framerate=60/1
  ! queue max-size-buffers=1 leaky=downstream
  ! vah264enc rate-control=cbr bitrate=12000 key-int-max=120
  ! h264parse config-interval=1 ! rtph264pay pt=96
  ! udpsink host=<target> port=5000 sync=false
```

接收端：

```
udpsrc port=5000 caps="application/x-rtp,media=video,encoding-name=H264,payload=96,clock-rate=90000"
  ! rtpjitterbuffer latency=0
  ! rtph264depay
  ! decodebin
  ! autovideosink sync=false
```

约定：
- 无 B 帧、CBR、关键帧间隔 2s；
- `queue` 只留 1 帧且 `leaky=downstream`，丢旧帧保新帧；
- `rtpjitterbuffer latency=0`、`sync=false`；
- 采集到编码尽量 GPU 内完成；KWin 源是 BGRx，`vapostproc` 做缩放/转 NV12；
- 音频与视频分开 RTP 端口（Opus 10ms，见第 7 节）。

## 5. 代码结构

| 文件 | 职责 |
|---|---|
| `src/pipeline.rs` | 采集源/编码器选择、管线字符串构造、低延迟默认值、单元测试 |
| `src/session.rs` | 每会话一个 `gst-launch-1.0` 子进程；KWin 时再加一个 helper；强杀整组 |
| `src/api.rs` | axum 控制面；CORS；可选 Bearer token |
| `src/lib.rs` | 环境探测（gst 元素/IP）、管线 token 化、能力清单 |
| `src/kwin.rs` | KWin 权限 desktop 文件 + C helper 的 Rust 封装 |
| `src/kwin_capture.c` | `zkde_screencast_unstable_v1` 客户端：创建流、输出 PipeWire node、保持连接 |
| `src/probe.rs` | UDP RTT/抖动/丢包探测（`probe --listen/--peer`） |
| `src/devices.rs` | 音频设备枚举（Linux `pactl -f json`，Windows `gst-device-monitor`） |
| `build.rs` | `wayland-scanner` 生成协议代码 + `cc` 编译 C helper |
| `protocols/zkde-screencast-unstable-v1.xml` | KWin screencast 协议（LGPL-2.1-or-later） |
| `src/main.rs` | CLI：capabilities / pipeline / serve / run / receive / 隐藏的 kwin-capture |

构建依赖：`wayland-scanner`、`wayland-client` 开发头（`pkg-config`）、`cc`、`pkg-config`。

## 6. 已验证（2026-10-31，KDE Plasma 6.7.4 / Sunshine 同款协议）

- `cargo test`：**11 个测试**通过（管线参数、PipeWire 参数校验、音频端口/通道、RTCP 同步管线、X11 xid、接收端零抖动缓冲、token 化、会话启停、probe、KWin 窗口解析）；
- **KWin 免授权采集端到端**：helper 输出 `NODE=79` → 管线 `pipewiresrc path=79 ! video/x-raw ! vapostproc ! 1920x1080 NV12 ! vah264enc` → 接收端解出 `video/x-h264`；
- 本机回环 1080p60：接收端 `rendered ~195→258 帧 / 6s（约 57–74 fps，dropped 0）`（发送+接收同机，含解码开销）；
- **音频端到端**：`--audio-port 5006` 起独立 gst 进程（`pulsesrc @DEFAULT_MONITOR@ ! opusenc ! rtpopuspay`），接收端 3s 录出 537KB WAV，链路无错误；`--audio-source mic` 映射到 `@DEFAULT_SOURCE@` 验证通过；
- **KWin 窗口采集**：`windows` 枚举 UUID → helper `--window "{uuid}"` → `NODE=95` → `pipewiresrc path=95` → 接收端收到 H.264；窗口 UUID 不带 `0_` 前缀；
- **音频设备/窗口 API**：`/v1/audio-devices`、`/v1/windows` 返回 system/mic 与窗口（`backend: kwin|x11`）；
- **A/V 同步管线**：`--sync` 生成 `rtpbin + RTCP` 双流单进程管线，接收端 `ntp-sync=true` 运行 5s 无错误；
- **X11 单窗口**：`ximagesrc xid=...` + `vapostproc` 缩放/转 NV12，端到端接收 H.264 正常；
- **probe loopback**：100 包 0 丢包，p50 ≈ 0.08ms；网络 RTT 工具可用；
- 强杀：`DELETE /v1/sessions/{id}` 后 helper、gst、socket 全部清理，无残留；
- 能力探测：`vah264enc/vah265enc/x264enc`、`pipewiresrc/ximagesrc`、VAAPI 可用；
- 踩过并修掉的坑：
  - `gst-launch` 不接受“整条管线作为单个参数”，必须 token 化；
  - `pipewiresrc` **没有默认 caps**，不显式给 `video/x-raw` 会协商失败；
  - `vah264dec` 输出 VAAPI 内存，普通 sink 协商失败，测试链路用 `decodebin`；
  - KWin 给的是 BGRx，需要 `vapostproc` 转 NV12 才能进 VAAPI 编码器。

## 7. 音频

```bash
# 发送：默认采系统声音（@DEFAULT_MONITOR@），Opus 10ms / 128kbps
./target/debug/lan_video run --target <peer> --port 5000 --audio-port 5002 --capture kwin ...

# 接收
./target/debug/lan_video receive --port 5000 --audio-port 5002 \
  --sink autovideosink --audio-sink autoaudiosink
```

- `--audio-source system|mic`：Linux 映射到 `@DEFAULT_MONITOR@` / `@DEFAULT_SOURCE@`；Windows 映射到 `wasapisrc loopback=true` / `wasapisrc`；
- `--audio-device <id>` 优先于 `--audio-source`，`lan_video devices`（或 `GET /v1/audio-devices`）列出 id；
- 管线：`pulsesrc device="..." ! audio/x-raw,rate=48000,channels=2 ! queue leaky ! opusenc frame-size=10 ! rtpopuspay pt=97 ! udpsink`；
- 音频是独立 gst 进程、独立 RTP 端口，会话强杀时一起结束；
- **A/V 同步（实验）**：加 `--sync` 后音视频合进一个 gst 进程，用 `rtpbin` + RTCP SR/NTP 对齐；
  接收端也要 `--sync --audio-port`，管线里 `ntp-sync=true` + sink `sync=true`。
  建议用相机法实测确认（不同 gst 版本/采集源可能有偏差）。

> 双机（开发机 KWin → Debian KDE Wayland）接入与验证记录见 [debian-test.md](debian-test.md)。

## 8. 延迟测量（两台机器）

### 8.1 网络基线

```bash
# B 机
./target/debug/lan_video probe --listen --bind 0.0.0.0:40000
# A 机
./target/debug/lan_video probe --peer <B-IP>:40000 --count 500 --interval-ms 5 [--json]
```

输出 RTT `min/mean/p50/p95/max`、jitter、loss。注意：**这是网络 RTT，不是玻璃到玻璃延迟**；本机 loopback p50 ≈ 0.08ms。

### 8.2 玻璃到玻璃（相机法，唯一可靠）

1. A 机全屏显示毫秒时钟（`time.is/ms`，或 `gst-launch-1.0 videotestsrc pattern=black is-live=true ! timeoverlay font-size=72 ! autovideosink`）；
2. A 机起采集推流到 B，B 机全屏接收；
3. 手机开 240fps 慢动作，同时拍 A、B 两块屏；
4. 同一帧里 `A 时钟 - B 时钟` 即玻璃到玻璃延迟；取 20 次以上报 p50/p95。

软件侧参考预算：采集 1–5ms + 编码 5–15ms + 网络 + 抖动 0–20ms + 解码/呈现 10–25ms。

> 远程控制/鉴权/剪贴板/文件传输的协议与用法见 [control.md](control.md)。

## 9. 下一步

1. **双机实测**：用相机法验证 `--sync` 的音画偏差和玻璃到玻璃 p50/p95。
2. **Windows 窗口枚举**：目前 `--window-handle` 可用，但列表要靠 PowerShell；可补 EnumWindows 集成。
3. **WebRTC 适配层**：同一会话可切换 RTP / WebRTC 出口，供浏览器/Tauri 播放。
4. **多流**：一个会话多 track（屏幕/窗口/音频）、多观众 SFU（MediaMTX/LiveKit）。
5. **会话增强**：token 校验、配置验签、审计、每会话 cgroup/资源限制。
