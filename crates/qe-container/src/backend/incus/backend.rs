use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hyper::Method;
use serde_json::{Value, json};
use tokio::time::sleep;

use super::client::{is_transient, parse_envelope, IncusClient};
use super::mapping::*;
use super::streams::{exec_stream, logs_stream, stats_stream};
use super::transport::{HttpUnixTransport, Transport};
use super::wire::*;
use super::{DEFAULT_INCUS_SOCKET, EXIT_CODE_FILE, POLL_INTERVAL};
use crate::backend::Backend;
use crate::config::EngineConfig;
use crate::error::{Error, Result};
use crate::model::*;
use crate::stream::{
    EventStream, ExecSession, ExecSpec, LogLine, LogOptions, PullProgress, StatsSample,
};

// ---------------------------------------------------------------------------
// IncusBackend
// ---------------------------------------------------------------------------

/// A [`Backend`] talking to a local Incus daemon over its unix socket.
#[derive(Clone)]
pub struct IncusBackend {
    client: IncusClient,
    socket: PathBuf,
    exec_results: Arc<Mutex<HashMap<String, i64>>>,
}

impl std::fmt::Debug for IncusBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IncusBackend")
            .field("socket", &self.socket)
            .finish_non_exhaustive()
    }
}

impl IncusBackend {
    /// Connect to the Incus daemon at `socket`, verifying it responds.
    pub async fn connect(socket: impl AsRef<Path>, timeout: Duration) -> Result<Self> {
        let socket = socket.as_ref().to_path_buf();
        let backend = Self::from_transport(Arc::new(HttpUnixTransport::new(&socket)), timeout, socket);
        // Verify the daemon is reachable.
        backend
            .client
            .request(&Method::GET, "/1.0", None)
            .await
            .map_err(|error| match error {
                Error::EngineUnavailable(message) => {
                    Error::EngineUnavailable(format!("incus is not available: {message}"))
                }
                other => other,
            })?;
        Ok(backend)
    }

    /// Connect using [`EngineConfig`].
    pub async fn connect_with_config(config: &EngineConfig) -> Result<Self> {
        let socket = config
            .incus_socket
            .clone()
            .unwrap_or_else(|| DEFAULT_INCUS_SOCKET.to_string());
        Self::connect(socket, config.timeout).await
    }

