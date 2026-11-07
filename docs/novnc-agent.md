# noVNC 阶段 2（agent 侧）：VNC 隧道

状态：**agent 侧已实现并单测通过**（vnc.info + vnc.open mTLS WS ↔ 本地 TCP 桥）。
device-pki 中继 / qe-api WS 代理 / 前端由并行任务实现；端到端真机验收待环境。

设计见 `room_node/docs/novnc-design.md` §5。

## 1. 链路

```
浏览器 noVNC (RFB over WebSocket)
  └─ ws://control/api/v1/admin/machines/{serial}/vnc/ws?session&ticket   (qe-api)
       └─ qe-api 内存 session 表 → device-pki 内部中转 (127.0.0.1:4442)
            └─ device-pki 按 session 配对
                 └─ wss://<DEVICE_PKI_MTLS>/vnc/{serial}?session=<uuid>  （agent 主动连出）
                      └─ agent 本地 TCP 127.0.0.1:<libvirt VNC port>      （qemu VNC）
```

agent 只做**字节管道**：RFB 握手/认证（含 VNC 密码）由浏览器 noVNC 完成，
密码由 `vnc.info` 返回给控制面再下发给前端。agent 不解析 RFB。

## 2. `vm.command` op `vnc.info`

请求：

```json
{"request_id":"<uuid>","op":"vnc.info","args":{"name":"vm-1"}}
```

返回 data：

```json
{"name":"vm-1","host":"127.0.0.1","port":5901,"password":"s3cret"}
```

规则（`crates/qe-agent-node/src/vnc.rs`）：

- 从 libvirt `Domain::graphics()` 读 `<graphics type='vnc'>`；
- 只接受 VNC；`port == 0`（autoport 未分配/VM 未启动）→ 明确错误
  `VM "vm-1" 的 VNC 端口未分配（port=0）…`；
- 没有 VNC 控制台（headless / 只有 SPICE）→ `VM "vm-1" 无 VNC 图形控制台`；
- `host` 归一化：`127.0.0.1` / `0.0.0.0` / `::` / `::1` / `localhost` → `127.0.0.1`；
  其它非回环 host 拒绝（VNC 不应暴露到局域网）；
- `password`：libvirt `<graphics passwd='…'>` 有值则原样返回，否则 `null`
  （控制面下发给前端 `credentials.password`）。

错误通过 `machine_command_results`（`ok=false` + error）回传控制面。

## 3. downlink kind `vnc.open`

payload：

```json
{"session_id":"<uuid>","vm":"vm-1","serial":"<设备serial，可选>"}
```

行为：

1. `run.rs` 在 node/both 角色订阅 `vnc.open`，解析后 **`tokio::spawn`** 独立任务，
   不阻塞其它下行；解析失败只日志并 ack；
2. 用 `LibvirtExecutor::graphics(vm)` 解析 VNC host/port（规则同 `vnc.info`）；
3. 用**设备证书 mTLS** 连接
   `wss://<DEVICE_PKI_MTLS>/vnc/<serial>?session=<uuid>`：
   - serial 以设备证书里的 serial 为准（若 payload serial 与证书不一致会 warning 并
     使用证书 serial，避免被 device-pki 拒绝）；
   - 复用/扩展 `qe-device-pki-client::channel::connect_path()`（与 `/channel` 同一套
     rustls 客户端证书 + CA 链校验，ALPN `http/1.1`）。
4. 连接本地 `127.0.0.1:<port>`（10s 连接超时，`TCP_NODELAY`）；
5. 双向泵：
   - WS `Binary` → TCP write；单帧 > **2 MiB** 直接断开（防内存滥用）；
   - TCP read（64 KiB 块）→ WS `Binary`；
   - WS `Text` 帧忽略（只记 debug 日志），Ping/Pong 透传处理；
   - **空闲 30 分钟**（双向均无数据）→ 关闭；
   - 任一端 Close/EOF/错误 → 关闭另一端并结束；
