//! Container specifications and the read models returned by the engine.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

use super::units::{ByteSize, validate_name};

/// Restart policy applied to a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    /// Never restart.
    #[default]
    No,
    /// Restart only on a non-zero exit code.
    OnFailure,
    /// Restart unless the container was explicitly stopped.
    UnlessStopped,
    /// Always restart.
    Always,
}

impl RestartPolicy {
    /// Value accepted by the Docker API.
    pub fn as_docker(self) -> &'static str {
        match self {
            Self::No => "no",
            Self::OnFailure => "on-failure",
            Self::UnlessStopped => "unless-stopped",
            Self::Always => "always",
        }
    }
}

/// Port protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// TCP.
    #[default]
    Tcp,
    /// UDP.
    Udp,
}

impl Protocol {
    /// Value accepted by the Docker API.
    pub fn as_docker(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

fn default_host_ip() -> IpAddr {
    IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

/// A published container port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortBinding {
    /// Container-side port.
    pub container: u16,
    /// Host-side port. `None` asks Docker for an ephemeral port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<u16>,
    /// Host interface to bind. Defaults to loopback.
    #[serde(default = "default_host_ip")]
    pub host_ip: IpAddr,
    /// Protocol.
    #[serde(default)]
    pub protocol: Protocol,
}

impl PortBinding {
    /// Bind a container port to loopback with an ephemeral host port.
    pub fn loopback(container: u16) -> Self {
        Self {
            container,
            host: None,
            host_ip: default_host_ip(),
            protocol: Protocol::Tcp,
        }
    }

    /// Bind a container port to a fixed loopback host port.
    pub fn published(host: u16, container: u16) -> Self {
        Self {
            container,
            host: Some(host),
            host_ip: default_host_ip(),
            protocol: Protocol::Tcp,
        }
    }

    /// Docker's `container/protocol` key.
    pub fn key(&self) -> String {
        format!("{}/{}", self.container, self.protocol.as_docker())
    }

    fn validate(&self) -> Result<()> {
        if self.container == 0 {
            return Err(Error::InvalidSpec(
                "container port must be between 1 and 65535".to_string(),
            ));
        }
        if self.host == Some(0) {
            return Err(Error::InvalidSpec(
                "host port must be between 1 and 65535; omit it for an ephemeral port".to_string(),
            ));
        }
        Ok(())
    }
}

/// Kind of mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MountKind {
    /// Bind a host path.
    #[default]
    Bind,
    /// Mount a named volume.
    Volume,
}

/// A bind mount or named volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    /// Host path (bind) or volume name.
    pub source: PathBuf,
    /// Absolute path inside the container.
    pub target: String,
    /// Mount read-only.
    #[serde(default)]
    pub read_only: bool,
    /// Bind or volume.
    #[serde(default)]
    pub kind: MountKind,
}

impl Mount {
    /// Create a bind mount.
    pub fn bind(source: impl Into<PathBuf>, target: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            read_only: false,
            kind: MountKind::Bind,
        }
    }

    /// Create a named volume mount.
    pub fn volume(source: impl Into<PathBuf>, target: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            read_only: false,
            kind: MountKind::Volume,
        }
    }

    /// Mark the mount read-only.
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    fn validate(&self) -> Result<()> {
        if !self.target.starts_with('/') {
            return Err(Error::InvalidSpec(format!(
                "mount target `{}` must be an absolute container path",
                self.target
            )));
        }
        if self.source.as_os_str().is_empty() {
            return Err(Error::InvalidSpec(format!(
                "mount source for `{}` must not be empty",
                self.target
            )));
        }
        if self.kind == MountKind::Bind && !self.source.is_absolute() {
            return Err(Error::InvalidSpec(format!(
                "bind source `{}` must be an absolute host path",
                self.source.display()
            )));
        }
        Ok(())
    }
}

/// Resource limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    /// Memory limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<ByteSize>,
    /// CPU limit in cores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<f64>,
    /// PID limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids: Option<i64>,
}