    pub(super) fn from_transport(transport: Arc<dyn Transport>, timeout: Duration, socket: PathBuf) -> Self {
        Self {
            client: IncusClient::new(transport, timeout),
            socket,
            exec_results: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Resolve the storage pool used for root disks and volumes.
    async fn pick_pool(&self) -> Result<String> {
        let metadata = self
            .client
            .request(&Method::GET, "/1.0/storage-pools?recursion=1", None)
            .await?;
        let pools = serde_json::from_value::<Vec<Value>>(metadata).unwrap_or_default();
        if pools.is_empty() {
            return Err(Error::EngineUnavailable(
                "incus has no storage pools".to_string(),
            ));
        }
        let preferred = pools
            .iter()
            .find(|pool| pool.get("name").and_then(Value::as_str) == Some("default"))
            .or_else(|| pools.first());
        let name = preferred
            .and_then(|pool| pool.get("name"))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::EngineUnavailable("incus storage pool has no name".to_string()))?;
        Ok(name.to_string())
    }

    /// Resolve the default network (the nic network used by the default profile).
    async fn default_network(&self) -> Option<String> {
        let metadata = self
            .client
            .request(&Method::GET, "/1.0/profiles/default", None)
            .await
            .ok()?;
        let devices = metadata.get("devices")?.as_object()?;
        for device in devices.values() {
            if device.get("type").and_then(Value::as_str) == Some("nic") {
                if let Some(network) = device.get("network").and_then(Value::as_str) {
                    return Some(network.to_string());
                }
            }
        }
        None
    }

    /// Ensure a custom storage volume exists for a named volume mount.
    async fn ensure_volume(&self, pool: &str, volume: &str) -> Result<()> {
        let path = format!("/1.0/storage-pools/{pool}/volumes/custom/{volume}");
        let (status, _) = self.client.raw(&Method::GET, &path, None).await?;
        if status == 200 {
            return Ok(());
        }
        let body = json!({ "name": volume, "type": "custom" });
        self.client
            .request(
                &Method::POST,
                &format!("/1.0/storage-pools/{pool}/volumes"),
                Some(body),
            )
            .await?;
        Ok(())
    }

    /// Fetch an instance entry by name.
    async fn instance(&self, id: &str) -> Result<InstanceEntry> {
        let metadata = self
            .client
            .request(&Method::GET, &format!("/1.0/instances/{id}?recursion=1"), None)
            .await
            .map_err(|error| match error {
                Error::NotFound(_) => Error::NotFound(format!("container `{id}`")),
                other => other,
            })?;
        serde_json::from_value(metadata)
            .map_err(|error| Error::EngineUnavailable(format!("bad instance payload: {error}")))
    }

    /// Fetch instance state.
    async fn instance_state(&self, id: &str) -> Result<InstanceState> {
        let metadata = self
            .client
            .request(&Method::GET, &format!("/1.0/instances/{id}/state"), None)
            .await
            .map_err(|error| match error {
                Error::NotFound(_) => Error::NotFound(format!("container `{id}`")),
                other => other,
            })?;
        serde_json::from_value(metadata)
            .map_err(|error| Error::EngineUnavailable(format!("bad state payload: {error}")))
    }

    /// Set the full device map of an instance (PATCH with null is rejected by
    /// Incus, so updates go through a read-modify-write on `devices`).
    async fn set_devices(&self, id: &str, devices: BTreeMap<String, Value>) -> Result<()> {
        self.client
            .request(
                &Method::PUT,
                &format!("/1.0/instances/{id}"),
                Some(json!({ "devices": devices })),
            )
            .await?;
        Ok(())
    }

    /// Read the exit-code file written by the init wrapper. Only reliable on
    /// stopped instances (the files API returns empty bodies while running).
    async fn read_exit_code(&self, id: &str) -> Option<i64> {
        let path = format!("/1.0/instances/{id}/files?path={EXIT_CODE_FILE}");
        let (status, bytes) = self.client.raw(&Method::GET, &path, None).await.ok()?;
        if status != 200 {
            return None;
        }
        String::from_utf8_lossy(&bytes).trim().parse().ok()
    }
}

#[async_trait]
impl Backend for IncusBackend {
    async fn info(&self) -> Result<EngineInfo> {
        let metadata = self.client.request(&Method::GET, "/1.0", None).await?;
        let environment = metadata.get("environment").cloned().unwrap_or(Value::Null);
        let get = |key: &str| {
            environment
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };

        let instances = self
            .client
            .request(&Method::GET, "/1.0/instances?recursion=1", None)
            .await?;
        let entries: Vec<InstanceEntry> = serde_json::from_value(instances).unwrap_or_default();
        let running = entries
            .iter()
            .filter(|entry| entry.status_code == Some(103))
            .count() as i64;

        let images = self
            .client
            .request(&Method::GET, "/1.0/images?recursion=1", None)
            .await?;
        let images: Vec<ImageEntry> = serde_json::from_value(images).unwrap_or_default();

        Ok(EngineInfo {
            version: get("server_version"),
            api_version: get("api_version"),
            min_api_version: None,
            os: get("os_name"),
            arch: get("kernel_architecture"),
            rootless: false,
            security_options: vec!["incus".to_string()],
            containers: entries.len() as i64,
            containers_running: running,
            images: images.len() as i64,
        })
    }

