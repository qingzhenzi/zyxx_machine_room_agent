//! Container engine backends.
//!
//! [`Backend`] is the low-level port used by [`crate::Engine`]. The default
//! production implementation talks to the Docker Engine API through `bollard`;
//! [`incus::IncusBackend`] maps the same contract onto a local Incus daemon,
//! and [`mock::MockBackend`] keeps it in memory for tests and UI dry-runs.

pub mod docker;
pub mod incus;
pub mod mock;

use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;

use crate::error::Result;
use crate::model::{
    ContainerDetail, ContainerFilter, ContainerSpec, ContainerSummary, EngineInfo, ExitStatus,
    ImageInfo, NetworkInfo, NetworkSpec,
};
use crate::stream::{
    EventStream, ExecSession, ExecSpec, LogLine, LogOptions, PullProgress, StatsSample,
};

/// Low-level operations implemented by a container engine.
///
/// Implementations must be cheap to clone through `Arc` and safe to call
/// concurrently.
#[async_trait]
pub trait Backend: Send + Sync + 'static {
    /// Daemon version and inventory.
    async fn info(&self) -> Result<EngineInfo>;

    /// List containers.
    async fn list_containers(&self, filter: &ContainerFilter) -> Result<Vec<ContainerSummary>>;

    /// Inspect one container by id or name.
    async fn inspect_container(&self, id: &str) -> Result<ContainerDetail>;

    /// Create a container and return its id.
    async fn create_container(&self, spec: &ContainerSpec) -> Result<String>;

    /// Start a container.
    async fn start_container(&self, id: &str) -> Result<()>;

    /// Stop a container.
    async fn stop_container(&self, id: &str, timeout: Option<Duration>) -> Result<()>;

    /// Restart a container.
    async fn restart_container(&self, id: &str, timeout: Option<Duration>) -> Result<()>;

    /// Remove a container.
    async fn remove_container(&self, id: &str, force: bool) -> Result<()>;

    /// Stream logs.
    fn logs(&self, id: &str, options: &LogOptions) -> Result<EventStream<LogLine>>;

    /// Wait for a container to stop.
    async fn wait_container(&self, id: &str) -> Result<ExitStatus>;

    /// Start an exec session.
    async fn exec(&self, id: &str, spec: &ExecSpec) -> Result<ExecSession>;

    /// Inspect an exec session (used to obtain its exit code).
    async fn inspect_exec(&self, exec_id: &str) -> Result<Option<i64>>;

    /// Stream resource stats.
    fn stats(&self, id: &str) -> Result<EventStream<StatsSample>>;

    /// Pull an image, streaming progress.
    fn pull_image(&self, image: &str, platform: Option<&str>) -> Result<EventStream<PullProgress>>;

    /// Inspect a local image. Returns `None` when the image is missing.
    async fn inspect_image(&self, image: &str) -> Result<Option<ImageInfo>>;

    /// List local images.
    async fn list_images(&self) -> Result<Vec<ImageInfo>>;

    /// List networks.
    async fn list_networks(&self) -> Result<Vec<NetworkInfo>>;

    /// Inspect a network by name or id.
    async fn inspect_network(&self, name: &str) -> Result<NetworkInfo>;

    /// Create a network and return its id.
    async fn create_network(&self, spec: &NetworkSpec) -> Result<String>;

    /// Remove a network.
    async fn remove_network(&self, name: &str) -> Result<()>;

    /// Connect a container to a network.
    async fn connect_network(
        &self,
        network: &str,
        container: &str,
        ip: Option<IpAddr>,
    ) -> Result<()>;

    /// Disconnect a container from a network.
    async fn disconnect_network(&self, network: &str, container: &str, force: bool) -> Result<()>;
}
