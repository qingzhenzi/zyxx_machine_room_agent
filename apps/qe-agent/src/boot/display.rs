//! 阶段 3：按配置拉起 boot-display。
//!
//! 配置来自 `<data_dir>/agent-config.json` 的 `boot.display` 段，
//! 环境变量 `QE_BOOT_DISPLAY` / `QE_BOOT_DISPLAY_BIN` 可覆盖（见 `config.rs`）。
//!
//! agent 只负责 spawn；supervise（崩溃自动重启）由 [`supervise`] 线程负责，
//! 采用指数退避（1s 起，翻倍至 60s 封顶）。

use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::boot::config::DisplayConfig;
use crate::boot::state::SharedState;

/// 已拉起的子进程（供后续 supervise）。
static DISPLAY_CHILD: Mutex<Option<Child>> = Mutex::new(None);
/// 首次 spawn 是否已经完成（supervise 线程必须等它，否则会和初始 spawn 竞态拉起两份）。
static DISPLAY_INIT_DONE: AtomicBool = AtomicBool::new(false);
/// boot 流水线结束后要求 supervise 不再重启。
static DISPLAY_STOP: AtomicBool = AtomicBool::new(false);

struct InitDoneGuard;
impl Drop for InitDoneGuard {
    fn drop(&mut self) {
        DISPLAY_INIT_DONE.store(true, Ordering::Release);
    }
}

fn binary_path(config: &DisplayConfig) -> Option<PathBuf> {
    if let Some(path) = &config.bin {
        return Some(path.clone());
    }
    if let Some(path) = std::env::var_os("QE_BOOT_DISPLAY_BIN") {
        return Some(PathBuf::from(path));
    }
    which::which("boot-display").ok()
}

/// 尝试拉起 boot-display（spawn-and-forget；失败不中断 boot）。
pub fn spawn_if_enabled(state: &SharedState, config: &DisplayConfig) {
    let _init_done = InitDoneGuard;
    state.begin(crate::boot::Phase::Display);

    if !config.enabled {
        state.finish(
            crate::boot::Phase::Display,
            "skipped",
            "配置禁用 boot-display",
            None,
        );
        return;
    }

    let Some(path) = binary_path(config) else {
        state.finish(
            crate::boot::Phase::Display,
            "skipped",
            "找不到 boot-display 可执行文件",
            None,
        );
        return;
    };

    match Command::new(&path).spawn() {
        Ok(child) => {
            let pid = child.id();
            if let Ok(mut guard) = DISPLAY_CHILD.lock() {
                *guard = Some(child);
            }
            state.finish(
                crate::boot::Phase::Display,
                "ok",
                format!("boot-display 已启动 (pid {pid})"),
                Some(serde_json::json!({ "pid": pid, "path": path })),
            );
        }
        Err(error) => {
            state.finish(
                crate::boot::Phase::Display,
                "failed",
                format!("boot-display 启动失败: {error}"),
                None,
            );
        }
    }
}

/// boot 流水线结束后退出展示层（配置 `exit_after_boot` / 环境变量）。
pub fn shutdown() {
    DISPLAY_STOP.store(true, Ordering::Release);
    if let Ok(mut guard) = DISPLAY_CHILD.lock()
        && let Some(mut child) = guard.take()
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// supervise 线程：boot-display 崩溃后按退避重启。
///
/// 拿不到子进程句柄（未启用/启动失败）时直接退出，不做任何事。
pub fn supervise(config: DisplayConfig) {
    let path = match binary_path(&config) {
        Some(path) => path,
        None => return,
    };
    // 等首轮 spawn 完成，避免 supervise 与 spawn_if_enabled 同时拉起两份 boot-display。
    while !DISPLAY_INIT_DONE.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);

    loop {
        if DISPLAY_STOP.load(Ordering::Acquire) {
            return;
        }
        let running = {
            let mut guard = match DISPLAY_CHILD.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
            match guard.as_mut() {
                Some(child) => match child.try_wait() {
                    Ok(Some(_)) | Err(_) => {
                        // 已退出或状态未知 → 标记需要重启。
                        *guard = None;
                        true
                    }
                    Ok(None) => {
                        backoff = Duration::from_secs(1); // 正常运行，重置退避
                        false
                    }
                },
                None => true,
            }
        };

        if running {
            if let Ok(child) = Command::new(&path).spawn()
                && let Ok(mut guard) = DISPLAY_CHILD.lock()
            {
                *guard = Some(child);
            }
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(max_backoff);
        } else {
            // 正常运行时每 5s 轮询一次；shutdown() 后下一轮直接退出。
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}
