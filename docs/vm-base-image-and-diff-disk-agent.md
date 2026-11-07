# VM 底包 + 差分盘 + 持久化快照 + 导出（agent 侧，阶段 1.5）

状态：**agent 侧已实现并单测/真实 qemu-img 集成测试通过**；libvirt 真机
（define/运行域）尚未在本机验收（见文末验证命令）。

设计契约见 `room_node/docs/vm-base-image-and-diff-disk.md` §3。本文件记录
`qe-agent-node` / `qe-agent` 侧实现细节、错误语义与真机验证方法。

## 1. 扩展的 `vm.command` ops

| op | args | data |
|----|------|------|
| `create` | `{name, vcpus?=2, memory_mib?=2048, disk_gb?=20, base_volume?, base_path?, gpu?[], start?}` | `{name, disk_path, base, defined:true, started}` |
| `snapshot.create` | `{name, snapshot, description?}` | `{name, snapshot}` |
| `snapshot.list` | `{name}` | `{name, snapshots:[{id,tag,vm_size,date,vm_clock,icount}]}` |
| `snapshot.restore` | `{name, snapshot}` | `{name, snapshot}` |
| `snapshot.delete` | `{name, snapshot}` | `{name, snapshot}` |
| `disk.info` | `{name}` | `{disks:[{path,format,virtual_size,backing_path,backing_format}], chain:[...]}` |
| `export` | `{name, format?="qcow2"}` | `{path, sha256, size_bytes}` |

参数约束（`vm_control.rs::VmCommand::validate`）：

- `name`：`[A-Za-z0-9._-]{1,64}`（同时作为目录名，拒绝 `.`/`..`/含 `/`）；
- `create`：`vcpus 1..=64`、`memory_mib 256..=262144`、`disk_gb 1..=2048`、
  `gpu ≤ 2`（PCI 地址格式 `dddd:bb:dd.f`）；`base_volume` 与 `base_path` 互斥；
- `snapshot.*` 的 `snapshot` 必须是 `[A-Za-z0-9._-]{1,64}`；
- `export` 只接受 `format=qcow2`；
- 超时：普通 op 30s、screenshot 20s、**create/export 120s**；执行在
  `spawn_blocking` 中，panic 只让该条命令失败。

## 2. 实现要点

### create（底包 → 差分盘）

1. 解析底包：
   - `base_volume`：池名取 `LIBVIRT_POOL`（默认 `default`），
     `virsh [--connect $LIBVIRT_URI] vol-path --pool <pool> <vol>` 得到路径；
   - `base_path`：直接使用并校验文件存在；
   - 二者都为空 → 创建普通空盘（无 backing）。
2. 差分盘路径固定 `<data-dir>/vms/<name>/disk.qcow2`：
   - 有底包：`qemu-img create -f qcow2 -b <base> -F qcow2 <disk> <disk_gb>G`
     （稀疏，只存增量）；
   - 无底包：`qemu-img create -f qcow2 <disk> <disk_gb>G`。
3. 用 `virt-crate::Backend::libvirt()` define（VNC 回环、GPU hostdev），
   `start=true` 时再 `start()`；重名在 define 前检查并返回明确错误。
4. 返回 `{name, disk_path, base, defined, started}`。

### snapshot.*（qcow2 内部快照）

- 先经 libvirt 解析域的**第一块可写磁盘**（`Domain::disk_paths()`）；
- 校验域状态：仅 `ShutOff` 允许，`Running/Paused/...` 返回
  "虚拟机正在运行…要求先 shutdown" 的明确错误（包括 `snapshot.list`）；
- `qemu-img snapshot -c/-a/-d <tag> <disk>`；
- `snapshot.list` 解析 `qemu-img snapshot -l` 文本（ID/TAG/VM_SIZE/DATE/
  VM_CLOCK/ICOUNT；兼容旧版无 ICOUNT 列）为 JSON 数组。
- `description` 字段当前**被接受但不持久化**（qcow2 内部快照无描述字段；
  阶段 2 可在控制面无数据表记录）。

### disk.info

- `qemu-img info --backing-chain --output=json <disk>`，归一化为
  `{disks:[...], chain:[路径...]}`；chain 从差分盘到最底层底包。

### export

- 要求域 `ShutOff`；
- `qemu-img convert -O qcow2 <disk> <data-dir>/exports/<name>-<unix_ts>.qcow2`
  （扁平化，去掉 backing 依赖）；
- 最终文件流式计算 sha256 + 字节数返回；上传到对象存储/发布镜像属阶段 2。

## 3. CLI（`qe-agent cli vm`）

