//! 房间节点 agent：经 device-pki mTLS 拉取、校验并导入镜像/资源。
//!
//! 实现契约见 `docs/storage-design.md` §16.4 / §16.5 / §17.5：
//!
//! - [`config::AgentConfig`]：环境变量 + 本地配置文件（`AGENT_CONFIG_FILE`）
//! - [`policy::RuntimePolicy`]：启动拉取 + `config.update` 的运行时策略
//! - [`manifest::Manifest`]：与 §14.2 对齐的清单 + sha256 digest + Ed25519 验签
//! - [`presign::PresignClient`]：mTLS 预签名（`POST /resources/presign`）
//! - [`cache::ObjectCache`]：内容寻址缓存 + LRU
//! - [`pull::Puller`]：下载 / 校验 / 组装（进度回调）
//! - [`report::Reporter`]：上行 `transfer.progress` / `transfer.completed`
//! - [`importer::TopologyImporter`]：Docker/OCI、Incus/LXC、VM/libvirt 导入
//! - [`job::ImagePullJob`]：一次 `image.pull` 的编排

#![deny(unsafe_code)]

use std::future::Future;
use std::pin::Pin;

/// boxed future 别名（trait 方法返回异步结果，避免 `async fn in trait` 的对象安全问题）。
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub mod agent_control;
pub mod cache;
pub mod config;
pub mod container_control;
pub mod host_power;
pub mod image_make;
pub mod importer;
pub mod job;
pub mod manifest;
pub mod p2p;
pub mod policy;
pub mod presign;
pub mod pull;
pub mod report;
pub mod run;
pub mod vm_control;
pub mod vnc;

pub use cache::ObjectCache;
pub use config::AgentConfig;
pub use importer::{ImportRequest, ImportResult, ImporterSet, TopologyImporter};
pub use job::{ImagePullJob, ImagePullPayload, JobOutcome};
pub use manifest::{Manifest, ManifestChunk, ManifestRuntime};
pub use p2p::{BlobExchange, MtlsSeedRegistry, P2pDistribution, PeerSeed, SeedRegistry};
pub use policy::{MtlsAgentConfigClient, RuntimePolicy};
pub use presign::{
    MtlsPresignClient, PresignClient, PresignKind, PresignReply, PresignRequest, UrlItem,
};
pub use pull::{HttpObjectFetcher, ObjectFetcher, PullRequest, PulledImage, Puller};
pub use report::{CompletedPayload, MtlsReporter, Phase, ProgressEvent, ProgressPayload, Reporter};
pub use run::{P2pNodeProvider, on_remote_policy_applied, run, run_with_config, run_with_p2p};
