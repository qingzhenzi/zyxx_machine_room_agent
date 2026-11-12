use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use hyper::Method;
use serde_json::Value;
use tokio::time::sleep;

use super::client::{parse_envelope, IncusClient};
use super::mapping::state_to_stats;
use super::wire::{Envelope, InstanceState, Operation};
use super::{INIT_STDERR_FILE, INIT_STDOUT_FILE, POLL_INTERVAL};
use crate::error::{Error, Result};
use crate::stream::{EventStream, ExecChunk, LogLine, LogOptions, OutputStream, StatsSample};

// ---------------------------------------------------------------------------
// Streams (poll-based, built with stream::unfold)
// ---------------------------------------------------------------------------

/// A file channel polled incrementally via the files API.
pub(super) struct FileChannel {
    pub(super) path: String,
    pub(super) stream: OutputStream,
    pub(super) offset: usize,
    pub(super) partial: Vec<u8>,
}

pub(super) struct LogState {
    pub(super) client: IncusClient,
    pub(super) id: String,
    pub(super) follow: bool,
    pub(super) tail: Option<usize>,
    pub(super) files: Vec<FileChannel>,
    pub(super) console_partial: Vec<u8>,
    pub(super) pending: VecDeque<LogLine>,
    pub(super) primed: bool,
    pub(super) saw_stopped: bool,
    pub(super) finished: bool,
}

/// Append bytes to a per-source line buffer, enqueueing completed lines.
pub(super) fn push_lines(
    pending: &mut VecDeque<LogLine>,
    partial: &mut Vec<u8>,
    bytes: &[u8],
    stream: OutputStream,
) {
    partial.extend_from_slice(bytes);
    while let Some(pos) = partial.iter().position(|b| *b == b'\n') {
        let line: Vec<u8> = partial.drain(..=pos).collect();
        let text = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
        pending.push_back(LogLine {
            stream,
            text: format!("{text}\n"),
        });
    }
}

/// Emit a trailing unterminated buffer as a final line (output that never
/// ended with `\n`, e.g. `printf`/`echo -n` style output).
pub(super) fn push_tail(pending: &mut VecDeque<LogLine>, partial: &mut Vec<u8>, stream: OutputStream) {
    if partial.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(partial).into_owned();
    partial.clear();
    pending.push_back(LogLine {
        stream,
        text: format!("{text}\n"),
    });
}

