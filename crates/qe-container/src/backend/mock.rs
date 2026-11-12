//! In-memory backend used by tests and UI dry-runs.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use super::Backend;
use crate::error::{Error, Result};
use crate::model::{
    ContainerDetail, ContainerFilter, ContainerNetwork, ContainerSpec, ContainerStateKind,
    ContainerSummary, EngineInfo, ExitStatus, HealthState, ImageInfo, MountPoint, NetworkContainer,
    NetworkInfo, NetworkSpec, PublishedPort,
};
use crate::stream::{
    EventStream, ExecChunk, ExecResult, ExecSession, ExecSpec, LogLine, LogOptions, OutputStream,
    PullProgress, StatsSample,
};

#[derive(Debug, Clone)]
struct MockContainer {
    id: String,
    spec: ContainerSpec,
    state: ContainerStateKind,
    running: bool,
    paused: bool,
    exit_code: i64,
    oom_killed: bool,
    health: Option<HealthState>,
    logs: Vec<LogLine>,
}

#[derive(Debug, Clone)]
struct ExecRule {
    container: String,
    command_contains: String,
    exit_code: i64,
    output: String,
}

#[derive(Debug, Default)]
struct State {
    containers: HashMap<String, MockContainer>,
    networks: HashMap<String, NetworkInfo>,
    images: HashMap<String, ImageInfo>,
    exec_rules: Vec<ExecRule>,
    exec_results: HashMap<String, i64>,
    stats: Option<StatsSample>,
}

/// A deterministic, in-memory [`Backend`].
///
/// It is intentionally small: enough state to exercise service lifecycle,
/// drift detection, jobs, networks and health checks without a daemon.
#[derive(Debug, Clone, Default)]
pub struct MockBackend {
    state: Arc<Mutex<State>>,
    next_id: Arc<AtomicU64>,
}

impl MockBackend {
    /// Create an empty backend.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a container. Returns its id.
    pub fn seed_container(&self, spec: ContainerSpec, running: bool) -> String {
        let mut state = self.state.lock().expect("mock state poisoned");
        let id = self.new_id();
        let container = MockContainer {
            id: id.clone(),
            spec,
            state: if running {
                ContainerStateKind::Running
            } else {
                ContainerStateKind::Created
            },
            running,
            paused: false,
            exit_code: 0,
            oom_killed: false,
            health: None,
            logs: Vec::new(),
        };
        state.containers.insert(id.clone(), container);
        id
    }

    /// Seed an image.
    pub fn seed_image(&self, image: impl Into<String>) {
        let image = image.into();
        let mut state = self.state.lock().expect("mock state poisoned");
        state.images.insert(
            image.clone(),
            ImageInfo {
                id: format!("sha256:{image}"),
                tags: vec![image],
                size: 1,
                created: None,
            },
        );
    }

    /// Append a log line to a container.
    pub fn push_log(&self, container: &str, line: LogLine) -> Result<()> {
        let mut state = self.state.lock().expect("mock state poisoned");
        let container = find_container_mut(&mut state, container)
            .ok_or_else(|| Error::NotFound(container.to_string()))?;
        container.logs.push(line);
        Ok(())
    }

    /// Configure the result of future exec calls.
    pub fn set_exec_result(
        &self,
        container: impl Into<String>,
        command_contains: impl Into<String>,
        exit_code: i64,
        output: impl Into<String>,
    ) {
        let mut state = self.state.lock().expect("mock state poisoned");
        state.exec_rules.push(ExecRule {
            container: container.into(),
            command_contains: command_contains.into(),
            exit_code,
            output: output.into(),
        });
    }

    /// Configure a stats sample returned by [`Backend::stats`].
    pub fn set_stats(&self, sample: StatsSample) {
        let mut state = self.state.lock().expect("mock state poisoned");
        state.stats = Some(sample);
    }

    fn new_id(&self) -> String {
        let value = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        format!("mock-{value:08x}")
    }

