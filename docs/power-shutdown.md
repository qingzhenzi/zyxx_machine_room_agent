# 主机电源动作：关机 / 重启 / 伪关机

## 设计

电源动作统一在 `qe-agent-node::host_power` 实现，三个入口共用：

1. 控制面 `agent.command` 下行（只有 poweroff/reboot）
2. 本机 IPC `POST /v1/power`（agent-client「电源」页）
3. CLI `qe-agent cli power shutdown|reboot|fake`

## 真正关机/重启

1. 读取 `virsh list --name --state-running`，逐个 `virsh shutdown`；
   等待 8 秒，仍运行则 `virsh destroy` 强制关机。
2. 读取 `incus list --format csv`，对 running 容器 `incus stop -f`。
3. 写入 `/run/qe-agent/last-power.json`。
4. 延迟 `QE_AGENT_POWEROFF_DELAY_SECS`（默认 3s，给结果回传）后执行
   `QE_AGENT_POWEROFF_PROGRAM`（默认 `systemctl`）的 `poweroff`/`reboot`。

安全开关：`QE_AGENT_ALLOW_POWEROFF=1`，默认关闭。

## 伪关机

`fake` 只写 `/run/qe-agent/fake-shutdown`，不真正断电，也不停 VM/容器；
用于演示/测试。清除时删除该文件即可。

## 与 systemd/libvirt 的关系

安装器会配置 `/etc/default/libvirt-guests`：

```ini
ON_SHUTDOWN=shutdown
PARALLEL_SHUTDOWN=1
SHUTDOWN_TIMEOUT=30
```

避免 VM 无法 ACPI 响应时 host 卡在 systemd shutdown。即使不经过本功能，
用户从桌面点关机也不会无限等待。

## boot-display

安装器给桌面机器设置：

- `QE_BOOT_DISPLAY_BIN=/usr/local/bin/qe-boot-display-session`
- `QE_BOOT_DISPLAY_EXIT_AFTER_BOOT=1`

boot-display 在图形会话出现后全屏显示，boot 关键阶段结束自动退出；
kiosk 安装（默认）会配置 SDDM 自动登录、关闭 Plasma 锁屏、`NAutoVTs=0`
并 mask tty2-6 的 getty，避免锁屏/切 TTY 盖住展示层。
