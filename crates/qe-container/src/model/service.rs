//! Service specifications: a long-running container plus health policy.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::PullPolicy;
use crate::error::{Error, Result};

use super::container::{ContainerSpec, ContainerStateKind, HealthState};
use super::network::NetworkSpec;

fn default_interval() -> Duration {
    Duration::from_secs(2)
}

fn default_timeout() -> Duration {
    Duration::from_secs(2)
}

fn default_start_period() -> Duration {
    Duration::from_secs(0)
}

fn default_retries() -> u32 {
    30
}

fn default_expect_status() -> u16 {
    200
}

/// How the crate probes a service after starting it.
///
/// Probes run from the host (HTTP/TCP) or through `exec` (inside the
/// container). The crate does not inject a Docker `HEALTHCHECK`, because many
/// minimal images do not contain curl or wget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HealthCheck {
    /// HTTP probe against a published container port.
    Http {
        /// Container port to probe.
        port: u16,
        /// Request path, starting with `/`.
        path: String,
        /// Expected HTTP status code.
        #[serde(default = "default_expect_status")]
        expect_status: u16,
        /// Delay between probes.
        #[serde(default = "default_interval", with = "humantime_serde")]
        interval: Duration,
        /// Per-probe timeout.
        #[serde(default = "default_timeout", with = "humantime_serde")]
        timeout: Duration,
        /// Delay before the first probe.
        #[serde(default = "default_start_period", with = "humantime_serde")]
        start_period: Duration,
        /// Maximum number of probes.
        #[serde(default = "default_retries")]
        retries: u32,
    },
    /// TCP connect probe against a published container port.
    Tcp {
        /// Container port to probe.
        port: u16,
        /// Delay between probes.
        #[serde(default = "default_interval", with = "humantime_serde")]
        interval: Duration,
        /// Per-probe timeout.
        #[serde(default = "default_timeout", with = "humantime_serde")]
        timeout: Duration,
        /// Delay before the first probe.
        #[serde(default = "default_start_period", with = "humantime_serde")]
        start_period: Duration,
        /// Maximum number of probes.
        #[serde(default = "default_retries")]
        retries: u32,
    },
    /// Exec probe inside the container. Exit code 0 means healthy.
    Exec {
        /// Command and arguments.
        command: Vec<String>,
        /// Delay between probes.
        #[serde(default = "default_interval", with = "humantime_serde")]
        interval: Duration,
        /// Per-probe timeout.
        #[serde(default = "default_timeout", with = "humantime_serde")]
        timeout: Duration,
        /// Delay before the first probe.
        #[serde(default = "default_start_period", with = "humantime_serde")]
        start_period: Duration,
        /// Maximum number of probes.
        #[serde(default = "default_retries")]
        retries: u32,
    },
}

impl HealthCheck {
    /// HTTP probe.
    pub fn http(port: u16, path: impl Into<String>) -> Self {
        Self::Http {
            port,
            path: path.into(),
            expect_status: 200,
            interval: default_interval(),
            timeout: default_timeout(),
            start_period: default_start_period(),
            retries: default_retries(),
        }
    }

    /// TCP probe.
    pub fn tcp(port: u16) -> Self {
        Self::Tcp {
            port,
            interval: default_interval(),
            timeout: default_timeout(),
            start_period: default_start_period(),
            retries: default_retries(),
        }
    }

    /// Exec probe.
    pub fn exec<I, S>(command: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Exec {
            command: command.into_iter().map(Into::into).collect(),
            interval: default_interval(),
            timeout: default_timeout(),
            start_period: default_start_period(),
            retries: default_retries(),
        }
    }

    /// The published container port this probe needs, if any.
    pub fn port(&self) -> Option<u16> {
        match self {
            Self::Http { port, .. } | Self::Tcp { port, .. } => Some(*port),
            Self::Exec { .. } => None,
        }
    }

