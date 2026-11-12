//! Windows 平台支持：窗口枚举（EnumWindows）、输入注入（SendInput）、剪贴板。
//!
//! 只依赖 `windows-sys`（Win32 FFI），不引入 GStreamer 绑定；
//! 非 Windows 平台这些函数返回「不支持」错误。

use anyhow::Result;
#[cfg(not(target_os = "windows"))]
use anyhow::bail;
use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct WindowInfo {
    /// 传给 `--window-handle` 的 HWND。
    pub hwnd: u64,
    pub title: String,
    pub app: String,
}

/// evdev BTN_LEFT/RIGHT/MIDDLE。
pub mod mouse_codes {
    pub const BTN_LEFT: u16 = 0x110;
    pub const BTN_RIGHT: u16 = 0x111;
    pub const BTN_MIDDLE: u16 = 0x112;
}

pub fn list_windows() -> Result<Vec<WindowInfo>> {
    #[cfg(target_os = "windows")]
    {
        windows_list_windows()
    }
    #[cfg(not(target_os = "windows"))]
    {
        bail!("窗口枚举仅支持 Windows")
    }
}

pub fn move_mouse(dx: i16, dy: i16) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        mouse_send(MOUSEEVENTF_MOVE, 0, dx as i32, dy as i32, 0)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (dx, dy);
        bail!("输入注入仅支持 Windows")
    }
}

pub fn mouse_button(evdev_code: u16, down: bool) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        let (down_flag, up_flag) = match evdev_code {
            mouse_codes::BTN_RIGHT => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
            mouse_codes::BTN_MIDDLE => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
            _ => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        };
        mouse_send(if down { down_flag } else { up_flag }, 0, 0, 0, 0)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (evdev_code, down);
        bail!("输入注入仅支持 Windows")
    }
}

/// 垂直滚轮；delta 为行数，Windows 按 WHEEL_DELTA=120 换算。
pub fn wheel(delta: i16) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        mouse_send(MOUSEEVENTF_WHEEL, 0, 0, 0, delta as i32 * 120)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = delta;
        bail!("输入注入仅支持 Windows")
    }
}

/// Windows VK 码按键。
pub fn key(vk: u16, down: bool) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        keyboard_send(vk, down, 0)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (vk, down);
        bail!("输入注入仅支持 Windows")
    }
}

/// Unicode 字符注入（KEYEVENTF_UNICODE），用于跨平台文本输入。
pub fn unicode_char(ch: char) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        let code = u32::from(ch);
        if code <= 0xFFFF {
            keyboard_scan(code as u16, true);
            keyboard_scan(code as u16, false);
            Ok(())
        } else {
            // 代理对：高低两个 UTF-16 码元
            let mut buf = [0u16; 2];
            for unit in ch.encode_utf16(&mut buf) {
                keyboard_scan(*unit, true);
                keyboard_scan(*unit, false);
            }
            Ok(())
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = ch;
        bail!("输入注入仅支持 Windows")
    }
}

/// evdev KEY_* -> Windows VK 码（常用键）。未覆盖的返回 None，调用方应丢弃。
pub fn evdev_to_vk(code: u16) -> Option<u16> {
    Some(match code {
        // 数字排：KEY_1(2)..KEY_0(11)
        2..=10 => 0x30 + code - 1,    // '1'..'9'
        11 => 0x30,                   // KEY_0
        // QWERTY 排：Q(16)..P(25) -> 'Q'(0x51)..'P'(0x50)
        16 => 0x51, // Q
        17 => 0x57, // W
        18 => 0x45, // E
        19 => 0x52, // R
        20 => 0x54, // T
        21 => 0x59, // Y
        22 => 0x55, // U
        23 => 0x49, // I
        24 => 0x4F, // O
        25 => 0x50, // P
        // Home 排：A(30)..L(38)
        30 => 0x41, // A
        31 => 0x53, // S
        32 => 0x44, // D
        33 => 0x46, // F
        34 => 0x47, // G
        35 => 0x48, // H
        36 => 0x4A, // J
        37 => 0x4B, // K
        38 => 0x4C, // L
        // Bottom 排：Z(44)..M(50)
        44 => 0x5A, // Z
        45 => 0x58, // X
        46 => 0x43, // C
        47 => 0x56, // V
        48 => 0x42, // B
        49 => 0x4E, // N
        50 => 0x4D, // M
        57 => 0x20,                   // KEY_SPACE
        28 | 96 => 0x0D,              // ENTER
        14 => 0x08,                   // BACKSPACE
        15 => 0x09,                   // TAB
        1 => 0x1B,                    // ESC
        51 => 0xBC,                   // COMMA
        52 => 0xBE,                   // DOT
        53 => 0xBF,                   // SLASH
        12 => 0xBD,                   // MINUS
        13 => 0xBB,                   // EQUAL
        39 => 0xBA,                   // SEMICOLON
        40 => 0xDE,                   // APOSTROPHE
        41 => 0xC0,                   // GRAVE
        26 => 0xDB,                   // LEFTBRACE
        27 => 0xDD,                   // RIGHTBRACE
        43 => 0xDC,                   // BACKSLASH
        105 => 0x25,                  // LEFT
        106 => 0x27,                  // RIGHT
        103 => 0x26,                  // UP
        108 => 0x28,                  // DOWN
        111 => 0x2E,                  // DELETE
        110 => 0x2D,                  // INSERT
        102 => 0x24,                  // HOME
        107 => 0x23,                  // END
        104 => 0x21,                  // PAGEUP
        109 => 0x22,                  // PAGEDOWN
        42 => 0x10,                   // LEFTSHIFT
        54 => 0x10,                   // RIGHTSHIFT
        29 => 0x11,                   // LEFTCTRL
        97 => 0x11,                   // RIGHTCTRL
        56 => 0x12,                   // LEFTALT
        100 => 0x12,                  // RIGHTALT
        125 => 0x5B,                  // LEFTMETA
        126 => 0x5C,                  // RIGHTMETA
        58 => 0x14,                   // CAPSLOCK
        59..=68 => 0x70 + code - 59,  // F1..F10
        87 => 0x7A,                   // F11
        88 => 0x7B,                   // F12
        _ => return None,
    })
}

