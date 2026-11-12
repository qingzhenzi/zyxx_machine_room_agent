//! Docker Engine API backend powered by `bollard`.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::models::{
    ContainerCreateBody, ContainerInspectResponse, ContainerStateStatusEnum,
    ContainerStatsResponse, EndpointIpamConfig, EndpointResource, EndpointSettings,
    HealthStatusEnum, HostConfig, HostConfigLogConfig, ImageInspect, ImageSummary, Ipam,
    IpamConfig, Mount as DockerMount, MountPoint as DockerMountPoint, MountType, Network,
    NetworkConnectRequest, NetworkCreateRequest, NetworkDisconnectRequest, NetworkInspect,
    PortBinding as DockerPortBinding, PortMap, PortSummary, PortSummaryTypeEnum, ResourcesUlimits,
    RestartPolicy as DockerRestartPolicy, RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, InspectContainerOptions,
    InspectNetworkOptions, ListContainersOptionsBuilder, ListImagesOptions, ListNetworksOptions,
    LogsOptionsBuilder, RemoveContainerOptionsBuilder, RestartContainerOptionsBuilder,
    StatsOptionsBuilder, StopContainerOptionsBuilder, WaitContainerOptions,
};
use futures_util::StreamExt;

use super::Backend;
use crate::error::{Error, Result, from_bollard};
use crate::model::{
    ContainerDetail, ContainerFilter, ContainerNetwork, ContainerSpec, ContainerStateKind,
    ContainerSummary, EngineInfo, ExitStatus, HealthState, ImageInfo, LogConfig, MountKind,
    NetworkContainer, NetworkInfo, NetworkSpec, Protocol, PublishedPort,
};
use crate::stream::{
    EventStream, ExecChunk, ExecSession, ExecSpec, LogLine, LogOptions, OutputStream, PullProgress,
    StatsSample,
};

/// Docker Engine API backend.
#[derive(Debug, Clone)]
pub struct DockerBackend {
    docker: Docker,
}

impl DockerBackend {
    /// Wrap an existing bollard client.
    pub fn new(docker: Docker) -> Self {
        Self { docker }
    }

    /// Access the underlying client.
    pub fn docker(&self) -> &Docker {
        &self.docker
    }
}

#[async_trait]
impl Backend for DockerBackend {
    async fn info(&self) -> Result<EngineInfo> {
        let info = self.docker.info().await.map_err(from_bollard)?;
        let version = self.docker.version().await.map_err(from_bollard)?;
        let security_options = info.security_options.unwrap_or_default();
        let rootless = security_options.iter().any(|option| {
            option.eq_ignore_ascii_case("rootless") || option.starts_with("rootless,")
        });

        Ok(EngineInfo {
            version: version.version.unwrap_or_default(),
            api_version: version
                .api_version
                .unwrap_or_else(|| self.docker.client_version().to_string()),
            min_api_version: version.min_api_version,
            os: version.os.unwrap_or_default(),
            arch: version.arch.unwrap_or_default(),
            rootless,
            security_options,
            containers: info.containers.unwrap_or_default(),
            containers_running: info.containers_running.unwrap_or_default(),
            images: info.images.unwrap_or_default(),
        })
    }

    async fn list_containers(&self, filter: &ContainerFilter) -> Result<Vec<ContainerSummary>> {
        let mut filters: HashMap<String, Vec<String>> = HashMap::new();
        if let Some(name) = &filter.name {
            filters.insert("name".to_string(), vec![name.clone()]);
        }
        if let Some(label) = &filter.label {
            filters.insert("label".to_string(), vec![label.clone()]);
        }
        if let Some(status) = filter.status {
            filters.insert("status".to_string(), vec![state_name(status).to_string()]);
        }
        if let Some(network) = &filter.network {
            filters.insert("network".to_string(), vec![network.clone()]);
        }

        let options = ListContainersOptionsBuilder::default()
            .all(filter.all)
            .filters(&filters)
            .build();

        let containers = self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(from_bollard)?;

        Ok(containers.into_iter().map(summary_from_docker).collect())
    }

    async fn inspect_container(&self, id: &str) -> Result<ContainerDetail> {
        let inspect = self
            .docker
            .inspect_container(id, None::<InspectContainerOptions>)
            .await
            .map_err(from_bollard)?;
        detail_from_inspect(inspect)
    }

