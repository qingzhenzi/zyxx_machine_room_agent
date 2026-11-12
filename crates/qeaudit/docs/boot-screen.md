# 开机画面：让程序最早、独占地上屏

目标：开机后**你的程序最先出现在屏幕上，并一直占着**，不被内核日志、getty、plymouth 或桌面环境覆盖。

## 1. 开机时谁在抢屏幕

按时间顺序，屏幕上会依次出现四个主人：

| 顺序 | 主人 | 何时退出 |
| --- | --- | --- |
| 1 | 内核 console（fbcon） | 一直存在，除非被覆盖 |
| 2 | plymouth（如果装了） | `plymouth-quit` 释放 DRM master |
| 3 | display manager / getty | 常驻，等你登录 |
| 4 | **你的程序** | 你说了算 |

要"优先显示"，本质是**排掉 1、2、3，让 4 尽早拿到屏幕**。任何一步没排干净，表现都是：程序起来了但屏幕被抢回去 / 闪一下黑屏 / 只在某个 tty 有画面。

## 2. 四条路线

| 路线 | 出画面速度 | 开发成本 | 适用 |
| --- | --- | --- | --- |
| A. 文本界面接管 `tty1` | 快 | 最低 | 状态屏、进度、日志、TUI |
| B. Wayland kiosk（cage + 你的窗口） | 中 | 低 | 需要真正的 GUI，有 GPU 驱动 |
| C. DRM/KMS 直绘（无显示服务器） | **最快** | 高 | 嵌入式/一体机，要 logo/动画 |
| D. plymouth 主题 | 最早 | 中 | 只要开机 logo，最终仍要交给你的程序 |

> 只有 C 能早到 initramfs 阶段（根分区还没挂载时）。A、B 都要等 systemd 起来。

## 3. 方案 A：文本界面接管 tty1

`/etc/systemd/system/boot-screen.service`：

```ini
[Unit]
Description=Boot screen (takes over tty1)
After=systemd-vconsole-setup.service plymouth-quit-wait.service
Before=getty@tty1.service
Conflicts=getty@tty1.service
# 只想在正常启动时出现，救援模式不抢屏
ConditionKernelCommandLine=!systemd.unit=rescue.target

[Service]
Type=notify
ExecStart=/usr/local/bin/myapp --boot-screen
StandardInput=tty
StandardOutput=tty
StandardError=tty
TTYPath=/dev/tty1
TTYReset=yes
TTYVHangup=yes
TTYVTDisallocate=yes
Restart=always
RestartSec=1
# 崩溃重启不要被 start-limit 挡住（否则 5 次之后屏幕就再也回不来了）
StartLimitIntervalSec=0
Nice=-5
OOMScoreAdjust=-500

[Install]
WantedBy=multi-user.target
```

再禁用 tty1 的登录：

```bash
sudo systemctl mask getty@tty1.service
```

程序侧要点：

```rust
use std::io::{stdout, Write};

fn main() {
    // 清屏 + 隐藏光标，避免内核日志残影和光标闪烁
    print!("\x1b[2J\x1b[H\x1b[?25l");
    stdout().flush().ok();

    draw_first_frame();          // 先把画面画出来

    // 首帧完成后再告诉 systemd「我准备好了」，用于依赖排序和启动耗时统计
    notify_ready();              // 见下

    event_loop();
}

fn notify_ready() {
    // 简单实现可直接用 sd-notify crate: sd_notify::notify(true, &[NotifyState::Ready])
    if let Ok(addr) = std::env::var("NOTIFY_SOCKET") {
        if let Ok(sock) = std::os::unix::net::UnixDatagram::unbound() {
            let _ = sock.send_to(b"READY=1", addr.trim_start_matches('@'));
        }
    }
}
```

`Type=notify` + `READY=1` 的价值：systemd 只有在**你真正画出第一帧**后才认为服务启动完成，`systemd-analyze critical-chain` 才能反映真实的上屏时间。

## 4. 方案 B：Wayland kiosk

不用自己写合成器，用 `cage`（单客户端 kiosk 合成器）：