pub fn set_clipboard(text: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        windows_set_clipboard(text)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = text;
        bail!("剪贴板写入仅支持 Windows")
    }
}

// ---- Win32 实现 ----

#[cfg(target_os = "windows")]
mod win {
    use anyhow::{Result, bail};
    use std::sync::mpsc;
    use super::WindowInfo;

    const WS_EX_TOOLWINDOW: u32 = 0x80;

    unsafe fn process_name(pid: u32) -> Option<String> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        };
        if pid == 0 {
            return None;
        }
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
            CloseHandle(handle);
            (ok != 0).then(|| {
                String::from_utf16_lossy(&buf[..len as usize])
                    .rsplit('\\')
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
        }
    }

    pub unsafe fn list_windows() -> Result<Vec<WindowInfo>> {
        use windows_sys::Win32::Foundation::{HWND, LPARAM};
        use windows_sys::core::BOOL;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
            GetWindowLongPtrW, GWL_EXSTYLE, IsIconic, IsWindowVisible,
        };

        unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
            unsafe {
                let tx = &*(lparam as *const mpsc::Sender<WindowInfo>);
                if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
                    return 1;
                }
                // WS_EX_TOOLWINDOW（工具提示等）跳过
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
                if ex & WS_EX_TOOLWINDOW != 0 {
                    return 1;
                }
                let len = GetWindowTextLengthW(hwnd);
                if len <= 0 {
                    return 1;
                }
                let mut buf = vec![0u16; len as usize + 1];
                GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
                let title = String::from_utf16_lossy(&buf[..len as usize]);
                if title.trim().is_empty() {
                    return 1;
                }
                let mut pid: u32 = 0;
                GetWindowThreadProcessId(hwnd, &mut pid);
                let _ = tx.send(WindowInfo {
                    hwnd: hwnd as u64,
                    title,
                    app: process_name(pid).unwrap_or_default(),
                });
            }
            1
        }

        let (tx, rx) = mpsc::channel();
        let lparam = &tx as *const _ as LPARAM;
        unsafe {
            if EnumWindows(Some(callback), lparam) == 0 {
                return Err(anyhow::anyhow!("EnumWindows 失败"));
            }
        }
        drop(tx);
        let mut out: Vec<WindowInfo> = rx.into_iter().collect();
        out.dedup_by(|a, b| a.hwnd == b.hwnd);
        Ok(out)
    }

    fn send_input(input: windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT) -> Result<()> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::SendInput;
        let sent = unsafe { SendInput(1, &input, std::mem::size_of_val(&input) as i32) };
        if sent != 1 {
            bail!("SendInput 失败（可能被 UIPI 拦截：目标窗口以管理员运行而本进程没有）");
        }
        Ok(())
    }

    pub fn mouse(flags: u32, data: u32, dx: i32, dy: i32, wheel_delta: i32) -> Result<()> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_MOUSE, MOUSEINPUT,
        };
        let mut input: INPUT = unsafe { std::mem::zeroed() };
        input.r#type = INPUT_MOUSE;
        let mi = MOUSEINPUT {
            dx,
            dy,
            mouseData: if wheel_delta != 0 { wheel_delta as u32 } else { data },
            dwFlags: flags,
            time: 0,
            dwExtraInfo: 0,
        };
        input.Anonymous.mi = mi;
        send_input(input)
    }

    pub fn keyboard(vk: u16, down: bool) -> Result<()> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_KEYBOARD, KEYBDINPUT};
        let mut input: INPUT = unsafe { std::mem::zeroed() };
        input.r#type = INPUT_KEYBOARD;
        input.Anonymous.ki = KEYBDINPUT {
            wVk: vk,
            wScan: 0,
            dwFlags: if down { 0 } else { KEYEVENTF_KEYUP },
            time: 0,
            dwExtraInfo: 0,
        };
        send_input(input)
    }

    use windows_sys::Win32::UI::Input::KeyboardAndMouse::KEYEVENTF_KEYUP;

    pub fn keyboard_scan(scan: u16, down: bool) {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_UNICODE,
        };
        let mut input: INPUT = unsafe { std::mem::zeroed() };
        input.r#type = INPUT_KEYBOARD;
        input.Anonymous.ki = KEYBDINPUT {
            wVk: 0,
            wScan: scan,
            dwFlags: KEYEVENTF_UNICODE | if down { 0 } else { KEYEVENTF_KEYUP },
            time: 0,
            dwExtraInfo: 0,
        };
        unsafe {
            windows_sys::Win32::UI::Input::KeyboardAndMouse::SendInput(
                1,
                &input,
                std::mem::size_of_val(&input) as i32,
            );
        }
    }

    pub fn set_clipboard(text: &str) -> Result<()> {
        use windows_sys::Win32::System::DataExchange::{
            CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
        };
        use windows_sys::Win32::System::Memory::{
            GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
        };
        use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

        if text.is_empty() {
            return Ok(());
        }
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0);
        let bytes = wide.len() * 2;

        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                bail!("OpenClipboard 失败");
            }
            struct Guard;
            impl Drop for Guard {
                fn drop(&mut self) {
                    unsafe { CloseClipboard() };
                }
            }
            let _guard = Guard;
            EmptyClipboard();
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if handle.is_null() {
                bail!("GlobalAlloc 失败");
            }
            let dst = GlobalLock(handle);
            if dst.is_null() {
                bail!("GlobalLock 失败");
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), dst as *mut u16, wide.len());
            GlobalUnlock(handle);
                if SetClipboardData(CF_UNICODETEXT as u32, handle as _).is_null() {
                    bail!("SetClipboardData 失败");
                }
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_WHEEL,
};