impl Resources {
    fn validate(&self) -> Result<()> {
        if let Some(cpus) = self.cpus {
            if !cpus.is_finite() || cpus <= 0.0 {
                return Err(Error::InvalidSpec(
                    "resources.cpus must be a positive finite number".to_string(),
                ));
            }
        }
        if let Some(pids) = self.pids {
            if pids <= 0 {
                return Err(Error::InvalidSpec(
                    "resources.pids must be positive".to_string(),
                ));
            }
        }
        Ok(())
    }
}

/// A ulimit entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ulimit {
    /// Limit name (for example, `nofile`).
    pub name: String,
    /// Soft limit.
    pub soft: i64,
    /// Hard limit.
    pub hard: i64,
}

/// Container isolation knobs.
///
/// The default is intentionally conservative: writable root filesystem, no
/// added capabilities and no privileged mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SecurityOptions {
    /// Mount the root filesystem read-only.
    #[serde(default)]
    pub read_only: bool,
    /// Linux capabilities to drop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cap_drop: Vec<String>,
    /// Linux capabilities to add.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cap_add: Vec<String>,
    /// Set `no-new-privileges`.
    #[serde(default)]
    pub no_new_privileges: bool,
    /// tmpfs mounts, keyed by container path.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tmpfs: BTreeMap<String, String>,
    /// ulimits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ulimits: Vec<Ulimit>,
}

impl SecurityOptions {
    fn validate(&self) -> Result<()> {
        for path in self.tmpfs.keys() {
            if !path.starts_with('/') {
                return Err(Error::InvalidSpec(format!(
                    "tmpfs path `{path}` must be absolute"
                )));
            }
        }
        for ulimit in &self.ulimits {
            if ulimit.name.is_empty() {
                return Err(Error::InvalidSpec(
                    "ulimit name must not be empty".to_string(),
                ));
            }
            if ulimit.soft < 0 || ulimit.hard < 0 || ulimit.soft > ulimit.hard {
                return Err(Error::InvalidSpec(format!(
                    "ulimit `{}` must satisfy 0 <= soft <= hard",
                    ulimit.name
                )));
            }
        }
        Ok(())
    }
}

fn default_log_driver() -> String {
    "json-file".to_string()
}

fn default_log_max_size() -> Option<ByteSize> {
    Some(ByteSize::mib(10))
}

fn default_log_max_file() -> Option<u32> {
    Some(3)
}

/// Docker log driver configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogConfig {
    /// Log driver name.
    #[serde(default = "default_log_driver")]
    pub driver: String,
    /// Maximum size of one log file. `0b` disables the limit.
    #[serde(default = "default_log_max_size")]
    pub max_size: Option<ByteSize>,
    /// Maximum number of rotated files.
    #[serde(default = "default_log_max_file")]
    pub max_file: Option<u32>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            driver: default_log_driver(),
            max_size: default_log_max_size(),
            max_file: default_log_max_file(),
        }
    }
}