    async fn create_container(&self, spec: &ContainerSpec) -> Result<String> {
        spec.validate()?;

        let mut options = CreateContainerOptionsBuilder::default().name(&spec.name);
        if let Some(platform) = &spec.platform {
            options = options.platform(platform);
        }

        let config = ContainerCreateBody {
            image: Some(spec.image.clone()),
            cmd: (!spec.command.is_empty()).then(|| spec.command.clone()),
            entrypoint: spec.entrypoint.clone(),
            env: (!spec.env.is_empty()).then(|| {
                spec.env
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect()
            }),
            working_dir: spec.workdir.clone(),
            user: spec.user.clone(),
            hostname: spec.hostname.clone(),
            labels: Some(labels_from(spec)),
            stop_signal: spec.stop_signal.clone(),
            stop_timeout: spec
                .stop_grace
                .map(|grace| i64::try_from(grace.as_secs()).unwrap_or(i64::MAX)),
            exposed_ports: (!spec.ports.is_empty()).then(|| spec.exposed_ports()),
            host_config: Some(host_config(spec)),
            networking_config: networking_config(spec),
            ..Default::default()
        };

        let response = self
            .docker
            .create_container(Some(options.build()), config)
            .await
            .map_err(from_bollard)?;
        Ok(response.id)
    }

    async fn start_container(&self, id: &str) -> Result<()> {
        self.docker
            .start_container(id, None::<bollard::query_parameters::StartContainerOptions>)
            .await
            .map_err(from_bollard)
    }

    async fn stop_container(&self, id: &str, timeout: Option<Duration>) -> Result<()> {
        let options = timeout.map(|timeout| {
            StopContainerOptionsBuilder::default()
                .t(i32::try_from(timeout.as_secs()).unwrap_or(i32::MAX))
                .build()
        });
        self.docker
            .stop_container(id, options)
            .await
            .map_err(from_bollard)
    }

    async fn restart_container(&self, id: &str, timeout: Option<Duration>) -> Result<()> {
        let options = timeout.map(|timeout| {
            RestartContainerOptionsBuilder::default()
                .t(i32::try_from(timeout.as_secs()).unwrap_or(i32::MAX))
                .build()
        });
        self.docker
            .restart_container(id, options)
            .await
            .map_err(from_bollard)
    }

    async fn remove_container(&self, id: &str, force: bool) -> Result<()> {
        let options = RemoveContainerOptionsBuilder::default()
            .force(force)
            .v(true)
            .build();
        self.docker
            .remove_container(id, Some(options))
            .await
            .map_err(from_bollard)
    }

    fn logs(&self, id: &str, options: &LogOptions) -> Result<EventStream<LogLine>> {
        let mut builder = LogsOptionsBuilder::default()
            .follow(options.follow)
            .stdout(options.stdout)
            .stderr(options.stderr)
            .timestamps(options.timestamps)
            .tail(
                &options
                    .tail
                    .map(|tail| tail.to_string())
                    .unwrap_or_else(|| "all".to_string()),
            );
        if let Some(since) = options.since {
            builder = builder.since(i32::try_from(since).unwrap_or(i32::MAX));
        }
        if let Some(until) = options.until {
            builder = builder.until(i32::try_from(until).unwrap_or(i32::MAX));
        }

        let stream = self
            .docker
            .logs(id, Some(builder.build()))
            .map(|item| item.map(log_line).map_err(from_bollard));
        Ok(Box::pin(stream))
    }

    async fn wait_container(&self, id: &str) -> Result<ExitStatus> {
        let mut stream = self.docker.wait_container(id, None::<WaitContainerOptions>);
        let item = stream.next().await.ok_or_else(|| {
            Error::DockerClient(format!("wait stream for `{id}` ended without a response"))
        })?;

        let (code, error) = match item {
            Ok(response) => (
                response.status_code,
                response.error.and_then(|error| error.message),
            ),
            // bollard converts non-zero exits into this error variant. For a
            // job runner a non-zero exit is data, not a transport failure.
            Err(bollard::errors::Error::DockerContainerWaitError { error, code }) => {
                (code, (!error.is_empty()).then_some(error))
            }
            Err(error) => return Err(from_bollard(error)),
        };

        let oom_killed = self
            .inspect_container(id)
            .await
            .map(|detail| detail.oom_killed)
            .unwrap_or(false);

        Ok(ExitStatus {
            code,
            error,
            oom_killed,
        })
    }

