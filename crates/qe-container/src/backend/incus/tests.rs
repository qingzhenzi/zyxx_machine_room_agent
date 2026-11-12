use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use hyper::Method;
use serde_json::{Value, json};

use super::backend::IncusBackend;
use super::client::IncusClient;
use super::mapping::*;
use super::streams::*;
use super::transport::Transport;
use super::INIT_STDOUT_FILE;
use crate::backend::Backend;
use crate::error::Result;
use crate::model::{
    ContainerSpec, ContainerStateKind, Mount, PortBinding, Protocol, Resources,
    RestartPolicy, Ulimit,
};
use crate::stream::OutputStream;

#[test]
fn shell_quote_escapes_single_quotes() {
    assert_eq!(shell_quote("abc").unwrap(), "'abc'");
    assert_eq!(shell_quote("a b").unwrap(), "'a b'");
    assert_eq!(shell_quote("it's").unwrap(), r"'it'\''s'");
    assert!(shell_quote(r#"say "hi""#).is_err());
}

#[test]
fn init_cmd_wraps_with_exit_file() {
    let argv = vec!["echo".to_string(), "hello world".to_string()];
    let cmd = init_cmd(&argv).unwrap();
    assert_eq!(
        cmd,
        "/bin/sh -c \"'echo' 'hello world' > /qe-init.out 2> /qe-init.err; echo $? > /qe-exit-code\""
    );
}

fn log_state_for_test(transport: Arc<FakeTransport>, tail: Option<usize>, follow: bool) -> LogState {
    LogState {
        client: IncusClient::new(transport, Duration::from_secs(5)),
        id: "demo".to_string(),
        follow,
        tail,
        files: vec![FileChannel {
            path: INIT_STDOUT_FILE.to_string(),
            stream: OutputStream::Stdout,
            offset: 0,
            partial: Vec::new(),
        }],
        console_partial: Vec::new(),
        pending: VecDeque::new(),
        primed: false,
        saw_stopped: false,
        finished: false,
    }
}

#[test]
fn log_accept_splits_and_buffers_partial_lines() {
    let mut pending = VecDeque::new();
    let mut partial = Vec::new();
    push_lines(&mut pending, &mut partial, b"one\ntwo\nthr", OutputStream::Stdout);
    let lines: Vec<String> = pending.drain(..).map(|l| l.text).collect();
    assert_eq!(lines, vec!["one\n".to_string(), "two\n".to_string()]);
    push_lines(&mut pending, &mut partial, b"ee\n", OutputStream::Stdout);
    let lines: Vec<String> = pending.drain(..).map(|l| l.text).collect();
    assert_eq!(lines, vec!["three\n".to_string()]);
}

#[test]
fn log_push_tail_emits_unterminated_trailer_once() {
    let mut pending = VecDeque::new();
    let mut partial = b"mount-hi".to_vec();
    push_tail(&mut pending, &mut partial, OutputStream::Stdout);
    let lines: Vec<String> = pending.drain(..).map(|l| l.text).collect();
    assert_eq!(lines, vec!["mount-hi\n".to_string()]);
    assert!(partial.is_empty());
    // A second flush is a no-op.
    push_tail(&mut pending, &mut partial, OutputStream::Stdout);
    assert!(pending.is_empty());
}

#[tokio::test]
async fn log_poll_flushes_unterminated_output_on_finish() {
    let transport = Arc::new(FakeTransport::default());
    // Output without a trailing newline (like `echo -n` / `printf`).
    transport.route(
        "GET /1.0/instances/demo/files?path=/qe-init.out",
        200,
        "mount-hi",
    );
    transport.route("GET /1.0/instances/demo/state", 200, &sync(json!({"status": "Stopped", "status_code": 102})));
    let mut state = log_state_for_test(transport.clone(), None, true);
    // Round 1: buffers the trailer, sees the instance stopped.
    state.poll_once().await.unwrap();
    assert!(state.saw_stopped, "stopped instance must be noticed");
    // Round 2 (final): flushes the trailer as a line.
    state.poll_once().await.unwrap();
    let lines: Vec<String> = state.pending.drain(..).map(|l| l.text).collect();
    assert_eq!(lines, vec!["mount-hi\n".to_string()]);
    assert!(!state.finished, "stream must yield the flushed line first");
    // Round 3: nothing left, the stream finishes.
    state.poll_once().await.unwrap();
    assert!(state.finished);
    assert!(state.pending.is_empty());
}

#[tokio::test]
async fn log_poll_reads_file_incrementally_and_applies_tail() {
    let transport = Arc::new(FakeTransport::default());
    transport.route(
        "GET /1.0/instances/demo/files?path=/qe-init.out",
        200,
        "a\nb\nc\nd\n",
    );
    let mut state = log_state_for_test(transport.clone(), Some(2), true);
    state.poll_once().await.unwrap();
    let lines: Vec<String> = state.pending.drain(..).map(|l| l.text).collect();
    assert_eq!(lines, vec!["c\n".to_string(), "d\n".to_string()]);

    // The next poll only sees newly grown bytes.
    transport.route(
        "GET /1.0/instances/demo/files?path=/qe-init.out",
        200,
        "a\nb\nc\nd\ne\n",
    );
    state.poll_once().await.unwrap();
    let lines: Vec<String> = state.pending.drain(..).map(|l| l.text).collect();
    assert_eq!(lines, vec!["e\n".to_string()]);
}

#[test]
fn labels_round_trip_through_config() {
    let mut labels = BTreeMap::new();
    labels.insert("qe.managed".to_string(), "true".to_string());
    labels.insert("team".to_string(), "core".to_string());
    let config = labels_config(&labels);
    assert_eq!(config.get("user.qe.managed").map(String::as_str), Some("true"));
    let back = config_labels(&config);
    assert_eq!(back, labels);
}

#[test]
fn state_codes_map_to_kinds() {
    assert_eq!(state_code_to_kind(103, None), ContainerStateKind::Running);
    assert_eq!(state_code_to_kind(110, None), ContainerStateKind::Paused);
    assert_eq!(state_code_to_kind(102, None), ContainerStateKind::Created);
    assert_eq!(
        state_code_to_kind(102, Some("1970-01-01T00:00:00Z")),
        ContainerStateKind::Created
    );
    assert_eq!(
        state_code_to_kind(102, Some("2026-11-03T07:23:50Z")),
        ContainerStateKind::Exited
    );
}

#[test]
fn proxy_endpoint_parsing() {
    assert_eq!(
        parse_proxy_endpoint("tcp:127.0.0.1:8080"),
        Some((Protocol::Tcp, 8080))
    );
    assert_eq!(
        parse_proxy_endpoint("udp:0.0.0.0:53"),
        Some((Protocol::Udp, 53))
    );
}

#[test]
fn rfc3339_to_unix() {
    assert_eq!(
        unix_from_rfc3339(Some("1970-01-01T00:00:00Z")),
        Some(0)
    );
    assert_eq!(
        unix_from_rfc3339(Some("2026-11-03T07:23:50.957794227Z")),
        Some(1793690630)
    );
}

#[test]
fn network_subnet_and_gateway_parse() {
    let mut config = BTreeMap::new();
    config.insert("ipv4.address".to_string(), "10.77.88.1/24".to_string());
    let (subnet, gateway) = network_subnet_gateway(&config);
    assert_eq!(gateway, Some(IpAddr::V4(Ipv4Addr::new(10, 77, 88, 1))));
    assert_eq!(
        subnet.map(|net| net.to_string()),
        Some("10.77.88.0/24".to_string())
    );
}

#[test]
fn spec_to_config_maps_limits_and_env() {
    let mut spec = ContainerSpec::new("demo", "alpine");
    spec = spec.with_env("FOO", "bar");
    spec.resources = Resources {
        memory: Some(crate::model::ByteSize::mib(128)),
        cpus: Some(2.0),
        pids: Some(100),
    };
    spec.security.ulimits.push(Ulimit {
        name: "nofile".to_string(),
        soft: 1024,
        hard: 2048,
    });
    spec.restart = RestartPolicy::Always;
    let config = spec_to_config(&spec).unwrap();
    assert_eq!(config.get("environment.FOO").map(String::as_str), Some("bar"));
    assert_eq!(config.get("limits.memory").map(String::as_str), Some("134217728"));
    assert_eq!(config.get("limits.cpu").map(String::as_str), Some("2"));
    assert_eq!(config.get("limits.processes").map(String::as_str), Some("100"));
    assert_eq!(
        config.get("limits.kernel.nofile").map(String::as_str),
        Some("1024:2048")
    );
    assert_eq!(config.get("boot.autostart").map(String::as_str), Some("true"));
    assert_eq!(
        config.get("user.qe.image-ref").map(String::as_str),
        Some("alpine")
    );
}

#[test]
fn spec_to_devices_maps_ports_mounts_and_nic() {
    let mut spec = ContainerSpec::new("demo", "alpine");
    spec = spec
        .with_port(PortBinding::published(18080, 80))
        .with_mount(Mount::bind("/srv/data", "/data").read_only())
        .with_network("qenet")
        .with_static_ip(IpAddr::V4(Ipv4Addr::new(10, 77, 88, 50)));
    spec.security.read_only = true;

    let devices = spec_to_devices(&spec, "default", Some("qenet")).unwrap();
    assert_eq!(devices["root"]["readonly"], json!("true"));
    assert_eq!(devices["qe-p1"]["type"], json!("proxy"));
    assert_eq!(devices["qe-p1"]["listen"], json!("tcp:127.0.0.1:18080"));
    assert_eq!(devices["qe-p1"]["connect"], json!("tcp:127.0.0.1:80"));
    assert_eq!(devices["qe-m1"]["source"], json!("/srv/data"));
    assert_eq!(devices["qe-m1"]["readonly"], json!("true"));
    assert_eq!(devices["eth0"]["network"], json!("qenet"));
    assert_eq!(devices["eth0"]["ipv4.address"], json!("10.77.88.50"));
}

#[test]
fn devices_round_trip_to_read_models() {
    let mut devices = BTreeMap::new();
    devices.insert(
        "root".to_string(),
        json!({"type": "disk", "pool": "default", "path": "/", "readonly": "true"}),
    );
    devices.insert(
        "qe-p1".to_string(),
        json!({"type": "proxy", "listen": "tcp:127.0.0.1:18080", "connect": "tcp:127.0.0.1:80", "bind": "host"}),
    );
    devices.insert(
        "qe-m1".to_string(),
        json!({"type": "disk", "source": "/srv", "path": "/data", "readonly": "true"}),
    );
    devices.insert(
        "eth0".to_string(),
        json!({"type": "nic", "network": "qenet"}),
    );

    let ports = devices_to_ports(&devices);
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0].container, 80);
    assert_eq!(ports[0].host, Some(18080));

    let mounts = devices_to_mounts(&devices);
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].target, "/data");
    assert!(mounts[0].read_only);

    let networks = devices_to_networks(&devices, None);
    assert_eq!(networks.len(), 1);
    assert_eq!(networks[0].name, "qenet");
}

