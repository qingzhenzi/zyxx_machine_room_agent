use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use hyper::Method;
use serde_json::Value;
use tokio::time::{Instant, sleep};

use super::transport::Transport;
use super::wire::{Envelope, Operation};
use super::POLL_INTERVAL;
use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// IncusClient: envelope handling + operation waiting
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(super) struct IncusClient {
    transport: Arc<dyn Transport>,
    timeout: Duration,
}

impl IncusClient {
    pub(super) fn new(transport: Arc<dyn Transport>, timeout: Duration) -> Self {
        Self { transport, timeout }
    }

    /// Raw request returning status and body bytes.
    pub(super) async fn raw(&self, method: &Method, path: &str, body: Option<Value>) -> Result<(u16, Bytes)> {
        let payload = match body {
            Some(value) => Some(Bytes::from(serde_json::to_vec(&value)?)),
            None => None,
        };
        let call = self.transport.call(method, path, payload);
        match tokio::time::timeout(self.timeout, call).await {
            Ok(result) => result,
            Err(_) => Err(Error::Timeout(format!(
                "incus request timed out: {method} {path}"
            ))),
        }
    }

    /// Envelope request; async operations are waited out. Returns `metadata`.
    pub(super) async fn request(&self, method: &Method, path: &str, body: Option<Value>) -> Result<Value> {
        let (status, bytes) = self.raw(method, path, body).await?;
        let envelope = parse_envelope(status, &bytes)?;
        match envelope.kind.as_str() {
            "sync" => Ok(envelope.metadata.unwrap_or(Value::Null)),
            "async" => {
                let op: Operation = serde_json::from_value(
                    envelope.metadata.clone().unwrap_or(Value::Null),
                )?;
                let op_id = op.id.clone();
                let result = self.wait_operation(&op_id).await?;
                Ok(result.unwrap_or(Value::Null))
            }
            _ => Err(envelope_error(&envelope)),
        }
    }

    /// Envelope request that does NOT wait for the async operation; returns
    /// the started operation (empty `id` when the request was synchronous).
    pub(super) async fn request_async(
        &self,
        method: &Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Operation> {
        let (status, bytes) = self.raw(method, path, body).await?;
        let envelope = parse_envelope(status, &bytes)?;
        match envelope.kind.as_str() {
            "sync" => Ok(Operation {
                id: String::new(),
                status: "Success".to_string(),
                err: None,
                metadata: envelope.metadata,
            }),
            "async" => serde_json::from_value(
                envelope.metadata.clone().unwrap_or(Value::Null),
            )
            .map_err(|error| Error::EngineUnavailable(format!("bad operation payload: {error}"))),
            _ => Err(envelope_error(&envelope)),
        }
    }

    /// Poll an operation until it finishes.
    ///
    /// Incus cleans up completed operations quickly; a 404 while polling is
    /// treated as "completed" and callers verify postconditions themselves.
    /// Returns the final operation `metadata` (or `None` when cleaned up).
    pub(super) async fn wait_operation(&self, op_id: &str) -> Result<Option<Value>> {
        if op_id.is_empty() {
            return Ok(None);
        }
        let deadline = Instant::now() + self.timeout;
        loop {
            let path = format!("/1.0/operations/{op_id}");
            let (status, bytes) = self.raw(&Method::GET, &path, None).await?;
            if status == 404 {
                return Ok(None);
            }
            let envelope = parse_envelope(status, &bytes)?;
            if envelope.kind == "error" {
                return Err(envelope_error(&envelope));
            }
            let op: Operation = serde_json::from_value(
                envelope.metadata.clone().unwrap_or(Value::Null),
            )?;
            match op.status.as_str() {
                "Success" => return Ok(op.metadata),
                "Failure" => {
                    return Err(Error::Other(format!(
                        "incus operation failed: {}",
                        op.err.unwrap_or_else(|| "unknown error".to_string())
                    )));
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout(format!(
                    "incus operation `{op_id}` did not finish in time"
                )));
            }
            sleep(POLL_INTERVAL).await;
        }
    }
}

pub(super) fn parse_envelope(status: u16, bytes: &Bytes) -> Result<Envelope> {
    if bytes.is_empty() && status == 200 {
        return Ok(Envelope {
            kind: "sync".to_string(),
            error: None,
            error_code: None,
            metadata: Some(Value::Null),
        });
    }
    match serde_json::from_slice::<Envelope>(bytes) {
        Ok(envelope) => Ok(envelope),
        Err(error) => Err(Error::EngineUnavailable(format!(
            "invalid incus response (HTTP {status}): {error}"
        ))),
    }
}

pub(super) fn envelope_error(envelope: &Envelope) -> Error {
    let message = envelope
        .error
        .clone()
        .unwrap_or_else(|| "unknown incus error".to_string());
    match envelope.error_code.unwrap_or(0) {
        404 => Error::NotFound(message),
        409 => Error::Conflict(message),
        400 => Error::InvalidSpec(message),
        code => Error::Docker {
            status: u16::try_from(code).unwrap_or(500),
            message,
        },
    }
}

/// Whether an error is a transient daemon/transport failure worth retrying.
pub(super) fn is_transient(error: &Error) -> bool {
    match error {
        Error::EngineUnavailable(_) => true,
        Error::Docker { status, .. } => (500..600).contains(status),
        _ => false,
    }
}

