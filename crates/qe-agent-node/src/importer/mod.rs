//! 导入适配器：按 `kind` / `format` 选择具体实现（契约 §16.4）。
//!
//! - [`DockerImporter`]：`docker_archive`（`docker load`）/ `oci_layout`（`ctr images import`）
//! - [`IncusImporter`]：LXC rootfs tar（Incus 优先，plain LXC 解包兜底）
//! - [`VmImporter`]：`qcow2` / `raw` → libvirt storage pool
//!
//! 本模块只定义契约与分发；具体命令构造在各自文件里，命令路径由
//! [`AgentConfig`](crate::AgentConfig) 注入，便于用 fake 脚本测试。

pub mod docker;
pub mod incus;
pub mod vm;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use crate::BoxFuture;
use crate::config::AgentConfig;
use crate::manifest::{ImageFormat, ImageKind};

pub use docker::DockerImporter;
pub use incus::IncusImporter;
pub use vm::VmImporter;

/// 一次导入的输入（字段与 §16.4 一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRequest {
    pub kind: String,
    pub format: String,
    pub name: String,
    pub version: String,
    /// 校验后落盘目录（含组装文件/tar）。
    pub staging_dir: PathBuf,
    /// 单个文件（qcow2/tar/rootfs tar）或 OCI 目录。
    pub artifact_path: PathBuf,
    /// 导入后的本地名称/引用。
    pub imported_name: String,
}

/// 导入结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportResult {
    /// 本地引用，如 `docker:debian:12`、`incus:debian`、`libvirt:default/debian-12`。
    pub imported_ref: String,
}

/// 拓扑导入器抽象。
pub trait TopologyImporter: Send + Sync {
    fn supports(&self, kind: &str, format: &str) -> bool;

    fn import<'a>(&'a self, request: &'a ImportRequest) -> BoxFuture<'a, Result<ImportResult>>;
}

/// 一组 importer，按注册顺序匹配第一个 `supports` 的实现。
pub struct ImporterSet {
    importers: Vec<Arc<dyn TopologyImporter>>,
}

impl ImporterSet {
    pub fn new(importers: Vec<Arc<dyn TopologyImporter>>) -> Self {
        Self { importers }
    }

    /// 生产环境三件套（Docker/OCI、Incus/LXC、VM/libvirt）。
    pub fn builtin(config: &AgentConfig) -> Self {
        Self::new(vec![
            Arc::new(DockerImporter::new(&config.docker_bin, &config.ctr_bin)),
            Arc::new(IncusImporter::with_auto_start(
                &config.incus_bin,
                &config.lxc_rootfs_dir,
                config.lxc_auto_start,
            )),
            Arc::new(VmImporter::with_uri(
                &config.virsh_bin,
                &config.libvirt_pool,
                &config.libvirt_uri,
            )),
        ])
    }

    /// 选择能处理 `kind/format` 的 importer；没有则返回 `None`（job 会上报失败）。
    pub fn select(&self, kind: &str, format: &str) -> Option<&Arc<dyn TopologyImporter>> {
        self.importers
            .iter()
            .find(|importer| importer.supports(kind, format))
    }

    /// 便捷版：按 manifest 的枚举选择。
    pub fn select_for(
        &self,
        kind: ImageKind,
        format: ImageFormat,
    ) -> Option<&Arc<dyn TopologyImporter>> {
        self.select(kind.as_str(), format.as_str())
    }

    pub fn len(&self) -> usize {
        self.importers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.importers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[derive(Debug)]
    struct FixedImporter {
        kind: &'static str,
        format: &'static str,
        reference: &'static str,
    }

    impl TopologyImporter for FixedImporter {
        fn supports(&self, kind: &str, format: &str) -> bool {
            kind == self.kind && format == self.format
        }

        fn import<'a>(
            &'a self,
            _request: &'a ImportRequest,
        ) -> BoxFuture<'a, Result<ImportResult>> {
            Box::pin(async move {
                Ok(ImportResult {
                    imported_ref: self.reference.to_owned(),
                })
            })
        }
    }

    fn request() -> ImportRequest {
        ImportRequest {
            kind: "docker".into(),
            format: "docker_archive".into(),
            name: "demo".into(),
            version: "1".into(),
            staging_dir: Path::new("/tmp/staging").to_path_buf(),
            artifact_path: Path::new("/tmp/staging/demo.tar").to_path_buf(),
            imported_name: "demo:1".into(),
        }
    }

    fn config() -> AgentConfig {
        AgentConfig {
            mtls_endpoint: "https://node:4441".into(),
            slot: "slot-1".into(),
            data_dir: PathBuf::from("./data"),
            cache_dir: PathBuf::from("./cache"),
            cache_max_bytes: 1024,
            download_concurrency: 4,
            docker_bin: "docker".into(),
            ctr_bin: "ctr".into(),
            incus_bin: "incus".into(),
            virsh_bin: "virsh".into(),
            libvirt_uri: "qemu:///system".into(),
            libvirt_pool: "default".into(),
            lxc_rootfs_dir: PathBuf::from("/var/lib/lxc"),
            lxc_auto_start: false,
            config_file: PathBuf::from("./data/agent-config.json"),
            local_policy: qe_domain::agent_config::AgentPolicy::default(),
        }
    }

    #[test]
    fn selects_first_matching_importer() {
        let set = ImporterSet::new(vec![
            Arc::new(FixedImporter {
                kind: "docker",
                format: "docker_archive",
                reference: "docker",
            }),
            Arc::new(FixedImporter {
                kind: "oci",
                format: "oci_layout",
                reference: "oci",
            }),
        ]);

        let docker = set.select("docker", "docker_archive").unwrap();
        assert!(docker.supports("docker", "docker_archive"));
        assert!(set.select("vm_disk", "raw").is_none());
        assert_eq!(set.len(), 2);
        assert!(!set.is_empty());
        assert!(ImporterSet::new(Vec::new()).is_empty());
    }

    #[test]
    fn select_for_uses_manifest_enums() {
        let set = ImporterSet::new(vec![Arc::new(FixedImporter {
            kind: "lxc_rootfs",
            format: "tar",
            reference: "lxc",
        })]);
        assert!(
            set.select_for(ImageKind::LxcRootfs, ImageFormat::Tar)
                .is_some()
        );
        assert!(
            set.select_for(ImageKind::VmDisk, ImageFormat::Qcow2)
                .is_none()
        );
    }

    #[test]
    fn builtin_supports_all_three_contract_pairs() {
        let set = ImporterSet::builtin(&config());
        assert_eq!(set.len(), 3, "docker / incus / vm 三个适配器");
        for (kind, format) in [
            ("docker", "docker_archive"),
            ("oci", "oci_layout"),
            ("lxc_rootfs", "tar"),
            ("vm_disk", "qcow2"),
            ("vm_disk", "raw"),
        ] {
            assert!(
                set.select(kind, format).is_some(),
                "内置 importer 应覆盖 {kind}/{format}"
            );
        }
        assert!(set.select("unknown", "raw").is_none());
    }

    #[tokio::test]
    async fn import_forwards_request() {
        let set = ImporterSet::new(vec![Arc::new(FixedImporter {
            kind: "docker",
            format: "docker_archive",
            reference: "docker:demo:1",
        })]);
        let importer = set.select("docker", "docker_archive").unwrap();
        let result = importer.import(&request()).await.unwrap();
        assert_eq!(result.imported_ref, "docker:demo:1");
    }
}