/// Fake transport returning canned payloads keyed by `METHOD path`.
#[derive(Default)]
struct FakeTransport {
    routes: Mutex<HashMap<String, (u16, String)>>,
}

impl FakeTransport {
    fn route(&self, key: &str, status: u16, body: &str) {
        self.routes
            .lock()
            .unwrap()
            .insert(key.to_string(), (status, body.to_string()));
    }
}

#[async_trait]
impl Transport for FakeTransport {
    async fn call(
        &self,
        method: &Method,
        path: &str,
        _body: Option<Bytes>,
    ) -> Result<(u16, Bytes)> {
        let key = format!("{} {}", method.as_str(), path);
        let routes = self.routes.lock().unwrap();
        if let Some((status, body)) = routes.get(&key) {
            return Ok((*status, Bytes::from(body.clone())));
        }
        Ok((
            404,
            Bytes::from(
                r#"{"type":"error","error_code":404,"error":"not found","metadata":null}"#
                    .to_string(),
            ),
        ))
    }
}

fn sync(metadata: Value) -> String {
    json!({"type": "sync", "status": "Success", "status_code": 200, "metadata": metadata})
        .to_string()
}

fn backend_with(transport: Arc<FakeTransport>) -> IncusBackend {
    IncusBackend::from_transport(
        transport,
        Duration::from_secs(5),
        PathBuf::from("/tmp/fake.sock"),
    )
}

