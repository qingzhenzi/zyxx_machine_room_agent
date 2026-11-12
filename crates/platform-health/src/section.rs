//! Per-section collection bookkeeping.
//!
//! Collection "never fails" — a missing sensor degrades to `None`. But a
//! consumer still needs to tell *why* a domain is absent: unsupported on this
//! OS, feature disabled, permission denied, or the tool wasn't installed. Each
//! collector reports through a [`SectionMeta`] so the backend can distinguish
//! those cases and surface genuinely slow sections.

use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Timing + outcome for one logical section of the snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionMeta {
    /// Section name, e.g. `cpu`, `displays`, `wifi`, `smart`.
    pub name: String,
    /// Wall-clock milliseconds spent collecting.
    pub duration_ms: u64,
    /// `ok` when data was produced; otherwise a coarse reason.
    ///
    /// One of: `ok`, `unsupported`, `disabled`, `denied`, `missing_tool`,
    /// `error`. Kept as a string (not an enum) so sections can add finer
    /// reasons without a schema break.
    pub status: String,
    /// Human-readable detail when `status != "ok"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl SectionMeta {
    pub fn ok(name: &str, duration_ms: u64) -> Self {
        Self {
            name: name.to_string(),
            duration_ms,
            status: "ok".to_string(),
            detail: None,
        }
    }

    pub fn failed(name: &str, duration_ms: u64, status: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            duration_ms,
            status: status.to_string(),
            detail: Some(detail.into()),
        }
    }

    pub fn is_ok(&self) -> bool {
        self.status == "ok"
    }
}

/// A collector's return value plus the section metadata describing it.
///
/// Collectors return [`Collected::ok`] / [`Collected::unsupported`] /
/// [`Collected::err`]; the orchestrator times the call and appends the meta.
pub struct Collected<T> {
    pub value: Option<T>,
    pub status: &'static str,
    pub detail: Option<String>,
}

impl<T> Collected<T> {
    pub fn ok(value: T) -> Self {
        Self {
            value: Some(value),
            status: "ok",
            detail: None,
        }
    }

    /// Domain absent because the platform/feature doesn't support it.
    pub fn unsupported(detail: impl Into<String>) -> Self {
        Self {
            value: None,
            status: "unsupported",
            detail: Some(detail.into()),
        }
    }

    /// Domain absent because a required external tool isn't installed.
    pub fn missing_tool(detail: impl Into<String>) -> Self {
        Self {
            value: None,
            status: "missing_tool",
            detail: Some(detail.into()),
        }
    }

    /// Domain absent because of a permission error.
    pub fn denied(detail: impl Into<String>) -> Self {
        Self {
            value: None,
            status: "denied",
            detail: Some(detail.into()),
        }
    }

    /// Domain absent because collection errored.
    pub fn err(detail: impl Into<String>) -> Self {
        Self {
            value: None,
            status: "error",
            detail: Some(detail.into()),
        }
    }
}

/// Run a collector, measuring its duration and converting its [`Collected`]
/// outcome into `(value, SectionMeta)`.
pub fn timed<T>(name: &str, f: impl FnOnce() -> Collected<T>) -> (Option<T>, SectionMeta) {
    let start = Instant::now();
    let collected = f();
    let duration_ms = start.elapsed().as_millis() as u64;
    let meta = if collected.status == "ok" {
        SectionMeta::ok(name, duration_ms)
    } else {
        SectionMeta::failed(
            name,
            duration_ms,
            collected.status,
            collected.detail.unwrap_or_default(),
        )
    };
    (collected.value, meta)
}
