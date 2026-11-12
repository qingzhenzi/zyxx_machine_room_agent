//! Engine-level tests against the in-memory backend.

use std::sync::Arc;
use std::time::Duration;

use qe_container::{
    ContainerFilter, ContainerSpec, EnsureOptions, EnsurePolicy, HealthCheck, JobOptions, LogLine,
    MockBackend, NetworkAction, NetworkEnsureOptions, NetworkSpec, PortBinding, PullPolicy,
    ServiceSpec,
};

fn engine() -> (qe_container::Engine, Arc<MockBackend>) {
    let backend = Arc::new(MockBackend::new());
    (qe_container::Engine::new(backend.clone()), backend)
}

fn service(name: &str) -> ServiceSpec {
    ServiceSpec::new(ContainerSpec::new(name, "alpine:3.20").with_command(["sleep", "infinity"]))
        .with_health_check(HealthCheck::exec(["true"]))
}

fn options(policy: EnsurePolicy) -> EnsureOptions {
    EnsureOptions {
        policy,
        wait_healthy: true,
        timeout: Duration::from_secs(2),
        pull_policy: PullPolicy::IfMissing,
    }
}

#[tokio::test]
async fn ensure_service_is_idempotent() {
    let (engine, _) = engine();
    let spec = service("api");

    let first = engine
        .ensure_service(&spec, &options(EnsurePolicy::Error))
        .await
        .unwrap();
    assert!(first.created);
    assert!(first.running);
    assert!(first.managed);

    let second = engine
        .ensure_service(&spec, &options(EnsurePolicy::Error))
        .await
        .unwrap();
    assert!(!second.created);
    assert!(second.running);
    assert!(!second.drift);
}

#[tokio::test]
async fn spec_drift_requires_explicit_policy() {
    let (engine, _) = engine();
    let spec = service("api");
    engine
        .ensure_service(&spec, &options(EnsurePolicy::Error))
        .await
        .unwrap();

    let changed =
        ServiceSpec::new(ContainerSpec::new("api", "alpine:3.20").with_command(["sleep", "10"]))
            .with_health_check(HealthCheck::exec(["true"]));

    let error = engine
        .ensure_service(&changed, &options(EnsurePolicy::Error))
        .await
        .unwrap_err();
    assert!(matches!(error, qe_container::Error::SpecDrift { .. }));

    let kept = engine
        .ensure_service(&changed, &options(EnsurePolicy::Keep))
        .await
        .unwrap();
    assert!(kept.drift);
    let desired = changed.spec_hash().unwrap();
    assert_ne!(kept.spec_hash.as_deref(), Some(desired.as_str()));

    let recreated = engine
        .ensure_service(&changed, &options(EnsurePolicy::Recreate))
        .await
        .unwrap();
    assert!(recreated.created);
    assert!(recreated.drift);
}

#[tokio::test]
async fn job_streams_logs_and_removes_container() {
    let (engine, backend) = engine();
    backend.seed_image("debian:bookworm-slim");
    let spec = ContainerSpec::new("job", "debian:bookworm-slim").with_command(["echo", "hello"]);
    let job = engine
        .start_job(&spec, &JobOptions::default())
        .await
        .unwrap();
    backend
        .push_log(job.name(), LogLine::stdout("hello\n"))
        .unwrap();
    let mut logs = job.logs(&qe_container::LogOptions::default()).unwrap();
    let mut text = String::new();
    while let Some(line) = futures_util::StreamExt::next(&mut logs).await {
        text.push_str(&line.unwrap().text);
    }
    let status = job.finish().await.unwrap();
    assert!(status.success());
    assert!(
        engine.inspect_container("job").await.is_err(),
        "job should be removed"
    );
    assert_eq!(text, "hello\n");
}

