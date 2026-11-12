//! 输入注入 + 单向剪贴板。
//!
//! - Linux：uinput 虚拟键鼠（helper 子进程）+ KDE Klipper 剪贴板；
//! - Windows：SendInput 直接注入，无需子进程；剪贴板走 Win32 API。
//! 两者的注入语义在 `Injector` 上统一：move/button/key/wheel。
//!
//! 注意：键盘码在两平台不同 —— Linux 用 evdev KEY_*，Windows 用 VK 码。
//! 控制通道协议传 evdev 码，Windows 端用 `windows::evdev_to_vk` 转换
//! （不支持的部分静默丢弃，避免注入随机键）。

use anyhow::{Result};
#[cfg(target_os = "linux")]
use anyhow::Context;
#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader, Write};
#[cfg(target_os = "linux")]
use std::process::{Child, Command, Stdio};
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn lan_video_uinput_inject() -> i32;
}

#[cfg(target_os = "linux")]
pub fn run_uinput() -> i32 {
    unsafe { lan_video_uinput_inject() }
}

/// 长生命周期注入器：Linux 是 uinput helper 子进程，Windows 是无状态 SendInput。
pub enum Injector {
    #[cfg(target_os = "linux")]
    Uinput { child: Child },
    #[cfg(target_os = "windows")]
    SendInput,
}

impl Injector {
    pub fn spawn() -> Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let exe = std::env::current_exe().context("获取当前可执行文件")?;
            let mut child = Command::new(exe)
                .arg("uinput-inject")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .context("启动 uinput-inject helper 失败")?;
            let stdout = child.stdout.take().context("uinput helper stdout 不可用")?;
            let mut line = String::new();
            BufReader::new(stdout)
                .read_line(&mut line)
                .context("等待 uinput helper 就绪失败")?;
            if line.trim() != "READY" {
                let _ = child.kill();
                anyhow::bail!("uinput helper 未就绪（{line:?}）");
            }
            // 设备注册到 X11/合成器还需要一点时间，太早注入会丢开头几个键。
            std::thread::sleep(Duration::from_millis(350));
            Ok(Self::Uinput { child })
        }
        #[cfg(target_os = "windows")]
        {
            Ok(Self::SendInput)
        }
    }

    #[cfg(target_os = "linux")]
    fn send(&mut self, record: [u8; 5]) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let child = match self {
                Self::Uinput { child } => child,
                #[cfg(target_os = "windows")]
                Self::SendInput => unreachable!(),
            };
            let stdin = child
                .stdin
                .as_mut()
                .context("uinput helper stdin 不可用")?;
            stdin.write_all(&record)?;
            stdin.flush()?;
            Ok(())
        }
        #[cfg(target_os = "windows")]
        {
            let _ = record;
            Ok(())
        }
    }

    pub fn mouse_move(&mut self, dx: i16, dy: i16) -> Result<()> {
        #[cfg(target_os = "windows")]
        if matches!(self, Self::SendInput) {
            return crate::windows::move_mouse(dx, dy);
        }
        #[cfg(target_os = "linux")]
        {
            self.send(record(2, 0, dx))?; // REL_X
            self.send(record(2, 1, dy))?; // REL_Y
        }
        Ok(())
    }

    pub fn mouse_button(&mut self, code: u16, down: bool) -> Result<()> {
        #[cfg(target_os = "windows")]
        if matches!(self, Self::SendInput) {
            return crate::windows::mouse_button(code, down);
        }
        #[cfg(target_os = "linux")]
        self.send(record(1, code, i16::from(down)))?;
        Ok(())
    }

    pub fn key(&mut self, code: u16, down: bool) -> Result<()> {
        #[cfg(target_os = "windows")]
        if matches!(self, Self::SendInput) {
            // evdev KEY_* -> VK；未映射的键丢弃（避免注入错键）
            let Some(vk) = crate::windows::evdev_to_vk(code) else {
                return Ok(());
            };
            return crate::windows::key(vk, down);
        }
        #[cfg(target_os = "linux")]
        self.send(record(1, code, i16::from(down)))?;
        Ok(())
    }

    pub fn wheel(&mut self, delta: i16) -> Result<()> {
        #[cfg(target_os = "windows")]
        if matches!(self, Self::SendInput) {
            return crate::windows::wheel(delta);
        }
        #[cfg(target_os = "linux")]
        self.send(record(2, 8, delta))?; // REL_WHEEL
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Drop for Injector {
    fn drop(&mut self) {
        let Self::Uinput { child } = self;
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(target_os = "linux")]
fn record(kind: u8, code: u16, value: i16) -> [u8; 5] {
    let v = value as u16;
    [
        kind,
        (code & 0xff) as u8,
        (code >> 8) as u8,
        (v & 0xff) as u8,
        (v >> 8) as u8,
    ]
}

/// 单向剪贴板：控制端文本 → 被控端。
/// Linux 走 KDE Klipper；Windows 走 Win32 剪贴板。
pub fn set_clipboard(text: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        crate::windows::set_clipboard(text)
    }
    #[cfg(target_os = "linux")]
    {
        let status = Command::new("busctl")
            .args([
                "--user",
                "call",
                "org.kde.klipper",
                "/klipper",
                "org.kde.klipper.klipper",
                "setClipboardContents",
                "s",
                text,
            ])
            .status()
            .context("调用 Klipper setClipboardContents 失败")?;
        if !status.success() {
            anyhow::bail!("Klipper 拒绝剪贴板写入（{status}）");
        }
        Ok(())
    }
}

/// evdev 键码常用常量（viewer 也可以直接发这些值）。
pub mod keys {
    pub const BTN_LEFT: u16 = 0x110;
    pub const BTN_RIGHT: u16 = 0x111;
    pub const BTN_MIDDLE: u16 = 0x112;
}