    async fn list_containers(&self, filter: &ContainerFilter) -> Result<Vec<ContainerSummary>> {
        let metadata = self
            .client
            .request(&Method::GET, "/1.0/instances?recursion=1", None)
            .await?;
        let entries: Vec<InstanceEntry> = serde_json::from_value(metadata).unwrap_or_default();
        let mut containers: Vec<ContainerSummary> = entries
            .iter()
            .map(instance_to_summary)
            .filter(|summary| filter.all || summary.state.is_running())
            .filter(|summary| {
                filter
                    .name
                    .as_ref()
                    .is_none_or(|name| summary.names.iter().any(|n| n.contains(name.as_str())))
            })
            .filter(|summary| {
                filter
                    .status
                    .is_none_or(|status| summary.state == status)
            })
            .filter(|summary| {
                filter.label.as_ref().is_none_or(|label| {
                    if let Some((key, value)) = label.split_once('=') {
                        summary.labels.get(key).map(String::as_str) == Some(value)
                    } else {
                        summary.labels.contains_key(label)
                    }
                })
            })
            .filter(|summary| match &filter.network {
                None => true,
                Some(network) => entries
                    .iter()
                    .find(|entry| entry.name == summary.id)
                    .map(|entry| {
                        devices_to_networks(entry.effective_devices(), entry.state.as_ref())
                            .iter()
                            .any(|attached| &attached.name == network)
                    })
                    .unwrap_or(false),
            })
            .collect();
        containers.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(containers)
    }

    async fn inspect_container(&self, id: &str) -> Result<ContainerDetail> {
        let entry = self.instance(id).await?;
        let state = self.instance_state(id).await.ok();
        let mut detail = instance_to_detail(&entry, state.as_ref());
        // Recover the recorded exit code from the wrapper file when stopped.
        if !detail.running {
            if let Some(code) = self.read_exit_code(id).await {
                detail.exit_code = Some(code);
            }
        }
        Ok(detail)
    }

    async fn create_container(&self, spec: &ContainerSpec) -> Result<String> {
        spec.validate()?;
        let resolved = spec.resolved()?;

        let pool = self.pick_pool().await?;
        for mount in &resolved.mounts {
            if mount.kind == MountKind::Volume {
                let volume = mount
                    .source
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| {
                        Error::InvalidSpec(format!(
                            "volume mount `{}` has an invalid volume name",
                            mount.target
                        ))
                    })?;
                self.ensure_volume(&pool, &volume).await?;
            }
        }

        let network = match &resolved.network {
            Some(network) => Some(network.clone()),
            None => self.default_network().await,
        };
        let devices = spec_to_devices(&resolved, &pool, network.as_deref())?;
        let config = spec_to_config(&resolved)?;

        let image = resolved.image.trim();
        let fingerprint = image.len() == 64 && image.chars().all(|c| c.is_ascii_hexdigit());
        let source = if fingerprint {
            json!({ "type": "image", "fingerprint": image })
        } else {
            json!({ "type": "image", "alias": image })
        };

        let body = json!({
            "name": resolved.name,
            "type": "container",
            "source": source,
            "config": config,
            "devices": devices,
            "profiles": [],
            "start": false,
        });

        let result = self
            .client
            .request(&Method::POST, "/1.0/instances", Some(body))
            .await;
        if let Err(error) = result {
            let message = error.to_string();
            if message.contains("mage") || message.contains("alias") || message.contains("fingerprint")
            {
                return Err(Error::Image(format!(
                    "image `{image}` not found in Incus; import it with `incus image import` \
                     (or use qe-agent-node's importer): {error}"
                )));
            }
            return Err(error);
        }

        // The operation may have been cleaned up before wait saw its result;
        // verify the instance now exists.
        if self.instance(&resolved.name).await.is_err() {
            return Err(Error::Other(format!(
                "incus create for `{}` did not produce an instance",
                resolved.name
            )));
        }
        Ok(resolved.name.clone())
    }

