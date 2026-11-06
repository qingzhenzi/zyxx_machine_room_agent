//! Local IPC client for the `qe-agent` daemon.
//!
//! Same protocol and socket discovery as the agent GUI client: HTTP/1.1 over
//! a Unix domain socket. The webview never touches the socket; this module
//! performs requests on the Rust side and exposes results via commands.
//!
//! Socket discovery: `QE_AGENT_IPC_SOCKET` (development override) or the
//! production default `/run/qe-agent/agent.sock`.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Production socket path used by the systemd unit.
pub const DEFAULT_SOCKET_PATH: &str = "/run/qe-agent/agent.sock";

/// Display state polls are cheap; never block the UI for long.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// One display state snapshot served to the webview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayState {
    /// Which scene to render: `boot` | `progress` | `idle`.
    pub scene: String,
    /// Primary headline text.
    #[serde(default)]
    pub message: String,
    /// Secondary detail text.
    #[serde(default)]
    pub detail: String,
    /// Optional progress in `0..=100`; `None` hides the bar.
    #[serde(default)]
    pub progress: Option<f64>,
}

impl Default for DisplayState {
    fn default() -> Self {
        Self {
            scene: "boot".into(),
            message: "正在启动…".into(),
            detail: String::new(),
            progress: None,
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

/// Fetch the current display state from the agent.
///
/// Falls back to [`DisplayState::default`] when the agent is unreachable so
/// the display keeps showing something instead of erroring out.
pub fn display_state() -> DisplayState {
    let _ = request("/display/state").and_then(|value| {
        serde_json::from_value::<DisplayState>(value).map_err(|error| error.to_string())
    });
    // TODO(qe-agent): parse real response once `/display/state` lands in the
    // daemon. Until then the default boot scene is returned below.
    DisplayState::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_boot_scene() {
        let state = DisplayState::default();
        assert_eq!(state.scene, "boot");
        assert!(state.progress.is_none());
    }

    #[test]
    fn parses_partial_json() {
        let state: DisplayState =
            serde_json::from_str(r#"{"scene": "progress", "progress": 42}"#).unwrap();
        assert_eq!(state.scene, "progress");
        assert_eq!(state.progress, Some(42.0));
        assert_eq!(state.message, "");
    }
}
