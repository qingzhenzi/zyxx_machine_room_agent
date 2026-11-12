# 控制其他 Debian 设备

## 网络方向 / UDP 优先

控制机可以先开本机防火墙 UDP 入站（见下），然后 Debian 直接发 RTP/UDP 到控制机：

```
Debian: ximagesrc/KWin → x264 → RTP → udpsink <控制机>:6000
控制端: udpsrc :6000 → 解码 → viewer
控制端: control WebSocket → Debian agent :38080（token）
```

如果反向 UDP 不可达（NAT/防火墙/AP 隔离），再回退 TCP：

```
Debian: ... → rtpstreampay → tcpserversink :6000
控制端: viewer --tcp --host <debian> --port 6000
```

### 本机防火墙（UFW 实际是开启的）

```bash
sudo ufw allow from 192.168.8.0/24 to any port 6000:6010 proto udp
sudo ufw reload
# 验证：在控制机起 probe --listen，Debian 上 probe --peer <控制机>:40000
```

> 如果 UFW 默认 `deny incoming`，Debian→控制机的 UDP 会被丢弃；
> 这就是之前 TCP 看似“必须”的原因。开端口后 probe 应 0% loss。



```
Debian: ximagesrc/KWin → x264 → rtpstreampay → tcpserversink :6000
控制端: tcpclientsrc → rtpstreamdepay → 解码 → viewer
控制端: control WebSocket → Debian agent :38080（token）
```

UDP 模式仍然保留（`--tcp` 不传即可），适合双向可达的 LAN。

## Debian 侧准备

1. GStreamer runtime：

```bash
sudo apt install gstreamer1.0-tools gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-ugly gstreamer1.0-libav
```

2. 部署二进制（本机编译后 scp，目标不需要 Rust/开发头）：

```bash
scp target/release/lan_video dev@<debian>:~/lan_video
```

3. 安装 agent 服务 + uinput 权限：

```bash
# 在 Debian 上
chmod +x ~/lan_video
AGENT_TOKEN='换成一个随机长字符串' ./scripts/setup-debian-agent.sh ~/lan_video
# 或：脚本自动生成 token，打印 ~/.config/lan_video-agent-token
```

4. 桌面会话：**当前验证的是 Plasma X11**。
   Debian KDE 6.3 的 KWin Wayland screencast 节点能创建但不出帧（实测），
   所以控制场景先用 X11：

```bash
# /etc/sddm.conf.d/10-autologin.conf
[Autologin]
User=dev
Session=plasmax11.desktop
Relogin=false
sudo systemctl restart sddm
```

5. 确认 agent：

```bash
systemctl --user status lan-video-agent.service
ss -lnt | grep 38080
```

## 控制端准备

```bash
# 构建带 viewer 的二进制
cargo build --release --features viewer

# 从 Debian 拷 token
scp dev@<debian>:~/.config/lan-video-agent-token \
  ~/.config/lan_video/debian-agent-token
chmod 600 ~/.config/lan_video/debian-agent-token

# 一键连接（创建 x11 TCP 会话 + 打开 viewer，退出自动删会话）
./scripts/connect-debian.sh <debian-ip> 6000 x11
```

指定 token 文件/二进制：

```bash
LAN_VIDEO_AGENT_TOKEN_FILE=/path/to/token \
LAN_VIDEO_BIN=/path/to/lan_video \
./scripts/connect-debian.sh 192.168.8.115 6000 x11
```

viewer 快捷键：F11 全屏、F12 退出、F10 鼠标捕获开关、Ctrl+V 发剪贴板、拖文件上传。

## 手工 API（不用脚本）

控制端默认 UDP（`tcp:false`，target 是本机在 Debian 眼里的源地址）：

```bash
TOKEN=$(cat ~/.config/lan_video/debian-agent-token)
RESP=$(curl -sS -X POST http://<debian>:38080/v1/sessions \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"target":"0.0.0.0","port":6000,"capture":{"type":"x11"},
       "encoder":"x264","width":1280,"height":720,"fps":30,
       "bitrate_kbps":6000,"tcp":true}')
# 取 session.id / control_token，然后：
lan_video view --tcp --host <debian> --port 6000 \
  --agent http://<debian>:38080 --session <id> --token <control_token>
```

## 安全

- agent HTTP API 用 `--token` 保护；会话控制/文件用 per-session `control_token`；
- 媒体流（TCP RTP）目前**未加密**，只建议在可信 LAN/VPN；
- uinput 注入需要 `/dev/uinput` 权限（`scripts/debian-uinput.sh`）。

## 已验证

- 本机 viewer 成功收到 Debian X11 桌面（TCP 6000）；
- Debian 上成功创建 `lan-video virtual input`，控制 WS 连接、关闭后销毁；
- Debian 侧 agent 服务、uinput、TCP 发送均正常。