    fn network_for(name: &str) -> NetworkInfo {
        NetworkInfo {
            id: format!("net-{name}"),
            name: name.to_string(),
            driver: "bridge".to_string(),
            subnet: None,
            gateway: None,
            bridge_name: None,
            labels: BTreeMap::new(),
            options: BTreeMap::new(),
            containers: Vec::new(),
        }
    }
}

fn find_container<'a>(state: &'a State, id_or_name: &str) -> Option<&'a MockContainer> {
    state.containers.get(id_or_name).or_else(|| {
        state
            .containers
            .values()
            .find(|item| item.spec.name == id_or_name)
    })
}

fn find_container_mut<'a>(state: &'a mut State, id_or_name: &str) -> Option<&'a mut MockContainer> {
    if state.containers.contains_key(id_or_name) {
        return state.containers.get_mut(id_or_name);
    }
    let id = state
        .containers
        .values()
        .find(|item| item.spec.name == id_or_name)
        .map(|item| item.id.clone())?;
    state.containers.get_mut(&id)
}

fn detail(container: &MockContainer) -> ContainerDetail {
    let mut host_port_offset = 0u16;
    let ports = container
        .spec
        .ports
        .iter()
        .map(|port| {
            host_port_offset = host_port_offset.saturating_add(1);
            PublishedPort {
                container: port.container,
                host: Some(port.host.unwrap_or_else(|| 30_000 + host_port_offset)),
                host_ip: Some(port.host_ip),
                protocol: port.protocol,
            }
        })
        .collect::<Vec<_>>();

    let mounts = container
        .spec
        .mounts
        .iter()
        .map(|mount| MountPoint {
            source: mount.source.to_string_lossy().into_owned(),
            target: mount.target.clone(),
            read_only: mount.read_only,
            kind: match mount.kind {
                crate::model::MountKind::Bind => "bind".to_string(),
                crate::model::MountKind::Volume => "volume".to_string(),
            },
        })
        .collect::<Vec<_>>();

    let networks = container
        .spec
        .network
        .as_ref()
        .map(|network| {
            vec![ContainerNetwork {
                name: network.clone(),
                ip_address: container.spec.static_ip.map(|ip| ip.to_string()),
                gateway: None,
            }]
        })
        .unwrap_or_default();

    ContainerDetail {
        id: container.id.clone(),
        name: container.spec.name.clone(),
        image: container.spec.image.clone(),
        image_id: Some(format!("sha256:{}", container.spec.image)),
        state: container.state,
        running: container.running,
        paused: container.paused,
        exit_code: Some(container.exit_code),
        oom_killed: container.oom_killed,
        health: container.health,
        labels: container.spec.labels.clone(),
        ports,
        mounts,
        networks,
        created: None,
        restart_count: 0,
    }
}

fn summary(container: &MockContainer) -> ContainerSummary {
    let detail = detail(container);
    ContainerSummary {
        id: detail.id,
        names: vec![detail.name],
        image: detail.image,
        image_id: detail.image_id,
        command: None,
        state: detail.state,
        status: Some(
            match detail.state {
                ContainerStateKind::Running => "Up",
                ContainerStateKind::Exited => "Exited",
                ContainerStateKind::Created => "Created",
                _ => "Unknown",
            }
            .to_string(),
        ),
        labels: detail.labels,
        ports: detail.ports,
        created: None,
    }
}

#[async_trait]
impl Backend for MockBackend {
    async fn info(&self) -> Result<EngineInfo> {
        let state = self.state.lock().expect("mock state poisoned");
        let containers = state.containers.len() as i64;
        let running = state
            .containers
            .values()
            .filter(|item| item.running)
            .count() as i64;
        Ok(EngineInfo {
            version: "mock".to_string(),
            api_version: "1.53".to_string(),
            min_api_version: Some("1.24".to_string()),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            rootless: false,
            security_options: vec!["mock".to_string()],
            containers,
            containers_running: running,
            images: state.images.len() as i64,
        })
    }