#[tokio::test]
async fn info_reads_environment() {
    let transport = Arc::new(FakeTransport::default());
    transport.route(
        "GET /1.0",
        200,
        &sync(json!({"environment": {
            "server_version": "7.5.1",
            "api_version": "1.0",
            "os_name": "Ubuntu",
            "kernel_architecture": "x86_64",
        }})),
    );
    transport.route("GET /1.0/instances?recursion=1", 200, &sync(json!([])));
    transport.route("GET /1.0/images?recursion=1", 200, &sync(json!([])));
    let backend = backend_with(transport);
    let info = backend.info().await.unwrap();
    assert_eq!(info.version, "7.5.1");
    assert_eq!(info.api_version, "1.0");
    assert_eq!(info.os, "Ubuntu");
}

#[tokio::test]
async fn create_start_stop_flow() {
    let transport = Arc::new(FakeTransport::default());
    // pick_pool
    transport.route(
        "GET /1.0/storage-pools?recursion=1",
        200,
        &sync(json!([{"name": "default"}])),
    );
    // default_network
    transport.route(
        "GET /1.0/profiles/default",
        200,
        &sync(json!({"devices": {"eth0": {"type": "nic", "network": "incusbr0"}}})),
    );
    // create
    transport.route(
        "POST /1.0/instances",
        200,
        &json!({"type": "async", "operation": "/1.0/operations/op1",
                "metadata": {"id": "op1", "status": "Success", "err": ""}}).to_string(),
    );
    let instance = json!({
        "name": "demo", "status": "Stopped", "status_code": 102,
        "created_at": "2026-11-03T00:00:00Z", "last_used_at": "1970-01-01T00:00:00Z",
        "config": {"user.qe.image-ref": "alpine", "user.qe.managed": "true"},
        "devices": {"root": {"type": "disk", "pool": "default", "path": "/"}},
    });
    transport.route("GET /1.0/instances/demo?recursion=1", 200, &sync(instance.clone()));
    transport.route("GET /1.0/operations/op1", 200, &sync(json!({"id": "op1", "status": "Success"})));

    let backend = backend_with(transport.clone());
    let mut spec = ContainerSpec::new("demo", "alpine");
    spec = spec.with_command(["echo", "hi"]);
    let id = backend.create_container(&spec).await.unwrap();
    assert_eq!(id, "demo");

    // start
    transport.route(
        "PUT /1.0/instances/demo/state",
        200,
        &json!({"type": "async", "operation": "/1.0/operations/op2",
                "metadata": {"id": "op2", "status": "Success"}}).to_string(),
    );
    transport.route("GET /1.0/operations/op2", 200, &sync(json!({"id": "op2", "status": "Success"})));
    backend.start_container("demo").await.unwrap();

    // stop
    transport.route(
        "PUT /1.0/instances/demo/state",
        200,
        &json!({"type": "async", "operation": "/1.0/operations/op3",
                "metadata": {"id": "op3", "status": "Success"}}).to_string(),
    );
    transport.route("GET /1.0/operations/op3", 200, &sync(json!({"id": "op3", "status": "Success"})));
    backend.stop_container("demo", None).await.unwrap();
}

