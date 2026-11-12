//! `qe-container` is a small, standalone control plane for local containers,
//! services, and networks, targeting both Docker and Incus.
//!
//! The crate is deliberately independent from any desktop UI:
//!
//! - [`Engine`] is the high-level API used by applications and the CLI.
//! - [`ContainerSpec`] / [`ServiceSpec`] are the single configuration model;
//!   TOML service files are just a deserialization front-end.
//! - [`Backend`] hides the concrete engine. Implementations exist for the
//!   Docker Engine API (through `bollard`), for the Incus REST API (through
//!   its unix socket), and in memory ([`MockBackend`]) for tests.
//!
//! # Example
//!
//! ```no_run
//! use qe_container::{Engine, EngineConfig, PullPolicy, JobOptions, ContainerSpec};
//!
//! # async fn run() -> qe_container::Result<()> {
//! let engine = Engine::connect(EngineConfig::default()).await?;
//! engine.ensure_image("alpine:3.20", PullPolicy::IfMissing, None).await?;
//!
//! let spec = ContainerSpec::new("hello", "alpine:3.20")
//!     .with_command(["echo", "hello from qe-container"]);
//! let job = engine.start_job(&spec, &JobOptions::default()).await?;
//! let mut logs = job.logs(&qe_container::LogOptions::follow())?;
//! while let Some(line) = futures_util::StreamExt::next(&mut logs).await {
//!     print!("{}", line?.text);
//! }
//! let status = job.finish().await?;
//! assert!(status.success());
//! # Ok(())
//! # }
//! ```
//!
//! For a declarative service:
//!
//! ```no_run
//! # async fn run() -> qe_container::Result<()> {
//! use qe_container::{Engine, EnsureOptions, NetworkEnsureOptions, ServiceFile};
//!
//! let engine = Engine::connect_from_env().await?;
//! let file = ServiceFile::load("qe-container.toml")?;
//! let status = engine
//!     .ensure_file(&file, &EnsureOptions::default(), NetworkEnsureOptions::default())
//!     .await?;
//! println!("{} is {}", status.name, status.state as u8);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod backend;
pub mod config;
#[cfg(feature = "config")]
pub mod config_file;
pub mod engine;
pub mod error;
pub mod model;
pub mod stream;

pub use backend::Backend;
pub use backend::docker::DockerBackend;
pub use backend::incus::IncusBackend;
pub use backend::mock::MockBackend;
pub use config::{BackendKind, EngineConfig, NetworkEnsureOptions, PullPolicy};
pub use engine::{Engine, JobHandle, JobOptions};
pub use error::{Error, Result};
pub use model::{
    ByteSize, ContainerDetail, ContainerFilter, ContainerNetwork, ContainerSpec,
    ContainerStateKind, ContainerSummary, EngineInfo, EnsureOptions, EnsurePolicy, ExitStatus,
    HealthCheck, HealthState, ImageInfo, LogConfig, Mount, MountKind, MountPoint, NetworkAction,
    NetworkContainer, NetworkInfo, NetworkPlan, NetworkSpec, PortBinding, Protocol, PublishedPort,
    Resources, RestartPolicy, SERVICE_FILE_VERSION, SecurityOptions, ServiceFile, ServiceSpec,
    ServiceStatus, Ulimit,
};
pub use stream::{
    EventStream, ExecChunk, ExecResult, ExecSession, ExecSpec, LogLine, LogOptions, OutputStream,
    PullProgress, StatsSample,
};

/// Common imports.
pub mod prelude {
    pub use crate::{
        Backend, ByteSize, ContainerFilter, ContainerSpec, Engine, EngineConfig, EnsureOptions,
        EnsurePolicy, Error, HealthCheck, JobHandle, JobOptions, LogOptions, Mount,
        NetworkEnsureOptions, NetworkSpec, PortBinding, Protocol, PullPolicy, RestartPolicy,
        Result, ServiceFile, ServiceSpec, ServiceStatus,
    };
}
