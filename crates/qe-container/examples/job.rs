//! Run a one-shot container with `cargo run --example job`.

use futures_util::StreamExt;
use qe_container::{ContainerSpec, Engine, JobOptions, LogOptions};

#[tokio::main]
async fn main() -> qe_container::Result<()> {
    let engine = Engine::connect_from_env().await?;
    let spec = ContainerSpec::new("qe-example-job", "alpine:3.20").with_command([
        "sh",
        "-c",
        "echo hello from qe-container; sleep 1",
    ]);

    let job = engine.start_job(&spec, &JobOptions::default()).await?;
    let mut logs = job.logs(&LogOptions::follow())?;
    while let Some(line) = logs.next().await {
        print!("{}", line?.text);
    }

    let status = job.finish().await?;
    println!("exit code: {}", status.code);
    Ok(())
}
