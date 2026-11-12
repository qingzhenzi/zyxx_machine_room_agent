# 局域网音视频传输 + 远程控制：选型与落地建议

> 环境实测时间：2026-10-30。本机：CachyOS / KDE Plasma 6.7.4 (Wayland) / AMD Radeon 680M (RDNA2) / PipeWire 1.6.8 / GStreamer 1.28.6 / Sunshine 2026.724 / Moonlight 6.1.0 / RustDesk 1.4.9 / OBS 32.2.2。

## 1. 结论

| 目标 | 首选 | 备注 |
|---|---|---|
| 只想「局域网看+控这台机器」 | **Sunshine + Moonlight** | 本机已装、已配对过 Android；延迟 10–30ms 量级，支持键鼠/手柄/音频 |
| 跨网、无人值守、文件传输 | **RustDesk**（可自建 hbbs/hbbr） | 本机已装 1.4.9；延迟高于 Sunshine，但运维功能完整 |
| 浏览器观看 / 多观众 | **OBS + WHIP** 或 **MediaMTX** | 延迟 100–300ms，不适合实时操作 |
| 在 `lan_video` 里自研 | **GStreamer(rs) + VAAPI + RTP/UDP**（LAN）；WebRTC/SRT 作扩展 | 不要从零写协议 |
| 纯音频超低延迟 | `pipewire-roc` / roc、JackTrip；普通场景 Opus 10ms | 本机仓库有 pipewire-roc |

## 2. 现成方案对比

| 方案 | 典型延迟 | 远程控制 | Wayland | 许可 | 适用 |
|---|---|---|---|---|---|
| Sunshine + Moonlight | 10–30ms | 键鼠/手柄 | KDE 用 KWin screencast（本机有权限文件） | GPL | 串流/低延迟桌面 |
| RustDesk | 50–200ms | 完整 | portal 抓屏，新版本支持无人值守 | AGPL | 跨网运维 |
| xrdp (RDP) | 30–80ms | 完整 | 仅 X11 会话 | Apache | 办公 |
| TigerVNC/TurboVNC | 20–60ms | 完整 | 仅 X11 | GPL | 简单可靠 |
| Xpra | 50–150ms | 完整 | 部分 | GPL | 单应用/低带宽 |
| OBS + WebRTC/WHIP | 100–300ms | 无 | 任意 | GPL | 直播/浏览器 |
| GStreamer 自建 | 20–50ms | 需自研 | portal/KWin | LGPL | 定制产品 |
| FFmpeg + SRT | 50–150ms | 无 | 任意 | LGPL | 弱网点对点 |

## 3. 本机现状（重要）

- Sunshine 的 `~/.config/sunshine/sunshine.conf` 里 `keyboard/mouse/controller = disabled`，所以之前只「看画面」、不能控制；且服务当前是 inactive。
- Sunshine 已具备 `cap_sys_admin`（KMS 抓屏）、随包安装的 KWin 权限 desktop 文件、`/dev/uinput` 的 uaccess ACL 和 udev 规则。
- `ufw` 处于 active，默认会拦 Sunshine/RustDesk 端口。
- KDE portal 已实现 `ScreenCast` / `RemoteDesktop` / `InputCapture`，libei 1.6.0 已装 → 自研 Wayland 输入注入可行。

## 4. 立刻可用：Sunshine + Moonlight

```bash
# 1) 打开键鼠控制（原文件里是 disabled）
sed -i 's/^keyboard = disabled/keyboard = enabled/; s/^mouse = disabled/mouse = enabled/; s/^controller = disabled/controller = enabled/' ~/.config/sunshine/sunshine.conf

# 2) 放行端口（Sunshine 默认端口）
sudo ufw allow 47984:47990/tcp
sudo ufw allow 47998:48010/udp

# 3) 启动（包自带 user unit；或前台运行 sunshine）
systemctl --user enable --now app-dev.lizardbyte.app.Sunshine.service

# 4) 客户端
moonlight pair <host-ip>      # 或打开 Moonlight Qt 图形界面配对
moonlight stream <host-ip> Desktop
```