    async fn start_container(&self, id: &str) -> Result<()> {
        match self
            .client
            .request(
                &Method::PUT,
                &format!("/1.0/instances/{id}/state"),
                Some(json!({ "action": "start" })),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(Error::Conflict(message)) if message.contains("already running") => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn stop_container(&self, id: &str, timeout: Option<Duration>) -> Result<()> {
        let mut body = json!({ "action": "stop", "force": false });
        if let Some(timeout) = timeout {
            body["timeout"] = json!(timeout.as_secs().max(1));
        }
        self.client
            .request(&Method::PUT, &format!("/1.0/instances/{id}/state"), Some(body))
            .await?;
        Ok(())
    }

    async fn restart_container(&self, id: &str, timeout: Option<Duration>) -> Result<()> {
        let mut body = json!({ "action": "restart", "force": false });
        if let Some(timeout) = timeout {
            body["timeout"] = json!(timeout.as_secs().max(1));
        }
        self.client
            .request(&Method::PUT, &format!("/1.0/instances/{id}/state"), Some(body))
            .await?;
        Ok(())
    }

    async fn remove_container(&self, id: &str, force: bool) -> Result<()> {
        let state = self.instance_state(id).await?;
        let live = matches!(state.status_code.unwrap_or(102), 103..=110);
        if live {
            if !force {
                return Err(Error::Conflict(format!(
                    "container `{id}` is running (use force to remove)"
                )));
            }
            // Incus refuses to delete running instances regardless of query
            // parameters; stop first.
            self.client
                .request(
                    &Method::PUT,
                    &format!("/1.0/instances/{id}/state"),
                    Some(json!({ "action": "stop", "force": true })),
                )
                .await?;
        }
        self.client
            .request(&Method::DELETE, &format!("/1.0/instances/{id}"), None)
            .await?;
        Ok(())
    }

    fn logs(&self, id: &str, options: &LogOptions) -> Result<EventStream<LogLine>> {
        if !options.stdout && !options.stderr {
            return Ok(Box::pin(futures_util::stream::empty()));
        }
        Ok(logs_stream(self.client.clone(), id, options))
    }

    async fn wait_container(&self, id: &str) -> Result<ExitStatus> {
        loop {
            let state = match self.instance_state(id).await {
                Ok(state) => state,
                Err(error) if is_transient(&error) => {
                    // Transient daemon errors during process exit (observed:
                    // 500 "Invalid PID -1" / "connection refused"): retry.
                    tracing::debug!("incus state fetch retry: {error}");
                    sleep(POLL_INTERVAL).await;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let code = state.status_code.unwrap_or(102);
            if !matches!(code, 103..=110) {
                return Ok(ExitStatus {
                    code: self.read_exit_code(id).await.unwrap_or(0),
                    error: None,
                    oom_killed: false,
                });
            }
            sleep(POLL_INTERVAL).await;
        }
    }

    async fn exec(&self, id: &str, spec: &ExecSpec) -> Result<ExecSession> {
        if spec.command.is_empty() {
            return Err(Error::InvalidSpec("exec command must not be empty".into()));
        }
        let mut command = spec.command.clone();
        if let Some(workdir) = &spec.workdir {
            let quoted = shell_quote(workdir)?;
            let inner = spec
                .command
                .iter()
                .map(|arg| shell_quote(arg))
                .collect::<Result<Vec<_>>>()?
                .join(" ");
            command = vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                format!("cd {quoted} && exec {inner}"),
            ];
        }

        let mut body = json!({
            "command": command,
            "wait-for-websocket": false,
            "interactive": false,
            "record-output": true,
        });
        if !spec.env.is_empty() {
            body["environment"] = json!(spec.env);
        }
        if let Some(user) = &spec.user {
            let uid: u32 = user.parse().map_err(|_| {
                Error::InvalidSpec(format!(
                    "Incus exec requires a numeric user id, got `{user}`"
                ))
            })?;
            body["user"] = json!(uid);
        }

        let operation = self
            .client
            .request_async(
                &Method::POST,
                &format!("/1.0/instances/{id}/exec"),
                Some(body),
            )
            .await?;
        let op_id = operation.id.clone();
        if op_id.is_empty() {
            return Err(Error::EngineUnavailable(
                "incus exec did not return an operation".to_string(),
            ));
        }

        // Recorded output URLs appear in the operation metadata; fall back to
        // the documented `exec-output/exec_<op-uuid>.{stdout,stderr}` layout.
        let output = operation.metadata.as_ref().and_then(|meta| meta.get("output"));
        let stdout_url = output
            .and_then(|o| o.get("1"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!("/1.0/instances/{id}/logs/exec-output/exec_{op_id}.stdout")
            });
        let stderr_url = output
            .and_then(|o| o.get("2"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!("/1.0/instances/{id}/logs/exec-output/exec_{op_id}.stderr")
            });

        let results = Arc::clone(&self.exec_results);
        Ok(ExecSession {
            id: op_id.clone(),
            output: exec_stream(self.client.clone(), &op_id, &stdout_url, &stderr_url, results),
        })
    }

    async fn inspect_exec(&self, exec_id: &str) -> Result<Option<i64>> {
        if let Some(code) = self
            .exec_results
            .lock()
            .expect("exec results poisoned")
            .get(exec_id)
            .copied()
        {
            return Ok(Some(code));
        }
        let (status, bytes) = self
            .client
            .raw(&Method::GET, &format!("/1.0/operations/{exec_id}"), None)
            .await?;
        if status == 404 {
            return Ok(None);
        }
        let envelope = parse_envelope(status, &bytes)?;
        if envelope.kind == "error" {
            return Ok(None);
        }
        let op: Operation = serde_json::from_value(
            envelope.metadata.unwrap_or(Value::Null),
        )?;
        if op.status == "Success" {
            if let Some(code) = op
                .metadata
                .as_ref()
                .and_then(|meta| meta.get("return"))
                .and_then(Value::as_i64)
            {
                self.exec_results
                    .lock()
                    .expect("exec results poisoned")
                    .insert(exec_id.to_string(), code);
                return Ok(Some(code));
            }
        }
        Ok(None)
    }

    fn stats(&self, id: &str) -> Result<EventStream<StatsSample>> {
        Ok(stats_stream(self.client.clone(), id))
    }

    fn pull_image(&self, image: &str, _platform: Option<&str>) -> Result<EventStream<PullProgress>> {
        let message = format!(
            "Incus cannot pull from a registry; import `{image}` with `incus image import` \
             (or run qe-agent-node's IncusImporter)"
        );
        Ok(Box::pin(futures_util::stream::once(async move {
            Err(Error::Image(message))
        })))
    }

    async fn inspect_image(&self, image: &str) -> Result<Option<ImageInfo>> {
        // Try alias first, then fingerprint.
        let alias_path = format!("/1.0/images/aliases/{image}");
        if let Ok(metadata) = self.client.request(&Method::GET, &alias_path, None).await {
            if let Some(fingerprint) = metadata.get("target").and_then(Value::as_str) {
                let path = format!("/1.0/images/{fingerprint}");
                if let Ok(image_meta) = self.client.request(&Method::GET, &path, None).await {
                    let entry: ImageEntry = serde_json::from_value(image_meta)
                        .map_err(|error| {
                            Error::EngineUnavailable(format!("bad image payload: {error}"))
                        })?;
                    return Ok(Some(image_to_info(&entry)));
                }
            }
        }
        let path = format!("/1.0/images/{image}");
        match self.client.request(&Method::GET, &path, None).await {
            Ok(image_meta) => {
                let entry: ImageEntry = serde_json::from_value(image_meta)
                    .map_err(|error| {
                        Error::EngineUnavailable(format!("bad image payload: {error}"))
                    })?;
                Ok(Some(image_to_info(&entry)))
            }
            Err(Error::NotFound(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn list_images(&self) -> Result<Vec<ImageInfo>> {
        let metadata = self
            .client
            .request(&Method::GET, "/1.0/images?recursion=1", None)
            .await?;
        let entries: Vec<ImageEntry> = serde_json::from_value(metadata).unwrap_or_default();
        let mut images: Vec<ImageInfo> = entries.iter().map(image_to_info).collect();
        images.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(images)
    }

    async fn list_networks(&self) -> Result<Vec<NetworkInfo>> {
        let metadata = self
            .client
            .request(&Method::GET, "/1.0/networks?recursion=1", None)
            .await?;
        let entries: Vec<NetworkEntry> = serde_json::from_value(metadata).unwrap_or_default();
        let mut networks: Vec<NetworkInfo> = entries
            .iter()
            .filter(|network| network.managed)
            .map(network_to_info)
            .collect();
        networks.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(networks)
    }

    async fn inspect_network(&self, name: &str) -> Result<NetworkInfo> {
        let metadata = self
            .client
            .request(&Method::GET, &format!("/1.0/networks/{name}"), None)
            .await
            .map_err(|error| match error {
                Error::NotFound(_) => Error::NotFound(format!("network `{name}`")),
                other => other,
            })?;
        let entry: NetworkEntry = serde_json::from_value(metadata)
            .map_err(|error| Error::EngineUnavailable(format!("bad network payload: {error}")))?;
        Ok(network_to_info(&entry))
    }

    async fn create_network(&self, spec: &NetworkSpec) -> Result<String> {
        spec.validate()?;
        // Incus names the Linux bridge after the network, so the name doubles
        // as an interface name and is capped at 15 characters.
        if spec.name.len() > 15 {
            return Err(Error::InvalidSpec(format!(
                "network name `{}` is too long: Incus bridge interface names are limited to \
                 15 characters",
                spec.name
            )));
        }
        let mut config = BTreeMap::new();
        if let Some(subnet) = spec.subnet {
            let gateway = spec.gateway.unwrap_or_else(|| {
                subnet
                    .hosts()
                    .next()
                    .unwrap_or(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 1)))
            });
            config.insert(
                "ipv4.address".to_string(),
                format!("{gateway}/{}", subnet.prefix_len()),
            );
        }
        config.insert("ipv6.address".to_string(), "none".to_string());
        config.insert(
            "ipv4.nat".to_string(),
            if spec.internal {
                "false".to_string()
            } else {
                "true".to_string()
            },
        );
        for (key, value) in labels_config(&spec.labels) {
            config.insert(key, value);
        }
        if spec.bridge_name.is_some() {
            tracing::warn!("NetworkSpec.bridge_name has no Incus equivalent; the bridge is named after the network");
        }

        let body = json!({
            "name": spec.name,
            "type": "bridge",
            "description": "managed by qe-container",
            "config": config,
        });
        self.client
            .request(&Method::POST, "/1.0/networks", Some(body))
            .await?;
        Ok(spec.name.clone())
    }

    async fn remove_network(&self, name: &str) -> Result<()> {
        self.client
            .request(&Method::DELETE, &format!("/1.0/networks/{name}"), None)
            .await
            .map_err(|error| match error {
                Error::NotFound(_) => Error::NotFound(format!("network `{name}`")),
                other => other,
            })?;
        Ok(())
    }

    async fn connect_network(
        &self,
        network: &str,
        container: &str,
        ip: Option<IpAddr>,
    ) -> Result<()> {
        let entry = self.instance(container).await?;
        let mut devices = entry.devices.clone();
        let key = format!("qe-n-{network}");
        if devices.contains_key(&key) {
            return Err(Error::Conflict(format!(
                "container `{container}` is already connected to `{network}`"
            )));
        }
        let mut nic = json!({ "type": "nic", "network": network });
        if let Some(ip) = ip {
            nic["ipv4.address"] = json!(ip.to_string());
        }
        devices.insert(key, nic);
        self.set_devices(container, devices).await.map_err(|error| {
            if let Error::Conflict(message) = &error {
                if message.contains("Failed add validation") {
                    return Error::Conflict(format!(
                        "cannot attach `{container}` to `{network}` while running; stop it first ({message})"
                    ));
                }
            }
            error
        })
    }

    async fn disconnect_network(&self, network: &str, container: &str, force: bool) -> Result<()> {
        let entry = self.instance(container).await?;
        let mut devices = entry.devices.clone();
        let before = devices.len();
        devices.retain(|_, device| {
            !(device.get("type").and_then(Value::as_str) == Some("nic")
                && device.get("network").and_then(Value::as_str) == Some(network))
        });
        if devices.len() == before {
            if force {
                return Ok(());
            }
            return Err(Error::NotFound(format!(
                "container `{container}` is not connected to `{network}`"
            )));
        }
        self.set_devices(container, devices).await
    }
}