#[tokio::test]
async fn mock_exec_drives_health_checks() {
    let (engine, backend) = engine();
    backend.set_exec_result("api", "health", 0, "ok");
    let spec = ServiceSpec::new(ContainerSpec::new("api", "alpine:3.20"))
        .with_health_check(HealthCheck::exec(["health"]));
    let status = engine
        .ensure_service(&spec, &options(EnsurePolicy::Error))
        .await
        .unwrap();
    assert_eq!(status.health, Some(qe_container::HealthState::Healthy));
}

#[tokio::test]
async fn network_plan_create_reuse_and_recreate() {
    let (engine, _) = engine();
    let spec = NetworkSpec::new("docker1")
        .with_subnet("172.30.0.0/16".parse().unwrap())
        .with_gateway("172.30.0.1".parse().unwrap())
        .with_bridge_name("docker1");

    let plan = engine.plan_network(&spec).await.unwrap();
    assert_eq!(plan.action, NetworkAction::Create);

    engine
        .ensure_network(&spec, NetworkEnsureOptions::default())
        .await
        .unwrap();

    let plan = engine.plan_network(&spec).await.unwrap();
    assert_eq!(plan.action, NetworkAction::Reuse);

    let changed = NetworkSpec::new("docker1")
        .with_subnet("172.29.0.0/16".parse().unwrap())
        .with_gateway("172.29.0.1".parse().unwrap())
        .with_bridge_name("docker1");
    let plan = engine.plan_network(&changed).await.unwrap();
    assert_eq!(plan.action, NetworkAction::Conflict);

    let error = engine
        .ensure_network(&changed, NetworkEnsureOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(error, qe_container::Error::Conflict(_)));

    let plan = engine
        .ensure_network(&changed, NetworkEnsureOptions::recreate())
        .await
        .unwrap();
    assert_eq!(plan.action, NetworkAction::Recreate);
}

#[tokio::test]
async fn service_ports_are_exposed_and_resolvable() {
    let (engine, backend) = engine();
    backend.seed_image("redis:7-alpine");
    let spec = ServiceSpec::new(
        ContainerSpec::new("redis", "redis:7-alpine")
            .with_port(PortBinding::published(16379, 6379)),
    );
    let status = engine
        .ensure_service(
            &spec,
            &EnsureOptions {
                policy: EnsurePolicy::Error,
                wait_healthy: false,
                timeout: Duration::from_secs(2),
                pull_policy: PullPolicy::IfMissing,
            },
        )
        .await
        .unwrap();
    assert_eq!(status.host_ports.get(&6379), Some(&16379));
    let address = engine.host_port("redis", 6379).await.unwrap();
    assert_eq!(address.port(), 16379);
    assert!(address.ip().is_loopback());
}

#[tokio::test]
async fn external_containers_are_not_adopted_silently() {
    let (engine, backend) = engine();
    backend.seed_container(ContainerSpec::new("external", "alpine:3.20"), true);
    let spec = service("external");
    let error = engine
        .ensure_service(&spec, &options(EnsurePolicy::Error))
        .await
        .unwrap_err();
    assert!(matches!(error, qe_container::Error::Conflict(_)));
}

#[tokio::test]
async fn prune_removes_jobs_only() {
    let (engine, backend) = engine();
    backend.seed_image("alpine:3.20");
    let job = ContainerSpec::new("job-1", "alpine:3.20")
        .with_label("qe.kind", "job")
        .with_command(["true"]);
    let job = engine
        .start_job(&job, &JobOptions::default())
        .await
        .unwrap();
    job.wait().await.unwrap();

    let service = ServiceSpec::new(
        ContainerSpec::new("service-1", "alpine:3.20").with_command(["sleep", "infinity"]),
    );
    engine
        .ensure_service(
            &service,
            &EnsureOptions {
                policy: EnsurePolicy::Error,
                wait_healthy: false,
                timeout: Duration::from_secs(2),
                pull_policy: PullPolicy::IfMissing,
            },
        )
        .await
        .unwrap();

    let removed = engine.prune_jobs(false).await.unwrap();
    assert_eq!(removed, 1);
    let remaining = engine
        .list_containers(&ContainerFilter {
            all: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].name(), Some("service-1"));
}
