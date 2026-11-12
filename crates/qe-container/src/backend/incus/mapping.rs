use std::collections::BTreeMap;
use std::net::{IpAddr, TcpListener};

use serde_json::{Value, json};

use super::wire::{ImageEntry, InstanceEntry, InstanceState, NetworkEntry};
use super::{EXIT_CODE_FILE, INIT_STDERR_FILE, INIT_STDOUT_FILE};
use crate::error::{Error, Result};
use crate::model::{
    ContainerDetail, ContainerNetwork, ContainerSpec, ContainerStateKind, ContainerSummary,
    HealthState, ImageInfo, MountKind, MountPoint, NetworkContainer, NetworkInfo,
    Protocol, PublishedPort,
};
use crate::stream::StatsSample;

// ---------------------------------------------------------------------------
// Mapping helpers
// ---------------------------------------------------------------------------

/// Quote an argument for the `sh -c` init wrapper (single-quote style).
pub(super) fn shell_quote(arg: &str) -> Result<String> {
    if arg.contains('"') || arg.contains('\\') {
        return Err(Error::InvalidSpec(format!(
            "argument {arg:?} contains characters (`\"` or `\\`) that cannot be encoded \
             for Incus raw.lxc init commands"
        )));
    }
    Ok(format!("'{}'", arg.replace('\'', r"'\''")))
}

/// Build the `lxc.init.cmd` value that runs `argv` with its output redirected
/// to per-stream files and its exit code recorded in [`EXIT_CODE_FILE`].
///
/// The files API is the only reliable capture channel: console reads drain
/// content while the instance runs and replay is inconsistent afterwards.
/// The files are readable incrementally while running and idempotently after
/// stop.
pub(super) fn init_cmd(argv: &[String]) -> Result<String> {
    let quoted = argv
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    Ok(format!(
        "/bin/sh -c \"{quoted} > {INIT_STDOUT_FILE} 2> {INIT_STDERR_FILE}; echo $? > {EXIT_CODE_FILE}\""
    ))
}

/// Map instance `user.*` config keys back to labels.
pub(super) fn config_labels(config: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    config
        .iter()
        .filter_map(|(key, value)| {
            key.strip_prefix("user.")
                .map(|label| (label.to_string(), value.clone()))
        })
        .collect()
}

/// Map labels onto `user.*` config keys.
pub(super) fn labels_config(labels: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    labels
        .iter()
        .map(|(key, value)| (format!("user.{key}"), value.clone()))
        .collect()
}

/// Incus status code to the normalized state enum.
///
/// Codes observed on Incus 7.5: `102` Stopped (also for never-started
/// instances), `103` Running, `110` Frozen. Never-started instances carry the
/// zero `last_used_at` timestamp and map to [`ContainerStateKind::Created`].
pub(super) fn state_code_to_kind(code: i64, last_used_at: Option<&str>) -> ContainerStateKind {
    match code {
        103 => ContainerStateKind::Running,
        110 => ContainerStateKind::Paused,
        102 => {
            let never_started = last_used_at
                .map(|value| value.is_empty() || value.starts_with("1970-01-01"))
                .unwrap_or(true);
            if never_started {
                ContainerStateKind::Created
            } else {
                ContainerStateKind::Exited
            }
        }
        0..=101 => ContainerStateKind::Created,
        104..=109 => ContainerStateKind::Stopping,
        _ => ContainerStateKind::Unknown,
    }
}

/// Parse a proxy device `listen`/`connect` value of the form `tcp:host:port`.
pub(super) fn parse_proxy_endpoint(value: &str) -> Option<(Protocol, u16)> {
    let (_, rest) = value.split_once(':')?;
    let (_, port) = rest.rsplit_once(':')?;
    let protocol = if value.starts_with("udp") {
        Protocol::Udp
    } else {
        Protocol::Tcp
    };
    Some((protocol, port.parse().ok()?))
}

