//! Standalone CLI for `qe-container`.

use std::io::Write;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use qe_container::{
    BackendKind, ContainerFilter, ContainerSpec, Engine, EngineConfig, EnsureOptions, EnsurePolicy,
    Error, LogOptions, Mount, NetworkEnsureOptions, NetworkSpec, PortBinding, Protocol, PullPolicy,
    Result, ServiceFile,
};

#[derive(Debug, Parser)]
#[command(
    name = "qe-container",
    version,
    about = "Local container control plane (Docker or Incus): containers, services, and networks"
)]
struct Cli {
    /// Container engine backend: docker, incus, or mock (overrides QE_CONTAINER_BACKEND).
    #[arg(long, global = true)]
    engine: Option<String>,

    /// Docker host URI (overrides DOCKER_HOST / QE_CONTAINER_HOST).
    #[arg(long, global = true)]
    host: Option<String>,

    /// Request timeout, for example `30s` (defaults to QE_CONTAINER_TIMEOUT or 120s).
    #[arg(long = "request-timeout", global = true)]
    request_timeout: Option<String>,

    /// Machine-readable output.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show daemon information.
    Info,
    /// List containers.
    Ps {
        #[arg(short, long)]
        all: bool,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        network: Option<String>,
    },
    /// Inspect a container.
    Inspect { container: String },
    /// Start a container.
    Start { container: String },
    /// Stop a container.
    Stop {
        container: String,
        /// Seconds to wait before killing.
        #[arg(long, default_value = "10")]
        timeout: u64,
    },
    /// Restart a container.
    Restart {
        container: String,
        /// Seconds to wait before killing.
        #[arg(long, default_value = "10")]
        timeout: u64,
    },
    /// Remove a container.
    Rm {
        container: String,
        #[arg(short, long)]
        force: bool,
    },
    /// Stream container logs.
    Logs {
        container: String,
        #[arg(short, long)]
        follow: bool,
        #[arg(long, default_value = "all")]
        tail: String,
        #[arg(long)]
        timestamps: bool,
    },
    /// Execute a command inside a running container.
    Exec {
        container: String,
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },
    /// Stream resource stats.
    Stats {
        container: String,
        /// Print one sample and exit.
        #[arg(long)]
        once: bool,
    },
    /// Pull an image.
    Pull {
        image: String,
        #[arg(long)]
        platform: Option<String>,
    },
    /// Start a one-shot container and stream its logs.
    Run {
        #[arg(long)]
        image: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long = "env")]
        env: Vec<String>,
        #[arg(long = "mount")]
        mount: Vec<String>,
        #[arg(long = "port")]
        port: Vec<String>,
        #[arg(long)]
        network: Option<String>,
        #[arg(long, default_value = "if-missing")]
        pull: String,
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Manage a long-running service from a TOML file.
    Service {
        #[command(subcommand)]
        action: ServiceCommand,
    },
    /// Manage Docker networks.
    Network {
        #[command(subcommand)]
        action: NetworkCommand,
    },
    /// Inspect or validate TOML service files.
    Config {
        #[command(subcommand)]
        action: ConfigCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// Ensure the service (and its network) exists and is healthy.
    Up {
        #[arg(short, long, default_value = "qe-container.toml")]
        file: PathBuf,
        /// Recreate the container when its spec changed.
        #[arg(long)]
        recreate: bool,
        /// Keep the existing container when its spec changed.
        #[arg(long, conflicts_with = "recreate")]
        keep: bool,
        /// Do not wait for the health check.
        #[arg(long)]
        no_wait: bool,
        /// Recreate a differing network.
        #[arg(long)]
        recreate_network: bool,
        /// Detach containers from a differing network while recreating it.
        #[arg(long, requires = "recreate_network")]
        force_network: bool,
    },
    /// Show the service status.
    Status {
        #[arg(short, long, default_value = "qe-container.toml")]
        file: PathBuf,
    },
    /// Stop and remove the service.
    Down {
        #[arg(short, long, default_value = "qe-container.toml")]
        file: PathBuf,
        /// Also remove the network when no containers remain.
        #[arg(long)]
        remove_network: bool,
    },
    /// Stream the service logs.
    Logs {
        #[arg(short, long, default_value = "qe-container.toml")]
        file: PathBuf,
        #[arg(long)]
        follow: bool,
        #[arg(long, default_value = "all")]
        tail: String,
    },
}