    async fn list_containers(&self, filter: &ContainerFilter) -> Result<Vec<ContainerSummary>> {
        let state = self.state.lock().expect("mock state poisoned");
        let mut containers = state
            .containers
            .values()
            .filter(|container| filter.all || container.running)
            .filter(|container| {
                filter
                    .name
                    .as_ref()
                    .is_none_or(|name| container.spec.name.contains(name))
            })
            .filter(|container| filter.status.is_none_or(|status| container.state == status))
            .filter(|container| {
                filter.network.as_ref().is_none_or(|network| {
                    container.spec.network.as_deref() == Some(network.as_str())
                })
            })
            .filter(|container| {
                filter.label.as_ref().is_none_or(|label| {
                    if let Some((key, value)) = label.split_once('=') {
                        container.spec.labels.get(key).map(String::as_str) == Some(value)
                    } else {
                        container.spec.labels.contains_key(label)
                    }
                })
            })
            .map(summary)
            .collect::<Vec<_>>();
        containers.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(containers)
    }

    async fn inspect_container(&self, id: &str) -> Result<ContainerDetail> {
        let state = self.state.lock().expect("mock state poisoned");
        find_container(&state, id)
            .map(detail)
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    async fn create_container(&self, spec: &ContainerSpec) -> Result<String> {
        spec.validate()?;
        let mut state = self.state.lock().expect("mock state poisoned");
        if state
            .containers
            .values()
            .any(|container| container.spec.name == spec.name)
        {
            return Err(Error::Conflict(format!(
                "container `{}` already exists",
                spec.name
            )));
        }
        let id = self.new_id();
        state.containers.insert(
            id.clone(),
            MockContainer {
                id: id.clone(),
                spec: spec.clone(),
                state: ContainerStateKind::Created,
                running: false,
                paused: false,
                exit_code: 0,
                oom_killed: false,
                health: None,
                logs: Vec::new(),
            },
        );
        Ok(id)
    }

    async fn start_container(&self, id: &str) -> Result<()> {
        let mut state = self.state.lock().expect("mock state poisoned");
        let container =
            find_container_mut(&mut state, id).ok_or_else(|| Error::NotFound(id.to_string()))?;
        if container.running {
            return Err(Error::Conflict(format!(
                "container `{}` is already running",
                container.spec.name
            )));
        }
        container.running = true;
        container.paused = false;
        container.state = ContainerStateKind::Running;
        Ok(())
    }

    async fn stop_container(&self, id: &str, _timeout: Option<Duration>) -> Result<()> {
        let mut state = self.state.lock().expect("mock state poisoned");
        let container =
            find_container_mut(&mut state, id).ok_or_else(|| Error::NotFound(id.to_string()))?;
        container.running = false;
        container.state = ContainerStateKind::Exited;
        container.exit_code = 0;
        Ok(())
    }

    async fn restart_container(&self, id: &str, _timeout: Option<Duration>) -> Result<()> {
        let mut state = self.state.lock().expect("mock state poisoned");
        let container =
            find_container_mut(&mut state, id).ok_or_else(|| Error::NotFound(id.to_string()))?;
        container.running = true;
        container.state = ContainerStateKind::Running;
        Ok(())
    }

    async fn remove_container(&self, id: &str, force: bool) -> Result<()> {
        let mut state = self.state.lock().expect("mock state poisoned");
        let container =
            find_container(&state, id).ok_or_else(|| Error::NotFound(id.to_string()))?;
        if container.running && !force {
            return Err(Error::Conflict(format!(
                "container `{}` is running",
                container.spec.name
            )));
        }
        let id = container.id.clone();
        state.containers.remove(&id);
        Ok(())
    }

    fn logs(&self, id: &str, _options: &LogOptions) -> Result<EventStream<LogLine>> {
        let state = self.state.lock().expect("mock state poisoned");
        let container =
            find_container(&state, id).ok_or_else(|| Error::NotFound(id.to_string()))?;
        Ok(Box::pin(futures_util::stream::iter(
            container.logs.clone().into_iter().map(Ok),
        )))
    }

    async fn wait_container(&self, id: &str) -> Result<ExitStatus> {
        let mut state = self.state.lock().expect("mock state poisoned");
        let container =
            find_container_mut(&mut state, id).ok_or_else(|| Error::NotFound(id.to_string()))?;
        container.running = false;
        container.state = ContainerStateKind::Exited;
        Ok(ExitStatus {
            code: container.exit_code,
            error: None,
            oom_killed: container.oom_killed,
        })
    }

    async fn exec(&self, id: &str, spec: &ExecSpec) -> Result<ExecSession> {
        let mut state = self.state.lock().expect("mock state poisoned");
        let (container_name, container_id, running) = {
            let container =
                find_container(&state, id).ok_or_else(|| Error::NotFound(id.to_string()))?;
            (
                container.spec.name.clone(),
                container.id.clone(),
                container.running,
            )
        };
        if !running {
            return Err(Error::Conflict(format!(
                "container `{container_name}` is not running"
            )));
        }

        let command = spec.command.join(" ");
        let rule = state
            .exec_rules
            .iter()
            .find(|rule| {
                (rule.container == "*"
                    || rule.container == container_name
                    || rule.container == container_id)
                    && command.contains(&rule.command_contains)
            })
            .cloned()
            .unwrap_or(ExecRule {
                container: "*".to_string(),
                command_contains: String::new(),
                exit_code: 0,
                output: String::new(),
            });

        let exec_id = format!("exec-{}", self.new_id());
        state.exec_results.insert(exec_id.clone(), rule.exit_code);
        let chunk = rule.output;
        Ok(ExecSession {
            id: exec_id,
            output: Box::pin(futures_util::stream::iter((!chunk.is_empty()).then_some(
                Ok(ExecChunk {
                    stream: OutputStream::Stdout,
                    text: chunk,
                }),
            ))),
        })
    }

    async fn inspect_exec(&self, exec_id: &str) -> Result<Option<i64>> {
        let state = self.state.lock().expect("mock state poisoned");
        Ok(state.exec_results.get(exec_id).copied())
    }

    fn stats(&self, _id: &str) -> Result<EventStream<StatsSample>> {
        let sample = self
            .state
            .lock()
            .expect("mock state poisoned")
            .stats
            .clone()
            .unwrap_or(StatsSample {
                read_at: None,
                cpu_percent: Some(0.0),
                memory_usage: Some(0),
                memory_limit: Some(0),
                network_rx: Some(0),
                network_tx: Some(0),
                pids: Some(0),
            });
        Ok(Box::pin(futures_util::stream::iter(vec![Ok(sample)])))
    }

    fn pull_image(
        &self,
        image: &str,
        _platform: Option<&str>,
    ) -> Result<EventStream<PullProgress>> {
        {
            let mut state = self.state.lock().expect("mock state poisoned");
            state
                .images
                .entry(image.to_string())
                .or_insert_with(|| ImageInfo {
                    id: format!("sha256:{image}"),
                    tags: vec![image.to_string()],
                    size: 1,
                    created: None,
                });
        }
        Ok(Box::pin(futures_util::stream::iter(vec![Ok(
            PullProgress {
                status: "pulled".to_string(),
                id: None,
                current: Some(1),
                total: Some(1),
            },
        )])))
    }

    async fn inspect_image(&self, image: &str) -> Result<Option<ImageInfo>> {
        let state = self.state.lock().expect("mock state poisoned");
        Ok(state.images.get(image).cloned())
    }

    async fn list_images(&self) -> Result<Vec<ImageInfo>> {
        let state = self.state.lock().expect("mock state poisoned");
        Ok(state.images.values().cloned().collect())
    }

    async fn list_networks(&self) -> Result<Vec<NetworkInfo>> {
        let state = self.state.lock().expect("mock state poisoned");
        Ok(state.networks.values().cloned().collect())
    }

    async fn inspect_network(&self, name: &str) -> Result<NetworkInfo> {
        let state = self.state.lock().expect("mock state poisoned");
        state
            .networks
            .get(name)
            .cloned()
            .or_else(|| {
                state
                    .networks
                    .values()
                    .find(|network| network.name == name)
                    .cloned()
            })
            .ok_or_else(|| Error::NotFound(name.to_string()))
    }

    async fn create_network(&self, spec: &NetworkSpec) -> Result<String> {
        spec.validate()?;
        let mut state = self.state.lock().expect("mock state poisoned");
        if state.networks.contains_key(&spec.name) {
            return Err(Error::Conflict(format!(
                "network `{}` already exists",
                spec.name
            )));
        }
        let network = NetworkInfo {
            id: format!("net-{}", spec.name),
            name: spec.name.clone(),
            driver: spec.driver.clone(),
            subnet: spec.subnet,
            gateway: spec.gateway,
            bridge_name: spec.bridge_name.clone(),
            labels: spec.labels.clone(),
            options: spec
                .bridge_name
                .as_ref()
                .map(|bridge| {
                    BTreeMap::from([("com.docker.network.bridge.name".to_string(), bridge.clone())])
                })
                .unwrap_or_default(),
            containers: Vec::new(),
        };
        let id = network.id.clone();
        state.networks.insert(spec.name.clone(), network);
        Ok(id)
    }

    async fn remove_network(&self, name: &str) -> Result<()> {
        let mut state = self.state.lock().expect("mock state poisoned");
        let key = state
            .networks
            .iter()
            .find(|(_, network)| network.name == name || network.id == name)
            .map(|(key, _)| key.clone())
            .ok_or_else(|| Error::NotFound(name.to_string()))?;
        if state
            .networks
            .get(&key)
            .is_some_and(|network| !network.containers.is_empty())
        {
            return Err(Error::Conflict(format!(
                "network `{name}` still has containers attached"
            )));
        }
        state.networks.remove(&key);
        Ok(())
    }

    async fn connect_network(
        &self,
        network: &str,
        container: &str,
        ip: Option<IpAddr>,
    ) -> Result<()> {
        let mut state = self.state.lock().expect("mock state poisoned");
        let container_id = find_container(&state, container)
            .map(|item| item.id.clone())
            .ok_or_else(|| Error::NotFound(container.to_string()))?;
        let container_name = find_container(&state, &container_id)
            .map(|item| item.spec.name.clone())
            .unwrap_or_default();
        let network = state
            .networks
            .get_mut(network)
            .ok_or_else(|| Error::NotFound(network.to_string()))?;
        network.containers.push(NetworkContainer {
            id: container_id,
            name: container_name,
            ipv4: ip.map(|ip| ip.to_string()),
        });
        Ok(())
    }

    async fn disconnect_network(&self, network: &str, container: &str, force: bool) -> Result<()> {
        let mut state = self.state.lock().expect("mock state poisoned");
        let network = state
            .networks
            .get_mut(network)
            .ok_or_else(|| Error::NotFound(network.to_string()))?;
        if !force {
            let exists = network
                .containers
                .iter()
                .any(|item| item.name == container || item.id == container);
            if !exists {
                return Err(Error::NotFound(container.to_string()));
            }
        }
        network
            .containers
            .retain(|item| item.name != container && item.id != container);
        Ok(())
    }
}

impl MockBackend {
    /// Network that is created implicitly by `seed_network`.
    pub fn seed_network(&self, spec: &NetworkSpec) -> Result<()> {
        let mut state = self.state.lock().expect("mock state poisoned");
        state
            .networks
            .insert(spec.name.clone(), MockBackend::network_for(&spec.name));
        Ok(())
    }

    /// Last exec result, useful in tests.
    pub fn exec_result(&self, exec_id: &str) -> Option<i64> {
        let state = self.state.lock().expect("mock state poisoned");
        state.exec_results.get(exec_id).copied()
    }
}

/// Helper for tests that want a captured exec result.
pub async fn capture(
    engine: &crate::Engine,
    container: &str,
    command: &[&str],
) -> Result<ExecResult> {
    engine
        .exec_capture(container, &ExecSpec::new(command.iter().copied()))
        .await
}