impl LogState {
    /// One polling round. Enqueues lines into `pending` or marks `finished`.
    pub(super) async fn poll_once(&mut self) -> Result<()> {
        // Primary channel: the wrapper's output files (cumulative, idempotent).
        for index in 0..self.files.len() {
            let (status, bytes) = {
                let channel = &self.files[index];
                let path = format!("/1.0/instances/{}/files?path={}", self.id, channel.path);
                match self.client.raw(&Method::GET, &path, None).await {
                    Ok(result) => result,
                    Err(_) => continue,
                }
            };
            if status != 200 {
                // 404: not created yet (or a non-wrapper container). Other
                // codes (e.g. transient 500s around process exit) are retried.
                continue;
            }
            let channel = &mut self.files[index];
            if bytes.len() < channel.offset {
                channel.offset = 0;
            }
            let slice = &bytes[channel.offset..];
            if slice.is_empty() {
                continue;
            }
            channel.offset = bytes.len();
            let stream = channel.stream;
            let mut partial = std::mem::take(&mut channel.partial);
            push_lines(&mut self.pending, &mut partial, slice, stream);
            self.files[index].partial = partial;
        }

        // Fallback channel: the console (drains while running, best effort).
        // The files channel above is authoritative; console errors are
        // tolerated entirely (transient 500s around process exit are common).
        match self
            .client
            .raw(&Method::GET, &format!("/1.0/instances/{}/console", self.id), None)
            .await
        {
            Ok((200, bytes)) if !bytes.is_empty() => {
                push_lines(
                    &mut self.pending,
                    &mut self.console_partial,
                    &bytes,
                    OutputStream::Console,
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!("incus console fetch failed (ignored): {error}");
            }
        }

        if !self.primed {
            if let Some(tail) = self.tail {
                while self.pending.len() > tail {
                    self.pending.pop_front();
                }
            }
            self.primed = true;
        }

        if !self.pending.is_empty() {
            return Ok(());
        }
        if self.saw_stopped || !self.follow {
            // Flush trailing unterminated output ("mount-hi" with no final
            // newline) before closing the stream.
            for index in 0..self.files.len() {
                let stream = self.files[index].stream;
                let mut partial = std::mem::take(&mut self.files[index].partial);
                push_tail(&mut self.pending, &mut partial, stream);
                self.files[index].partial = partial;
            }
            push_tail(
                &mut self.pending,
                &mut self.console_partial,
                OutputStream::Console,
            );
            if self.pending.is_empty() {
                self.finished = true;
            }
            return Ok(());
        }

        if self.instance_live().await {
            sleep(POLL_INTERVAL).await;
        } else {
            // Run one final poll to pick up trailing output, then stop.
            self.saw_stopped = true;
            sleep(POLL_INTERVAL).await;
        }
        Ok(())
    }

    pub(super) async fn instance_live(&self) -> bool {
        let Ok((200, bytes)) = self
            .client
            .raw(&Method::GET, &format!("/1.0/instances/{}/state", self.id), None)
            .await
        else {
            return false;
        };
        serde_json::from_slice::<Envelope>(&bytes)
            .ok()
            .and_then(|env| {
                serde_json::from_value::<InstanceState>(env.metadata.unwrap_or(Value::Null)).ok()
            })
            .map(|state| matches!(state.status_code.unwrap_or(102), 103..=110))
            .unwrap_or(false)
    }
}

pub(super) fn logs_stream(client: IncusClient, id: &str, options: &LogOptions) -> EventStream<LogLine> {
    let mut files = Vec::new();
    if options.stdout {
        files.push(FileChannel {
            path: INIT_STDOUT_FILE.to_string(),
            stream: OutputStream::Stdout,
            offset: 0,
            partial: Vec::new(),
        });
    }
    if options.stderr {
        files.push(FileChannel {
            path: INIT_STDERR_FILE.to_string(),
            stream: OutputStream::Stderr,
            offset: 0,
            partial: Vec::new(),
        });
    }
    let state = LogState {
        client,
        id: id.to_string(),
        follow: options.follow,
        tail: options.tail,
        files,
        console_partial: Vec::new(),
        pending: VecDeque::new(),
        primed: false,
        saw_stopped: false,
        finished: false,
    };
    Box::pin(futures_util::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(line) = st.pending.pop_front() {
                return Some((Ok(line), st));
            }
            if st.finished {
                return None;
            }
            if let Err(error) = st.poll_once().await {
                st.finished = true;
                return Some((Err(error), st));
            }
        }
    }))
}

pub(super) struct ExecState {
    pub(super) client: IncusClient,
    pub(super) op_id: String,
    pub(super) stdout_url: String,
    pub(super) stderr_url: String,
    pub(super) stdout_off: usize,
    pub(super) stderr_off: usize,
    pub(super) pending: VecDeque<ExecChunk>,
    pub(super) results: Arc<Mutex<HashMap<String, i64>>>,
    pub(super) finished: bool,
}

