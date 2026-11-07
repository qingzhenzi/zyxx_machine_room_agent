//! Local IPC client for the `qe-agent` daemon.
//!
//! Same protocol and socket discovery as the agent GUI client: HTTP/1.1 over
//! a Unix domain socket, `GET /v1/state`. The webview never touches the
//! socket; this module performs requests on the Rust side and exposes the
//! result through a `#[tauri::command]` handler.
//!
//! Socket discovery: `QE_AGENT_IPC_SOCKET` (development override) or the
//! production default `/run/qe-agent/agent.sock`.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Production socket path used by the systemd unit.
pub const DEFAULT_SOCKET_PATH: &str = "/run/qe-agent/agent.sock";

/// State polls are cheap; never block the UI for long.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// 展示层指令：版本号单调递增，模板由 agent 决定。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DisplayDirective {
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub template: String,
    #[serde(default)]
    pub params: Value,
}

/// `GET /v1/state` 的响应（只取展示层需要的字段）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayState {
    #[serde(default)]
    pub overall: String,
    #[serde(default)]
    pub config_sufficient: bool,
    #[serde(default)]
    pub display: DisplayDirective,
}

impl Default for DisplayState {
    fn default() -> Self {
        Self {
            overall: "booting".into(),
            config_sufficient: false,
            display: DisplayDirective::default(),
        }
    }
}

/// Socket path the display should talk to.
pub fn socket_path() -> PathBuf {
    std::env::var_os("QE_AGENT_IPC_SOCKET")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET_PATH))
}

/// Perform one IPC request and return the parsed JSON body.
#[cfg(unix)]
fn request(path: &str) -> Result<Value, String> {
    use std::io::{Read, Write};

    let socket = socket_path();
    let mut stream = std::os::unix::net::UnixStream::connect(&socket)
        .map_err(|error| format!("connect {}: {error}", socket.display()))?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|error| format!("set timeout: {error}"))?;
    stream
        .set_write_timeout(Some(READ_TIMEOUT))
        .map_err(|error| format!("set timeout: {error}"))?;

    let request = format!("GET {path} HTTP/1.1\r\nHost: qe-agent\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("write: {error}"))?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|error| format!("read: {error}"))?;
    let text = String::from_utf8_lossy(&response);

    // Split headers from body on the first empty line.
    let body = match text.split_once("\r\n\r\n") {
        Some((_, body)) => body,
        None => text.as_ref(),
    };
    serde_json::from_str(body).map_err(|error| format!("decode: {error}"))
}

#[cfg(not(unix))]
fn request(_path: &str) -> Result<Value, String> {
    Err("IPC is only supported on Unix platforms".into())
}

/// Fetch the current state (including the display directive) from the agent.
///
/// Falls back to [`DisplayState::default`] when the agent is unreachable so
/// the display keeps showing something instead of erroring out.
pub fn display_state() -> DisplayState {
    request("/v1/state")
        .and_then(|value| serde_json::from_value::<DisplayState>(value).map_err(|e| e.to_string()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_boot() {
        let state = DisplayState::default();
        assert_eq!(state.display.version, 0);
        assert_eq!(state.display.template, "");
        assert_eq!(state.overall, "booting");
    }

    #[test]
    fn parses_full_state() {
        let raw = serde_json::json!({
            "overall": "degraded",
            "config_sufficient": false,
            "display": {
                "version": 2,
                "template": "message",
                "params": { "title": "维护通知" }
            },
            "phases": []
        });
        let state: DisplayState = serde_json::from_value(raw).unwrap();
        assert_eq!(state.overall, "degraded");
        assert!(!state.config_sufficient);
        assert_eq!(state.display.version, 2);
        assert_eq!(state.display.template, "message");
        assert_eq!(state.display.params["title"], "维护通知");
    }

    #[test]
    fn tolerates_missing_display() {
        let state: DisplayState =
            serde_json::from_value(serde_json::json!({ "overall": "ready" })).unwrap();
        assert_eq!(state.overall, "ready");
        assert_eq!(state.display.version, 0);
    }
}