```bash
# 用 libvirt 卷作底包创建差分盘 VM（可 --start / --gpu）
qe-agent cli vm create demo-01 \
  --base-volume base-win11 \
  --vcpus 4 --memory 8192 --disk-size 40G \
  --start

# 或用宿主机路径作底包
qe-agent cli vm create demo-02 --base-image /var/lib/libvirt/images/base.qcow2 --start

# 快照（要求 VM 已关机）
qe-agent cli vm snapshot create demo-01 snap-before-exam --description "考前基线"
qe-agent cli vm snapshot list demo-01
qe-agent cli vm snapshot restore demo-01 snap-before-exam
qe-agent cli vm snapshot delete demo-01 snap-before-exam

# 磁盘链 / 导出
qe-agent cli vm disk-info demo-01
qe-agent cli vm export demo-01 --format qcow2
```

`qe-agent cli vm snapshot/disk-info/export` 与下行 `vm.command` 共用
`qe_agent_node::vm_control::LibvirtExecutor`，行为一致。

## 4. 错误语义（控制面可直接展示）

| 场景 | agent 返回 |
| --- | --- |
| 名字非法 / 参数越界 | `ok=false` + 明确字段错误 |
| `base_volume` 与 `base_path` 同时给 | `ok=false`，要求二选一 |
| 底包卷/路径不存在 | `virsh`/文件检查错误（含路径） |
| VM 重名 | "虚拟机 … 已存在" |
| 运行中做快照/导出 | "虚拟机 … 正在运行（state=running）：要求先 shutdown" |
| 非法导出格式 | "export 目前只支持 format=qcow2" |
| qemu-img 失败 | 附带 qemu-img stderr 与目标路径 |
| 执行超时（120s） | `ok=false` + 超时说明（后台任务不可取消） |
| 阻塞任务 panic | `ok=false` + "命令执行 panic（已隔离）" |

## 5. 测试

```bash
# 单测（mock 后端：解析/校验/分发/超时/panic）
cargo test -p qe-agent-node -j4

# 真实 qemu-img 集成测试（本机无 qemu-img 自动 skip）：
# 差分盘 backing chain → 快照 -c/-l/-a/-d → convert 导出 + sha256 复算
cargo test -p qe-agent-node --test vm_disk_qemu_img -j4 -- --nocapture

# CLI 相关（gpu list / vm 帮助）
cargo test -p qe-agent -j4
```

本次实测：`vm_disk_qemu_img::qemu_img_overlay_snapshot_export_roundtrip` 与
`create_overlay_disk_rejects_missing_base` 真跑通过（qemu-img 11.1.0），
覆盖 overlay 生成、`--backing-chain` 链长=2、快照增删恢复、导出后链长=1、
sha256 复算一致。

## 6. 真机 libvirt 验证（不碰生产域，手工）

```bash
# 用 session libvirt（当前用户私有域），只做 define/查询，不启动
export LIBVIRT_URI=qemu:///session
LIBVIRT_URI=qemu:///session cargo test -p qe-agent-node \
  --test vm_disk_qemu_img -- --ignored create_defined_in_session_uri

# 或直接 CLI（session 连接）
qe-agent cli vm --uri qemu:///session create qe-demo-01 \
  --base-image /path/to/base.qcow2 --memory 1024 --disk-size 10G
qe-agent cli vm --uri qemu:///session disk-info qe-demo-01
```

未验证项（本机无可用 libvirt 会话/域）：

- `create` 在真实 libvirt 上的 define + start（含 VNC 回环、GPU hostdev）：
  代码路径与 CLI 已就绪，需在宿主机执行上面命令确认；
- 运行中域的快照拒绝：单测覆盖状态分支，真机需启动一台 VM 后验证；
- qcow2 内部快照 + backing file 的真实恢复一致性（qemu-img 11 支持；
  老版本 qemu 可能拒绝"带 backing file 的内部快照"，需按发行版确认）；
- 导出大磁盘（>数 GB）的 120s 超时是否足够：可调 `HEAVY_TIMEOUT`。

## 7. 已知限制（阶段 1.5）

- 快照只对**第一块可写磁盘**生效；多盘 VM 的其它盘不参与（契约范围内）；
- `description` 不持久化；快照跨主机迁移/增量分发属阶段 3（与 P2P 结合）；
- 导出到本地 `<data-dir>/exports/`，上传对象存储与发布镜像属阶段 2；
- `create` 不自动扩容/不修改底包；底包始终只读（backing file 语义）；
- 同名差分盘已存在时不会覆盖（qemu-img 会失败），需先删除或换名。