impl LogConfig {
    fn validate(&self) -> Result<()> {
        if self.driver.trim().is_empty() {
            return Err(Error::InvalidSpec(
                "log.driver must not be empty".to_string(),
            ));
        }
        if self.max_file == Some(0) {
            return Err(Error::InvalidSpec(
                "log.max_file must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }
}

/// Everything needed to create one container.
///
/// This is the single model used by the Rust API and by TOML service files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerSpec {
    /// Container name.
    pub name: String,
    /// Image reference.
    pub image: String,
    /// Command and arguments. Empty means the image's default command.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// Override the image entrypoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
    /// Environment variables. Wins over `env_file`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Optional dotenv file, resolved relative to the config file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_file: Option<PathBuf>,
    /// Working directory inside the container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    /// User to run as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Container hostname.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// Bind mounts and named volumes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<Mount>,
    /// Published ports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<PortBinding>,
    /// Network to attach to. `None` uses Docker's default bridge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    /// Static IPv4 address on `network`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_ip: Option<IpAddr>,
    /// Restart policy.
    #[serde(default)]
    pub restart: RestartPolicy,
    /// Signal used to stop the container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_signal: Option<String>,
    /// Seconds to wait after `stop_signal` before killing.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "humantime_serde"
    )]
    pub stop_grace: Option<Duration>,
    /// Resource limits.
    #[serde(default)]
    pub resources: Resources,
    /// Isolation knobs.
    #[serde(default)]
    pub security: SecurityOptions,
    /// User labels. `qe.*` labels are reserved for the crate.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Log rotation.
    #[serde(default)]
    pub log: LogConfig,
    /// Target platform, for example `linux/amd64`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

impl ContainerSpec {
    /// Create a minimal spec.
    pub fn new(name: impl Into<String>, image: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            image: image.into(),
            command: Vec::new(),
            entrypoint: None,
            env: BTreeMap::new(),
            env_file: None,
            workdir: None,
            user: None,
            hostname: None,
            mounts: Vec::new(),
            ports: Vec::new(),
            network: None,
            static_ip: None,
            restart: RestartPolicy::No,
            stop_signal: None,
            stop_grace: None,
            resources: Resources::default(),
            security: SecurityOptions::default(),
            labels: BTreeMap::new(),
            log: LogConfig::default(),
            platform: None,
        }
    }

    /// Set the command.
    pub fn with_command<I, S>(mut self, command: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.command = command.into_iter().map(Into::into).collect();
        self
    }

    /// Set the entrypoint.
    pub fn with_entrypoint<I, S>(mut self, entrypoint: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.entrypoint = Some(entrypoint.into_iter().map(Into::into).collect());
        self
    }

    /// Add an environment variable.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Set the working directory.
    pub fn with_workdir(mut self, workdir: impl Into<String>) -> Self {
        self.workdir = Some(workdir.into());
        self
    }

    /// Set the user.
    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    /// Add a mount.
    pub fn with_mount(mut self, mount: Mount) -> Self {
        self.mounts.push(mount);
        self
    }

    /// Add a published port.
    pub fn with_port(mut self, port: PortBinding) -> Self {
        self.ports.push(port);
        self
    }

    /// Attach to a network.
    pub fn with_network(mut self, network: impl Into<String>) -> Self {
        self.network = Some(network.into());
        self
    }

    /// Set a static IPv4 address. Requires `network`.
    pub fn with_static_ip(mut self, ip: IpAddr) -> Self {
        self.static_ip = Some(ip);
        self
    }

    /// Set the restart policy.
    pub fn with_restart(mut self, policy: RestartPolicy) -> Self {
        self.restart = policy;
        self
    }

    /// Add a label.
    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }

    /// Validate the spec before touching the daemon.
    pub fn validate(&self) -> Result<()> {
        validate_name("container", &self.name)?;
        if self.image.trim().is_empty() {
            return Err(Error::InvalidSpec(
                "container image must not be empty".to_string(),
            ));
        }
        if self.static_ip.is_some() && self.network.is_none() {
            return Err(Error::InvalidSpec(
                "static_ip requires a network".to_string(),
            ));
        }
        if let Some(network) = &self.network {
            validate_name("network", network)?;
        }
        for key in self.env.keys() {
            if key.is_empty() || key.contains('=') || key.contains('\0') {
                return Err(Error::InvalidSpec(format!(
                    "environment variable name `{key}` is invalid"
                )));
            }
        }
        for mount in &self.mounts {
            mount.validate()?;
        }
        for (index, left) in self.mounts.iter().enumerate() {
            if self.mounts[index + 1..]
                .iter()
                .any(|right| right.target == left.target)
            {
                return Err(Error::InvalidSpec(format!(
                    "duplicate mount target `{}`",
                    left.target
                )));
            }
        }
        for port in &self.ports {
            port.validate()?;
        }
        for (index, left) in self.ports.iter().enumerate() {
            if self.ports[index + 1..].iter().any(|right| {
                right.container == left.container
                    && right.protocol == left.protocol
                    && right.host_ip == left.host_ip
            }) {
                return Err(Error::InvalidSpec(format!(
                    "duplicate port binding `{}`",
                    left.key()
                )));
            }
        }
        self.resources.validate()?;
        self.security.validate()?;
        self.log.validate()?;
        if let Some(grace) = self.stop_grace {
            if grace.is_zero() {
                return Err(Error::InvalidSpec(
                    "stop_grace must be greater than zero".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// Merge `env_file` into `env` and clear `env_file`.
    ///
    /// Explicit `env` values win over file values.
    pub fn resolved(&self) -> Result<Self> {
        let Some(path) = &self.env_file else {
            return Ok(self.clone());
        };

        #[cfg(feature = "config")]
        {
            let mut resolved = self.clone();
            let iter = dotenvy::from_path_iter(path)
                .map_err(|error| Error::config(path, format!("cannot read env_file: {error}")))?;
            for item in iter {
                let (key, value) = item.map_err(|error| {
                    Error::config(path, format!("invalid env_file entry: {error}"))
                })?;
                resolved.env.entry(key).or_insert(value);
            }
            resolved.env_file = None;
            Ok(resolved)
        }

        #[cfg(not(feature = "config"))]
        {
            Err(Error::config(
                path,
                "env_file support requires the `config` feature",
            ))
        }
    }

    /// Stable hash of the normalized spec, used for drift detection.
    pub fn spec_hash(&self) -> Result<String> {
        let canonical = self.canonical();
        let bytes = serde_json::to_vec(&canonical)?;
        let digest = Sha256::digest(bytes);
        Ok(hex::encode(digest))
    }

    /// A canonical copy with order-independent collections sorted.
    pub fn canonical(&self) -> Self {
        let mut canonical = self.clone();
        canonical.mounts.sort_by(|left, right| {
            left.target
                .cmp(&right.target)
                .then_with(|| left.source.cmp(&right.source))
        });
        canonical.ports.sort_by(|left, right| {
            left.container
                .cmp(&right.container)
                .then_with(|| left.protocol.as_docker().cmp(right.protocol.as_docker()))
                .then_with(|| left.host_ip.cmp(&right.host_ip))
                .then_with(|| left.host.cmp(&right.host))
        });
        canonical.security.cap_drop.sort();
        canonical.security.cap_add.sort();
        canonical.security.ulimits.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.soft.cmp(&right.soft))
                .then_with(|| left.hard.cmp(&right.hard))
        });
        canonical.env_file = None;
        canonical
    }

    /// Container-side ports that should be exposed.
    pub fn exposed_ports(&self) -> Vec<String> {
        self.ports.iter().map(PortBinding::key).collect()
    }

    /// Look up the host port for a container port.
    pub fn configured_host_port(&self, container: u16) -> Option<u16> {
        self.ports
            .iter()
            .find(|port| port.container == container)
            .and_then(|port| port.host)
    }
}

impl std::fmt::Display for ContainerSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.name, self.image)
    }
}

/// Container state as a normalized enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerStateKind {
    /// Created but never started.
    Created,
    /// Running.
    Running,
    /// Paused.
    Paused,
    /// Restarting.
    Restarting,
    /// Being removed.
    Removing,
    /// Exited.
    Exited,
    /// Dead.
    Dead,
    /// Stopping.
    Stopping,
    /// Unknown state.
    Unknown,
}

impl ContainerStateKind {
    /// Parse Docker's state string.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "created" => Self::Created,
            "running" => Self::Running,
            "paused" => Self::Paused,
            "restarting" => Self::Restarting,
            "removing" => Self::Removing,
            "exited" => Self::Exited,
            "dead" => Self::Dead,
            "stopping" => Self::Stopping,
            _ => Self::Unknown,
        }
    }

    /// Whether the state implies a live process.
    pub fn is_running(self) -> bool {
        matches!(self, Self::Running | Self::Paused | Self::Restarting)
    }
}

/// Normalized health status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthState {
    /// No health check configured.
    None,
    /// Starting.
    Starting,
    /// Healthy.
    Healthy,
    /// Unhealthy.
    Unhealthy,
    /// Unknown.
    Unknown,
}

impl HealthState {
    /// Parse Docker's health status.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "none" => Self::None,
            "starting" => Self::Starting,
            "healthy" => Self::Healthy,
            "unhealthy" => Self::Unhealthy,
            _ => Self::Unknown,
        }
    }
}

