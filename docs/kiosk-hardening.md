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

## boot-display 置顶/防关闭（KDE Wayland）

- Tauri `alwaysOnTop` 在 KWin Wayland 不一定可靠，安装器额外安装 KWin 脚本
  `qe-kiosk`：窗口 added/activated 时强制 `keepAbove/fullScreen/skipTaskbar/noBorder`。
- boot-display 拦截 `CloseRequested`，Alt+F4/窗口关闭不会结束展示层。
- 安装器禁用 KWin/Plasma 常用快捷键：Alt+F4、Alt+F3、Alt+Tab、Meta 启动器、
  Ctrl+Alt+Del 注销等；这些快捷键下次登录生效。
- 调试解锁：`Ctrl+Alt+Shift+U`，输入 `QE_BOOT_DISPLAY_DEBUG_PASSWORD`
  （安装默认 `QEBootDebug2026!`，可改）后通知 agent 停止 supervise 并退出展示层。
- 真机验证：KWin 脚本日志 `QE-KIOSK enforced`，截图确认全屏置顶；
  agent IPC `/v1/display/unlock` 返回 ok 并结束 boot-display。

## 首次启动也全屏置顶

- KWin 脚本除了 `windowAdded/windowActivated`，加载时还会用 `workspace.windowList()`
  遍历已存在窗口补一次 enforce，解决“boot-display 已存在但脚本后加载”首次漏掉的问题。
- Plasma 面板默认会盖在 boot-display 上；安装器设置 `plasmashellrc: panelVisibility=1`
  并写 `plasmanotifyrc DoNotDisturb=true`，避免任务栏/Fcitx 通知等桌面 UI 露出。
- 真机重启验证：boot-display 首次启动即全屏，截图只有 QESTUDIO 画面，无面板/通知。

### 新窗口/其他应用盖住 boot-display 的处理

- KWin 6 的 `workspace.windowClosed` 信号不可用，脚本连接它会导致整个 qe-kiosk 脚本加载失败；
  已去掉该连接，改用 `windowList()` 判断 boot window 是否仍存活。
- boot-display 存在期间：新窗口立即 `minimized=true`，激活其他窗口会被强制拉回
  boot-display；同时 Plasma 面板自动隐藏 + 勿扰。
- 真机验证：在 boot-display 运行时启动 Discover，截图仍只有 QESTUDIO 画面。
