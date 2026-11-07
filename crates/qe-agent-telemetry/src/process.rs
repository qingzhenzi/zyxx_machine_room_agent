//! 带硬超时的子进程执行器。
//!
//! 外部工具（platform-health 等）可能挂起（磁盘/NFS/docker daemon 卡住），
//! 因此每次调用都有硬超时：超时后 `kill` 子进程并返回 [`CommandOutput::timed_out`]。
//! stdout/stderr 用独立线程持续读走，避免子进程输出超过 pipe 缓冲区时死锁。
//!
//! 统一设置 `LC_ALL=C`，让 `docker` / `virsh` 的输出字段名保持英文，解析稳定。

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// 轮询子进程状态的间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// 一次子进程调用的结果。
#[derive(Debug, Clone, Default)]
pub struct CommandOutput {
    /// 退出码为 0。
    pub success: bool,
    /// 退出码（被信号杀死时为 None）。
    pub code: Option<i32>,
    /// 标准输出（按 UTF-8 lossy 解码）。
    pub stdout: String,
    /// 标准错误（按 UTF-8 lossy 解码）。
    pub stderr: String,
    /// 实际耗时。
    pub duration: Duration,
    /// 是否因超时被杀。
    pub timed_out: bool,
}

/// 运行 `bin args...` 并等待至多 `timeout`。
///
/// 启动失败返回 `Err`（可执行文件不存在/无权限）；其余情况（非 0 退出码、
/// 超时）返回 `Ok`，由调用方决定如何记录。
pub fn run_with_timeout(
    bin: &str,
    args: &[String],
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut child = Command::new(bin)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("启动 `{bin}` 失败: {error}"))?;

    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| format!("无法读取 `{bin}` 的 stdout"))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| format!("无法读取 `{bin}` 的 stderr"))?;
    let stdout_reader = thread::spawn(move || read_all(stdout_pipe));
    let stderr_reader = thread::spawn(move || read_all(stderr_pipe));

    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if started.elapsed() >= timeout {
                    // 先杀再回收，避免僵尸；读取线程在 pipe 关闭后自然结束。
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                thread::sleep(POLL_INTERVAL);
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                join_reader(stdout_reader);
                join_reader(stderr_reader);
                return Err(format!("等待 `{bin}` 退出失败: {error}"));
            }
        }
    };

    let stdout = String::from_utf8_lossy(&join_reader(stdout_reader)).into_owned();
    let stderr = String::from_utf8_lossy(&join_reader(stderr_reader)).into_owned();
    let duration = started.elapsed();

    Ok(CommandOutput {
        success: status.is_some_and(|status| status.success()),
        code: status.and_then(|status| status.code()),
        stdout,
        stderr,
        duration,
        timed_out,
    })
}

fn read_all(mut reader: impl Read) -> Vec<u8> {
    let mut buffer = Vec::new();
    // 读取错误（管道被关闭等）视为输出结束，不向上传播。
    let _ = reader.read_to_end(&mut buffer);
    buffer
}

fn join_reader(handle: thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn successful_command_captures_stdout() {
        let output = run_with_timeout(
            "sh",
            &args(&["-c", "printf 'hello'; printf 'warn' >&2"]),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(output.success);
        assert_eq!(output.code, Some(0));
        assert_eq!(output.stdout, "hello");
        assert_eq!(output.stderr, "warn");
        assert!(!output.timed_out);
    }

    #[test]
    fn non_zero_exit_is_reported_without_error() {
        let output =
            run_with_timeout("sh", &args(&["-c", "exit 3"]), Duration::from_secs(5)).unwrap();
        assert!(!output.success);
        assert_eq!(output.code, Some(3));
        assert!(!output.timed_out);
    }

    #[test]
    fn missing_binary_is_an_error() {
        let error = run_with_timeout("/nonexistent/room-agent-tool", &[], Duration::from_secs(1))
            .unwrap_err();
        assert!(error.contains("启动"), "{error}");
    }

    #[test]
    fn timeout_kills_command() {
        let output =
            run_with_timeout("sh", &args(&["-c", "sleep 5"]), Duration::from_millis(100)).unwrap();
        assert!(output.timed_out);
        assert!(!output.success);
        assert!(output.duration < Duration::from_secs(3));
    }

    #[test]
    fn large_output_does_not_deadlock() {
        // 256 KiB 远超 pipe 缓冲区，验证读取线程持续排空。
        let output = run_with_timeout(
            "sh",
            &args(&["-c", "head -c 262144 /dev/zero | tr '\\0' 'x'"]),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(output.success);
        assert_eq!(output.stdout.len(), 262_144);
    }
}