#[tokio::test]
async fn wait_container_reads_exit_code_file() {
    let transport = Arc::new(FakeTransport::default());
    transport.route(
        "GET /1.0/instances/demo/state",
        200,
        &sync(json!({"status": "Stopped", "status_code": 102})),
    );
    transport.route("GET /1.0/instances/demo/files?path=/qe-exit-code", 200, "7\n");
    let backend = backend_with(transport);
    let status = backend.wait_container("demo").await.unwrap();
    assert_eq!(status.code, 7);
}

#[tokio::test]
async fn remove_force_stops_first() {
    let transport = Arc::new(FakeTransport::default());
    transport.route(
        "GET /1.0/instances/demo/state",
        200,
        &sync(json!({"status": "Running", "status_code": 103})),
    );
    transport.route(
        "PUT /1.0/instances/demo/state",
        200,
        &json!({"type": "async", "operation": "/1.0/operations/ops",
                "metadata": {"id": "ops", "status": "Success"}}).to_string(),
    );
    transport.route("GET /1.0/operations/ops", 200, &sync(json!({"id": "ops", "status": "Success"})));
    transport.route(
        "DELETE /1.0/instances/demo",
        200,
        &json!({"type": "async", "operation": "/1.0/operations/opd",
                "metadata": {"id": "opd", "status": "Success"}}).to_string(),
    );
    transport.route("GET /1.0/operations/opd", 200, &sync(json!({"id": "opd", "status": "Success"})));
    let backend = backend_with(transport);
    backend.remove_container("demo", true).await.unwrap();
}

#[tokio::test]
async fn networks_map_from_incus_shape() {
    let transport = Arc::new(FakeTransport::default());
    transport.route(
        "GET /1.0/networks/qenet",
        200,
        &sync(json!({
            "name": "qenet", "type": "bridge", "managed": true,
            "config": {"ipv4.address": "10.77.88.1/24", "user.team": "core"},
            "used_by": ["/1.0/instances/demo?project=default", "/1.0/profiles/default"],
        })),
    );
    let backend = backend_with(transport);
    let info = backend.inspect_network("qenet").await.unwrap();
    assert_eq!(info.driver, "bridge");
    assert_eq!(info.gateway, Some(IpAddr::V4(Ipv4Addr::new(10, 77, 88, 1))));
    assert_eq!(info.labels.get("team").map(String::as_str), Some("core"));
    assert_eq!(info.containers.len(), 1);
    assert_eq!(info.containers[0].name, "demo");
}

