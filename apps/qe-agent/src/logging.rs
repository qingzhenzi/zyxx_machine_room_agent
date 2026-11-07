//! daemon 日志环形缓冲：tracing 输出同时写 stderr 与内存，供本机 IPC `/v1/logs` 读取。
//!
//! 只在 `qe-agent serve --ipc-socket ...` 时安装；未启用 IPC 时各角色沿用自己原来的
//! `init_tracing`（行为不变）。安装后角色的 `try_init` 会静默失败，日志统一走这里。

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriter;

/// 内存里最多保留的日志行数（不落盘，重启清空）。
pub const MAX_LOG_LINES: usize = 2000;

/// 可跨线程共享的日志环形缓冲。
#[derive(Clone, Default)]
pub struct LogBuffer {
    inner: Arc<Mutex<VecDeque<String>>>,
}

impl LogBuffer {
    pub fn push(&self, line: String) {
        if line.trim().is_empty() {
            return;
        }
        let mut lines = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if lines.len() >= MAX_LOG_LINES {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    /// 取最近 `limit` 行（按时间顺序，旧 → 新）。
    pub fn snapshot(&self, limit: usize) -> Vec<String> {
        let lines = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let limit = limit.clamp(1, MAX_LOG_LINES);
        let skip = lines.len().saturating_sub(limit);
        lines.iter().skip(skip).cloned().collect()
    }
}

/// `MakeWriter`：每个事件一个 writer，写 stderr 的同时按行切进环形缓冲。
#[derive(Clone)]
pub struct TeeMakeWriter {
    buffer: LogBuffer,
    pending: Arc<Mutex<String>>,
}

impl TeeMakeWriter {
    fn new(buffer: LogBuffer) -> Self {
        Self {
            buffer,
            pending: Arc::new(Mutex::new(String::new())),
        }
    }
}

/// 单个事件的 writer。
pub struct TeeWriter {
    buffer: LogBuffer,
    pending: Arc<Mutex<String>>,
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut stderr = io::stderr();
        stderr.write_all(buf)?;
        if !buf.is_empty() {
            let text = String::from_utf8_lossy(buf);
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pending.push_str(&text);
            while let Some(index) = pending.find('\n') {
                let line: String = pending.drain(..=index).collect();
                self.buffer
                    .push(line.trim_end_matches(['\r', '\n']).to_owned());
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()
    }
}

impl<'a> MakeWriter<'a> for TeeMakeWriter {
    type Writer = TeeWriter;

    fn make_writer(&'a self) -> Self::Writer {
        TeeWriter {
            buffer: self.buffer.clone(),
            pending: Arc::clone(&self.pending),
        }
    }
}

/// 安装带环形缓冲的 tracing subscriber（只应调用一次；失败静默，保持进程可用）。
pub fn init(buffer: LogBuffer) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,qe_agent=debug,qe_agent_node=debug,qe_agent_telemetry=debug")
    });
    let make = TeeMakeWriter::new(buffer);
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(true)
        .with_writer(make)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_keeps_last_lines_and_skips_blank() {
        let buffer = LogBuffer::default();
        for index in 0..(MAX_LOG_LINES + 5) {
            buffer.push(format!("line-{index}"));
        }
        buffer.push("   ".to_owned());

        let lines = buffer.snapshot(MAX_LOG_LINES);
        assert_eq!(lines.len(), MAX_LOG_LINES);
        assert_eq!(lines.first().map(String::as_str), Some("line-5"));
        assert_eq!(
            lines.last().map(String::as_str),
            Some(format!("line-{}", MAX_LOG_LINES + 4).as_str())
        );
        assert_eq!(buffer.snapshot(3).len(), 3);
    }
}
