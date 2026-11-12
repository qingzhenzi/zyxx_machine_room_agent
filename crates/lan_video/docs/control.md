# 远程控制 / 鉴权 / 文件传输

## 1. 现状

| 能力 | 状态 |
|---|---|
| 视频/音频单向传输 | ✅ |
| 键鼠远程控制（Linux uinput） | ✅ 已跨机验证（本机 viewer → Debian agent） |
| 控制会话鉴权 | ✅ 每会话 `control_token` |
| 单向剪贴板（控制端 → 被控端） | ✅ KDE Klipper（已跨机验证） |
| 文件传输（上传/列表/下载） | ✅ HTTP + token（本机 128KB 往返验证） |
| Windows 输入注入 | ❌ 还没做（Windows 目前只做接收端） |
| 媒体流（SRTP）加密 | ✅ 默认开启（AES-128-ICM + HMAC-SHA1-80，每会话随机密钥） |

## 2. 会话鉴权

创建会话时返回 `control_token`：

```bash
curl -s -X POST http://127.0.0.1:38080/v1/sessions \
  -H 'Content-Type: application/json' \
  -d '{"target":"192.168.8.115","port":5000,"capture":{"type":"kwin"}}'
# -> {"session": {"id": "...", "control_token": "80b263..."}}
```

控制 WebSocket 和文件接口都必须带这个 token（query 或 `Authorization: Bearer`）。
token 在会话强杀后失效。

## 3. 控制通道

```
ws://<agent>:38080/v1/sessions/{id}/control?token=<control_token>
```

**二进制帧**（低延迟主路径）：

| 首字节 | 负载 | 含义 |
|---|---|---|
| `1` | `dx i16 LE`, `dy i16 LE` | 鼠标相对移动 |
| `2` | `button u16 LE`, `down u8` | 鼠标按键（evdev BTN_*） |
| `3` | `key u16 LE`, `down u8` | 键盘键（evdev KEY_*） |
| `4` | `delta i16 LE` | 滚轮 |
| `5` | UTF-8 文本 | 剪贴板（控制端 → 被控端） |
| `6` | 无 | 延迟探测：agent 原样回显，用于测 WS RTT |

也支持文本 JSON 兜底：

```json
{"t":"move","dx":-5,"dy":10}
{"t":"button","code":272,"down":true}
{"t":"key","code":30,"down":true}
{"t":"wheel","delta":-1}
{"t":"clipboard","text":"hello"}
```

注入后端：Linux `/dev/uinput` 虚拟键鼠（鼠标相对移动 + 全键盘），事件经 stdin 二进制管道直达 C helper，不经过 JSON 解析，链路极短。

### uinput 权限

被控端需要 `/dev/uinput` 可写：

```bash
# Debian / Ubuntu
echo 'KERNEL=="uinput", GROUP="input", MODE="0660", TAG+="uaccess"' \
  | sudo tee /etc/udev/rules.d/99-uinput.rules
sudo udevadm control --reload-rules
sudo udevadm trigger /dev/uinput || true
sudo setfacl -m u:$(id -un):rw /dev/uinput || true
test -w /dev/uinput && echo OK
```

也提供了 `scripts/debian-uinput.sh`。

## 4. 剪贴板（单向）

控制端发 `type=5` 帧，agent 调 KDE Klipper：

```bash
busctl --user call org.kde.klipper /klipper \
  org.kde.klipper.klipper setClipboardContents s "text"
```

- 只支持“控制端 → 被控端”，不做双向同步；
- 依赖 Klipper（KDE 默认有）；X11/其他桌面需要另接 xclip/wl-copy。

## 5. 文件传输

```bash
# 上传（控制端 -> 被控端）
curl -X POST "http://<agent>:38080/v1/sessions/{id}/files?token=<token>&name=report.pdf" \
  --data-binary @report.pdf

# 列表
curl "http://<agent>:38080/v1/sessions/{id}/files?token=<token>"

# 下载（被控端 -> 控制端）
curl "http://<agent>:38080/v1/sessions/{id}/files/report.pdf?token=<token>" -o report.pdf
```

- 存到 `~/Downloads/lan_video/<session_id>/`；
- 文件名做了 basename 清理，防止路径穿越；
- 单次请求 body 上限 4GB；
- 必须带 session token。

## 6. 延迟（实测）

- 控制走 WebSocket TCP，**已设置 TCP_NODELAY**（axum `tap_io`），每帧二进制 5 字节左右；
- 应用层 echo RTT（`type=6`）实测：
  - 同机：p50 ≈ **0.11ms**，p95 ≈ 0.23ms；
  - 本机 → Debian（LAN）：p50 ≈ **0.83ms**，p95 ≈ 1.0ms，max ≈ 4ms；
- agent 收到帧后直接写 uinput helper 的 stdin（无 JSON、无队列），helper 立即写 `/dev/uinput`；
- 端到端“鼠标到屏幕”延迟 = WS RTT + 注入 + 合成器处理，估计 LAN 下几 ms；尚未加时间戳做最终量化。
- 如果走 WiFi/有丢包，TCP 会出现队头阻塞；要更稳可以后续加 UDP/QUIC DATAGRAM 控制通道（鼠标事件可丢、不重传）。

## 7. 已知限制

1. **SRTP 密钥派生是简化实现**：媒体流已默认 SRTP 加密（`srtpenc`/`srtpdec`），
   但 `srtp_master_key()` 只是「取十六进制字符 + 截断/补零到 30 字节」，不是 KDF。
   默认随机 secret 可用，**手填短口令会显著削弱密钥强度**。严格场景应改用 DTLS-SRTP
   协商（未实现）。另：加密不隐藏端口存在性，不可信网络仍需 VLAN/防火墙/WireGuard。
2. **Windows 输入注入未实现**：Windows 目前只做接收端；控制端/被控端暂时是 Linux。
3. **剪贴板只支持 KDE**（Klipper）；其他桌面要加后端。
4. **Wayland 全局键鼠**：uinput 是内核级，KDE/GNOME 都有效；但部分安全桌面/UAC 场景不可控。
5. **窗口/全屏**：接收端全屏用 `waylandsink fullscreen=true` 在 Debian 上出现过断言失败，建议先用 `autovideosink` 窗口或手动全屏。
6. viewer 的键鼠捕获 UI 还没写；当前提供 API/协议，前端需要做 pointer lock + 键码映射（evdev）。
