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
use std::time::Duration;

use crate::boot::config::DisplayConfig;
use crate::boot::state::SharedState;

/// 已拉起的子进程（供后续 supervise）。
static DISPLAY_CHILD: Mutex<Option<Child>> = Mutex::new(None);

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

/// supervise 线程：boot-display 崩溃后按退避重启。
///
/// 拿不到子进程句柄（未启用/启动失败）时直接退出，不做任何事。
pub fn supervise(config: DisplayConfig) {
    let path = match binary_path(&config) {
        Some(path) => path,
        None => return,
    };
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);

    loop {
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
            // 正常运行时每 5s 轮询一次。
            std::thread::sleep(Duration::from_secs(5));
        }
    }
}
