# 原生 Viewer（Linux）

一个最小可用的"看 + 控"客户端：GStreamer 解码、SDL2 窗口、键鼠捕获、控制 WebSocket、剪贴板、拖拽上传。

## 依赖

运行/构建需要：

```bash
# Arch
sudo pacman -S sdl2 gstreamer gst-plugins-base gst-plugins-good \
  gst-plugins-bad gst-plugins-ugly gst-plugin-va
```

构建时 cargo 会拉 `gstreamer-rs` / `sdl2` crate；需要 `gstreamer-1.0`、`gstreamer-app-1.0`、`sdl2` 的 pkg-config 文件。

## 用法

```bash
# 先创建/获取会话：agent 返回 id + control_token
curl -X POST http://192.168.8.115:38080/v1/sessions \
  -H 'Content-Type: application/json' \
  -d '{"target":"192.168.91.113","port":5000,"audio_port":5002,
       "capture":{"type":"kwin"},"encoder":"vah264",
       "width":1920,"height":1080,"fps":30,"bitrate_kbps":20000}'

# 观看 + 控制
./target/release/lan_video view --port 5000 --audio-port 5002 \
  --agent http://192.168.8.115:38080 \
  --session <session-id> --token <control_token>

# 只看不控（不传 agent/session/token 即可）
./target/release/lan_video view --port 5000 --audio-port 5002

# 启动即全屏
./target/release/lan_video view --port 5000 --fullscreen
```

## 快捷键

| 键 | 功能 |
|---|---|
| `F11` | 全屏切换 |
| `F12` | 退出 viewer |
| `F10` | 鼠标捕获开/关（关闭后可用鼠标操作本机窗口） |
| `Ctrl+V` | 读取本机剪贴板 → 发到被控端 → 触发远端粘贴 |
| 拖拽文件到窗口 | POST 上传到当前会话的 `~/Downloads/lan_video/<session>/` |

窗口标题显示 `lan_video viewer`；拖拽上传时会显示状态。

## 输入

- 鼠标：SDL 相对模式（`SDL_SetRelativeMouseMode`），发送相对位移，避免坐标/缩放问题；
- 键盘：SDL 键码 → evdev 键码，直接发二进制帧；
- 鼠标键：左/中/右 → evdev `BTN_LEFT`/`BTN_MIDDLE`/`BTN_RIGHT`；
- 控制帧走 `ws://.../v1/sessions/{id}/control?token=...`，服务端直接写 uinput helper。

## 限制

1. 只支持 **H.264 / payload 96**（和当前 RTP 发送端一致）；
2. viewer 是 Linux 原生程序（SDL2）；Windows/macOS 暂未接；
3. 音频是独立 pipeline，未和视频做 rtpbin NTP 同步（A/V sync 需要走 `--sync` 发送）；
4. 还没有会话列表/创建 UI，session id/token 由外部传入；
5. 文件拖拽只做上传，没有下载 UI；下载用 HTTP 接口。