#[derive(Debug, Subcommand)]
enum NetworkCommand {
    /// List networks.
    Ls,
    /// Inspect a network.
    Inspect { name: String },
    /// Create or reuse a bridge network.
    Create {
        name: String,
        #[arg(long)]
        subnet: Option<String>,
        #[arg(long)]
        gateway: Option<String>,
        #[arg(long)]
        bridge_name: Option<String>,
        /// Recreate the network when its spec differs.
        #[arg(long)]
        recreate: bool,
        /// Detach containers from a differing network while recreating it.
        #[arg(long, requires = "recreate")]
        force: bool,
    },
    /// Remove a network.
    Rm { name: String },
    /// Show how a file's network differs from the daemon.
    Plan {
        #[arg(short, long, default_value = "qe-container.toml")]
        file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Parse and validate a service file.
    Validate {
        #[arg(short, long, default_value = "qe-container.toml")]
        file: PathBuf,
    },
    /// Print the resolved service file as TOML.
    Show {
        #[arg(short, long, default_value = "qe-container.toml")]
        file: PathBuf,
    },
    /// Write the resolved service file to another path.
    Export {
        #[arg(short, long, default_value = "qe-container.toml")]
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    init_tracing();
    if let Err(error) = run().await {
        eprintln!("qe-container: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let timeout = match cli.request_timeout.as_deref() {
        Some(raw) => {
            let parsed = humantime::parse_duration(raw).map_err(|error| {
                Error::config_at("--request-timeout", format!("invalid duration: {error}"))
            })?;
            if parsed.is_zero() {
                return Err(Error::config_at(
                    "--request-timeout",
                    "must be greater than zero",
                ));
            }
            Some(parsed)
        }
        None => None,
    };

    match cli.command {
        Command::Config { action } => run_config(action, cli.json).await,
        command => {
            let mut config = EngineConfig::from_env()?;
            if let Some(timeout) = timeout {
                config.timeout = timeout;
            }
            if let Some(host) = cli.host {
                config.host = Some(host);
            }
            if let Some(engine) = cli.engine {
                config.backend = BackendKind::parse(&engine)?;
            }
            let engine = Engine::connect(config).await?;
            run_command(&engine, command, cli.json).await
        }
    }
}

async fn run_command(engine: &Engine, command: Command, json: bool) -> Result<()> {
    match command {
        Command::Info => {
            let info = engine.info().await?;
            if json {
                print_json(&info)?;
            } else {
                println!("version:      {}", info.version);
                println!("api version:  {}", info.api_version);
                println!(
                    "min api:      {}",
                    info.min_api_version.as_deref().unwrap_or("-")
                );
                println!("os/arch:      {}/{}", info.os, info.arch);
                println!("rootless:     {}", info.rootless);
                println!(
                    "containers:   {} ({} running)",
                    info.containers, info.containers_running
                );
                println!("images:       {}", info.images);
            }
        }
        Command::Ps {
            all,
            name,
            label,
            network,
        } => {
            let filter = ContainerFilter {
                all,
                name,
                label,
                status: None,
                network,
            };
            let containers = engine.list_containers(&filter).await?;
            if json {
                print_json(&containers)?;
            } else {
                println!(
                    "{:<14} {:<24} {:<28} {:<10} {:<7} PORTS",
                    "ID", "NAME", "IMAGE", "STATE", "MANAGED"
                );
                for container in containers {
                    println!(
                        "{:<14} {:<24} {:<28} {:<10} {:<7} {}",
                        short_id(&container.id),
                        container.name().unwrap_or("-"),
                        truncate(&container.image, 28),
                        state_name(container.state),
                        if container.managed() { "yes" } else { "no" },
                        format_ports(&container.ports),
                    );
                }
            }
        }
        Command::Inspect { container } => {
            let detail = engine.inspect_container(&container).await?;
            print_json(&detail)?;
        }
        Command::Start { container } => {
            engine.start_container(&container).await?;
            if !json {
                println!("started {container}");
            }
        }
        Command::Stop { container, timeout } => {
            engine
                .stop_container(&container, Some(Duration::from_secs(timeout)))
                .await?;
            if !json {
                println!("stopped {container}");
            }
        }
        Command::Restart { container, timeout } => {
            engine
                .restart_container(&container, Some(Duration::from_secs(timeout)))
                .await?;
            if !json {
                println!("restarted {container}");
            }
        }
        Command::Rm { container, force } => {
            engine.remove_container(&container, force).await?;
            if !json {
                println!("removed {container}");
            }
        }
        Command::Logs {
            container,
            follow,
            tail,
            timestamps,
        } => {
            let options = LogOptions {
                follow,
                timestamps,
                tail: parse_tail(&tail)?,
                ..LogOptions::default()
            };
            stream_logs(engine, &container, &options, json).await?;
        }
        Command::Exec { container, command } => {
            let spec = qe_container::ExecSpec::new(command);
            let result = engine.exec_capture(&container, &spec).await?;
            for chunk in &result.output {
                if json {
                    print_json(chunk)?;
                } else {
                    print_chunk(chunk);
                }
            }
            if let Some(code) = result.exit_code {
                if code != 0 {
                    std::process::exit(code.clamp(1, 255) as i32);
                }
            }
        }
        Command::Stats { container, once } => {
            let mut stream = engine.stats(&container)?;
            while let Some(sample) = stream.next().await {
                let sample = sample?;
                if json {
                    print_json(&sample)?;
                } else {
                    println!(
                        "cpu={:>6.2}%  mem={} / {}  net_rx={}  net_tx={}  pids={}",
                        sample.cpu_percent.unwrap_or_default(),
                        format_bytes(sample.memory_usage.unwrap_or_default()),
                        format_bytes(sample.memory_limit.unwrap_or_default()),
                        format_bytes(sample.network_rx.unwrap_or_default()),
                        format_bytes(sample.network_tx.unwrap_or_default()),
                        sample.pids.unwrap_or_default(),
                    );
                }
                if once {
                    break;
                }
            }
        }
        Command::Pull { image, platform } => {
            let mut stream = engine.pull_image(&image, platform.as_deref())?;
            while let Some(progress) = stream.next().await {
                let progress = progress?;
                if json {
                    print_json(&progress)?;
                } else if let (Some(current), Some(total)) = (progress.current, progress.total) {
                    println!("{} ({} / {})", progress.status, current, total);
                } else {
                    println!("{}", progress.status);
                }
            }
        }
        Command::Run {
            image,
            name,
            env,
            mount,
            port,
            network,
            pull,
            command,
        } => {
            let name = name.unwrap_or_else(|| format!("qe-job-{:08x}", fastrand::u32(..)));
            let mut spec = ContainerSpec::new(name, image);
            if !command.is_empty() {
                spec = spec.with_command(command);
            }
            for item in env {
                let (key, value) = parse_env(&item)?;
                spec.env.insert(key, value);
            }
            for item in mount {
                spec.mounts.push(parse_mount(&item)?);
            }
            for item in port {
                spec.ports.push(parse_port(&item)?);
            }
            if let Some(network) = network {
                spec = spec.with_network(network);
            }

            let options = qe_container::JobOptions {
                pull_policy: PullPolicy::parse(&pull)?,
                remove_on_finish: true,
            };
            let job = engine.start_job(&spec, &options).await?;
            stream_logs(engine, job.name(), &LogOptions::follow(), json).await?;
            let status = job.finish().await?;
            if !status.success() {
                eprintln!("container exited with code {}", status.code);
                std::process::exit(status.code.clamp(1, 255) as i32);
            }
        }
        Command::Service { action } => match action {
            ServiceCommand::Up {
                file,
                recreate,
                keep,
                no_wait,
                recreate_network,
                force_network,
            } => {
                let file = ServiceFile::load(&file)?;
                let policy = if recreate {
                    EnsurePolicy::Recreate
                } else if keep {
                    EnsurePolicy::Keep
                } else {
                    EnsurePolicy::Error
                };
                let options = EnsureOptions {
                    policy,
                    wait_healthy: !no_wait,
                    timeout: engine.config().timeout,
                    pull_policy: PullPolicy::IfMissing,
                };
                let network_options = NetworkEnsureOptions {
                    recreate: recreate_network,
                    force: force_network,
                };
                let status = engine.ensure_file(&file, &options, network_options).await?;
                if json {
                    print_json(&status)?;
                } else {
                    println!(
                        "{}: {} (created={}, drift={})",
                        status.name,
                        state_name(status.state),
                        status.created,
                        status.drift
                    );
                    for (container, host) in &status.host_ports {
                        println!("  {container} -> 127.0.0.1:{host}");
                    }
                }
            }
            ServiceCommand::Status { file } => {
                let file = ServiceFile::load(&file)?;
                let status = engine.service_status_for(&file.service).await?;
                if json {
                    print_json(&status)?;
                } else {
                    println!(
                        "{}: {} (running={}, drift={}, managed={})",
                        status.name,
                        state_name(status.state),
                        status.running,
                        status.drift,
                        status.managed
                    );
                    for (container, host) in &status.host_ports {
                        println!("  {container} -> 127.0.0.1:{host}");
                    }
                }
            }
            ServiceCommand::Down {
                file,
                remove_network,
            } => {
                let file = ServiceFile::load(&file)?;
                engine.stop_file(&file, remove_network).await?;
                if !json {
                    println!("stopped {}", file.service.container.name);
                }
            }
            ServiceCommand::Logs { file, follow, tail } => {
                let file = ServiceFile::load(&file)?;
                let options = LogOptions {
                    follow,
                    tail: parse_tail(&tail)?,
                    ..LogOptions::default()
                };
                stream_logs(engine, &file.service.container.name, &options, json).await?;
            }
        },
        Command::Network { action } => match action {
            NetworkCommand::Ls => {
                let networks = engine.list_networks().await?;
                if json {
                    print_json(&networks)?;
                } else {
                    println!("{:<24} {:<10} {:<20} BRIDGE", "NAME", "DRIVER", "SUBNET");
                    for network in networks {
                        println!(
                            "{:<24} {:<10} {:<20} {}",
                            network.name,
                            network.driver,
                            network
                                .subnet
                                .map(|subnet| subnet.to_string())
                                .unwrap_or_else(|| "-".to_string()),
                            network.bridge_name.as_deref().unwrap_or("-"),
                        );
                    }
                }
            }
            NetworkCommand::Inspect { name } => {
                let network = engine.inspect_network(&name).await?;
                if json {
                    print_json(&network)?;
                } else {
                    println!("name:        {}", network.name);
                    println!("id:          {}", network.id);
                    println!("driver:      {}", network.driver);
                    println!(
                        "subnet:      {}",
                        network
                            .subnet
                            .map(|subnet| subnet.to_string())
                            .unwrap_or_else(|| "-".into())
                    );
                    println!(
                        "gateway:     {}",
                        network
                            .gateway
                            .map(|gateway| gateway.to_string())
                            .unwrap_or_else(|| "-".into())
                    );
                    println!(
                        "bridge:      {}",
                        network.bridge_name.as_deref().unwrap_or("-")
                    );
                    for container in &network.containers {
                        println!(
                            "container:   {} ({}) {}",
                            container.name,
                            container.id,
                            container.ipv4.as_deref().unwrap_or("-")
                        );
                    }
                }
            }
            NetworkCommand::Create {
                name,
                subnet,
                gateway,
                bridge_name,
                recreate,
                force,
            } => {
                let mut spec = NetworkSpec::new(name);
                if let Some(subnet) = subnet {
                    spec.subnet = Some(subnet.parse().map_err(|error| {
                        Error::config_at("--subnet", format!("invalid CIDR: {error}"))
                    })?);
                }
                if let Some(gateway) = gateway {
                    spec.gateway = Some(gateway.parse().map_err(|error| {
                        Error::config_at("--gateway", format!("invalid IP: {error}"))
                    })?);
                }
                spec.bridge_name = bridge_name;
                let plan = engine
                    .ensure_network(&spec, NetworkEnsureOptions { recreate, force })
                    .await?;
                if json {
                    print_json(&plan)?;
                } else {
                    println!("{}: {}", spec.name, action_name(plan.action));
                }
            }
            NetworkCommand::Rm { name } => {
                engine.remove_network(&name).await?;
                if !json {
                    println!("removed {name}");
                }
            }
            NetworkCommand::Plan { file } => {
                let file = ServiceFile::load(&file)?;
                let network = file
                    .network
                    .as_ref()
                    .ok_or_else(|| Error::config_at("<toml>", "file has no [network] section"))?;
                let plan = engine.plan_network(network).await?;
                if json {
                    print_json(&plan)?;
                } else {
                    println!("{}: {}", network.name, action_name(plan.action));
                    for reason in &plan.reasons {
                        println!("  - {reason}");
                    }
                    for container in plan.attached() {
                        println!("  attached: {} ({})", container.name, container.id);
                    }
                }
            }
        },
        Command::Config { .. } => unreachable!("config commands are handled before connecting"),
    }
    Ok(())
}

async fn run_config(action: ConfigCommand, json: bool) -> Result<()> {
    match action {
        ConfigCommand::Validate { file } => {
            let file = ServiceFile::load(&file)?;
            if json {
                print_json(&serde_json::json!({
                    "ok": true,
                    "service": file.service.container.name,
                    "version": file.version,
                }))?;
            } else {
                println!(
                    "ok: {} (version {}, image {})",
                    file.service.container.name, file.version, file.service.container.image
                );
            }
        }
        ConfigCommand::Show { file } => {
            let file = ServiceFile::load(&file)?;
            if json {
                print_json(&file)?;
            } else {
                print!("{}", file.to_toml_string()?);
            }
        }
        ConfigCommand::Export { file, output } => {
            let file = ServiceFile::load(&file)?;
            let contents = if json {
                serde_json::to_string_pretty(&file).map_err(Error::SerdeJson)?
            } else {
                file.to_toml_string()?
            };
            std::fs::write(&output, contents)
                .map_err(|error| Error::config(&output, format!("cannot write: {error}")))?;
            if !json {
                println!("wrote {}", output.display());
            }
        }
    }
    Ok(())
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

async fn stream_logs(
    engine: &Engine,
    container: &str,
    options: &LogOptions,
    json: bool,
) -> Result<()> {
    let mut stream = engine.logs(container, options)?;
    while let Some(line) = stream.next().await {
        let line = line?;
        if json {
            print_json(&line)?;
        } else {
            match line.stream {
                qe_container::OutputStream::Stderr => write_text(true, &line.text),
                _ => write_text(false, &line.text),
            }
        }
    }
    Ok(())
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(Error::SerdeJson)?
    );
    Ok(())
}

fn print_chunk(chunk: &qe_container::ExecChunk) {
    match chunk.stream {
        qe_container::OutputStream::Stderr => write_text(true, &chunk.text),
        _ => write_text(false, &chunk.text),
    }
}

/// Write raw output without panicking on a closed pipe (`qe-container ... | head`).
fn write_text(stderr: bool, text: &str) {
    let result = if stderr {
        std::io::stderr().write_all(text.as_bytes())
    } else {
        std::io::stdout().write_all(text.as_bytes())
    };
    if result.is_err() {
        // The consumer closed the pipe. Keep going so cleanup still runs.
    }
}

fn parse_env(raw: &str) -> Result<(String, String)> {
    let (key, value) = raw
        .split_once('=')
        .ok_or_else(|| Error::config_at("--env", format!("expected KEY=VALUE, got `{raw}`")))?;
    if key.is_empty() {
        return Err(Error::config_at("--env", "environment key is empty"));
    }
    Ok((key.to_string(), value.to_string()))
}

fn parse_mount(raw: &str) -> Result<Mount> {
    let mut parts = raw.splitn(3, ':');
    let source = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let options = parts.next().unwrap_or_default();
    if source.is_empty() || target.is_empty() {
        return Err(Error::config_at(
            "--mount",
            format!("expected SOURCE:TARGET[:ro], got `{raw}`"),
        ));
    }
    let mut mount = Mount::bind(source, target);
    if options.split(',').any(|option| option == "ro") {
        mount = mount.read_only();
    }
    Ok(mount)
}

fn parse_port(raw: &str) -> Result<PortBinding> {
    let (raw, protocol) = match raw.split_once('/') {
        Some((raw, "udp")) => (raw, Protocol::Udp),
        Some((raw, "tcp")) => (raw, Protocol::Tcp),
        None => (raw, Protocol::Tcp),
        Some((_, other)) => {
            return Err(Error::config_at(
                "--port",
                format!("unsupported protocol `{other}`"),
            ));
        }
    };
    let parts = raw.split(':').collect::<Vec<_>>();
    let (host_ip, host, container) = match parts.as_slice() {
        [host, container] => (
            Ok(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            host.parse::<u16>(),
            container.parse::<u16>(),
        ),
        [ip, host, container] => (
            ip.parse::<IpAddr>(),
            host.parse::<u16>(),
            container.parse::<u16>(),
        ),
        _ => {
            return Err(Error::config_at(
                "--port",
                format!("expected [HOST_IP:]HOST:CONTAINER, got `{raw}`"),
            ));
        }
    };
    Ok(PortBinding {
        container: container.map_err(|_| {
            Error::config_at("--port", format!("invalid container port in `{raw}`"))
        })?,
        host: Some(
            host.map_err(|_| Error::config_at("--port", format!("invalid host port in `{raw}`")))?,
        ),
        host_ip: host_ip
            .map_err(|_| Error::config_at("--port", format!("invalid host IP in `{raw}`")))?,
        protocol,
    })
}

fn parse_tail(raw: &str) -> Result<Option<usize>> {
    if raw == "all" {
        return Ok(None);
    }
    raw.parse::<usize>()
        .map(Some)
        .map_err(|_| Error::config_at("--tail", format!("expected a number or `all`, got `{raw}`")))
}

fn short_id(id: &str) -> &str {
    &id[..id.len().min(12)]
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        value.to_string()
    } else {
        format!("{}…", &value[..max.saturating_sub(1)])
    }
}

fn format_ports(ports: &[qe_container::PublishedPort]) -> String {
    ports
        .iter()
        .map(|port| match port.host {
            Some(host) => {
                let ip = port
                    .host_ip
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|| "0.0.0.0".to_string());
                format!(
                    "{ip}:{host}->{}/{}",
                    port.container,
                    port.protocol.as_docker()
                )
            }
            None => format!("{}/{}", port.container, port.protocol.as_docker()),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_bytes(value: u64) -> String {
    qe_container::ByteSize::bytes(value).to_string()
}

fn state_name(state: qe_container::ContainerStateKind) -> &'static str {
    match state {
        qe_container::ContainerStateKind::Created => "created",
        qe_container::ContainerStateKind::Running => "running",
        qe_container::ContainerStateKind::Paused => "paused",
        qe_container::ContainerStateKind::Restarting => "restarting",
        qe_container::ContainerStateKind::Removing => "removing",
        qe_container::ContainerStateKind::Exited => "exited",
        qe_container::ContainerStateKind::Dead => "dead",
        qe_container::ContainerStateKind::Stopping => "stopping",
        qe_container::ContainerStateKind::Unknown => "unknown",
    }
}

fn action_name(action: qe_container::NetworkAction) -> &'static str {
    match action {
        qe_container::NetworkAction::Create => "create",
        qe_container::NetworkAction::Reuse => "reuse",
        qe_container::NetworkAction::Conflict => "conflict",
        qe_container::NetworkAction::Recreate => "recreate",
    }
}