    /// Interval between probes.
    pub fn interval(&self) -> Duration {
        match self {
            Self::Http { interval, .. }
            | Self::Tcp { interval, .. }
            | Self::Exec { interval, .. } => *interval,
        }
    }

    /// Per-probe timeout.
    pub fn timeout(&self) -> Duration {
        match self {
            Self::Http { timeout, .. } | Self::Tcp { timeout, .. } | Self::Exec { timeout, .. } => {
                *timeout
            }
        }
    }

    /// Delay before the first probe.
    pub fn start_period(&self) -> Duration {
        match self {
            Self::Http { start_period, .. }
            | Self::Tcp { start_period, .. }
            | Self::Exec { start_period, .. } => *start_period,
        }
    }

    /// Maximum number of probes.
    pub fn retries(&self) -> u32 {
        match self {
            Self::Http { retries, .. } | Self::Tcp { retries, .. } | Self::Exec { retries, .. } => {
                *retries
            }
        }
    }

    /// Validate the probe.
    pub fn validate(&self) -> Result<()> {
        if self.retries() == 0 {
            return Err(Error::InvalidSpec(
                "healthcheck.retries must be greater than zero".to_string(),
            ));
        }
        if self.interval().is_zero() {
            return Err(Error::InvalidSpec(
                "healthcheck.interval must be greater than zero".to_string(),
            ));
        }
        if self.timeout().is_zero() {
            return Err(Error::InvalidSpec(
                "healthcheck.timeout must be greater than zero".to_string(),
            ));
        }
        match self {
            Self::Http {
                port,
                path,
                expect_status,
                ..
            } => {
                if *port == 0 {
                    return Err(Error::InvalidSpec(
                        "healthcheck.port must be greater than zero".to_string(),
                    ));
                }
                if !path.starts_with('/') {
                    return Err(Error::InvalidSpec(
                        "healthcheck.path must start with `/`".to_string(),
                    ));
                }
                if !(100..600).contains(expect_status) {
                    return Err(Error::InvalidSpec(
                        "healthcheck.expect_status must be a valid HTTP status".to_string(),
                    ));
                }
            }
            Self::Tcp { port, .. } => {
                if *port == 0 {
                    return Err(Error::InvalidSpec(
                        "healthcheck.port must be greater than zero".to_string(),
                    ));
                }
            }
            Self::Exec { command, .. } => {
                if command.is_empty() {
                    return Err(Error::InvalidSpec(
                        "healthcheck.command must not be empty".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// A container that should stay running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceSpec {
    /// Container configuration, flattened so a TOML `[service]` table reads naturally.
    #[serde(flatten)]
    pub container: ContainerSpec,
    /// Optional health probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<HealthCheck>,
}

impl ServiceSpec {
    /// Wrap a container spec as a service.
    pub fn new(container: ContainerSpec) -> Self {
        Self {
            container,
            healthcheck: None,
        }
    }

    /// Attach a health check.
    pub fn with_health_check(mut self, healthcheck: HealthCheck) -> Self {
        self.healthcheck = Some(healthcheck);
        self
    }

    /// Service name (same as the container name).
    pub fn name(&self) -> &str {
        &self.container.name
    }

    /// Validate the service.
    pub fn validate(&self) -> Result<()> {
        self.container.validate()?;
        if let Some(healthcheck) = &self.healthcheck {
            healthcheck.validate()?;
            if let Some(port) = healthcheck.port() {
                if !self
                    .container
                    .ports
                    .iter()
                    .any(|item| item.container == port)
                {
                    return Err(Error::InvalidSpec(format!(
                        "healthcheck uses container port {port}, but no matching port is published"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Merge `env_file` values into the environment.
    pub fn resolved(&self) -> Result<Self> {
        Ok(Self {
            container: self.container.resolved()?,
            healthcheck: self.healthcheck.clone(),
        })
    }

    /// Stable hash of the resolved service configuration.
    pub fn spec_hash(&self) -> Result<String> {
        let container_hash = self.container.spec_hash()?;
        let healthcheck = match &self.healthcheck {
            Some(healthcheck) => serde_json::to_vec(healthcheck)?,
            None => Vec::new(),
        };
        let mut hasher = Sha256::new();
        hasher.update(container_hash.as_bytes());
        hasher.update(b"\n");
        hasher.update(healthcheck);
        Ok(hex::encode(hasher.finalize()))
    }
}

/// A single-service TOML file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceFile {
    /// Schema version. v1 is currently the only supported value.
    pub version: u32,
    /// Optional network to ensure before starting the service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkSpec>,
    /// The service.
    pub service: ServiceSpec,
}

/// Current service file schema version.
pub const SERVICE_FILE_VERSION: u32 = 1;

impl ServiceFile {
    /// Create a version-1 file around a service.
    pub fn new(service: ServiceSpec) -> Self {
        Self {
            version: SERVICE_FILE_VERSION,
            network: None,
            service,
        }
    }

    /// Attach a network definition.
    pub fn with_network(mut self, network: NetworkSpec) -> Self {
        self.network = Some(network);
        self
    }

    /// Validate version, network, and service.
    pub fn validate(&self) -> Result<()> {
        if self.version != SERVICE_FILE_VERSION {
            return Err(Error::InvalidSpec(format!(
                "unsupported service file version {}; expected {}",
                self.version, SERVICE_FILE_VERSION
            )));
        }
        if let Some(network) = &self.network {
            network.validate()?;
        }
        self.service.validate()?;

        if let Some(ip) = self.service.container.static_ip {
            match (&self.service.container.network, &self.network) {
                (Some(service_network), Some(network)) if service_network == &network.name => {
                    network.validate_static_ip(ip)?;
                }
                (Some(_), Some(_)) | (Some(_), None) => {
                    // The network is external; its subnet cannot be checked here.
                }
                _ => {
                    return Err(Error::InvalidSpec(
                        "static_ip requires a service network".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Attach the service to the file's network when it did not name one.
    pub fn apply_implicit_network(&mut self) {
        if self.service.container.network.is_none() {
            if let Some(network) = &self.network {
                self.service.container.network = Some(network.name.clone());
            }
        }
    }

    /// Fill in the implicit network reference and validate.
    pub fn resolved(&self) -> Result<Self> {
        let mut resolved = self.clone();
        resolved.apply_implicit_network();
        resolved.validate()?;
        resolved.service = resolved.service.resolved()?;
        Ok(resolved)
    }
}

/// What to do when the existing service spec differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EnsurePolicy {
    /// Return a [`crate::Error::SpecDrift`] error.
    #[default]
    Error,
    /// Keep the existing container and report `drift = true`.
    Keep,
    /// Remove and recreate the container.
    Recreate,
}

/// Options for [`crate::Engine::ensure_service`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsureOptions {
    /// Drift policy.
    pub policy: EnsurePolicy,
    /// Wait for the health check after starting.
    pub wait_healthy: bool,
    /// Overall start + health-check deadline.
    pub timeout: Duration,
    /// Image pull behavior when the service has to be created.
    pub pull_policy: PullPolicy,
}

impl Default for EnsureOptions {
    fn default() -> Self {
        Self {
            policy: EnsurePolicy::Error,
            wait_healthy: true,
            timeout: Duration::from_secs(60),
            pull_policy: PullPolicy::IfMissing,
        }
    }
}

/// State of a service after `ensure` or `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStatus {
    /// Service name.
    pub name: String,
    /// Normalized container state.
    pub state: ContainerStateKind,
    /// Running flag.
    pub running: bool,
    /// Health status, if the service declares a probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthState>,
    /// Container port to host port mapping.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub host_ports: BTreeMap<u16, u16>,
    /// Whether the container was created by this call.
    pub created: bool,
    /// Whether the container carries the `qe.managed=true` label.
    pub managed: bool,
    /// Stored spec hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_hash: Option<String>,
    /// Whether the stored spec differs from the requested one.
    pub drift: bool,
}
