//! Ensure the example Python service from a TOML file.
//!
//! ```sh
//! cargo run --example service
//! cargo run --features cli -- service down -f examples/python-service.toml
//! ```

use qe_container::{Engine, EnsureOptions, NetworkEnsureOptions, ServiceFile};

#[tokio::main]
async fn main() -> qe_container::Result<()> {
    let engine = Engine::connect_from_env().await?;
    let file = ServiceFile::load("examples/python-service.toml")?;
    let status = engine
        .ensure_file(
            &file,
            &EnsureOptions::default(),
            NetworkEnsureOptions::default(),
        )
        .await?;

    println!("{}: {:?}", status.name, status.state);
    for (container, host) in &status.host_ports {
        println!("http://127.0.0.1:{host} (container port {container})");
    }
    Ok(())
}