Web 管理页 `https://<host-ip>:47990`（本机已有账号 `sunshine`）。KDE Wayland 首次抓屏可能弹 KWin 授权；若权限文件未被识别，可临时 `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1 sunshine` 验证。

编码建议：1080p60 用 HEVC 20–30Mbps；680M 上 4K 建议 30fps HEVC（4K60 H.264 实测不足 20fps）。

## 5. 自研架构（lan_video）

```
采集  pipewiresrc(portal/KWin) 或 x11rb(XShm)
  → 编码  vah264enc/vah265enc（DMABUF 零拷贝；x264enc tune=zerolatency 兜底）
  → 封装  rtph264pay / rtpopuspay
  → 传输  RTP/UDP（LAN）| SRT（WiFi/弱网）| WebRTC（浏览器/多端）
  → 解包  rtpjitterbuffer(latency=10~30) → 解码 vah264dec → waylandsink/wgpu
旁路：mDNS 发现 · TCP/QUIC 控制通道 · 输入回注（libei/uinput/XTest）· 剪贴板
```

### 5.1 GStreamer 命令行原型（本机已验证元素与 loopback 收包）

```bash
# 发送端：node-id 来自 portal/KWin screencast
gst-launch-1.0 -v pipewiresrc path=<node-id> ! video/x-raw,format=NV12 \
  ! vah264enc rate-control=cbr bitrate=12000 key-int-max=60 \
  ! h264parse config-interval=1 ! rtph264pay pt=96 \
  ! udpsink host=<peer-ip> port=5000 sync=false

# 接收端
gst-launch-1.0 udpsrc port=5000 \
  caps="application/x-rtp,media=video,encoding-name=H264,payload=96,clock-rate=90000" \
  ! rtpjitterbuffer latency=20 ! rtph264depay ! vah264dec \
  ! autovideosink sync=false

# 音频（Opus 10ms；要极限低延迟可用 rtpL16pay 走 PCM，约 1.5Mbps）
pulsesrc device=<monitor> ! audio/x-raw,rate=48000,channels=2 \
  ! opusenc frame-size=10 bitrate=128000 ! rtpopuspay pt=97 \
  ! udpsink host=<peer-ip> port=5002 sync=false

# 弱网改用 SRT（本机已装 srt 插件）
gst-launch-1.0 ... ! h264parse ! mpegtsmux \
  ! srtclientsink uri="srt://<peer-ip>:9000?mode=caller&latency=20"
```

`webrtcsink` 需安装 `gst-plugin-rswebrtc`（本机未装）；浏览器端也可上 MediaMTX。

### 5.2 Rust crate 方向（版本与成熟度见 `docs/research/rust-stack.md`）

- 采集：`pipewire`、`ashpd`（portal）、`x11rb`、`wayland-client`
- 编码/封装：`gstreamer` + `gstreamer-video` + `gstreamer-rtp`（首选，VAAPI/NVENC/QSV 全走插件）；`ffmpeg-next` 备选
- 传输：`tokio` + `quinn`（QUIC DATAGRAM）或 `str0m`/`webrtc`；发现 `mdns-sd`
- 控制：`serde`/`bincode` 控制帧；`reis`(libei) / `evdev` / `x11rb`(XTEST)；`wl-clipboard-rs`
- 渲染：GStreamer sink 或 `wgpu` + DMA-BUF

## 6. 性能预算（1080p60，LAN）

采集 3–10ms → 编码 3–8ms（硬编）/ 10–20ms（x264 veryfast）→ 网络 1–3ms → 抖动缓冲 10–20ms → 解码 2–8ms → 呈现 8–16ms，玻璃到玻璃约 **30–70ms**。

码率（CBR）：1080p60 桌面 8–15Mbps、游戏 15–25Mbps；4K60 25–50Mbps；HEVC 省约 30%，AV1 省 40–50%（680M 仅 AV1 解码，无 AV1 硬编）。

本机空载合成画面吞吐（`gst-launch`，含系统内存拷贝，仅表示管线上限）：