/// A published port as observed on a container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedPort {
    /// Container port.
    pub container: u16,
    /// Host port, when published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<u16>,
    /// Host interface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_ip: Option<IpAddr>,
    /// Protocol.
    pub protocol: Protocol,
}

/// One container in a list response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerSummary {
    /// Container id.
    pub id: String,
    /// Names without the leading slash.
    pub names: Vec<String>,
    /// Image reference.
    pub image: String,
    /// Image id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_id: Option<String>,
    /// Command line, as reported by Docker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Normalized state.
    pub state: ContainerStateKind,
    /// Human-readable status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Labels.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Published ports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<PublishedPort>,
    /// Creation time (Unix seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
}

impl ContainerSummary {
    /// Primary name (without slash).
    pub fn name(&self) -> Option<&str> {
        self.names.first().map(String::as_str)
    }

    /// Whether the container is managed by this crate.
    pub fn managed(&self) -> bool {
        self.labels
            .get("qe.managed")
            .is_some_and(|value| value == "true")
    }
}

/// A mount point as observed on a container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountPoint {
    /// Host source.
    pub source: String,
    /// Container target.
    pub target: String,
    /// Read-only flag.
    pub read_only: bool,
    /// Docker mount type.
    pub kind: String,
}

/// A network attachment as observed on a container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerNetwork {
    /// Network name.
    pub name: String,
    /// IPv4 address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_address: Option<String>,
    /// Gateway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
}

/// Detailed container information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerDetail {
    /// Container id.
    pub id: String,
    /// Name without the leading slash.
    pub name: String,
    /// Image reference.
    pub image: String,
    /// Image id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_id: Option<String>,
    /// Normalized state.
    pub state: ContainerStateKind,
    /// Running flag.
    pub running: bool,
    /// Paused flag.
    pub paused: bool,
    /// Last exit code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// OOM killed flag.
    pub oom_killed: bool,
    /// Health status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthState>,
    /// Labels.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Published ports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<PublishedPort>,
    /// Mounts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<MountPoint>,
    /// Networks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<ContainerNetwork>,
    /// Creation time (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// Restart count.
    pub restart_count: i64,
}

impl ContainerDetail {
    /// Whether the container is managed by this crate.
    pub fn managed(&self) -> bool {
        self.labels
            .get("qe.managed")
            .is_some_and(|value| value == "true")
    }

    /// Host port for a container port, if published.
    pub fn host_port(&self, container: u16) -> Option<u16> {
        self.ports
            .iter()
            .find(|port| port.container == container)
            .and_then(|port| port.host)
    }
}

/// Filters for [`crate::Engine::list_containers`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContainerFilter {
    /// Include stopped containers.
    pub all: bool,
    /// Match by name substring.
    pub name: Option<String>,
    /// Match by label (`key` or `key=value`).
    pub label: Option<String>,
    /// Match by state.
    pub status: Option<ContainerStateKind>,
    /// Match by network.
    pub network: Option<String>,
}

/// Container engine information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineInfo {
    /// Daemon version.
    pub version: String,
    /// API version negotiated with the daemon.
    pub api_version: String,
    /// Minimum API version supported by the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_api_version: Option<String>,
    /// Operating system.
    pub os: String,
    /// Architecture.
    pub arch: String,
    /// Whether the daemon runs rootless.
    pub rootless: bool,
    /// Security options reported by the daemon.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security_options: Vec<String>,
    /// Total containers.
    pub containers: i64,
    /// Running containers.
    pub containers_running: i64,
    /// Total images.
    pub images: i64,
}

/// A local image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageInfo {
    /// Image id.
    pub id: String,
    /// Repository tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Size in bytes.
    pub size: u64,
    /// Creation time (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
}

/// Exit status of a finished container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitStatus {
    /// Exit code.
    pub code: i64,
    /// Daemon-reported wait error, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Whether the kernel OOM killer terminated the process.
    pub oom_killed: bool,
}

impl ExitStatus {
    /// Whether the exit code is zero.
    pub fn success(&self) -> bool {
        self.code == 0
    }
}