/// Build proxy devices for published ports.
pub(super) fn ports_to_devices(ports: &[crate::model::PortBinding]) -> Result<BTreeMap<String, Value>> {
    let mut devices = BTreeMap::new();
    for (index, port) in ports.iter().enumerate() {
        let host_port = match port.host {
            Some(host) => host,
            None => allocate_ephemeral_port(port.host_ip)?,
        };
        let proto = port.protocol.as_docker();
        devices.insert(
            format!("qe-p{}", index + 1),
            json!({
                "type": "proxy",
                "listen": format!("{proto}:{}:{host_port}", port.host_ip),
                "connect": format!("{proto}:127.0.0.1:{}", port.container),
                "bind": "host",
            }),
        );
    }
    Ok(devices)
}

/// Reserve an ephemeral host port for a publish binding.
pub(super) fn allocate_ephemeral_port(host_ip: IpAddr) -> Result<u16> {
    let listener = TcpListener::bind((host_ip, 0))
        .map_err(|error| Error::InvalidSpec(format!("cannot allocate ephemeral port: {error}")))?;
    let port = listener
        .local_addr()
        .map_err(|error| Error::InvalidSpec(error.to_string()))?
        .port();
    Ok(port)
}

/// Build the raw.lxc lines for init command, cwd, uid, stop signal, tmpfs and
/// capabilities.
pub(super) fn spec_to_raw_lxc(spec: &ContainerSpec) -> Result<Vec<String>> {
    let mut lines = Vec::new();

    let mut argv: Vec<String> = spec.entrypoint.clone().unwrap_or_default();
    argv.extend(spec.command.iter().cloned());
    if !argv.is_empty() {
        lines.push(format!("lxc.init.cmd = {}", init_cmd(&argv)?));
    }
    if let Some(workdir) = &spec.workdir {
        if !workdir.starts_with('/') {
            return Err(Error::InvalidSpec(format!(
                "workdir `{workdir}` must be an absolute container path"
            )));
        }
        lines.push(format!("lxc.init.cwd = {workdir}"));
    }
    if let Some(user) = &spec.user {
        let uid: u32 = user.parse().map_err(|_| {
            Error::InvalidSpec(format!(
                "Incus requires a numeric user id, got `{user}`"
            ))
        })?;
        lines.push(format!("lxc.init.uid = {uid}"));
    }
    if let Some(signal) = &spec.stop_signal {
        lines.push(format!("lxc.signal.stop = {signal}"));
    }
    if let Some(hostname) = &spec.hostname {
        if hostname != &spec.name {
            lines.push(format!("lxc.uts.name = {hostname}"));
        }
    }
    if spec.security.no_new_privileges {
        tracing::warn!("security.no_new_privileges has no Incus equivalent; ignoring");
    }
    for (path, options) in &spec.security.tmpfs {
        let rel = path.trim_start_matches('/');
        let mut opts = String::from("create=dir");
        if !options.trim().is_empty() {
            opts = format!("{},{}", options.trim(), opts);
        }
        lines.push(format!("lxc.mount.entry = tmpfs {rel} tmpfs {opts} 0 0"));
    }
    if !spec.security.cap_drop.is_empty() {
        lines.push(format!(
            "lxc.cap.drop = {}",
            spec.security.cap_drop.join(",")
        ));
    }
    if !spec.security.cap_add.is_empty() {
        lines.push(format!(
            "lxc.cap.keep = {}",
            spec.security.cap_add.join(",")
        ));
    }
    Ok(lines)
}