| 分辨率/编码 | 实测 |
|---|---|
| 1080p60 H.264 VAAPI | ~67fps |
| 1080p60 H.264 x264 veryfast | ~91fps |
| 1440p H.264 VAAPI | ~42fps |
| 4K30 H.265 VAAPI | ~36fps |
| 4K30 H.264 VAAPI | ~20fps |

## 7. 坑清单

1. **Wayland 采集/注入**：portal 会弹授权，需 `restore_token` 持久化；KWin 走 `zkde_screencast_unstable_v1`（Sunshine 方案），无头/多屏另算。
2. **零拷贝**：portal/KWin 的 DMA-BUF 常是 tiled modifier，VAAPI 导入失败就回退 memcpy，4K 直接崩；先打通 DMABUF 再谈指标。
3. **硬编**：680M 的 AV1 只能解不能编；`vah264enc` 支持 NV12 DMA_DRM 输入，优先 `pipewiresrc ! vah264enc` 不走 `videoconvert`。
4. **防火墙**：`ufw` active，Sunshine/RustDesk 端口不放行就连不上。
5. **安全**：局域网≠可信。Sunshine PIN/证书、RustDesk 自建密钥、WebRTC DTLS；裸 RTP 只应留在隔离网段。
6. **许可**：RustDesk AGPL、Sunshine GPL，参考/复用代码前先定策略。
7. **音频时钟**：PipeWire quantum 与编码器缓冲不匹配会白加 10–20ms；A/V 同步用 RTCP/NTP。

## 8. Wayland 采集实测（2026-10-30，KDE Plasma 6.7.4 / Sunshine 2026.724）

| 运行方式 | capture 配置 | 实测结果 |
|---|---|---|
| 桌面用户 | 不配置（自动） | 回退到 KMS，日志 `Screencasting with KMS` |
| 桌面用户 | `capture = kwin` | ✅ `Found matching system KWin desktop permission file` → PipeWire node → 2560x1440@60，DMA-BUF |
| root（手工注入用户 Wayland/DBus 环境） | `capture = kwin` | ❌ KWin 拒绝暴露 `zkde_screencast_unstable_v1`（按 uid/会话过滤），PipeWire 也不可用 |
| root | 默认 | ❌ KMS monitor 列表为空，`Couldn't find monitor [0]` |
| root 启动、进程 uid=桌面用户 | `capture = kwin` | ✅ KWin 正常授权，PipeWire node + DMA-BUF |

结论：

1. **Sunshine 必须以桌面用户身份运行**，root 抓不了 KWin Wayland 会话；`KWIN_WAYLAND_NO_PERMISSION_CHECKS=1` 只关闭 Sunshine 自己的权限文件逻辑，突破不了 KWin 的跨 uid 过滤。
2. **强制走 KWin screencast 的正确写法**是 `~/.config/sunshine/sunshine.conf` 里加 `capture = kwin`（可选值还有 `kms` / `wlr` / `x11`）。
3. 需要在 root 体系里管理时，用 **systemd system 服务 + `User=<桌面用户>`**（实测可行），或 systemd `--user` 服务 + `graphical-session.target`；root 只做编排。
4. 纯 headless：需要虚拟显示（KDE 可用 `krfb-virtualmonitor` 造虚拟屏 + `capture=kwin`；或 Xorg dummy + `capture=x11`；或 wlroots 合成器 + `capture=wlr`）。

```ini
# ~/.config/sunshine/sunshine.conf
capture = kwin
keyboard = enabled
mouse = enabled
controller = enabled
```

```ini
# /etc/systemd/system/sunshine-desktop.service（root 管理、用户身份运行）
[Unit]
Description=Sunshine (KDE Wayland, desktop user)
After=display-manager.service
[Service]
User=qingzhenzi
Environment=XDG_RUNTIME_DIR=/run/user/1000
Environment=WAYLAND_DISPLAY=wayland-0
Environment=DISPLAY=:0
Environment=XDG_SESSION_TYPE=wayland
Environment=DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
ExecStart=/usr/bin/sunshine
Restart=on-failure
[Install]
WantedBy=multi-user.target
```