    async fn exec(&self, id: &str, spec: &ExecSpec) -> Result<ExecSession> {
        if spec.command.is_empty() {
            return Err(Error::InvalidSpec("exec command must not be empty".into()));
        }

        let options = CreateExecOptions {
            cmd: Some(spec.command.clone()),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            attach_stdin: Some(false),
            tty: Some(spec.tty),
            env: (!spec.env.is_empty()).then(|| {
                spec.env
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<_>>()
            }),
            working_dir: spec.workdir.clone(),
            user: spec.user.clone(),
            ..Default::default()
        };

        let created = self
            .docker
            .create_exec(id, options)
            .await
            .map_err(from_bollard)?;

        let started = self
            .docker
            .start_exec(
                &created.id,
                Some(StartExecOptions {
                    detach: false,
                    tty: spec.tty,
                    output_capacity: None,
                }),
            )
            .await
            .map_err(from_bollard)?;

        match started {
            StartExecResults::Attached { output, .. } => {
                let stream = output.map(|item| item.map(exec_chunk).map_err(from_bollard));
                Ok(ExecSession {
                    id: created.id,
                    output: Box::pin(stream),
                })
            }
            StartExecResults::Detached => Err(Error::DockerClient(
                "exec started in detached mode unexpectedly".to_string(),
            )),
        }
    }

    async fn inspect_exec(&self, exec_id: &str) -> Result<Option<i64>> {
        let inspect = self
            .docker
            .inspect_exec(exec_id)
            .await
            .map_err(from_bollard)?;
        Ok(inspect.exit_code)
    }

    fn stats(&self, id: &str) -> Result<EventStream<StatsSample>> {
        let options = StatsOptionsBuilder::default()
            .stream(true)
            .one_shot(false)
            .build();
        let stream = self
            .docker
            .stats(id, Some(options))
            .map(|item| item.map(stats_sample).map_err(from_bollard));
        Ok(Box::pin(stream))
    }

    fn pull_image(&self, image: &str, platform: Option<&str>) -> Result<EventStream<PullProgress>> {
        let mut builder = CreateImageOptionsBuilder::default().from_image(image);
        if let Some(platform) = platform {
            builder = builder.platform(platform);
        }
        let stream = self
            .docker
            .create_image(Some(builder.build()), None, None)
            .map(|item| item.map(pull_progress).map_err(from_bollard));
        Ok(Box::pin(stream))
    }

    async fn inspect_image(&self, image: &str) -> Result<Option<ImageInfo>> {
        match self.docker.inspect_image(image).await {
            Ok(inspect) => Ok(Some(image_info_from_inspect(inspect))),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(error) => Err(from_bollard(error)),
        }
    }

    async fn list_images(&self) -> Result<Vec<ImageInfo>> {
        let images = self
            .docker
            .list_images(None::<ListImagesOptions>)
            .await
            .map_err(from_bollard)?;
        Ok(images.into_iter().map(image_info_from_summary).collect())
    }

    async fn list_networks(&self) -> Result<Vec<NetworkInfo>> {
        let networks = self
            .docker
            .list_networks(None::<ListNetworksOptions>)
            .await
            .map_err(from_bollard)?;
        Ok(networks.into_iter().map(network_info_from_list).collect())
    }

    async fn inspect_network(&self, name: &str) -> Result<NetworkInfo> {
        let network = self
            .docker
            .inspect_network(name, None::<InspectNetworkOptions>)
            .await
            .map_err(from_bollard)?;
        Ok(network_info_from_inspect(network))
    }

    async fn create_network(&self, spec: &NetworkSpec) -> Result<String> {
        spec.validate()?;
        let mut options: HashMap<String, String> = HashMap::new();
        if let Some(bridge_name) = &spec.bridge_name {
            options.insert(
                "com.docker.network.bridge.name".to_string(),
                bridge_name.clone(),
            );
        }

        let ipam = spec.subnet.map(|subnet| Ipam {
            driver: Some("default".to_string()),
            config: Some(vec![IpamConfig {
                subnet: Some(subnet.to_string()),
                gateway: spec.gateway.map(|gateway| gateway.to_string()),
                ..Default::default()
            }]),
            ..Default::default()
        });

        let request = NetworkCreateRequest {
            name: spec.name.clone(),
            driver: Some(spec.driver.clone()),
            ipam,
            options: (!options.is_empty()).then_some(options),
            labels: (!spec.labels.is_empty()).then(|| map_to_hashmap(&spec.labels)),
            internal: spec.internal.then_some(true),
            attachable: spec.attachable.then_some(true),
            ..Default::default()
        };

        let response = self
            .docker
            .create_network(request)
            .await
            .map_err(from_bollard)?;
        Ok(response.id)
    }