#[cfg(target_os = "windows")]
fn windows_list_windows() -> Result<Vec<WindowInfo>> {
    unsafe { win::list_windows() }
}
#[cfg(target_os = "windows")]
fn mouse_send(flags: u32, data: u32, dx: i32, dy: i32, wheel_delta: i32) -> Result<()> {
    win::mouse(flags, data, dx, dy, wheel_delta)
}
#[cfg(target_os = "windows")]
fn keyboard_send(vk: u16, down: bool, _extra: u32) -> Result<()> {
    win::keyboard(vk, down)
}
#[cfg(target_os = "windows")]
fn keyboard_scan(scan: u16, down: bool) {
    win::keyboard_scan(scan, down)
}
#[cfg(target_os = "windows")]
fn windows_set_clipboard(text: &str) -> Result<()> {
    win::set_clipboard(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// evdev → VK 映射：字母/数字/功能键/修饰键/方向键。
    /// 这些映射可在任意平台测试（纯表查询，不触 Win32）。
    #[test]
    fn evdev_to_vk_maps_common_keys() {
        assert_eq!(evdev_to_vk(30), Some(0x41), "KEY_A -> VK_A");
        assert_eq!(evdev_to_vk(44), Some(0x5A), "KEY_Z -> VK_Z");
        assert_eq!(evdev_to_vk(2), Some(0x31), "KEY_1 -> '1'");
        assert_eq!(evdev_to_vk(11), Some(0x30), "KEY_0 -> '0'");
        assert_eq!(evdev_to_vk(57), Some(0x20), "KEY_SPACE -> VK_SPACE");
        assert_eq!(evdev_to_vk(28), Some(0x0D), "KEY_ENTER -> VK_RETURN");
        assert_eq!(evdev_to_vk(29), Some(0x11), "KEY_LEFTCTRL -> VK_CONTROL");
        assert_eq!(evdev_to_vk(105), Some(0x25), "KEY_LEFT -> VK_LEFT");
        assert_eq!(evdev_to_vk(108), Some(0x28), "KEY_DOWN -> VK_DOWN");
        assert_eq!(evdev_to_vk(59), Some(0x70), "KEY_F1 -> VK_F1");
        assert_eq!(evdev_to_vk(88), Some(0x7B), "KEY_F12 -> VK_F12");
    }

    #[test]
    fn evdev_to_vk_rejects_unknown() {
        assert_eq!(evdev_to_vk(0), None);
        assert_eq!(evdev_to_vk(999), None);
    }

    /// 非 Windows 平台调用注入/枚举/剪贴板应返回错误（不 panic）。
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn non_windows_returns_unsupported_errors() {
        assert!(move_mouse(1, 1).is_err());
        assert!(mouse_button(mouse_codes::BTN_LEFT, true).is_err());
        assert!(wheel(1).is_err());
        assert!(key(0x41, true).is_err());
        assert!(list_windows().is_err());
        assert!(set_clipboard("x").is_err());
    }
}