/// Build the first-class Incus config keys for labels, env and resource limits.
pub(super) fn spec_to_config(spec: &ContainerSpec) -> Result<BTreeMap<String, String>> {
    let mut config = BTreeMap::new();

    config.insert("user.qe.image-ref".to_string(), spec.image.clone());
    for (key, value) in labels_config(&spec.labels) {
        config.insert(key, value);
    }
    for (key, value) in &spec.env {
        config.insert(format!("environment.{key}"), value.clone());
    }
    if let Some(memory) = spec.resources.memory {
        config.insert("limits.memory".to_string(), memory.0.to_string());
    }
    if let Some(cpus) = spec.resources.cpus {
        if cpus.fract() == 0.0 && cpus <= 64.0 {
            config.insert("limits.cpu".to_string(), cpus.to_string());
        } else {
            let percent = (cpus * 100.0).round().clamp(1.0, 10_000.0);
            config.insert("limits.cpu.allowance".to_string(), format!("{percent}%"));
        }
    }
    if let Some(pids) = spec.resources.pids {
        config.insert("limits.processes".to_string(), pids.to_string());
    }
    for ulimit in &spec.security.ulimits {
        config.insert(
            format!("limits.kernel.{}", ulimit.name),
            format!("{}:{}", ulimit.soft, ulimit.hard),
        );
    }
    if matches!(
        spec.restart,
        crate::model::RestartPolicy::Always | crate::model::RestartPolicy::UnlessStopped
    ) {
        config.insert("boot.autostart".to_string(), "true".to_string());
    }

    let raw_lines = spec_to_raw_lxc(spec)?;
    if !raw_lines.is_empty() {
        config.insert("raw.lxc".to_string(), raw_lines.join("\n"));
    }
    Ok(config)
}

/// Build the device map for a container spec.
pub(super) fn spec_to_devices(
    spec: &ContainerSpec,
    pool: &str,
    network: Option<&str>,
) -> Result<BTreeMap<String, Value>> {
    let mut devices = BTreeMap::new();

    let mut root = json!({ "type": "disk", "pool": pool, "path": "/" });
    if spec.security.read_only {
        root["readonly"] = json!("true");
    }
    devices.insert("root".to_string(), root);

    for (key, value) in ports_to_devices(&spec.ports)? {
        devices.insert(key, value);
    }

    for (index, mount) in spec.mounts.iter().enumerate() {
        let key = format!("qe-m{}", index + 1);
        let device = match mount.kind {
            MountKind::Bind => {
                let mut device = json!({
                    "type": "disk",
                    "source": mount.source.display().to_string(),
                    "path": mount.target,
                });
                if mount.read_only {
                    device["readonly"] = json!("true");
                }
                device
            }
            MountKind::Volume => {
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
                let mut device = json!({
                    "type": "disk",
                    "pool": pool,
                    "source": volume,
                    "path": mount.target,
                });
                if mount.read_only {
                    device["readonly"] = json!("true");
                }
                device
            }
        };
        devices.insert(key, device);
    }

    if let Some(network) = network {
        let mut nic = json!({ "type": "nic", "network": network });
        if let Some(ip) = spec.static_ip {
            nic["ipv4.address"] = json!(ip.to_string());
        }
        devices.insert("eth0".to_string(), nic);
    }
    Ok(devices)
}

/// Parse the gateway and subnet out of an Incus network config.
pub(super) fn network_subnet_gateway(
    config: &BTreeMap<String, String>,
) -> (Option<ipnet::IpNet>, Option<IpAddr>) {
    let Some(raw) = config.get("ipv4.address") else {
        return (None, None);
    };
    let raw = raw.trim();
    if raw.is_empty() || raw == "none" {
        return (None, None);
    }
    match raw.parse::<ipnet::IpNet>() {
        Ok(net) => {
            let subnet = ipnet::IpNet::new(net.network(), net.prefix_len()).ok();
            (subnet, Some(net.addr()))
        }
        Err(_) => (None, raw.parse::<IpAddr>().ok()),
    }
}

pub(super) fn network_to_info(entry: &NetworkEntry) -> NetworkInfo {
    let (subnet, gateway) = network_subnet_gateway(&entry.config);
    let containers = entry
        .used_by
        .iter()
        .filter_map(|uri| {
            let path = uri.split('?').next().unwrap_or(uri);
            let name = path.strip_prefix("/1.0/instances/")?;
            Some(NetworkContainer {
                id: name.to_string(),
                name: name.to_string(),
                ipv4: None,
            })
        })
        .collect();
    NetworkInfo {
        id: entry.name.clone(),
        name: entry.name.clone(),
        driver: if entry.kind.is_empty() {
            "bridge".to_string()
        } else {
            entry.kind.clone()
        },
        subnet,
        gateway,
        bridge_name: None,
        labels: config_labels(&entry.config),
        options: BTreeMap::new(),
        containers,
    }
}