impl ExecState {
    pub(super) async fn poll_once(&mut self) -> Result<()> {
        let (status, bytes) = self
            .client
            .raw(&Method::GET, &self.stdout_url, None)
            .await?;
        if status == 200 {
            push_slice(
                &mut self.pending,
                &bytes,
                &mut self.stdout_off,
                OutputStream::Stdout,
            );
        }
        let (status, bytes) = self
            .client
            .raw(&Method::GET, &self.stderr_url, None)
            .await?;
        if status == 200 {
            push_slice(
                &mut self.pending,
                &bytes,
                &mut self.stderr_off,
                OutputStream::Stderr,
            );
        }

        let (status, bytes) = self
            .client
            .raw(&Method::GET, &format!("/1.0/operations/{}", self.op_id), None)
            .await?;
        let op = if status == 404 {
            None
        } else {
            let envelope = parse_envelope(status, &bytes)?;
            Some(serde_json::from_value::<Operation>(
                envelope.metadata.unwrap_or(Value::Null),
            )?)
        };

        match op {
            Some(op) if op.status == "Success" => {
                if let Some(code) = op
                    .metadata
                    .as_ref()
                    .and_then(|meta| meta.get("return"))
                    .and_then(Value::as_i64)
                {
                    self.results
                        .lock()
                        .expect("exec results poisoned")
                        .insert(self.op_id.clone(), code);
                }
                self.finished = true;
            }
            Some(op) if op.status == "Failure" => {
                self.finished = true;
                return Err(Error::Other(format!(
                    "incus exec failed: {}",
                    op.err.unwrap_or_else(|| "unknown error".to_string())
                )));
            }
            _ => {}
        }

        if !self.pending.is_empty() {
            return Ok(());
        }
        if self.finished {
            return Ok(());
        }
        sleep(POLL_INTERVAL).await;
        Ok(())
    }
}

/// Append the newly grown tail of `data` to `pending` as one chunk.
pub(super) fn push_slice(
    pending: &mut VecDeque<ExecChunk>,
    data: &[u8],
    offset: &mut usize,
    stream: OutputStream,
) {
    if data.len() < *offset {
        *offset = 0;
    }
    let slice = &data[*offset..];
    if slice.is_empty() {
        return;
    }
    *offset = data.len();
    pending.push_back(ExecChunk {
        stream,
        text: String::from_utf8_lossy(slice).into_owned(),
    });
}

pub(super) fn exec_stream(
    client: IncusClient,
    op_id: &str,
    stdout_url: &str,
    stderr_url: &str,
    results: Arc<Mutex<HashMap<String, i64>>>,
) -> EventStream<ExecChunk> {
    let state = ExecState {
        client,
        op_id: op_id.to_string(),
        stdout_url: stdout_url.to_string(),
        stderr_url: stderr_url.to_string(),
        stdout_off: 0,
        stderr_off: 0,
        pending: VecDeque::new(),
        results,
        finished: false,
    };
    Box::pin(futures_util::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(chunk) = st.pending.pop_front() {
                return Some((Ok(chunk), st));
            }
            if st.finished {
                return None;
            }
            if let Err(error) = st.poll_once().await {
                st.finished = true;
                return Some((Err(error), st));
            }
        }
    }))
}

pub(super) fn stats_stream(client: IncusClient, id: &str) -> EventStream<StatsSample> {
    struct StatsState {
        client: IncusClient,
        id: String,
        prev: Option<(u64, u64)>,
        finished: bool,
    }

    let state = StatsState {
        client,
        id: id.to_string(),
        prev: None,
        finished: false,
    };
    Box::pin(futures_util::stream::unfold(state, |mut st| async move {
        if st.finished {
            return None;
        }
        let result = async {
            let (status, bytes) = st
                .client
                .raw(&Method::GET, &format!("/1.0/instances/{}/state", st.id), None)
                .await?;
            if status == 404 {
                return Err(Error::NotFound(format!("container `{}`", st.id)));
            }
            let envelope = parse_envelope(status, &bytes)?;
            let state: InstanceState = serde_json::from_value(
                envelope.metadata.unwrap_or(Value::Null),
            )?;
            let sample = state_to_stats(&state, st.prev);
            let cpu_ns = state.cpu.as_ref().and_then(|cpu| cpu.usage).unwrap_or(0);
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            st.prev = Some((cpu_ns, now_ns));
            let live = matches!(state.status_code.unwrap_or(102), 103..=110);
            if !live {
                st.finished = true;
            } else {
                sleep(POLL_INTERVAL).await;
            }
            Ok(sample)
        }
        .await;
        match result {
            Ok(sample) => Some((Ok(sample), st)),
            Err(error) => {
                st.finished = true;
                Some((Err(error), st))
            }
        }
    }))
}

