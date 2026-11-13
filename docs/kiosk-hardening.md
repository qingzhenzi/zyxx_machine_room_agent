# 教室机 kiosk 加固：GRUB / 锁屏 / TTY

## 为什么 GRUB 不放进 qe-agent 运行时

`qe-agent` 是常驻业务进程，不适合直接修改 `/boot`：

- 改 GRUB 写错会让机器无法启动，远程 agent 也随之失联，没有恢复通道；
- Debian 用 `update-grub`，Arch 用 `grub-mkconfig`，还有 systemd-boot/其他 bootloader，
  运行时逻辑会变成长期维护负担；
- 内核/grub 包升级会重新生成配置，agent 改了也会被覆盖；
- 这是**系统镜像/安装期策略**，应该由安装器或镜像构建完成。

建议：

- 安装器 `--kiosk` / 独立 hardening 脚本负责改 GRUB；
- agent 只做只读检查/告警（例如检测到启动菜单超时 > 0 上报），不远程改 `/boot`。

## 安装器当前做的 GRUB 处理

release 安装器 kiosk（默认开，`--no-kiosk` 可关）：

```ini
GRUB_TIMEOUT=0
GRUB_TIMEOUT_STYLE=hidden
GRUB_RECORDFAIL_TIMEOUT=0
GRUB_DISABLE_OS_PROBER=true
GRUB_DISABLE_RECOVERY=true
```

并执行：

- 备份 `/etc/default/grub` → `.qe-backup`
- `update-grub`（Debian）或 `grub-mkconfig`（Arch）
- 去掉 `/etc/grub.d/30_uefi-firmware` 的 UEFI 菜单项
- GRUB 密码（默认**不启用**）：`--grub-password PASS` 才写入
  - 写入前会 patch `/etc/grub.d/10_linux`、`20_linux_xen`，给菜单项加 `--unrestricted`：
    正常启动不需要密码，Esc/Shift 调菜单/编辑才需要
  - 备份放 `/root/qe-grub-backups`，绝不留在 `/etc/grub.d`（否则 grub-mkconfig 会执行备份，
    造成重复菜单/密码块，真机踩过）
  - 真机已验证：正常重启自动进 Debian；Esc/Shift 进密码提示
  - 密码保存在 `/root/qe-grub-password` (0600)

TTY：

- `/etc/systemd/logind.conf.d/90-qestudio-kiosk.conf`：`NAutoVTs=0`
- mask `getty@tty2..tty6`
- `/etc/X11/xorg.conf.d/99-qestudio-no-vtswitch.conf`：Xorg `DontVTSwitch=true`
- Wayland/Plasma 下的 Ctrl+Alt+F 由合成器/内核处理，没有统一配置可 100% 禁用；
  当前用“无 getty/无登录 VT + 锁屏关闭 + GRUB 密码”降低风险。

## 注意：隐藏不等于安全

GRUB 菜单隐藏只是体验/防误触，不是安全边界。懂的人仍可以：

- 开机按 Esc/Shift 调出 GRUB
- 进 BIOS 改启动顺序
- 用 U 盘启动 live 系统

真正要防考试机被改启动项，还需要：

- BIOS 密码
- 禁用 USB 启动
- 固定从内置硬盘启动
- 必要时 Secure Boot / TPM