pub(super) fn image_to_info(entry: &ImageEntry) -> ImageInfo {
    let mut tags: Vec<String> = entry.aliases.iter().map(|a| a.name.clone()).collect();
    tags.sort();
    ImageInfo {
        id: entry.fingerprint.clone(),
        tags,
        size: entry.size.unwrap_or(0),
        created: entry
            .created_at
            .clone()
            .or_else(|| entry.uploaded_at.clone()),
    }
}

/// Parse proxy devices into published ports.
pub(super) fn devices_to_ports(devices: &BTreeMap<String, Value>) -> Vec<PublishedPort> {
    let mut ports = Vec::new();
    for device in devices.values() {
        if device.get("type").and_then(Value::as_str) != Some("proxy") {
            continue;
        }
        let listen = device.get("listen").and_then(Value::as_str).unwrap_or("");
        let connect = device.get("connect").and_then(Value::as_str).unwrap_or("");
        let Some((protocol, host_port)) = parse_proxy_endpoint(listen) else {
            continue;
        };
        let container_port = parse_proxy_endpoint(connect)
            .map(|(_, port)| port)
            .unwrap_or(0);
        let host_ip = listen
            .split(':')
            .nth(1)
            .and_then(|ip| ip.parse::<IpAddr>().ok());
        ports.push(PublishedPort {
            container: container_port,
            host: Some(host_port),
            host_ip,
            protocol,
        });
    }
    ports.sort_by_key(|port| (port.container, port.host));
    ports
}

/// Parse disk devices into mount points (skipping the root device).
pub(super) fn devices_to_mounts(devices: &BTreeMap<String, Value>) -> Vec<MountPoint> {
    let mut mounts = Vec::new();
    for (key, device) in devices {
        if key == "root" || device.get("type").and_then(Value::as_str) != Some("disk") {
            continue;
        }
        let target = device
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if target.is_empty() {
            continue;
        }
        let source = device
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                device
                    .get("pool")
                    .and_then(Value::as_str)
                    .zip(device.get("source").and_then(Value::as_str))
                    .map(|(pool, source)| format!("{pool}/{source}"))
            })
            .unwrap_or_default();
        let read_only = device
            .get("readonly")
            .and_then(Value::as_str)
            .map(|value| value == "true")
            .unwrap_or(false);
        let kind = if device.get("pool").is_some() && device.get("source").is_some() {
            "volume"
        } else {
            "bind"
        };
        mounts.push(MountPoint {
            source,
            target,
            read_only,
            kind: kind.to_string(),
        });
    }
    mounts.sort_by(|a, b| a.target.cmp(&b.target));
    mounts
}

/// Parse nic devices into network attachments.
pub(super) fn devices_to_networks(
    devices: &BTreeMap<String, Value>,
    state: Option<&InstanceState>,
) -> Vec<ContainerNetwork> {
    let mut networks = Vec::new();
    for (key, device) in devices {
        if device.get("type").and_then(Value::as_str) != Some("nic") {
            continue;
        }
        let name = device
            .get("network")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| key.clone());
        let iface = device
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| key.clone());
        let ip_address = state
            .and_then(|state| state.network.as_ref())
            .and_then(|networks| networks.get(&iface))
            .and_then(|iface| {
                iface
                    .addresses
                    .iter()
                    .find(|addr| addr.family == "inet" && addr.scope == "global")
                    .map(|addr| addr.address.clone())
            })
            .or_else(|| {
                device
                    .get("ipv4.address")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        let gateway = state
            .and_then(|state| state.network.as_ref())
            .and_then(|networks| networks.get(&iface))
            .and_then(|iface| {
                iface
                    .addresses
                    .iter()
                    .find(|addr| addr.family == "inet" && addr.scope == "link")
                    .map(|addr| addr.address.clone())
            });
        networks.push(ContainerNetwork {
            name,
            ip_address,
            gateway,
        });
    }
    networks
}

