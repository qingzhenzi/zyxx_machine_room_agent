//! Live Incus smoke test, gated behind `QE_CONTAINER_INCUS_TESTS=1`.
//!
//! Requires a reachable Incus daemon and an imported container image:
//!
//! ```sh
//! QE_CONTAINER_INCUS_TESTS=1 \
//! QE_CONTAINER_INCUS_IMAGE=<fingerprint-or-alias> \
//! cargo test -p qe-container --test incus_smoke -- --nocapture
//! ```

use std::time::Duration;

use futures_util::StreamExt;
use qe_container::{
    BackendKind, ContainerFilter, ContainerSpec, Engine, EngineConfig, ExecSpec, LogOptions, Mount,
    NetworkSpec, PortBinding, Result,
};

fn enabled() -> bool {
    std::env::var("QE_CONTAINER_INCUS_TESTS").is_ok_and(|value| value == "1")
}

fn image() -> String {
    std::env::var("QE_CONTAINER_INCUS_IMAGE").unwrap_or_else(|_| {
        // debian bookworm-slim rootfs imported during the Incus probes.
        "bb3cc84827e727375e5bb2c7c5fb73e6f40db1e882f962a98269c084b2517be2".to_string()
    })
}

async fn engine() -> Result<Engine> {
    let config = EngineConfig::default()
        .with_backend(BackendKind::Incus)
        .with_timeout(Duration::from_secs(60));
    Engine::connect(config).await
}

#[tokio::test]
async fn incus_lifecycle_smoke() -> Result<()> {
    if !enabled() {
        eprintln!("skipping: set QE_CONTAINER_INCUS_TESTS=1 to run");
        return Ok(());
    }
    let engine = engine().await?;
    let name = format!("qe-smoke-{}", std::process::id());

    // Info must describe the Incus daemon.
    let info = engine.info().await?;
    assert!(!info.version.is_empty(), "expected an incus version");
    println!("incus version: {} api {}", info.version, info.api_version);

    // Create with command (exit-code marker), env, a published port and labels.
    let mut spec = ContainerSpec::new(name.clone(), image());
    spec = spec
        .with_command(["/bin/sh", "-c", "echo smoke-hello; sleep 2; exit 7"])
        .with_env("QE_SMOKE", "1")
        .with_port(PortBinding::published(18_999, 80))
        .with_label("qe.smoke", "true");
    let id = engine.create_container(&spec).await?;
    assert_eq!(id, name);

    let detail = engine.inspect_container(&id).await?;
    assert!(detail.labels.contains_key("qe.smoke"), "labels must round-trip");
    assert_eq!(detail.ports.len(), 1, "proxy device must be published");
    assert_eq!(detail.ports[0].host, Some(18_999));

    engine.start_container(&id).await?;

    // exec: run a command and capture its exit code.
    let result = engine
        .exec_capture(&id, &ExecSpec::new(["/bin/sh", "-c", "echo exec-hi; exit 3"]))
        .await?;
    let text = result.text();
    assert!(text.contains("exec-hi"), "exec output: {text:?}");
    assert_eq!(result.exit_code, Some(3), "exec exit code");

    // stats: one sample must be readable while running.
    let mut stats = engine.stats(&id)?;
    if let Some(sample) = stats.next().await {
        let sample = sample?;
        println!(
            "stats: cpu={:?} mem={:?} pids={:?}",
            sample.cpu_percent, sample.memory_usage, sample.pids
        );
    }

    // logs: app output is visible (follow until exit).
    let mut logs = engine.logs(&id, &LogOptions::follow())?;
    let mut text = String::new();
    while let Some(line) = logs.next().await {
        text.push_str(&line?.text);
    }
    assert!(text.contains("smoke-hello"), "logs: {text:?}");

    // wait: the init wrapper recorded the exit code 7.
    let status = engine.wait_container(&id).await?;
    assert_eq!(status.code, 7, "init exit code via wrapper file");

    // Listing sees the container (stopped now).
    let listed = engine
        .list_containers(&ContainerFilter {
            all: true,
            name: Some(name.clone()),
            ..ContainerFilter::default()
        })
        .await?;
    assert_eq!(listed.len(), 1);

    engine.remove_container(&id, true).await?;
    assert!(engine.inspect_container(&id).await.is_err(), "removed");
    Ok(())
}

#[tokio::test]
async fn incus_network_smoke() -> Result<()> {
    if !enabled() {
        return Ok(());
    }
    let engine = engine().await?;
    // Keep the name short: Incus network names double as bridge interface
    // names and are capped at 15 characters.
    let net_name = format!("qesn{}", std::process::id() % 100000);

    let spec = NetworkSpec::new(net_name.clone());
    engine.ensure_network(&spec, Default::default()).await?;
    let info = engine.inspect_network(&net_name).await?;
    assert_eq!(info.name, net_name);
    assert!(info.gateway.is_some(), "auto-managed bridge has a gateway");

    engine.remove_network(&net_name).await?;
    Ok(())
}

#[tokio::test]
async fn incus_mount_and_limits_smoke() -> Result<()> {
    if !enabled() {
        return Ok(());
    }
    let engine = engine().await?;
    // The temp directory must live on a filesystem visible to the Incus
    // daemon, so use the working directory instead of /tmp.
    let dir = tempfile::Builder::new()
        .prefix("qe-smoke-mnt-")
        .tempdir_in(std::env::current_dir().expect("cwd"))
        .expect("tempdir");
    std::fs::write(dir.path().join("hello.txt"), "mount-hi").expect("write");
    let name = format!("qe-smoke-mnt-{}", std::process::id());

    let mut spec = ContainerSpec::new(name.clone(), image());
    spec = spec
        .with_command(["/bin/sh", "-c", "cat /data/hello.txt"])
        .with_mount(Mount::bind(dir.path().to_path_buf(), "/data").read_only());
    spec.resources.pids = Some(128);
    let id = engine.create_container(&spec).await?;
    engine.start_container(&id).await?;

    let mut logs = engine.logs(&id, &LogOptions::follow())?;
    let mut text = String::new();
    while let Some(line) = logs.next().await {
        text.push_str(&line?.text);
    }
    assert!(text.contains("mount-hi"), "bind mount content: {text:?}");

    engine.wait_container(&id).await?;
    engine.remove_container(&id, true).await?;
    Ok(())
}
