//! The high-level engine: lifecycle, jobs, services, and network planning.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep};

use crate::backend::{Backend, docker::DockerBackend, incus::IncusBackend, mock::MockBackend};
use crate::config::{BackendKind, EngineConfig, NetworkEnsureOptions, PullPolicy};
use crate::error::{Error, Result};
use crate::model::{
    ContainerDetail, ContainerFilter, ContainerSpec, ContainerStateKind, ContainerSummary,
    EngineInfo, EnsureOptions, EnsurePolicy, ExitStatus, HealthCheck, HealthState, ImageInfo,
    NetworkAction, NetworkInfo, NetworkPlan, NetworkSpec, ServiceFile, ServiceSpec, ServiceStatus,
};
use crate::stream::{
    EventStream, ExecChunk, ExecResult, ExecSession, ExecSpec, LogLine, LogOptions, PullProgress,
    StatsSample,
};

/// A high-level handle to a container engine.
///
/// `Engine` is cheap to clone; clones share the same backend and service lock.
#[derive(Clone)]
pub struct Engine {
    backend: Arc<dyn Backend>,
    config: EngineConfig,
    ensure_lock: Arc<Mutex<()>>,
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Engine")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Wrap a custom backend (mostly for tests and dry-runs).
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            config: EngineConfig::default(),
            ensure_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Wrap a custom backend with explicit configuration.
    pub fn with_config(backend: Arc<dyn Backend>, config: EngineConfig) -> Self {
        Self {
            backend,
            config,
            ensure_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Connect to the daemon described by `config`.
    pub async fn connect(config: EngineConfig) -> Result<Self> {
        config.validate()?;

        let backend: Arc<dyn Backend> = match config.backend {
            BackendKind::Mock => Arc::new(MockBackend::new()),
            BackendKind::Incus => Arc::new(IncusBackend::connect_with_config(&config).await?),
            BackendKind::Docker => {
                let docker = match &config.host {
                    Some(host) => bollard::Docker::connect_with_host(host),
                    None => bollard::Docker::connect_with_defaults(),
                }
                .map_err(|error| Error::EngineUnavailable(error.to_string()))?;

                let docker = docker
                    .negotiate_version()
                    .await
                    .map_err(|error| Error::EngineUnavailable(error.to_string()))?;
                Arc::new(DockerBackend::new(docker))
            }
        };

        Ok(Self::with_config(backend, config))
    }

    /// Connect using environment overrides.
    pub async fn connect_from_env() -> Result<Self> {
        Self::connect(EngineConfig::from_env()?).await
    }

    /// Engine configuration.
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Access the backend (advanced use).
    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.backend
    }

    /// Daemon information.
    pub async fn info(&self) -> Result<EngineInfo> {
        self.backend.info().await
    }

    /// List containers.
    pub async fn list_containers(&self, filter: &ContainerFilter) -> Result<Vec<ContainerSummary>> {
        self.backend.list_containers(filter).await
    }

    /// Inspect a container by id or name.
    pub async fn inspect_container(&self, id: &str) -> Result<ContainerDetail> {
        self.backend.inspect_container(id).await
    }

    /// Create a managed container.
    #[tracing::instrument(skip(self, spec), fields(container = %spec.name))]
    pub async fn create_container(&self, spec: &ContainerSpec) -> Result<String> {
        let spec = self.prepare_spec(spec, "container", None)?;
        self.backend.create_container(&spec).await
    }

    /// Start a container.
    pub async fn start_container(&self, id: &str) -> Result<()> {
        self.backend.start_container(id).await
    }

    /// Stop a container.
    pub async fn stop_container(&self, id: &str, timeout: Option<Duration>) -> Result<()> {
        self.backend.stop_container(id, timeout).await
    }

    /// Restart a container.
    pub async fn restart_container(&self, id: &str, timeout: Option<Duration>) -> Result<()> {
        self.backend.restart_container(id, timeout).await
    }

    /// Remove a container. Only managed containers are removed by high-level
    /// helpers; this method is the explicit low-level escape hatch.
    pub async fn remove_container(&self, id: &str, force: bool) -> Result<()> {
        self.backend.remove_container(id, force).await
    }

    /// Stream logs.
    pub fn logs(&self, id: &str, options: &LogOptions) -> Result<EventStream<LogLine>> {
        self.backend.logs(id, options)
    }

    /// Wait for a container to stop.
    pub async fn wait_container(&self, id: &str) -> Result<ExitStatus> {
        self.backend.wait_container(id).await
    }

    /// Start an exec session.
    pub async fn exec(&self, id: &str, spec: &ExecSpec) -> Result<ExecSession> {
        self.backend.exec(id, spec).await
    }

    /// Execute and collect output plus exit code.
    pub async fn exec_capture(&self, container: &str, spec: &ExecSpec) -> Result<ExecResult> {
        let mut session = self.exec(container, spec).await?;
        let mut output = Vec::new();
        while let Some(item) = session.output.next().await {
            output.push(item?);
        }
        let exit_code = self
            .wait_exec_exit(&session.id, Duration::from_secs(10))
            .await?;
        Ok(ExecResult { exit_code, output })
    }

    /// Stream stats.
    pub fn stats(&self, id: &str) -> Result<EventStream<StatsSample>> {
        self.backend.stats(id)
    }

    /// Stream a pull.
    pub fn pull_image(
        &self,
        image: &str,
        platform: Option<&str>,
    ) -> Result<EventStream<PullProgress>> {
        self.backend.pull_image(image, platform)
    }

    /// Ensure an image exists locally according to `policy`.
    #[tracing::instrument(skip(self), fields(image = image))]
    pub async fn ensure_image(
        &self,
        image: &str,
        policy: PullPolicy,
        platform: Option<&str>,
    ) -> Result<()> {
        if policy != PullPolicy::Always && self.backend.inspect_image(image).await?.is_some() {
            return Ok(());
        }

        if policy == PullPolicy::Never {
            return Err(Error::Image(format!(
                "image `{image}` is not present locally and pull policy is `never`"
            )));
        }

        let mut stream = self.backend.pull_image(image, platform)?;
        while let Some(item) = stream.next().await {
            item?;
        }

        if self.backend.inspect_image(image).await?.is_none() {
            return Err(Error::Image(format!(
                "pull completed but image `{image}` is still missing"
            )));
        }
        Ok(())
    }

    /// List local images.
    pub async fn list_images(&self) -> Result<Vec<ImageInfo>> {
        self.backend.list_images().await
    }

    /// Inspect a local image.
    pub async fn inspect_image(&self, image: &str) -> Result<Option<ImageInfo>> {
        self.backend.inspect_image(image).await
    }

    /// List networks.
    pub async fn list_networks(&self) -> Result<Vec<NetworkInfo>> {
        self.backend.list_networks().await
    }

    /// Inspect a network.
    pub async fn inspect_network(&self, name: &str) -> Result<NetworkInfo> {
        self.backend.inspect_network(name).await
    }

    /// Plan a network create/reuse/recreate.
    pub async fn plan_network(&self, spec: &NetworkSpec) -> Result<NetworkPlan> {
        spec.validate()?;
        let existing = match self.backend.inspect_network(&spec.name).await {
            Ok(network) => Some(network),
            Err(Error::NotFound(_)) => None,
            Err(error) => return Err(error),
        };
        Ok(spec.plan(existing.as_ref()))
    }

    /// Ensure a network exists and matches `spec`.
    #[tracing::instrument(skip(self, spec), fields(network = %spec.name))]
    pub async fn ensure_network(
        &self,
        spec: &NetworkSpec,
        options: NetworkEnsureOptions,
    ) -> Result<NetworkPlan> {
        spec.validate()?;
        if matches!(spec.name.as_str(), "bridge" | "host" | "none") {
            return Err(Error::InvalidSpec(format!(
                "refusing to manage Docker's built-in network `{}`",
                spec.name
            )));
        }

        let plan = self.plan_network(spec).await?;
        match plan.action {
            NetworkAction::Create => {
                self.backend.create_network(spec).await?;
                Ok(plan)
            }
            NetworkAction::Reuse => Ok(plan),
            NetworkAction::Conflict => {
                if !options.recreate {
                    return Err(Error::Conflict(format!(
                        "network `{}` differs from the desired spec: {}",
                        spec.name,
                        plan.reasons.join("; ")
                    )));
                }
                if !plan.attached().is_empty() && !options.force {
                    let attached = plan
                        .attached()
                        .iter()
                        .map(|container| container.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(Error::Conflict(format!(
                        "network `{}` has attached containers ({attached}); pass force to detach them",
                        spec.name
                    )));
                }

                for container in plan.attached() {
                    self.backend
                        .disconnect_network(&spec.name, &container.id, true)
                        .await?;
                }
                self.backend.remove_network(&spec.name).await?;
                self.backend.create_network(spec).await?;
                Ok(NetworkPlan {
                    action: NetworkAction::Recreate,
                    ..plan
                })
            }
            NetworkAction::Recreate => {
                // `plan()` never returns Recreate directly, but keep the match exhaustive.
                Ok(plan)
            }
        }
    }

    /// Remove a network.
    pub async fn remove_network(&self, name: &str) -> Result<()> {
        self.backend.remove_network(name).await
    }

    /// Connect a container to a network.
    pub async fn connect_network(
        &self,
        network: &str,
        container: &str,
        ip: Option<IpAddr>,
    ) -> Result<()> {
        self.backend.connect_network(network, container, ip).await
    }

    /// Disconnect a container from a network.
    pub async fn disconnect_network(
        &self,
        network: &str,
        container: &str,
        force: bool,
    ) -> Result<()> {
        self.backend
            .disconnect_network(network, container, force)
            .await
    }

    /// Start a one-shot job. The caller owns the returned handle.
    #[tracing::instrument(skip(self, spec, options), fields(job = %spec.name))]
    pub async fn start_job(&self, spec: &ContainerSpec, options: &JobOptions) -> Result<JobHandle> {
        let prepared = self.prepare_spec(spec, "job", None)?;
        self.ensure_image(
            &prepared.image,
            options.pull_policy,
            prepared.platform.as_deref(),
        )
        .await?;
        let id = self.create_and_start(&prepared).await?;
        Ok(JobHandle {
            engine: self.clone(),
            id,
            name: prepared.name,
            remove_on_finish: options.remove_on_finish,
        })
    }

    /// Run a one-shot job to completion and remove it.
    pub async fn run_job(&self, spec: &ContainerSpec, options: &JobOptions) -> Result<ExitStatus> {
        let job = self.start_job(spec, options).await?;
        job.finish().await
    }

    /// Ensure a long-running service matches `spec`.
    #[tracing::instrument(skip(self, spec), fields(service = %spec.name()))]
    pub async fn ensure_service(
        &self,
        spec: &ServiceSpec,
        options: &EnsureOptions,
    ) -> Result<ServiceStatus> {
        let resolved = spec.resolved()?;
        resolved.validate()?;
        let hash = resolved.spec_hash()?;
        let prepared = self.prepare_spec(&resolved.container, "service", Some(&hash))?;

        let guard = self.ensure_lock.lock().await;
        let mut created = false;
        let mut drift = false;

        let existing = match self.backend.inspect_container(&prepared.name).await {
            Ok(detail) => Some(detail),
            Err(Error::NotFound(_)) => None,
            Err(error) => return Err(error),
        };

        match existing {
            None => {
                self.ensure_image(
                    &prepared.image,
                    options.pull_policy,
                    prepared.platform.as_deref(),
                )
                .await?;
                self.create_and_start(&prepared).await?;
                created = true;
            }
            Some(detail) => {
                if !detail.managed() {
                    return Err(Error::Conflict(format!(
                        "container `{}` exists but is not managed by qe-container",
                        prepared.name
                    )));
                }

                let stored_hash = detail.labels.get(&self.config.label("spec-hash")).cloned();
                let same = stored_hash.as_deref() == Some(hash.as_str());

                if same {
                    if !detail.running {
                        self.backend.start_container(&detail.id).await?;
                    }
                } else {
                    match options.policy {
                        EnsurePolicy::Error => {
                            return Err(Error::SpecDrift {
                                name: prepared.name,
                                detail: format!(
                                    "stored hash {}, desired hash {}",
                                    stored_hash.as_deref().unwrap_or("<missing>"),
                                    hash
                                ),
                            });
                        }
                        EnsurePolicy::Keep => {
                            drift = true;
                            if !detail.running {
                                self.backend.start_container(&detail.id).await?;
                            }
                        }
                        EnsurePolicy::Recreate => {
                            drift = true;
                            self.ensure_image(
                                &prepared.image,
                                options.pull_policy,
                                prepared.platform.as_deref(),
                            )
                            .await?;
                            self.backend.remove_container(&detail.id, true).await?;
                            self.create_and_start(&prepared).await?;
                            created = true;
                        }
                    }
                }
            }
        }

        drop(guard);

        if options.wait_healthy {
            if let Some(healthcheck) = &resolved.healthcheck {
                self.wait_healthy(&prepared.name, healthcheck, options.timeout)
                    .await?;
            }
        }

        let mut status = self.service_status(&prepared.name).await?;
        status.created = created;
        status.drift = drift;
        if options.wait_healthy && resolved.healthcheck.is_some() {
            status.health = Some(HealthState::Healthy);
        }
        Ok(status)
    }

    /// Ensure a service and its optional network together.
    pub async fn ensure_service_with_network(
        &self,
        spec: &ServiceSpec,
        network: Option<&NetworkSpec>,
        options: &EnsureOptions,
        network_options: NetworkEnsureOptions,
    ) -> Result<ServiceStatus> {
        if let Some(network) = network {
            self.ensure_network(network, network_options).await?;
            if let Some(ip) = spec.container.static_ip {
                network.validate_static_ip(ip)?;
            }
        }
        if let Some(network_name) = &spec.container.network {
            let declared = network.is_some_and(|network| &network.name == network_name);
            if !declared {
                match self.backend.inspect_network(network_name).await {
                    Ok(_) => {}
                    Err(Error::NotFound(_)) => {
                        return Err(Error::NotFound(format!(
                            "network `{network_name}` does not exist"
                        )));
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        self.ensure_service(spec, options).await
    }

    /// Resolve, validate and ensure a whole service file.
    pub async fn ensure_file(
        &self,
        file: &ServiceFile,
        options: &EnsureOptions,
        network_options: NetworkEnsureOptions,
    ) -> Result<ServiceStatus> {
        let file = file.resolved()?;
        self.ensure_service_with_network(
            &file.service,
            file.network.as_ref(),
            options,
            network_options,
        )
        .await
    }

    /// Current status of a service by name.
    pub async fn service_status(&self, name: &str) -> Result<ServiceStatus> {
        let detail = self.backend.inspect_container(name).await?;
        let host_ports = detail
            .ports
            .iter()
            .filter_map(|port| port.host.map(|host| (port.container, host)))
            .collect();
        Ok(ServiceStatus {
            name: detail.name.clone(),
            state: detail.state,
            running: detail.running,
            health: detail.health,
            host_ports,
            created: false,
            managed: detail.managed(),
            spec_hash: detail.labels.get(&self.config.label("spec-hash")).cloned(),
            drift: false,
        })
    }

    /// Status of a service compared with a desired spec.
    pub async fn service_status_for(&self, spec: &ServiceSpec) -> Result<ServiceStatus> {
        let resolved = spec.resolved()?;
        resolved.validate()?;
        let hash = resolved.spec_hash()?;
        let mut status = self.service_status(&resolved.container.name).await?;
        status.drift = status.spec_hash.as_deref() != Some(hash.as_str());
        Ok(status)
    }

    /// Stop a service and optionally remove it.
    pub async fn stop_service(&self, name: &str, remove: bool, force: bool) -> Result<()> {
        let detail = self.backend.inspect_container(name).await?;
        if !detail.managed() && !force {
            return Err(Error::Conflict(format!(
                "container `{name}` is not managed by qe-container; pass force to operate on it"
            )));
        }
        if detail.running {
            self.backend
                .stop_container(&detail.id, Some(Duration::from_secs(10)))
                .await?;
        }
        if remove {
            self.backend.remove_container(&detail.id, true).await?;
        }
        Ok(())
    }

    /// Stop a service described by a file, optionally removing its network.
    pub async fn stop_file(&self, file: &ServiceFile, remove_network: bool) -> Result<()> {
        let file = file.resolved()?;
        self.stop_service(&file.service.container.name, true, false)
            .await?;

        if remove_network {
            if let Some(network) = &file.network {
                let info = self.backend.inspect_network(&network.name).await?;
                if info.containers.is_empty() {
                    self.backend.remove_network(&network.name).await?;
                } else {
                    let names = info
                        .containers
                        .iter()
                        .map(|container| container.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(Error::Conflict(format!(
                        "network `{}` still has containers attached ({names})",
                        network.name
                    )));
                }
            }
        }
        Ok(())
    }

    /// Remove stopped job containers created by this crate.
    pub async fn prune_jobs(&self, force: bool) -> Result<usize> {
        let filter = ContainerFilter {
            all: true,
            label: Some(format!("{}=job", self.config.label("kind"))),
            ..Default::default()
        };
        let containers = self.backend.list_containers(&filter).await?;
        let mut removed = 0;
        for container in containers {
            if !force && container.state == ContainerStateKind::Running {
                continue;
            }
            self.backend.remove_container(&container.id, true).await?;
            removed += 1;
        }
        Ok(removed)
    }

    /// Resolve the host address for a published container port.
    pub async fn host_port(&self, container: &str, port: u16) -> Result<SocketAddr> {
        let detail = self.backend.inspect_container(container).await?;
        let published = detail
            .ports
            .iter()
            .find(|published| published.container == port && published.host.is_some())
            .ok_or_else(|| {
                Error::InvalidSpec(format!(
                    "container `{container}` does not publish port {port}"
                ))
            })?;
        let host = published.host.expect("checked is_some");
        let ip = published.host_ip.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let ip = match ip {
            IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
            other => other,
        };
        Ok(SocketAddr::new(ip, host))
    }

    async fn create_and_start(&self, spec: &ContainerSpec) -> Result<String> {
        let id = self.backend.create_container(spec).await?;
        if let Err(error) = self.backend.start_container(&id).await {
            let _ = self.backend.remove_container(&id, true).await;
            return Err(error);
        }
        Ok(id)
    }

    fn prepare_spec(
        &self,
        spec: &ContainerSpec,
        kind: &str,
        spec_hash: Option<&str>,
    ) -> Result<ContainerSpec> {
        let mut spec = spec.resolved()?;
        spec.validate()?;
        for (key, value) in &self.config.default_labels {
            spec.labels
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        spec.labels
            .insert(self.config.label("managed"), "true".to_string());
        spec.labels
            .insert(self.config.label("kind"), kind.to_string());
        spec.labels
            .insert(self.config.label("created-by"), "qe-container".to_string());
        if let Some(hash) = spec_hash {
            spec.labels
                .insert(self.config.label("spec-hash"), hash.to_string());
        }
        Ok(spec)
    }

    async fn wait_exec_exit(&self, exec_id: &str, timeout: Duration) -> Result<Option<i64>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(code) = self.backend.inspect_exec(exec_id).await? {
                return Ok(Some(code));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_healthy(
        &self,
        container: &str,
        healthcheck: &HealthCheck,
        total_timeout: Duration,
    ) -> Result<()> {
        healthcheck.validate()?;

        let deadline = Instant::now() + total_timeout;
        let start_period = healthcheck.start_period();
        if !start_period.is_zero() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            sleep(start_period.min(remaining)).await;
        }

        let mut last_error = String::from("no probe attempted");
        for attempt in 0..healthcheck.retries() {
            if Instant::now() >= deadline {
                last_error = format!("{last_error} (overall timeout reached)");
                break;
            }

            let probe = match healthcheck {
                HealthCheck::Http {
                    port,
                    path,
                    expect_status,
                    ..
                } => {
                    let address = self.host_port(container, *port).await?;
                    let host = address.ip().to_string();
                    tokio::time::timeout(
                        healthcheck.timeout(),
                        http_probe(address, &host, path, *expect_status),
                    )
                    .await
                }
                HealthCheck::Tcp { port, .. } => {
                    let address = self.host_port(container, *port).await?;
                    tokio::time::timeout(healthcheck.timeout(), tcp_probe(address)).await
                }
                HealthCheck::Exec { command, .. } => {
                    let spec = ExecSpec::new(command.iter().cloned());
                    tokio::time::timeout(healthcheck.timeout(), async {
                        let result = self.exec_capture(container, &spec).await?;
                        if result.success() {
                            Ok(())
                        } else {
                            Err(Error::HealthCheck {
                                name: container.to_string(),
                                detail: format!("exec probe exited with {:?}", result.exit_code),
                            })
                        }
                    })
                    .await
                }
            };

            match probe {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(error)) => last_error = error.to_string(),
                Err(_) => last_error = "probe timed out".to_string(),
            }

            if attempt + 1 < healthcheck.retries() {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let sleep_for = healthcheck.interval().min(remaining);
                if !sleep_for.is_zero() {
                    sleep(sleep_for).await;
                }
            }
        }

        Err(Error::HealthCheck {
            name: container.to_string(),
            detail: last_error,
        })
    }
}

async fn tcp_probe(address: SocketAddr) -> Result<()> {
    TcpStream::connect(address)
        .await
        .map(|_| ())
        .map_err(Error::Io)
}

async fn http_probe(address: SocketAddr, host: &str, path: &str, expect_status: u16) -> Result<()> {
    let mut stream = TcpStream::connect(address).await.map_err(Error::Io)?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nUser-Agent: qe-container\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(Error::Io)?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.map_err(Error::Io)?;
    let response = String::from_utf8_lossy(&response);
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok());

    match status {
        Some(status) if status == expect_status => Ok(()),
        Some(status) => Err(Error::HealthCheck {
            name: host.to_string(),
            detail: format!("expected HTTP {expect_status}, got {status}"),
        }),
        None => Err(Error::HealthCheck {
            name: host.to_string(),
            detail: "response had no HTTP status line".to_string(),
        }),
    }
}

/// Options for [`Engine::start_job`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobOptions {
    /// Image pull behavior.
    pub pull_policy: PullPolicy,
    /// Remove the container after the caller finishes it.
    pub remove_on_finish: bool,
}

impl Default for JobOptions {
    fn default() -> Self {
        Self {
            pull_policy: PullPolicy::IfMissing,
            remove_on_finish: true,
        }
    }
}

/// Handle to a started one-shot job.
#[derive(Clone)]
pub struct JobHandle {
    engine: Engine,
    id: String,
    name: String,
    remove_on_finish: bool,
}

impl fmt::Debug for JobHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobHandle")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish()
    }
}

impl JobHandle {
    /// Container id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Container name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Stream logs.
    pub fn logs(&self, options: &LogOptions) -> Result<EventStream<LogLine>> {
        self.engine.logs(&self.id, options)
    }

    /// Wait for completion.
    pub async fn wait(&self) -> Result<ExitStatus> {
        self.engine.wait_container(&self.id).await
    }

    /// Stop the job.
    pub async fn stop(&self, timeout: Option<Duration>) -> Result<()> {
        self.engine.stop_container(&self.id, timeout).await
    }

    /// Remove the container immediately.
    pub async fn remove(&self) -> Result<()> {
        self.engine.remove_container(&self.id, true).await
    }

    /// Wait for completion and remove the container when configured.
    pub async fn finish(&self) -> Result<ExitStatus> {
        let status = self.wait().await?;
        if self.remove_on_finish {
            self.engine.remove_container(&self.id, true).await?;
        }
        Ok(status)
    }
}

/// One chunk of an exec stream, re-exported for convenience.
pub type Chunk = ExecChunk;
