//! Engine configuration and small policy enums.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::error::{Error, Result};

/// Which container engine backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendKind {
    /// Docker Engine via `bollard` (default).
    #[default]
    Docker,
    /// Incus via its unix socket REST API.
    Incus,
    /// In-memory mock backend.
    Mock,
}

impl BackendKind {
    /// Parse `docker`, `incus`, or `mock`.
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "docker" => Ok(Self::Docker),
            "incus" => Ok(Self::Incus),
            "mock" => Ok(Self::Mock),
            other => Err(Error::InvalidSpec(format!(
                "unknown container backend `{other}`; expected docker, incus, or mock"
            ))),
        }
    }

    /// Stable identifier used in CLI flags and environment variables.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Incus => "incus",
            Self::Mock => "mock",
        }
    }
}

/// Connection and labeling configuration for an [`crate::Engine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    /// Backend implementation to use.
    pub backend: BackendKind,
    /// Docker host URI. `None` uses `DOCKER_HOST` or the platform default.
    /// Ignored by the Incus backend.
    pub host: Option<String>,
    /// Incus unix socket path. `None` uses `/var/lib/incus/unix.socket`.
    pub incus_socket: Option<String>,
    /// Per-request timeout.
    pub timeout: Duration,
    /// Prefix for internal labels (defaults to `qe`).
    pub label_prefix: String,
    /// Extra labels applied to every container created by the crate.
    pub default_labels: BTreeMap<String, String>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            backend: BackendKind::Docker,
            host: None,
            incus_socket: None,
            timeout: Duration::from_secs(120),
            label_prefix: "qe".to_string(),
            default_labels: BTreeMap::new(),
        }
    }
}

impl EngineConfig {
    /// Read overrides from `QE_CONTAINER_*` and `DOCKER_HOST`.
    pub fn from_env() -> Result<Self> {
        let mut config = Self::default();

        if let Ok(raw) = std::env::var("QE_CONTAINER_BACKEND") {
            if !raw.trim().is_empty() {
                config.backend = BackendKind::parse(&raw)?;
            }
        }

        if let Ok(host) = std::env::var("QE_CONTAINER_HOST") {
            if !host.trim().is_empty() {
                config.host = Some(host);
            }
        }

        if let Ok(socket) = std::env::var("QE_CONTAINER_INCUS_SOCKET") {
            if !socket.trim().is_empty() {
                config.incus_socket = Some(socket);
            }
        }

        if let Ok(raw) = std::env::var("QE_CONTAINER_TIMEOUT") {
            if !raw.trim().is_empty() {
                let parsed = humantime::parse_duration(&raw).map_err(|error| {
                    Error::config_at("QE_CONTAINER_TIMEOUT", format!("invalid duration: {error}"))
                })?;
                if parsed.is_zero() {
                    return Err(Error::config_at(
                        "QE_CONTAINER_TIMEOUT",
                        "timeout must be greater than zero",
                    ));
                }
                config.timeout = parsed;
            }
        }

        if let Ok(prefix) = std::env::var("QE_CONTAINER_PREFIX") {
            if !prefix.trim().is_empty() {
                config.label_prefix = prefix;
            }
        }

        config.validate()?;
        Ok(config)
    }

    /// Select the backend implementation.
    pub fn with_backend(mut self, backend: BackendKind) -> Self {
        self.backend = backend;
        self
    }

    /// Set the Docker host URI.
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    /// Set the Incus unix socket path.
    pub fn with_incus_socket(mut self, socket: impl Into<String>) -> Self {
        self.incus_socket = Some(socket.into());
        self
    }

    /// Set the per-request timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the internal label prefix.
    pub fn with_label_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.label_prefix = prefix.into();
        self
    }

    /// Add a label applied to every created container.
    pub fn with_default_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.default_labels.insert(key.into(), value.into());
        self
    }

    /// Validate configuration values.
    pub fn validate(&self) -> Result<()> {
        if self.timeout.is_zero() {
            return Err(Error::InvalidSpec(
                "engine timeout must be greater than zero".to_string(),
            ));
        }
        if self.label_prefix.trim().is_empty()
            || !self
                .label_prefix
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        {
            return Err(Error::InvalidSpec(
                "label prefix must be non-empty and contain only [a-zA-Z0-9_.-]".to_string(),
            ));
        }
        Ok(())
    }

    /// Internal label key, for example `qe.managed`.
    pub fn label(&self, name: &str) -> String {
        format!("{}.{}", self.label_prefix, name)
    }
}

/// Image pull behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PullPolicy {
    /// Pull only when the image is missing locally.
    #[default]
    IfMissing,
    /// Always pull, even if a local copy exists.
    Always,
    /// Never pull; fail if the image is missing.
    Never,
}

impl PullPolicy {
    /// Parse `if-missing`, `always`, or `never`.
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "if-missing" | "if_missing" | "missing" => Ok(Self::IfMissing),
            "always" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            other => Err(Error::InvalidSpec(format!(
                "unknown pull policy `{other}`; expected if-missing, always, or never"
            ))),
        }
    }
}

/// Options for ensuring a network exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NetworkEnsureOptions {
    /// Recreate the network when its spec differs. This is destructive.
    pub recreate: bool,
    /// Allow recreation while containers are still attached. The containers are
    /// disconnected and left without that network.
    pub force: bool,
}

impl NetworkEnsureOptions {
    /// Recreate on drift.
    pub fn recreate() -> Self {
        Self {
            recreate: true,
            force: false,
        }
    }

    /// Recreate on drift and detach attached containers.
    pub fn force_recreate() -> Self {
        Self {
            recreate: true,
            force: true,
        }
    }
}