```ini
[Unit]
Description=Kiosk UI
After=plymouth-quit-wait.service systemd-user-sessions.service
Conflicts=getty@tty1.service
Before=getty@tty1.service

[Service]
Type=simple
# cage 自己会拿 DRM master，并处理 VT 切换
ExecStart=/usr/bin/cage -- /usr/local/bin/myapp --kiosk
Restart=always
TTYPath=/dev/tty1
StandardInput=tty
TTYReset=yes
TTYVHangup=yes
TTYVTDisallocate=yes

[Install]
WantedBy=multi-user.target
```

Rust 侧用 `winit` / `slint` / `egui` 写普通窗口即可，`cage` 会把它铺满屏。这是**性价比最高**的图形方案：VT 切换、DRM master、输入设备都由 cage 处理。

## 5. 方案 C：DRM/KMS 直绘

没有任何显示服务器，你的进程直接是 DRM master：

- **Slint**：`backend-linuxkms` feature，`SLINT_BACKEND=linuxkms`，官方就是为这种场景做的；
- **Smithay**：自己写最小合成器，控制力最强；
- **drm-rs**：自己管 framebuffer / page flip，最底层。

代价：VT 切换要自己处理（用 `libseat` 拿 session），退出时要恢复 console 状态，GPU 驱动差异要自己填。**除非 A/B 满足不了，不建议一上来就走这条。**

## 6. 内核参数（决定"看不看得见"）

`/etc/default/grub`：

```bash
GRUB_CMDLINE_LINUX_DEFAULT="quiet loglevel=0 vt.global_cursor_default=0 plymouth.enable=0 systemd.show_status=0"
```

| 参数 | 作用 |
| --- | --- |
| `quiet` | 关掉大部分内核启动日志 |
| `loglevel=0` | 连 KERN_EMERG 也不打印到 console（排障时会变聋，出问题记得去掉） |
| `vt.global_cursor_default=0` | 隐藏光标，避免 TUI 边缘闪烁 |
| `plymouth.enable=0` | 干脆不用 plymouth，少一个抢 DRM 的 |
| `systemd.show_status=0` | 关掉 `[ OK ] Started ...` 那堆绿字 |

改完 `grub-mkconfig -o /boot/grub/grub.cfg` 并重启。

## 7. 常见坑

1. **闪一下黑屏**：plymouth 还握着 DRM master。要么 `After=plymouth-quit-wait.service`，要么 `plymouth.enable=0`。
2. **画面被 getty 顶掉**：只写 `Before=` 不够，必须 `Conflicts=getty@tty1.service` 且 `systemctl mask getty@tty1`。
3. **崩溃后屏幕回不来**：`Restart=always` 默认 10 秒内 5 次就放弃，加 `StartLimitIntervalSec=0`。
4. **字体不对/花屏**：`After=systemd-vconsole-setup.service` 没加，vconsole 还没设好字体你就开始画了。
5. **切到别的 VT 再切回来是黑的**：直接 KMS 的路线必须处理 VT switch（`libseat`），否则 console 状态没恢复。
6. **`loglevel=0` 把崩溃信息也吞了**：调试期用 `loglevel=7`，发布前再收紧。
7. **远程/无显示器机器**：这些配置没有意义，用 `ConditionPathExists=/dev/dri/card0` 之类的条件跳过。

## 8. 量化上屏时间

```bash
systemd-analyze                      # 总启动时间
systemd-analyze blame | head -20     # 谁拖慢了
systemd-analyze critical-chain boot-screen.service
```

`boot-screen.service` 后面显示的时间，就是"从内核启动到你的画面出现"的真实值。要压这个数，先看 `critical-chain`，通常是 plymouth、NetworkManager-wait-online、随机数初始化在拖。

## 9. 和 qeaudit 的关系

如果只是想把**上次扫描结果**开机显示出来（而不是做一个常驻 UI），最省事的是：

```ini
# 覆盖 qeaudit.service，开机跑一次并把报告打到 tty1
[Service]
Type=oneshot
ExecStart=/usr/local/bin/qeaudit --config /etc/qeaudit/baseline.toml scan
StandardOutput=tty
TTYPath=/dev/tty1
```

但 oneshot 跑完就退出，屏幕会停在最后一行、然后被 getty 接管。要"停住"，得在程序末尾加一个 `read`/按键等待，或者用方案 A 的常驻形态。
