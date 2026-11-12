//! Streaming payloads: logs, exec output, stats and image pulls.

use std::collections::BTreeMap;
use std::fmt;
use std::pin::Pin;

use futures_util::Stream;
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// A boxed stream of fallible values.
pub type EventStream<T> = Pin<Box<dyn Stream<Item = Result<T>> + Send>>;

/// Which stream a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    /// Container stdout.
    Stdout,
    /// Container stderr.
    Stderr,
    /// A TTY/console stream where stdout and stderr are merged.
    Console,
}

/// One line of container logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    /// Origin stream.
    pub stream: OutputStream,
    /// UTF-8 lossy text, without the trailing newline trimmed.
    pub text: String,
}

impl LogLine {
    /// Create a stdout line.
    pub fn stdout(text: impl Into<String>) -> Self {
        Self {
            stream: OutputStream::Stdout,
            text: text.into(),
        }
    }

    /// Create a stderr line.
    pub fn stderr(text: impl Into<String>) -> Self {
        Self {
            stream: OutputStream::Stderr,
            text: text.into(),
        }
    }
}

/// Options for [`crate::Engine::logs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogOptions {
    /// Keep the stream open after existing output.
    pub follow: bool,
    /// Include stdout.
    pub stdout: bool,
    /// Include stderr.
    pub stderr: bool,
    /// Prefix each line with a daemon timestamp.
    pub timestamps: bool,
    /// Number of lines from the end. `None` means all.
    pub tail: Option<usize>,
    /// Only logs after this Unix timestamp (seconds).
    pub since: Option<i64>,
    /// Only logs before this Unix timestamp (seconds).
    pub until: Option<i64>,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            follow: false,
            stdout: true,
            stderr: true,
            timestamps: false,
            tail: None,
            since: None,
            until: None,
        }
    }
}

impl LogOptions {
    /// Follow new output.
    pub fn follow() -> Self {
        Self {
            follow: true,
            ..Self::default()
        }
    }

    /// Set the tail count.
    pub fn with_tail(mut self, tail: usize) -> Self {
        self.tail = Some(tail);
        self
    }
}

/// One chunk of output from an exec session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecChunk {
    /// Origin stream.
    pub stream: OutputStream,
    /// UTF-8 lossy text.
    pub text: String,
}

/// Specification for a command executed inside a running container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecSpec {
    /// Command and arguments.
    pub command: Vec<String>,
    /// Extra environment variables.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Working directory inside the container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    /// User to run as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Allocate a TTY.
    #[serde(default)]
    pub tty: bool,
}

impl ExecSpec {
    /// Create an exec spec from a command.
    pub fn new<I, S>(command: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            command: command.into_iter().map(Into::into).collect(),
            env: BTreeMap::new(),
            workdir: None,
            user: None,
            tty: false,
        }
    }
}

/// A started exec session.
pub struct ExecSession {
    /// Daemon-side exec id, usable with inspect.
    pub id: String,
    /// Combined output stream.
    pub output: EventStream<ExecChunk>,
}

impl fmt::Debug for ExecSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecSession")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Result of a captured exec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    /// Exit code, when the daemon reported one.
    pub exit_code: Option<i64>,
    /// Captured output.
    pub output: Vec<ExecChunk>,
}

impl ExecResult {
    /// Whether the command exited successfully.
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// Captured output as one string.
    pub fn text(&self) -> String {
        self.output
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect()
    }
}

/// Progress emitted while pulling an image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullProgress {
    /// Human-readable status (for example, `Pulling from library/alpine`).
    pub status: String,
    /// Layer id, when the daemon reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Bytes transferred for the current layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<i64>,
    /// Total bytes for the current layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
}

/// A normalized stats sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatsSample {
    /// Sample timestamp, as reported by the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<String>,
    /// CPU usage in percent of one core (matching `docker stats`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    /// Current memory usage in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_usage: Option<u64>,
    /// Memory limit in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<u64>,
    /// Total received bytes across interfaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_rx: Option<u64>,
    /// Total transmitted bytes across interfaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_tx: Option<u64>,
    /// Number of PIDs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids: Option<u64>,
}
