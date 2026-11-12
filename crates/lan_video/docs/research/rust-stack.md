# Rust 局域网音视频低延迟传输 — 技术调研（2026-10）

版本号均取自 crates.io index 实测（2026-10-30），成熟度=近 3 个月更新 + 下载量 + API 稳定性综合判断。

## 0. 结论

**推荐主线：PipeWire/portal 采集 → GStreamer(rs) 或 FFmpeg 硬件编码 → str0m/webrtc-rs(SRTP+TWCC) 传输 → GStreamer/wgpu 零拷贝渲染；远控输入走 evdev(uinput) + libei(reis)/XTEST。**
理由：LAN 下 WebRTC 自带抖动缓冲、NACK/PLI、TWCC 拥塞控制，省掉最大一块自研成本；GStreamer 一栈通吃 VAAPI/NVENC/QSV 与 RTP 封装；纯 Rust 自研协议只在你要求 <20ms 且愿写 FEC/BWE 时才值得。

## 1. 采集

| 平台 | 方案 | Rust crate | 判断 |
|---|---|---|---|
| X11 | MIT-SHM（XShmGetImage） | [x11rb](https://crates.io/crates/x11rb) 0.14 | 成熟、纯 Rust、文档差；只能拿 system memory，仍需 memcpy 进编码器 |
| Wayland 通用 | xdg-desktop-portal `ScreenCast` → PipeWire 流 | [ashpd](https://crates.io/crates/ashpd) 0.13.13 + [pipewire](https://crates.io/crates/pipewire) 0.10.1 | **唯一跨合成器方案**；ashpd 支持 screencast/remote_desktop/input_capture feature，pipewire-rs 已暴露 DMA-BUF buffer |
| Wayland wlroots | wlr-screencopy | wayland-client 0.31 + 自写协议绑定 | 仅 Sway/Hyprland，不支持 GNOME/KDE；新代码不建议 |
| Wayland 新协议 | ext-image-copy-capture（staging） | 同上（wayland-protocols 0.32） | KWin 在实现（[KDE #513785](https://bugs.kde.org/show_bug.cgi?id=513785)）、[wlroots MR](https://gitlab.freedesktop.org/wlroots/wlroots/-/merge_requests/5356)，2026 仍属过渡期，别当主路径 |
| 跨平台封装 | 截图/单帧 | [xcap](https://crates.io/crates/xcap) 0.9.8 | 活跃、X11+portal 都有，但定位截图，README 明说录像 WIP；不适合 60fps 直播 |
| 旧方案 | — | scrap 0.5.0（2018 后无更新） | 弃用 |

音频：PipeWire/PulseAudio 走 monitor source；[cpal](https://crates.io/crates/cpal) 0.18.2 只到 ALSA/JACK 层，capture 桌面声需 `pipewire-alsa` 的 monitor 设备或直接用 pipewire crate 建 capture stream。注意 cpal 与 PipeWire 缓冲策略（quantum）不匹配时会额外加 10-20ms。

## 2. 编码

- 软件：x264（crate [x264](https://crates.io/crates/x264) 0.5.0 已停更，建议 FFmpeg/GStreamer 调 libx264）；[openh264](https://crates.io/crates/openh264) 0.9.9 活跃但只有 H.264、无 10bit、无 AV1；SVT-AV1 实时档（preset 12/13、low-delay）无成熟 Rust 绑定，走 FFmpeg；[rav1e](https://crates.io/crates/rav1e) 0.8.1 非实时，别用在 60fps 直播。
- 封装层（推荐）：[gstreamer](https://crates.io/crates/gstreamer) 0.25.4（对应 GStreamer 1.26/1.28，`va`/`nvcodec`/`qsv` 插件齐全，`webrtcbin` 直接出 SRTP）；[ffmpeg-next](https://crates.io/crates/ffmpeg-next) 9.0.0（VAAPI/NVENC/QSV 全支持，hwupload/download 开销需自己控）。
- 直接硬件绑定：[nvidia-video-codec-sdk](https://crates.io/crates/nvidia-video-codec-sdk) 0.4.0（2025-09，能用但小众、unsafe 面大）。Intel QSV 建议走 oneVPL+FFmpeg，Rust 侧无好绑定。
- 低延迟参数（务必全设）：无 B 帧；`tune=zerolatency`/NVENC `-preset p1 -tune ll -rc cbr -delay 0`；CBR+`nal-hrd=cbr`；关键帧间隔 1-2s（30-60 帧）；`rc-lookahead=0`、`sliced-threads=1`；VAAPI `low_power=1 rc_mode=CBR bf=0`。

## 3. 传输

| 方案 | crate | 取舍 |
|---|---|---|
| WebRTC | [webrtc](https://crates.io/crates/webrtc) 0.21.1（0.20→0.21 刚大改，API 不稳定）；[str0m](https://crates.io/crates/str0m) 0.24.1（sans-IO，自己管 socket/定时器，可控性最好） | LAN 内 ICE host candidate 直连、无需 STUN；自带 SRTP/NACK/PLI/jitter buffer。**BWE 需自实现**（str0m 只给 TWCC 反馈，GCC 要自己写） |
| RTP/UDP + 自定义 FEC | [rtp](https://crates.io/crates/rtp)/[rtcp](https://crates.io/crates/rtp) 0.17.2 + `raptorq`/`reed-solomon-erasure` | 延迟最低、开销最小；但丢包恢复、拥塞、A/V 同步全自研 |
| QUIC | [quinn](https://crates.io/crates/quinn) 0.11.12 | 0-RTT、多路复用、加密省事；媒体必须用 DATAGRAM 帧，否则 stream 队头阻塞致命 |
| SRT | srt-rs 0.2.3 / rsrt 0.4.0 / libsrt-sys 1.4.13 | ARQ+FEC、适合单向高码率；C API 绑定，无浏览器/SFU 生态 |
| TCP | — | 不推荐，丢包即队头阻塞，远控卡顿元凶 |

局域网首选：**WebRTC（str0m 或 webrtc-rs）+ 关掉码率探测的固定 CBR**，或 RTP+FEC 的极简自研。发现用 [mdns-sd](https://crates.io/crates/mdns-sd) 0.21.6（纯 Rust、无 async runtime 依赖）；MTU 按 1200B 分片，避免 IP 分片。

## 4. 渲染 / 播放端

GStreamer 管线（`webrtcbin ! rtpvp8depay ! vp8dec ! glimagesink/waylandsink`）最省事；自绘用 [wgpu](https://crates.io/crates/wgpu) 30 + `dma-buf` 导入（`VK_EXT_external_memory_dma_buf`）做零拷贝；[sdl2](https://crates.io/crates/sdl2) 0.38 简单但要走 CPU。零拷贝链路 = portal DMABUF → GBM/EGLImage → VAAPI surface / Vulkan 外部内存，任一环 modifier 不匹配就退回 memcpy（4K60 约 1.5–2 GB/s，CPU 直接吃满）。

## 5. 远程控制

- 输入注入：[evdev](https://crates.io/crates/evdev) 0.13.2 + `/dev/uinput`（需 root 或 udev rule + input 组）；X11 用 x11rb XTEST；**Wayland 必须 libei**：[reis](https://crates.io/crates/reis) 0.7.1（0.7 已活跃），EIS 需合成器支持（GNOME 46+/KDE 6），或经 portal `RemoteDesktop` 拿 EIS fd（ashpd）。
- 剪贴板：[wl-clipboard-rs](https://crates.io/crates/wl-clipboard-rs) 0.9.5；X11 用 arboard。
- 权限：portal 每次会话弹授权框，用 `restore_token` 持久化；Flatpak 下还要 `--talk-name=org.freedesktop.portal.*`。

## 6. 可参考项目

[RustDesk](https://github.com/rustdesk/rustdesk)（AGPL，`libs/hbb_common` 含 codec + tcp/udp wrapper，采集/编码分层可直接借鉴）；[Sunshine](https://github.com/LizardByte/Sunshine)（GPL，C++，NVENC/VAAPI + Moonlight 协议，最低延迟工程范本）；[webrtc-rs examples](https://github.com/webrtc-rs/webrtc/tree/master/examples)；Deskreen（Electron+WebRTC，仅思路）。均为强 copyleft，抄代码前先定许可证策略。

## 7. 架构与预算

```
采集 (pipewire/x11rb) → 编码 (GStreamer va/nvenc, 零 B 帧 CBR)
  → 打包 (rtp payloader) → str0m/webrtc-rs (SRTP+TWCC) → UDP/局域网
  → 解包 (jitter buffer 10-20ms) → 解码 (VAAPI) → waylandsink/wgpu 呈现
  旁路: mdns-sd 发现 · QUIC/TCP 控制通道 · evdev/libei 输回程
```

延迟预算（1080p60，LAN）：采集 3-10ms → 编码 3-8ms（NVENC）/10-20ms（x264 veryfast）→ 网络 1-3ms → 抖动缓冲 10-20ms → 解码 2-8ms → 呈现 8-16ms，**玻璃到玻璃 30–70ms**；4K60 编码端 +5-10ms。码率（CBR）：1080p60 桌面文本 8-15 Mbps、游戏 15-25 Mbps；HEVC 省 ~30%，AV1 省 ~40-50%；4K60 25-50 Mbps。

## 8. 常见坑

1. Wayland 采集只有 portal 一条路，弹窗/授权必须处理，不能假设无头可用。
2. portal 给的 DMA-BUF 常是 tiled modifier，VAAPI/NVENC 导入失败即回退 shm 拷贝，4K60 性能崩塌——先打通零拷贝再谈指标。
3. 硬件 AV1：NVENC AV1 需 RTX 40+，VAAPI AV1 需 Arc/RDNA3+，Intel QSV 需 oneVPL；老卡请默认 H.264/HEVC。
4. openh264 无 AV1/10bit，x264 crate 停更；新项目把编解码统一收敛到 GStreamer 或 FFmpeg，别绑单一厂商 SDK。
5. webrtc-rs 0.20→0.21 破坏性变更 + BWE 不完整；str0m 更稳但要自己写拥塞控制。
6. cpal 在 PipeWire 上缓冲不匹配会白加 10-20ms；音频优先 pipewire crate。

---
数据来源：crates.io index 直查（版本/发布日期）、[pipewire-rs](https://gitlab.freedesktop.org/pipewire/pipewire-rs)、[xdg-desktop-portal ScreenCast](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)、[wayland-protocols](https://gitlab.freedesktop.org/wayland/wayland-protocols)、[libei](https://gitlab.freedesktop.org/libinput/libei)、[NVENC 支持矩阵](https://developer.nvidia.com/video-encode-and-decode-gpu-support-matrix-new)、[SVT-AV1](https://gitlab.com/AOMediaCodec/SVT-AV1)。