pub(super) fn instance_to_summary(entry: &InstanceEntry) -> ContainerSummary {
    let config = entry.effective_config();
    let devices = entry.effective_devices();
    let kind = state_code_to_kind(
        entry.status_code.unwrap_or(102),
        entry.last_used_at.as_deref(),
    );
    ContainerSummary {
        id: entry.name.clone(),
        names: vec![entry.name.clone()],
        image: image_ref_from_config(config),
        image_id: config.get("volatile.base_image").cloned(),
        command: None,
        state: kind,
        status: entry.status.clone(),
        labels: config_labels(config),
        ports: devices_to_ports(devices),
        created: unix_from_rfc3339(entry.created_at.as_deref()),
    }
}

pub(super) fn image_ref_from_config(config: &BTreeMap<String, String>) -> String {
    config
        .get("user.qe.image-ref")
        .cloned()
        .or_else(|| config.get("volatile.base_image").cloned())
        .unwrap_or_default()
}

pub(super) fn unix_from_rfc3339(value: Option<&str>) -> Option<i64> {
    let raw = value?;
    // Cheap parse for `YYYY-MM-DDTHH:MM:SS(.fff)?Z` timestamps without a
    // chrono dependency: fall back to None on anything unusual.
    let bytes = raw.as_bytes();
    if bytes.len() < 19 || bytes[0..4].iter().any(|b| !b.is_ascii_digit()) {
        return None;
    }
    let year: i64 = raw.get(0..4)?.parse().ok()?;
    let month: i64 = raw.get(5..7)?.parse().ok()?;
    let day: i64 = raw.get(8..10)?.parse().ok()?;
    let hour: i64 = raw.get(11..13)?.parse().ok()?;
    let minute: i64 = raw.get(14..16)?.parse().ok()?;
    let second: i64 = raw.get(17..19)?.parse().ok()?;
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since the Unix epoch (Howard Hinnant's civil-from-days, inverted).
pub(super) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub(super) fn instance_to_detail(entry: &InstanceEntry, state: Option<&InstanceState>) -> ContainerDetail {
    let config = entry.effective_config();
    let devices = entry.effective_devices();
    let code = state
        .and_then(|state| state.status_code)
        .or(entry.status_code)
        .unwrap_or(102);
    let kind = state_code_to_kind(code, entry.last_used_at.as_deref());
    let running = kind.is_running();
    let paused = kind == ContainerStateKind::Paused;
    // Incus does not expose init exit codes; a stopped container reports 0
    // unless `inspect_container` replaces it with the console marker value.
    let exit_code = if running { None } else { Some(0) };

    ContainerDetail {
        id: entry.name.clone(),
        name: entry.name.clone(),
        image: image_ref_from_config(config),
        image_id: config.get("volatile.base_image").cloned(),
        state: kind,
        running,
        paused,
        exit_code,
        oom_killed: false,
        health: Some(HealthState::None),
        labels: config_labels(config),
        ports: devices_to_ports(devices),
        mounts: devices_to_mounts(devices),
        networks: devices_to_networks(devices, state),
        created: entry.created_at.clone(),
        restart_count: 0,
    }
}

pub(super) fn state_to_stats(state: &InstanceState, prev: Option<(u64, u64)>) -> StatsSample {
    let cpu_ns = state.cpu.as_ref().and_then(|cpu| cpu.usage).unwrap_or(0);
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let cpu_percent = prev.and_then(|(prev_ns, prev_wall)| {
        let dn = cpu_ns.saturating_sub(prev_ns) as f64;
        let dw = now_ns.saturating_sub(prev_wall) as f64;
        if dw > 0.0 {
            Some((dn / dw) * 100.0)
        } else {
            None
        }
    });
    let (network_rx, network_tx) = state
        .network
        .as_ref()
        .map(|networks| {
            let mut rx = 0u64;
            let mut tx = 0u64;
            for iface in networks.values() {
                if let Some(counters) = &iface.counters {
                    rx += counters.bytes_received;
                    tx += counters.bytes_sent;
                }
            }
            (rx, tx)
        })
        .unwrap_or((0, 0));

    StatsSample {
        read_at: Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_default(),
        ),
        cpu_percent,
        memory_usage: state.memory.as_ref().and_then(|m| m.usage),
        memory_limit: state.memory.as_ref().and_then(|m| m.total),
        network_rx: Some(network_rx),
        network_tx: Some(network_tx),
        pids: state.processes,
    }
}

