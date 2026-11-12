# Windows 作为接收端 / 发送端

## 1. 结论

- **Windows 作为接收端**：完全可行，推荐先用 GStreamer 直接收，最省事；也可以用本项目编译出的 `lan_video.exe receive`。
- **Windows 作为发送端**：代码里已接 `d3d11screencapturesrc`（WGC）和 `mfh264enc/nvh264enc/qsvh264enc`，音频走 `wasapisrc loopback`，但**尚未在 Windows 实测**，按实验特性对待。
- KWin 免授权采集只在 Linux/KDE；Windows 不受影响（build.rs 会自动跳过 KWin helper）。

## 2. Windows 接收端（最快路径，不用编译本项目）

1. 安装 **GStreamer MSVC 64-bit runtime**（gstreamer.freedesktop.org 下载安装包，选 Complete）；
2. 把 `C:\gstreamer\1.0\msvc_x86_64\bin` 加进 `PATH`；
3. 验证：

```powershell
gst-launch-1.0 --version
```

4. 收视频 + 音频（两个窗口/两个终端）：

```powershell
# 视频：RTP H.264 -> d3d11videosink
gst-launch-1.0 -v udpsrc port=5000 caps="application/x-rtp,media=video,encoding-name=H264,payload=96,clock-rate=90000" `
  ! rtpjitterbuffer latency=0 ! rtph264depay ! decodebin ! d3d11videosink sync=false

# 音频：RTP Opus -> 默认声卡
gst-launch-1.0 -v udpsrc port=5002 caps="application/x-rtp,media=audio,encoding-name=OPUS,payload=97,clock-rate=48000" `
  ! rtpjitterbuffer latency=0 ! rtpopusdepay ! opusdec ! autoaudiosink sync=false
```

> 如果 `d3d11videosink` 不可用，换 `autovideosink`；如果 `decodebin` 找不到解码器，说明安装 GStreamer 时没选完整插件（需要 base/good/bad/libav）。

防火墙：允许入站 UDP 5000、5002（`New-NetFirewallRule` 或首次运行时弹窗允许）。

## 3. 在 Windows 上编译本项目

前提：Rust（MSVC toolchain）+ GStreamer runtime（`gst-launch-1.0` 在 PATH）。**不需要** GStreamer 开发头，也不需要 Wayland 依赖。

```powershell
cargo build --release
.\target\release\lan_video.exe capabilities
.\target\release\lan_video.exe windows        # 枚举可采集窗口（EnumWindows）
.\target\release\lan_video.exe receive --port 5000 --audio-port 5002 `
  --sink d3d11videosink --audio-sink autoaudiosink --decoder auto
```

- `receive` 只依赖 `gst-launch-1.0`（运行时）；
- `probe` 也是跨平台的：`lan_video.exe probe --peer <linux-ip>:40000 --count 500 --interval-ms 5`；
- `kwin`、`pipewire` 采集参数在 Windows 下会报“不支持”，属预期。

## 4. Windows 作为发送端（实验）

```powershell
.\target\release\lan_video.exe run --target 192.168.1.20 --port 5000 --audio-port 5002 `
  --capture wgc --encoder auto --width 1920 --height 1080 --fps 60 --bitrate 12000
```

- 采集：`d3d11screencapturesrc show-cursor=true`（WGC，Win10 1903+）；
- 编码：`auto` 会依次尝试 `vah264`/`nvh264`/`mfh264`/`x264`，Windows 上通常是 `mfh264enc` 或 `nvh264enc`；
- 音频：`wasapisrc loopback=true` 采系统声音（`--audio-device` 可指定设备）；
- 首次监听 UDP 端口时 Windows 防火墙可能弹一次允许，装机时预加规则可避免。

## 5. 双机延迟测量（Windows 接收端）

- **网络基线**：Linux 发送机跑 `lan_video probe --listen --bind 0.0.0.0:40000`；Windows 上编译本项目的 `lan_video.exe probe --peer <linux-ip>:40000 ...`；或临时用 `ping` 只看 RTT（没有抖动/丢包统计）。
- **玻璃到玻璃**：Windows 接收端全屏播放（`d3d11videosink fullscreen=true` 或双击全屏），Linux 发送端全屏显示毫秒时钟，用手机 240fps 慢动作同时拍两个屏幕，同一帧差值即延迟。

## 6. 音频通道（Windows）

```powershell
# 系统声音（默认，wasapisrc loopback=true）
--audio-port 5002 --audio-source system
# 麦克风（wasapisrc）
--audio-port 5002 --audio-source mic
# 指定设备
lan_video.exe devices
--audio-port 5002 --audio-device "<设备名>"
```

## 7. 已知限制

- Windows 发送端未实测，编码器/采集参数可能需要按机器调整；
- `vah264enc` 在 Windows 不可用，`auto` 会回退到 MF/NVENC；
- **窗口采集**：`--capture wgc --window-handle <hwnd>` 已接好（实验特性，未实测）；
  窗口列表直接用 `lan_video.exe windows` 枚举（EnumWindows，返回 HWND/标题/进程名），
  也可以用 PowerShell：

  ```powershell
  Get-Process | Where-Object {$_.MainWindowTitle} |
    Select-Object MainWindowHandle, MainWindowTitle
  ```

  `MainWindowHandle` 直接传给 `--window-handle`（也支持 `0x` 前缀）。
- `/v1/windows` 与 `windows` 命令已支持 Windows（EnumWindows）+ KDE Linux（KWin/X11）；
- **远程键鼠/剪贴板**：Windows 端已实现（SendInput + Win32 剪贴板），
  与 Linux 控制端互通（控制协议仍传 evdev 码，Windows 端做 VK 映射，
  未映射的键丢弃）；提权窗口的注入会被 UIPI 拦截（SendInput 返回失败）；
- 本项目目前没有 Windows GUI，只有 CLI + HTTP API；
- A/V 同步是实验特性（`--sync`，rtpbin + RTCP），Windows 上未实测。