    async fn remove_network(&self, name: &str) -> Result<()> {
        self.docker.remove_network(name).await.map_err(from_bollard)
    }

    async fn connect_network(
        &self,
        network: &str,
        container: &str,
        ip: Option<IpAddr>,
    ) -> Result<()> {
        let request = NetworkConnectRequest {
            container: container.to_string(),
            endpoint_config: ip.map(|ip| EndpointSettings {
                ipam_config: Some(EndpointIpamConfig {
                    ipv4_address: Some(ip.to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        };
        self.docker
            .connect_network(network, request)
            .await
            .map_err(from_bollard)
    }

    async fn disconnect_network(&self, network: &str, container: &str, force: bool) -> Result<()> {
        let request = NetworkDisconnectRequest {
            container: container.to_string(),
            force: Some(force),
        };
        self.docker
            .disconnect_network(network, request)
            .await
            .map_err(from_bollard)
    }
}

fn state_name(state: ContainerStateKind) -> &'static str {
    match state {
        ContainerStateKind::Created => "created",
        ContainerStateKind::Running => "running",
        ContainerStateKind::Paused => "paused",
        ContainerStateKind::Restarting => "restarting",
        ContainerStateKind::Removing => "removing",
        ContainerStateKind::Exited => "exited",
        ContainerStateKind::Dead => "dead",
        ContainerStateKind::Stopping => "stopping",
        ContainerStateKind::Unknown => "unknown",
    }
}

fn labels_from(spec: &ContainerSpec) -> HashMap<String, String> {
    map_to_hashmap(&spec.labels)
}

fn map_to_hashmap(map: &BTreeMap<String, String>) -> HashMap<String, String> {
    map.iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn host_config(spec: &ContainerSpec) -> HostConfig {
    let mounts = (!spec.mounts.is_empty()).then(|| {
        spec.mounts
            .iter()
            .map(|mount| DockerMount {
                target: Some(mount.target.clone()),
                source: Some(mount.source.to_string_lossy().to_string()),
                typ: Some(match mount.kind {
                    MountKind::Bind => MountType::BIND,
                    MountKind::Volume => MountType::VOLUME,
                }),
                read_only: mount.read_only.then_some(true),
                ..Default::default()
            })
            .collect::<Vec<_>>()
    });

    let port_bindings = (!spec.ports.is_empty()).then(|| {
        let mut map: PortMap = HashMap::new();
        for port in &spec.ports {
            map.entry(port.key())
                .or_insert_with(|| Some(Vec::new()))
                .get_or_insert_with(Vec::new)
                .push(DockerPortBinding {
                    host_ip: Some(port.host_ip.to_string()),
                    host_port: port.host.map(|host| host.to_string()),
                });
        }
        map
    });

    let cap_drop = (!spec.security.cap_drop.is_empty()).then(|| spec.security.cap_drop.clone());
    let cap_add = (!spec.security.cap_add.is_empty()).then(|| spec.security.cap_add.clone());

    let security_opt = spec
        .security
        .no_new_privileges
        .then(|| vec!["no-new-privileges".to_string()]);

    let tmpfs = (!spec.security.tmpfs.is_empty()).then(|| map_to_hashmap(&spec.security.tmpfs));

    let ulimits = (!spec.security.ulimits.is_empty()).then(|| {
        spec.security
            .ulimits
            .iter()
            .map(|ulimit| ResourcesUlimits {
                name: Some(ulimit.name.clone()),
                soft: Some(ulimit.soft),
                hard: Some(ulimit.hard),
            })
            .collect::<Vec<_>>()
    });

    let log_config = Some(HostConfigLogConfig {
        typ: Some(spec.log.driver.clone()),
        config: Some(log_config_map(&spec.log)),
    });

    HostConfig {
        memory: spec.resources.memory.map(|memory| memory.0 as i64),
        nano_cpus: spec
            .resources
            .cpus
            .map(|cpus| (cpus * 1_000_000_000.0).round() as i64),
        pids_limit: spec.resources.pids,
        mounts,
        port_bindings,
        restart_policy: Some(DockerRestartPolicy {
            name: Some(match spec.restart {
                crate::model::RestartPolicy::No => RestartPolicyNameEnum::NO,
                crate::model::RestartPolicy::OnFailure => RestartPolicyNameEnum::ON_FAILURE,
                crate::model::RestartPolicy::UnlessStopped => RestartPolicyNameEnum::UNLESS_STOPPED,
                crate::model::RestartPolicy::Always => RestartPolicyNameEnum::ALWAYS,
            }),
            maximum_retry_count: None,
        }),
        readonly_rootfs: spec.security.read_only.then_some(true),
        cap_drop,
        cap_add,
        security_opt,
        tmpfs,
        ulimits,
        log_config,
        network_mode: spec.network.clone(),
        ..Default::default()
    }
}

fn log_config_map(config: &LogConfig) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(max_size) = config.max_size {
        if max_size.0 > 0 {
            map.insert("max-size".to_string(), max_size.to_string());
        }
    }
    if let Some(max_file) = config.max_file {
        map.insert("max-file".to_string(), max_file.to_string());
    }
    map
}

fn networking_config(spec: &ContainerSpec) -> Option<bollard::models::NetworkingConfig> {
    let network = spec.network.as_ref()?;
    let endpoint = EndpointSettings {
        ipam_config: spec.static_ip.map(|ip| EndpointIpamConfig {
            ipv4_address: Some(ip.to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut endpoints = HashMap::new();
    endpoints.insert(network.clone(), endpoint);
    Some(bollard::models::NetworkingConfig {
        endpoints_config: Some(endpoints),
    })
}

fn log_line(output: LogOutput) -> LogLine {
    let (stream, text) = match output {
        LogOutput::StdOut { message } => (OutputStream::Stdout, message),
        LogOutput::StdErr { message } => (OutputStream::Stderr, message),
        LogOutput::Console { message } | LogOutput::StdIn { message } => {
            (OutputStream::Console, message)
        }
    };
    LogLine {
        stream,
        text: String::from_utf8_lossy(&text).into_owned(),
    }
}

fn exec_chunk(output: LogOutput) -> ExecChunk {
    let (stream, text) = match output {
        LogOutput::StdOut { message } => (OutputStream::Stdout, message),
        LogOutput::StdErr { message } => (OutputStream::Stderr, message),
        LogOutput::Console { message } | LogOutput::StdIn { message } => {
            (OutputStream::Console, message)
        }
    };
    ExecChunk {
        stream,
        text: String::from_utf8_lossy(&text).into_owned(),
    }
}

fn pull_progress(info: bollard::models::CreateImageInfo) -> PullProgress {
    PullProgress {
        status: info.status.unwrap_or_default(),
        id: info.id,
        current: info
            .progress_detail
            .as_ref()
            .and_then(|detail| detail.current),
        total: info
            .progress_detail
            .as_ref()
            .and_then(|detail| detail.total),
    }
}

fn stats_sample(stats: ContainerStatsResponse) -> StatsSample {
    let cpu = stats.cpu_stats.as_ref();
    let precpu = stats.precpu_stats.as_ref();

    let cpu_percent = (|| {
        let total = cpu?.cpu_usage.as_ref()?.total_usage?;
        let pre_total = precpu?.cpu_usage.as_ref()?.total_usage?;
        let system = cpu?.system_cpu_usage?;
        let pre_system = precpu?.system_cpu_usage?;
        let delta = total.checked_sub(pre_total)?;
        let system_delta = system.checked_sub(pre_system)?;
        if system_delta == 0 {
            return None;
        }
        let online = cpu?.online_cpus.unwrap_or(1).max(1) as f64;
        Some(delta as f64 / system_delta as f64 * online * 100.0)
    })();

    let memory = stats.memory_stats.as_ref();
    let network_rx = stats.networks.as_ref().map(|networks| {
        networks
            .values()
            .filter_map(|network| network.rx_bytes)
            .sum::<u64>()
    });
    let network_tx = stats.networks.as_ref().map(|networks| {
        networks
            .values()
            .filter_map(|network| network.tx_bytes)
            .sum::<u64>()
    });

    StatsSample {
        read_at: stats.read,
        cpu_percent,
        memory_usage: memory.and_then(|memory| memory.usage),
        memory_limit: memory.and_then(|memory| memory.limit),
        network_rx,
        network_tx,
        pids: stats.pids_stats.and_then(|pids| pids.current),
    }
}

fn summary_from_docker(summary: bollard::models::ContainerSummary) -> ContainerSummary {
    ContainerSummary {
        id: summary.id.unwrap_or_default(),
        names: summary
            .names
            .unwrap_or_default()
            .into_iter()
            .map(|name| name.trim_start_matches('/').to_string())
            .collect(),
        image: summary.image.unwrap_or_default(),
        image_id: summary.image_id,
        command: summary.command,
        state: summary
            .state
            .map(|state| ContainerStateKind::parse(state.as_ref()))
            .unwrap_or(ContainerStateKind::Unknown),
        status: summary.status,
        labels: summary
            .labels
            .map(|labels| labels.into_iter().collect())
            .unwrap_or_default(),
        ports: summary
            .ports
            .unwrap_or_default()
            .into_iter()
            .map(port_from_summary)
            .collect(),
        created: summary.created,
    }
}

fn port_from_summary(port: PortSummary) -> PublishedPort {
    PublishedPort {
        container: port.private_port,
        host: port.public_port,
        host_ip: port.ip.and_then(|ip| ip.parse().ok()),
        protocol: match port.typ {
            Some(PortSummaryTypeEnum::UDP) => Protocol::Udp,
            _ => Protocol::Tcp,
        },
    }
}

fn detail_from_inspect(inspect: ContainerInspectResponse) -> Result<ContainerDetail> {
    let state = inspect.state.unwrap_or_default();
    let state_kind = state
        .status
        .map(|status| match status {
            ContainerStateStatusEnum::CREATED => ContainerStateKind::Created,
            ContainerStateStatusEnum::RUNNING => ContainerStateKind::Running,
            ContainerStateStatusEnum::PAUSED => ContainerStateKind::Paused,
            ContainerStateStatusEnum::RESTARTING => ContainerStateKind::Restarting,
            ContainerStateStatusEnum::REMOVING => ContainerStateKind::Removing,
            ContainerStateStatusEnum::EXITED => ContainerStateKind::Exited,
            ContainerStateStatusEnum::DEAD => ContainerStateKind::Dead,
            ContainerStateStatusEnum::STOPPING => ContainerStateKind::Stopping,
            _ => ContainerStateKind::Unknown,
        })
        .unwrap_or(ContainerStateKind::Unknown);

    let health = state
        .health
        .and_then(|health| health.status)
        .map(|status| match status {
            HealthStatusEnum::NONE => HealthState::None,
            HealthStatusEnum::STARTING => HealthState::Starting,
            HealthStatusEnum::HEALTHY => HealthState::Healthy,
            HealthStatusEnum::UNHEALTHY => HealthState::Unhealthy,
            _ => HealthState::Unknown,
        });

    let config = inspect.config.as_ref();
    let image = config
        .and_then(|config| config.image.clone())
        .unwrap_or_default();
    let labels = config
        .and_then(|config| config.labels.clone())
        .map(|labels| labels.into_iter().collect())
        .unwrap_or_default();

    let network_settings = inspect.network_settings;
    let ports = network_settings
        .as_ref()
        .and_then(|settings| settings.ports.clone())
        .map(parse_port_map)
        .unwrap_or_default();

    let networks = network_settings
        .and_then(|settings| settings.networks)
        .map(|networks| {
            networks
                .into_iter()
                .map(|(name, endpoint)| ContainerNetwork {
                    name,
                    ip_address: endpoint.ip_address,
                    gateway: endpoint.gateway,
                })
                .collect()
        })
        .unwrap_or_default();

    let mounts = inspect
        .mounts
        .unwrap_or_default()
        .into_iter()
        .map(mount_point_from_docker)
        .collect();

    Ok(ContainerDetail {
        id: inspect.id.unwrap_or_default(),
        name: inspect
            .name
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_string(),
        image,
        image_id: inspect.image,
        state: state_kind,
        running: state.running.unwrap_or(false),
        paused: state.paused.unwrap_or(false),
        exit_code: state.exit_code,
        oom_killed: state.oom_killed.unwrap_or(false),
        health,
        labels,
        ports,
        mounts,
        networks,
        created: inspect.created,
        restart_count: inspect.restart_count.unwrap_or_default(),
    })
}

fn mount_point_from_docker(mount: DockerMountPoint) -> crate::model::MountPoint {
    crate::model::MountPoint {
        source: mount.source.unwrap_or_default(),
        target: mount.destination.unwrap_or_default(),
        read_only: !mount.rw.unwrap_or(true),
        kind: mount.typ.unwrap_or_default(),
    }
}

fn parse_port_map(map: PortMap) -> Vec<PublishedPort> {
    let mut ports = Vec::new();
    for (key, bindings) in map {
        let (port, protocol) = key.split_once('/').unwrap_or((key.as_str(), "tcp"));
        let Ok(container) = port.parse::<u16>() else {
            continue;
        };
        let protocol = match protocol {
            "udp" => Protocol::Udp,
            _ => Protocol::Tcp,
        };
        match bindings {
            Some(bindings) if !bindings.is_empty() => {
                for binding in bindings {
                    ports.push(PublishedPort {
                        container,
                        host: binding.host_port.and_then(|port| port.parse::<u16>().ok()),
                        host_ip: binding.host_ip.and_then(|ip| ip.parse::<IpAddr>().ok()),
                        protocol,
                    });
                }
            }
            _ => ports.push(PublishedPort {
                container,
                host: None,
                host_ip: None,
                protocol,
            }),
        }
    }
    ports.sort_by_key(|port| (port.container, port.protocol.as_docker()));
    ports
}

fn image_info_from_inspect(inspect: ImageInspect) -> ImageInfo {
    ImageInfo {
        id: inspect.id.unwrap_or_default(),
        tags: inspect.repo_tags.unwrap_or_default(),
        size: inspect.size.unwrap_or_default().max(0) as u64,
        created: inspect.created,
    }
}

fn image_info_from_summary(summary: ImageSummary) -> ImageInfo {
    ImageInfo {
        id: summary.id,
        tags: summary.repo_tags,
        size: summary.size.max(0) as u64,
        created: Some(summary.created.to_string()),
    }
}

fn network_info_from_list(network: Network) -> NetworkInfo {
    let (subnet, gateway) = network_ipam(network.ipam);
    let bridge_name = network
        .options
        .as_ref()
        .and_then(|options| options.get("com.docker.network.bridge.name"))
        .cloned();
    let options = network
        .options
        .map(|options| options.into_iter().collect())
        .unwrap_or_default();
    NetworkInfo {
        id: network.id.unwrap_or_default(),
        name: network.name.unwrap_or_default(),
        driver: network.driver.unwrap_or_default(),
        subnet,
        gateway,
        bridge_name,
        labels: network
            .labels
            .map(|labels| labels.into_iter().collect())
            .unwrap_or_default(),
        options,
        containers: Vec::new(),
    }
}

fn network_info_from_inspect(network: NetworkInspect) -> NetworkInfo {
    let (subnet, gateway) = network_ipam(network.ipam);
    let bridge_name = network
        .options
        .as_ref()
        .and_then(|options| options.get("com.docker.network.bridge.name"))
        .cloned();
    let options = network
        .options
        .map(|options| options.into_iter().collect())
        .unwrap_or_default();
    let containers = network
        .containers
        .unwrap_or_default()
        .into_iter()
        .map(|(id, resource)| network_container_from_resource(id, resource))
        .collect();
    NetworkInfo {
        id: network.id.unwrap_or_default(),
        name: network.name.unwrap_or_default(),
        driver: network.driver.unwrap_or_default(),
        subnet,
        gateway,
        bridge_name,
        labels: network
            .labels
            .map(|labels| labels.into_iter().collect())
            .unwrap_or_default(),
        options,
        containers,
    }
}

fn network_container_from_resource(id: String, resource: EndpointResource) -> NetworkContainer {
    NetworkContainer {
        id,
        name: resource.name.unwrap_or_default(),
        ipv4: resource.ipv4_address,
    }
}

fn network_ipam(ipam: Option<Ipam>) -> (Option<ipnet::IpNet>, Option<IpAddr>) {
    let Some(config) = ipam
        .and_then(|ipam| ipam.config)
        .and_then(|configs| configs.into_iter().next())
    else {
        return (None, None);
    };
    let subnet = config.subnet.and_then(|subnet| subnet.parse().ok());
    let gateway = config.gateway.and_then(|gateway| gateway.parse().ok());
    (subnet, gateway)
}
