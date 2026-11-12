//! Optional smoke test against a real Docker daemon.
//!
//! Run with:
//!
//! ```sh
//! QE_CONTAINER_TESTS=1 cargo test --test docker_smoke -- --nocapture
//! ```
//!
//! The test is skipped (not failed) when the environment variable is absent.
//! Override the image with `QE_CONTAINER_TEST_IMAGE` if the default is not
//! available locally or through the daemon's registry mirror.

use qe_container::{ContainerSpec, Engine, EngineConfig, JobOptions, LogOptions, PullPolicy};

#[tokio::test]
async fn docker_job_roundtrip() {
    if std::env::var("QE_CONTAINER_TESTS").ok().as_deref() != Some("1") {
        eprintln!("skipping Docker smoke test; set QE_CONTAINER_TESTS=1 to enable");
        return;
    }

    let image =
        std::env::var("QE_CONTAINER_TEST_IMAGE").unwrap_or_else(|_| "debian:bookworm-slim".into());
    let engine = Engine::connect(EngineConfig::default()).await.unwrap();
    let info = engine.info().await.unwrap();
    assert!(!info.version.is_empty());

    let name = format!("qe-smoke-{:08x}", fastrand::u32(..));
    let spec = ContainerSpec::new(&name, &image).with_command(["sh", "-c", "echo qe-ok"]);
    let job = engine
        .start_job(
            &spec,
            &JobOptions {
                pull_policy: PullPolicy::IfMissing,
                remove_on_finish: true,
            },
        )
        .await
        .unwrap();

    let mut logs = job.logs(&LogOptions::follow()).unwrap();
    let mut text = String::new();
    while let Some(line) = futures_util::StreamExt::next(&mut logs).await {
        text.push_str(&line.unwrap().text);
    }
    let status = job.finish().await.unwrap();
    assert_eq!(status.code, 0, "job failed: {text}");
    assert!(text.contains("qe-ok"), "unexpected logs: {text}");
    assert!(
        engine.inspect_container(&name).await.is_err(),
        "job container should be removed"
    );
}