6. 日志：`session_id` / `vm` / `serial` / `bytes_to_vnc` / `bytes_to_ws` / `reason` /
   `elapsed_ms`。

结束原因：`idle`（空闲超时）/ `ws_closed`（device-pki 侧关闭）/ `tcp_closed`（qemu 侧关闭）。

## 4. 文件与函数

| 文件 | 内容 |
| --- | --- |
| `crates/qe-device-pki-client/src/channel.rs` | `connect_path(cfg, path_and_query)`、`ws_request_url(host_port, path)`（校验绝对路径/非法字符） |
| `crates/qe-agent-node/src/vnc.rs` | `VncOpen` 解析/校验、`effective_serial`、`vnc_ws_path`/`vnc_ws_url`、`encode_path_segment`、`vnc_target_from_graphics`、`vnc_info_from_graphics`、`is_idle`、`relay_session`、`run_session` |
| `crates/qe-agent-node/src/vm_control.rs` | `VmOp::VncInfo`、`VmExecutor::{graphics, vnc_info}`、`LibvirtExecutor` 实现 |
| `crates/qe-agent-node/src/run.rs` | `vnc.open` 订阅 + 异步 spawn |

## 5. 安全约束

- agent → device-pki 只走**设备证书 mTLS**（不允许明文/跳过校验）；
- VNC 端口只允许回环地址；本地 `127.0.0.1` 由 libvirt 的 `listen=127.0.0.1` 保证；
- WS 路径 serial 用证书身份，session uuid 由控制面生成；percent-encode 防注入；
- 单帧 ≤2 MiB；空闲 30min 自动断；不缓存/不落盘任何 VNC 数据；
- agent 不向公网主动连接，只连配置的 device-pki 端点。

## 6. 测试

```bash
cargo test -p qe-agent-node -j4          # vnc::tests（11 个）+ vm_control vnc.info 测试
cargo test -p qe-device-pki-client -j4   # channel::ws_request_url 测试
```

覆盖：

- `vnc.open` payload 解析（合法/缺字段/非法 uuid/非法 vm 名/空 serial）；
- `effective_serial`（证书优先）；
- WS 路径与 URL：https→wss、http→ws、尾部斜杠、特殊字符 percent-encode、
  非法 scheme/空主机；
- 本地目标解析：回环/`0.0.0.0`/`::`/`localhost` 归一化，非回环拒绝，port=0 拒绝，
  SPICE-only/无 graphics 拒绝；
- `vnc.info` 形状（含 password null 与非 null）；
- 空闲判断（30min 边界）；
- 命令解析/校验/分发（mock executor）。

**未覆盖**：真实 WS/TCP 端到端（需要 device-pki 中继 + libvirt VNC 环境）；
按设计由 Lead 在联调环境用 session VM 验收（qemu:///session、VNC 随机端口）。

## 7. 排障

| 现象 | 排查 |
| --- | --- |
| `vnc.info` 报 "无 VNC 图形控制台" | VM 定义是否 `display=vnc`（agent 创建默认 VNC 回环）；headless VM 没有 VNC |
| `vnc.info` 报 "VNC 端口未分配" | VM 未启动（autoport 只有运行后才有具体端口） |
| `vnc.open` 报 "连接 device-pki VNC 隧道失败" | `DEVICE_PKI_MTLS_URL`、设备证书（serial 与路径一致）、device-pki 是否已实现 `GET /vnc/{serial}` |
| 隧立即关闭（`ws_closed`） | device-pki session 配对过期（30s）/ qe-api ticket 一次性/过期 |
| 隧立即关闭（`tcp_closed`） | 本地 5900+ 端口未监听（VM 未启动/端口变化） |
| 浏览器黑屏但无报错 | 前端是否用 `vnc.info` 返回的 password 初始化 RFB；VNC 密码错误会被 qemu 拒绝并断开 |
| 高延迟 | agent ↔ device-pki ↔ qe-api 同机/同网段；单帧上限 2 MiB、无压缩（RFB 自身有编码） |
