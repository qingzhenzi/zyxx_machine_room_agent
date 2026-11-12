//! Error types shared by every layer of `qe-container`.

use std::path::Path;

/// Convenient result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by the container engine.
///
/// Public error variants deliberately avoid leaking `bollard` types, so callers
/// can depend on `qe-container` without depending on a specific API client.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A TOML/JSON configuration file is invalid.
    #[error("configuration error{path}: {message}")]
    Config {
        /// Rendered location (for example, ` at qe-container.toml:12`).
        path: String,
        /// Human-readable explanation.
        message: String,
    },

    /// The Docker daemon could not be reached.
    #[error("container engine is not available: {0}")]
    EngineUnavailable(String),

    /// The daemon answered with an error response.
    #[error("container engine error (HTTP {status}): {message}")]
    Docker {
        /// HTTP status returned by the daemon.
        status: u16,
        /// Error message returned by the daemon.
        message: String,
    },

    /// A client-side error while talking to the daemon.
    #[error("container engine client error: {0}")]
    DockerClient(String),

    /// The requested object does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// The operation conflicts with existing state.
    #[error("conflict: {0}")]
    Conflict(String),

    /// A spec is structurally invalid.
    #[error("invalid spec: {0}")]
    InvalidSpec(String),

    /// A managed container exists with a different spec.
    #[error(
        "service `{name}` exists with a different spec ({detail}); use EnsurePolicy::Recreate to replace it"
    )]
    SpecDrift {
        /// Container/service name.
        name: String,
        /// Short explanation of the drift.
        detail: String,
    },

    /// A health check did not become healthy in time.
    #[error("health check failed for `{name}`: {detail}")]
    HealthCheck {
        /// Container/service name.
        name: String,
        /// Last observed failure.
        detail: String,
    },

    /// An operation exceeded its deadline.
    #[error("timeout: {0}")]
    Timeout(String),

    /// Network planning failed.
    #[error("network error: {0}")]
    Network(String),

    /// Image handling failed.
    #[error("image error: {0}")]
    Image(String),

    /// I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// JSON serialization error.
    #[error(transparent)]
    SerdeJson(#[from] serde_json::Error),

    /// TOML parsing error.
    #[cfg(feature = "config")]
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),

    /// Anything else. Used sparingly.
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Build a configuration error with an optional location.
    pub fn config(path: impl AsRef<Path>, message: impl Into<String>) -> Self {
        let path = path.as_ref();
        let rendered = if path.as_os_str().is_empty() {
            String::new()
        } else {
            format!(" at {}", path.display())
        };
        Self::Config {
            path: rendered,
            message: message.into(),
        }
    }

    /// Build a configuration error from an already-rendered location.
    pub fn config_at(path: impl Into<String>, message: impl Into<String>) -> Self {
        let path = path.into();
        let rendered = if path.is_empty() {
            String::new()
        } else {
            format!(" at {path}")
        };
        Self::Config {
            path: rendered,
            message: message.into(),
        }
    }

    /// Whether this error represents a missing object.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound(_))
    }

    /// A stable machine-readable code, useful for IPC layers.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Config { .. } => "config",
            Self::EngineUnavailable(_) => "engine_unavailable",
            Self::Docker { .. } => "docker",
            Self::DockerClient(_) => "docker_client",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::InvalidSpec(_) => "invalid_spec",
            Self::SpecDrift { .. } => "spec_drift",
            Self::HealthCheck { .. } => "health_check",
            Self::Timeout(_) => "timeout",
            Self::Network(_) => "network",
            Self::Image(_) => "image",
            Self::Io(_) => "io",
            Self::SerdeJson(_) => "json",
            #[cfg(feature = "config")]
            Self::Toml(_) => "toml",
            Self::Other(_) => "other",
        }
    }
}

/// Translate a bollard error into the crate's public error type.
pub(crate) fn from_bollard(error: bollard::errors::Error) -> Error {
    use bollard::errors::Error as E;

    match error {
        E::DockerResponseServerError {
            status_code,
            message,
        } => match status_code {
            404 => Error::NotFound(message),
            409 => Error::Conflict(message),
            _ => Error::Docker {
                status: status_code,
                message,
            },
        },
        E::RequestTimeoutError => Error::Timeout("container engine request timed out".into()),
        E::IOError { err } => Error::Io(err),
        E::DockerStreamError { error } => Error::DockerClient(error),
        E::DockerContainerWaitError { error, code } => {
            Error::DockerClient(format!("wait failed with code {code}: {error}"))
        }
        other => Error::DockerClient(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_helper() {
        assert!(Error::NotFound("x".into()).is_not_found());
        assert_eq!(Error::NotFound("x".into()).code(), "not_found");
    }

    #[test]
    fn config_error_renders_location() {
        let error = Error::config_at("qe-container.toml:4", "bad field");
        assert_eq!(
            error.to_string(),
            "configuration error at qe-container.toml:4: bad field"
        );
    }
}
