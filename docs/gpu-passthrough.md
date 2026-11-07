# GPU 直通（Phase 1：单/多 GPU PCI passthrough）

状态：**agent 侧已实现（VmSpec/XML/CLI/单测）**，尚未在真实 IOMMU 硬件上验收。
Phase 1 只做 PCI 直通（`<hostdev type='pci'>`），不做 vGPU/SR-IOV、不做 MIG、
不做图形控制台代理；远程画面用 `vm.command` 的 `screenshot`（PNG）。

## 1. 宿主机前置条件（必须在 agent 之外完成）

1. **BIOS/UEFI 开启 VT-d / AMD-Vi（IOMMU）**；
2. 内核命令行加 `intel_iommu=on iommu=pt`（AMD：`amd_iommu=on iommu=pt`），重启；
3. 确认 IOMMU 分组：`ls /sys/kernel/iommu_groups/`；
4. 把目标 GPU 及其音频/串行功能（同一 IOMMU 组）绑定 `vfio-pci`：

   ```bash
   # 推荐用 driverctl（重启后仍生效）
   sudo driverctl set-override 0000:01:00.0 vfio-pci
   sudo driverctl set-override 0000:01:00.1 vfio-pci
   # 或临时：echo 0000:01:00.0 | sudo tee /sys/bus/pci/drivers/vfio-pci/bind
   ```

5. **确认宿主机没有其它消费者**占用该 GPU（Xorg/Wayland、CUDA、容器）；
   如果宿主是桌面环境，需把该 GPU 从显示栈移除（或使用独立 GPU + 核显输出）。

> IOMMU 组必须"干净"：如果 GPU 与其它关键设备（如 USB 控制器、网卡）同组，
> 直通会把整组设备一起交给 guest；必要时调整 PCIe 槽位或使用 ACS override
> （有安全风险，生产不建议）。

## 2. agent 侧用法

### 查看可直通的显示设备

```bash
qe-agent cli gpu list
# PCI 地址          class     厂商:设备     驱动          IOMMU 组   直通
# 0000:01:00.0      0x030000  10de:2684    vfio-pci     12         可用
# 0000:00:02.0      0x030000  8086:46a6    i915         -          需绑定 vfio-pci
```

数据来自 `/sys/bus/pci/devices/*`：class `0x03xxxx`（显示控制器）才会列出；
"直通"列显示驱动是否为 `vfio-pci`。该命令只读，不修改 sysfs。

### 创建带直通 GPU 的 VM

```bash
# --gpu 可重复，默认最多 2 个（LoadRules.max_gpus）
qe-agent vm create demo-gpu \
  --vcpus 4 --memory 8192 \
  --gpu 0000:01:00.0 \
  --gpu 0000:02:00.0 \
  --start
```

生成的 libvirt XML 片段：

```xml
<hostdev mode='subsystem' type='pci' managed='yes'>
  <source>
    <address domain='0x0000' bus='0x01' slot='0x00' function='0x0'/>
  </source>
</hostdev>
```

`managed='yes'` 让 libvirt 在 VM 启动/停止时自动绑定/解绑 vfio-pci（仍建议
提前 driverctl 固定，避免宿主其它驱动抢设备）。

### 配置文件方式

`VmSpec` 文本格式支持重复的 `gpu` 行：

```text
name = demo-gpu
vcpus = 4
memory_kib = 8388608
gpu = 0000:01:00.0
gpu = 0000:02:00.0
```

## 3. 校验与错误

- PCI 地址必须形如 `dddd:bb:dd.f`（hex；domain 最多 4 位、bus/slot ≤ 0xff、
  function ≤ 7），非法地址在 `LoadRules::validate` 与 `render_xml` 两处都会拒绝；
- `GpuPassthrough` 默认数量上限 2，可通过 `LoadRules { max_gpus, .. }` 调整；
- `VmSpec` 新增 `gpus` 字段默认空，**向后兼容**：现有调用方/JSON 不受影响。

## 4. 风险与限制

- **宿主稳定性**：错误直通（IOMMU 组不干净、GPU 被宿主占用）可能导致宿主机
  死机/内核报错；演示机建议先 `vm create` 不 `--start`，确认 XML 后再启动；
- **厂商差异**：NVIDIA 消费卡在 guest 内可能需要 vBIOS/驱动 workaround
  （Phase 1 不做隐藏 hypervisor 等处理）；
- **无热插拔**：当前只在 `vm create`（定义阶段）支持；运行中加/拔 GPU 未实现；
- **无 vGPU/SR-IOV**：多 VM 共享一块物理 GPU 需要厂商 vGPU 方案，Phase 1 不支持；
- **控制面契约**：控制节点可将 `vm.command` 的 `op=list` 返回的 VM 列表与
  GPU 信息结合展示；GPU 直通本身通过下发的 `image.pull`/创建参数落地（Phase 2
  计划做"远程创建 VM"命令）。

## 5. 测试方法

无 IOMMU 硬件时的验证路径（CI/开发机）：

```bash
# 1. XML 生成（不需要 libvirt/GPU）
cargo test -p virt-core -p virt-libvirt -j4
# 覆盖：多 GPU hostdev golden、非法地址拒绝、规则数量上限、向后兼容

# 2. CLI sysfs 解析（用假 sysfs 根目录）
cargo test -p qe-agent -j4
# 覆盖：class 过滤(0x03xxxx)、vendor/device/driver/iommu_group 解析、

# 3. 真实宿主机（有 IOMMU 时）
sudo qe-agent cli gpu list                # 确认设备与 vfio-pci 绑定
sudo qe-agent vm create gpu-test --gpu 0000:01:00.0   # 仅定义，不启动
sudo virsh dumpxml gpu-test | grep -A3 hostdev         # 确认 XML
sudo qe-agent vm start gpu-test && sudo qe-agent vm vnc gpu-test
```

若 `libvirtd` 报 `hostdev ... not found` / `vfio-pci` 绑定失败，先回到第 1 节
检查 IOMMU 与驱动绑定；agent 侧只会返回明确错误，不会修改 sysfs。
