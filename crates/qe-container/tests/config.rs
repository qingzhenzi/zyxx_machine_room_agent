//! TOML/model tests that do not need a container daemon.

use std::time::Duration;

use qe_container::{ContainerSpec, Mount, PortBinding, ServiceFile, ServiceSpec};

const EXAMPLE: &str = r#"
version = 1

[network]
name = "docker1"
subnet = "172.30.0.0/16"
gateway = "172.30.0.1"
bridge_name = "docker1"

[service]
name = "py-api"
image = "python:3.12-slim"
command = ["python", "-m", "http.server", "8000"]
workdir = "/workspace"
restart = "unless-stopped"
stop_grace = "10s"
env = { PYTHONUNBUFFERED = "1" }
mounts = [{ source = "./app", target = "/workspace", read_only = true }]
ports = [{ container = 8000, host = 18000 }]
network = "docker1"
static_ip = "172.30.0.10"
log = { max_size = "1m", max_file = 2 }
healthcheck = { type = "http", port = 8000, path = "/health", interval = "1s", timeout = "2s", retries = 5 }
"#;

#[test]
fn parses_full_example() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("app")).unwrap();
    let file = ServiceFile::from_toml_str(EXAMPLE, dir.path()).unwrap();
    let service = &file.service;
    assert_eq!(service.container.name, "py-api");
    assert_eq!(service.container.stop_grace, Some(Duration::from_secs(10)));
    assert_eq!(service.container.mounts[0].source, dir.path().join("app"));
    assert_eq!(service.container.log.max_size.unwrap().0, 1024 * 1024);
    assert_eq!(service.container.log.max_file, Some(2));
    assert!(service.healthcheck.is_some());
    assert_eq!(
        file.network.as_ref().unwrap().bridge_name.as_deref(),
        Some("docker1")
    );
}

#[test]
fn roundtrips_through_toml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("app")).unwrap();
    let file = ServiceFile::from_toml_str(EXAMPLE, dir.path()).unwrap();
    let rendered = file.to_toml_string().unwrap();
    let reparsed = ServiceFile::from_toml_str(&rendered, dir.path()).unwrap();
    assert_eq!(file, reparsed);
    assert!(rendered.contains("interval = \"1s\""), "{rendered}");
    assert!(rendered.contains("max_size = \"1m\""), "{rendered}");
}

#[test]
fn rejects_unknown_keys_at_any_depth() {
    let cases = [
        r#"
version = 1
[service]
name = "x"
image = "alpine"
nope = true
"#,
        r#"
version = 1
[service]
name = "x"
image = "alpine"
mounts = [{ source = "/tmp", target = "/tmp", nope = true }]
"#,
        r#"
version = 1
[service]
name = "x"
image = "alpine"
healthcheck = { type = "tcp", port = 80, nope = true }
"#,
        r#"
version = 1
[network]
name = "docker1"
nope = true
[service]
name = "x"
image = "alpine"
"#,
    ];

    for (index, text) in cases.iter().enumerate() {
        let error = ServiceFile::from_toml_str(text, ".").unwrap_err();
        assert!(
            error.to_string().contains("unknown field"),
            "case {index}: {error}"
        );
    }
}

#[test]
fn env_file_is_merged_and_explicit_env_wins() {
    let dir = tempfile::tempdir().unwrap();
    let env_file = dir.path().join(".env");
    std::fs::write(&env_file, "A=from-file\nB=from-file\n").unwrap();

    let text = r#"
version = 1
[service]
name = "x"
image = "alpine"
env_file = ".env"
env = { B = "explicit" }
"#;
    let file = ServiceFile::from_toml_str(text, dir.path()).unwrap();
    let resolved = file.service.resolved().unwrap();
    assert_eq!(
        resolved.container.env.get("A").map(String::as_str),
        Some("from-file")
    );
    assert_eq!(
        resolved.container.env.get("B").map(String::as_str),
        Some("explicit")
    );
    assert!(resolved.container.env_file.is_none());
}

#[test]
fn resolved_fills_implicit_network_reference() {
    let text = r#"
version = 1
[network]
name = "docker1"
subnet = "172.30.0.0/16"
[service]
name = "x"
image = "alpine"
static_ip = "172.30.0.10"
"#;
    let file = ServiceFile::from_toml_str(text, ".").unwrap();
    let resolved = file.resolved().unwrap();
    assert_eq!(
        resolved.service.container.network.as_deref(),
        Some("docker1")
    );
}

#[test]
fn hash_is_order_independent_for_mounts_and_ports() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir(&app).unwrap();

    let left = ContainerSpec::new("x", "alpine")
        .with_mount(Mount::bind(&app, "/one"))
        .with_mount(Mount::bind(&app, "/two"))
        .with_port(PortBinding::loopback(80))
        .with_port(PortBinding::loopback(443));
    let mut right = left.clone();
    right.mounts.reverse();
    right.ports.reverse();

    assert_eq!(left.spec_hash().unwrap(), right.spec_hash().unwrap());

    let changed = left.clone().with_port(PortBinding::published(8080, 80));
    assert_ne!(left.spec_hash().unwrap(), changed.spec_hash().unwrap());
}

#[test]
fn validation_rejects_bad_healthcheck_and_ports() {
    let spec = ServiceSpec::new(ContainerSpec::new("x", "alpine"))
        .with_health_check(qe_container::HealthCheck::tcp(53));
    // No published port 53.
    assert!(spec.validate().is_err());

    let mut spec = ContainerSpec::new("x", "alpine");
    spec.ports.push(PortBinding {
        container: 0,
        host: Some(80),
        host_ip: "127.0.0.1".parse().unwrap(),
        protocol: qe_container::Protocol::Tcp,
    });
    assert!(spec.validate().is_err());
}

#[test]
fn invalid_versions_are_rejected() {
    let text = r#"
version = 2
[service]
name = "x"
image = "alpine"
"#;
    assert!(ServiceFile::from_toml_str(text, ".").is_err());
}
