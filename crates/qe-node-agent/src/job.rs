//! 一次 `image.pull` 的编排（契约 §16.4）：下行 payload → pull → import → report。
//!
//! 进度阶段：download 0-80、verify 80-90、import 90-100。
//! 任何失败都上报 `transfer.completed{ok:false}` 并返回 `Ok(JobOutcome{ok:false})`：
//! 下行是至少一次投递，如果这里返回 `Err`，同一条坏消息会被无限重投；把失败
//! 变成"已处理并上报"才能让控制面把 transfer 置为 failed。只有 payload 无法解析
//! 才返回 `Err`（无法关联 transfer，交给运维排查）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::importer::{ImportRequest, ImporterSet};
use crate::manifest::{ImageKind, Manifest};
use crate::pull::{PullRequest, Puller};
use crate::report::{CompletedPayload, Phase, ProgressEvent, ProgressPayload, Reporter};

/// 下行 `image.pull` 的 payload（§16.1，字段一一对应）。
#[derive(Debug, Clone, Deserialize)]
pub struct ImagePullPayload {
    pub transfer_id: String,
    pub image_id: String,
    pub name: String,
    pub version: String,
    pub kind: ImageKind,
    pub format: crate::manifest::ImageFormat,
    pub platform: String,
    pub manifest_digest: String,
    pub total_size: i64,
    pub chunk_count: i64,
    /// 服务端指定的预签名路径；实际 mTLS 客户端在启动时已确定，这里只做校验/提示。
    #[serde(default)]
    pub presign_endpoint: Option<String>,
}

impl ImagePullPayload {
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json).context("image.pull payload JSON 解析失败")
    }

    /// 分片数量必须是正的 `usize`。
    pub fn chunk_count(&self) -> Result<usize> {
        if self.chunk_count <= 0 {
            bail!(
                "image.pull 的 chunk_count 必须 > 0，实际 {}",
                self.chunk_count
            );
        }
        usize::try_from(self.chunk_count).context("chunk_count 超出 usize")
    }
}

/// 一次 job 的结果。`ok=false` 表示失败已上报，调用方不需要重试。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobOutcome {
    pub transfer_id: String,
    pub ok: bool,
    pub imported_ref: Option<String>,
    pub error: Option<String>,
}

/// `image.pull` 执行器。由 main 通过 `downlink::Dispatcher` 串行调用。
pub struct ImagePullJob {
    puller: Puller,
    importers: ImporterSet,
    reporter: Arc<dyn Reporter>,
    staging_root: PathBuf,
}

impl ImagePullJob {
    pub fn new(
        puller: Puller,
        importers: ImporterSet,
        reporter: Arc<dyn Reporter>,
        staging_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            puller,
            importers,
            reporter,
            staging_root: staging_root.into(),
        }
    }

    /// 处理一条 `image.pull` 下行 payload（JSON 字符串）。
    pub async fn run(&self, payload_json: &str) -> Result<JobOutcome> {
        let payload = ImagePullPayload::from_json(payload_json)?;
        let chunk_count = payload
            .chunk_count()
            .context("image.pull payload 字段不合法")?;
        if payload
            .presign_endpoint
            .as_deref()
            .is_some_and(|endpoint| endpoint != crate::presign::DEFAULT_PRESIGN_PATH)
        {
            tracing::debug!(
                transfer_id = %payload.transfer_id,
                endpoint = ?payload.presign_endpoint,
                "下行 presign_endpoint 与 agent 配置不同，仍使用启动时的 mTLS 预签名路径"
            );
        }

        let staging_dir = self.staging_dir(&payload.transfer_id);
        let request = PullRequest {
            transfer_id: payload.transfer_id.clone(),
            manifest_digest: payload.manifest_digest.clone(),
            kind: payload.kind,
            format: payload.format,
            name: payload.name.clone(),
            version: payload.version.clone(),
            total_size: payload.total_size,
            chunk_count,
            staging_dir,
        };

        let pulled = match self
            .pull_with_progress(&payload.transfer_id, &request)
            .await
        {
            Ok(pulled) => pulled,
            Err(error) => return self.report_failure(&payload, error).await,
        };

        self.report_progress(
            &payload.transfer_id,
            ProgressEvent::new(Phase::Import, 90, pulled.bytes),
        )
        .await;

        let manifest = pulled.manifest.clone();
        let Some(importer) = self
            .importers
            .select(manifest.kind.as_str(), manifest.format.as_str())
        else {
            return self
                .report_failure(
                    &payload,
                    anyhow::anyhow!(
                        "没有可用的 importer（kind={} format={}）",
                        manifest.kind.as_str(),
                        manifest.format.as_str()
                    ),
                )
                .await;
        };

        let import_request = ImportRequest {
            kind: manifest.kind.as_str().to_owned(),
            format: manifest.format.as_str().to_owned(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            staging_dir: pulled.staging_dir.clone(),
            artifact_path: pulled.artifact_path.clone(),
            imported_name: imported_name(&manifest),
        };

        match importer.import(&import_request).await {
            Ok(result) => {
                self.report_progress(
                    &payload.transfer_id,
                    ProgressEvent::new(Phase::Import, 100, pulled.bytes),
                )
                .await;
                let completed = CompletedPayload::success(
                    &payload.transfer_id,
                    &payload.manifest_digest,
                    &result.imported_ref,
                );
                if let Err(error) = self.reporter.completed(&completed).await {
                    tracing::warn!(
                        transfer_id = %payload.transfer_id,
                        error = %error,
                        "transfer.completed 上报失败（消息已入 outbox，等待重放）"
                    );
                }
                tracing::info!(
                    transfer_id = %payload.transfer_id,
                    imported_ref = %result.imported_ref,
                    "image.pull 完成"
                );
                Ok(JobOutcome {
                    transfer_id: payload.transfer_id,
                    ok: true,
                    imported_ref: Some(result.imported_ref),
                    error: None,
                })
            }
            Err(error) => {
                self.report_failure(&payload, error.context("镜像导入失败"))
                    .await
            }
        }
    }

    /// 执行 pull，并把同步进度回调转成异步上报（带顺序保证的 channel）。
    async fn pull_with_progress(
        &self,
        transfer_id: &str,
        request: &PullRequest,
    ) -> Result<crate::pull::PulledImage> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<ProgressEvent>();
        let reporter = self.reporter.clone();
        let transfer_id_owned = transfer_id.to_owned();
        let forwarder = tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                let payload = ProgressPayload::new(&transfer_id_owned, event);
                if let Err(error) = reporter.progress(&payload).await {
                    tracing::warn!(
                        transfer_id = %transfer_id_owned,
                        error = %error,
                        "transfer.progress 上报失败（消息已入 outbox，等待重放）"
                    );
                }
            }
        });

        let result = self
            .puller
            .pull(request, &move |event| {
                let _ = sender.send(event);
            })
            .await;
        // pull 结束→闭包被丢弃→发送端关闭→转发任务退出
        let _ = forwarder.await;
        result
    }

    async fn report_progress(&self, transfer_id: &str, event: ProgressEvent) {
        let payload = ProgressPayload::new(transfer_id, event);
        if let Err(error) = self.reporter.progress(&payload).await {
            tracing::warn!(
                transfer_id,
                error = %error,
                "transfer.progress 上报失败（消息已入 outbox，等待重放）"
            );
        }
    }

    /// 上报失败并返回 `ok=false`；上报本身的错误只记日志（不能因此重投整条消息）。
    async fn report_failure(
        &self,
        payload: &ImagePullPayload,
        error: anyhow::Error,
    ) -> Result<JobOutcome> {
        let message = format!("{error:#}");
        tracing::error!(
            transfer_id = %payload.transfer_id,
            digest = %payload.manifest_digest,
            error = %message,
            "image.pull 失败"
        );
        let completed =
            CompletedPayload::failure(&payload.transfer_id, &payload.manifest_digest, &message);
        if let Err(report_error) = self.reporter.completed(&completed).await {
            tracing::warn!(
                transfer_id = %payload.transfer_id,
                error = %report_error,
                "transfer.completed(ok=false) 上报失败（消息已入 outbox，等待重放）"
            );
        }
        Ok(JobOutcome {
            transfer_id: payload.transfer_id.clone(),
            ok: false,
            imported_ref: None,
            error: Some(message),
        })
    }

    fn staging_dir(&self, transfer_id: &str) -> PathBuf {
        self.staging_root.join(sanitize_component(transfer_id))
    }
}

/// 导入后的本地名称/引用：Docker/OCI 用镜像引用，LXC 用名称，VM 用 `<name>-<version>`（卷名）。
pub fn imported_name(manifest: &Manifest) -> String {
    match manifest.kind {
        ImageKind::Docker | ImageKind::Oci => manifest
            .runtime
            .docker_image_ref
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}:{}", manifest.name, manifest.version)),
        ImageKind::LxcRootfs => manifest.name.clone(),
        ImageKind::VmDisk => format!("{}-{}", manifest.name, manifest.version),
    }
}

fn sanitize_component(raw: &str) -> String {
    let sanitized: String = raw
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "transfer".to_owned()
    } else {
        sanitized
    }
}

/// 便于 main 判断 staging 根目录是否可创建（启动自检）。
pub fn ensure_staging_root(root: &Path) -> Result<()> {
    std::fs::create_dir_all(root)
        .with_context(|| format!("创建 staging 根目录失败: {}", root.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::ObjectCache;
    use crate::importer::TopologyImporter;
    use crate::manifest::ImageFormat;
    use crate::pull::Puller;
    use crate::pull::test_util::{Fixture, MockFetcher, MockPresign, fixture, raw_fixture_for_job};
    use crate::report::test_support::RecordingReporter;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockImporter {
        requests: Mutex<Vec<ImportRequest>>,
        artifact_exists_at_import: Mutex<Vec<bool>>,
        fail: bool,
    }

    impl TopologyImporter for MockImporter {
        fn supports(&self, kind: &str, format: &str) -> bool {
            kind == "vm_disk" && format == "raw"
        }

        fn import<'a>(
            &'a self,
            request: &'a ImportRequest,
        ) -> crate::BoxFuture<'a, Result<crate::importer::ImportResult>> {
            Box::pin(async move {
                self.requests.lock().unwrap().push(request.clone());
                self.artifact_exists_at_import
                    .lock()
                    .unwrap()
                    .push(request.artifact_path.exists());
                if self.fail {
                    bail!("mock 导入失败");
                }
                Ok(crate::importer::ImportResult {
                    imported_ref: format!("mock:{}", request.imported_name),
                })
            })
        }
    }

    struct Harness {
        _dir: tempfile::TempDir,
        job: ImagePullJob,
        reporter: Arc<RecordingReporter>,
        importer: Arc<MockImporter>,
        fixture: Fixture,
    }

    fn harness(
        fixture: Fixture,
        importer: Arc<MockImporter>,
    ) -> (tempfile::TempDir, ImagePullJob, Arc<RecordingReporter>) {
        let fixture = Arc::new(fixture);
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(ObjectCache::new(dir.path().join("cache"), 1 << 30));
        let presign = Arc::new(MockPresign::new(fixture.clone()));
        let fetcher = Arc::new(MockFetcher::new(&fixture));
        let puller = Puller::new(presign, fetcher, cache, 2);
        let reporter = Arc::new(RecordingReporter::default());
        let job = ImagePullJob::new(
            puller,
            ImporterSet::new(vec![importer]),
            reporter.clone(),
            dir.path().join("staging"),
        );
        (dir, job, reporter)
    }

    fn build(fixture: Fixture, importer: Arc<MockImporter>) -> Harness {
        let (dir, job, reporter) = harness(fixture.clone(), importer.clone());
        Harness {
            _dir: dir,
            job,
            reporter,
            importer,
            fixture,
        }
    }

    fn payload_json(fixture: &Fixture) -> String {
        serde_json::json!({
            "transfer_id": "transfer-1",
            "image_id": "image-1",
            "name": fixture.manifest.name,
            "version": fixture.manifest.version,
            "kind": fixture.manifest.kind.as_str(),
            "format": fixture.manifest.format.as_str(),
            "platform": fixture.manifest.platform,
            "manifest_digest": fixture.digest,
            "total_size": fixture.manifest.total_size,
            "chunk_count": fixture.manifest.chunks.len(),
            "presign_endpoint": "/resources/presign",
        })
        .to_string()
    }

    #[tokio::test]
    async fn success_reports_progress_and_completed() {
        let fixture = raw_fixture_for_job();
        let importer = Arc::new(MockImporter::default());
        let harness = build(fixture, importer);
        let outcome = harness
            .job
            .run(&payload_json(&harness.fixture))
            .await
            .unwrap();

        assert!(outcome.ok);
        assert_eq!(outcome.transfer_id, "transfer-1");
        assert_eq!(outcome.imported_ref.as_deref(), Some("mock:demo-1.0"));
        assert_eq!(outcome.error, None);

        let progress = harness.reporter.progress_events();
        assert!(
            progress
                .iter()
                .any(|payload| payload.phase == Phase::Download && payload.progress == 0),
            "{progress:?}"
        );
        assert!(
            progress
                .iter()
                .any(|payload| payload.phase == Phase::Verify && payload.progress == 90),
            "{progress:?}"
        );
        assert_eq!(
            progress
                .iter()
                .filter(|payload| payload.phase == Phase::Import)
                .map(|payload| payload.progress)
                .collect::<Vec<_>>(),
            vec![90, 100],
            "导入阶段 90 → 100"
        );

        let completed = harness.reporter.completed_events();
        assert_eq!(completed.len(), 1);
        let completed = &completed[0];
        assert!(completed.ok);
        assert!(completed.verified);
        assert_eq!(completed.error, None);
        assert_eq!(completed.imported_ref.as_deref(), Some("mock:demo-1.0"));
        assert_eq!(completed.manifest_digest, harness.fixture.digest);

        let requests = harness.importer.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.kind, "vm_disk");
        assert_eq!(request.format, "raw");
        assert_eq!(request.name, "demo");
        assert_eq!(request.version, "1.0");
        assert_eq!(request.imported_name, "demo-1.0");
        assert_eq!(
            request.staging_dir,
            harness.job.staging_dir("transfer-1"),
            "staging 目录应按 transfer 隔离"
        );
        assert_eq!(
            *harness.importer.artifact_exists_at_import.lock().unwrap(),
            vec![true]
        );
    }

    #[tokio::test]
    async fn pull_failure_reports_completed_false() {
        let fixture = raw_fixture_for_job();
        let importer = Arc::new(MockImporter::default());
        // 换钥 → pull 验签失败（在 import 之前）
        let fixture_arc = Arc::new(fixture.clone());
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(ObjectCache::new(dir.path(), 1 << 30));
        let presign = Arc::new(
            MockPresign::new(fixture_arc.clone()).with_public_key(base64_public_key_other()),
        );
        let fetcher = Arc::new(MockFetcher::new(&fixture_arc));
        let puller = Puller::new(presign, fetcher, cache, 1);
        let reporter = Arc::new(RecordingReporter::default());
        let job = ImagePullJob::new(
            puller,
            ImporterSet::new(vec![importer.clone()]),
            reporter.clone(),
            dir.path().join("staging"),
        );

        let outcome = job.run(&payload_json(&fixture)).await.unwrap();
        assert!(!outcome.ok);
        assert!(outcome.imported_ref.is_none());
        assert!(outcome.error.unwrap().contains("验签"));

        assert!(
            importer.requests.lock().unwrap().is_empty(),
            "失败时不应调用 importer"
        );
        let completed = reporter.completed_events();
        assert_eq!(completed.len(), 1);
        assert!(!completed[0].ok);
        assert!(!completed[0].verified);
        assert_eq!(completed[0].imported_ref, None);
        assert!(completed[0].error.as_deref().unwrap().contains("验签"));
        assert_eq!(completed[0].manifest_digest, fixture.digest);
    }

    #[tokio::test]
    async fn unsupported_importer_reports_failure() {
        // 拉取能成功（docker_archive 不做 magic 校验），但没有 importer 认领。
        let fixture = fixture(
            ImageKind::Docker,
            ImageFormat::DockerArchive,
            vec![b"docker-archive".to_vec()],
        );
        let importer = Arc::new(MockImporter::default());
        let harness = build(fixture, importer);
        let outcome = harness
            .job
            .run(&payload_json(&harness.fixture))
            .await
            .unwrap();
        assert!(!outcome.ok);
        let completed = harness.reporter.completed_events();
        let error = completed[0].error.as_deref().unwrap();
        assert!(error.contains("importer"), "{error}");
        assert!(harness.importer.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn import_failure_reports_completed_false() {
        let fixture = raw_fixture_for_job();
        let importer = Arc::new(MockImporter {
            requests: Mutex::new(Vec::new()),
            artifact_exists_at_import: Mutex::new(Vec::new()),
            fail: true,
        });
        let harness = build(fixture, importer);
        let outcome = harness
            .job
            .run(&payload_json(&harness.fixture))
            .await
            .unwrap();
        assert!(!outcome.ok);
        assert!(outcome.error.as_deref().unwrap().contains("导入"));
        let completed = harness.reporter.completed_events();
        assert!(!completed[0].ok);
        assert!(completed[0].error.as_deref().unwrap().contains("导入"));
    }

    #[tokio::test]
    async fn malformed_payload_returns_error() {
        let fixture = raw_fixture_for_job();
        let importer = Arc::new(MockImporter::default());
        let harness = build(fixture, importer);
        let error = harness.job.run("{}").await.unwrap_err();
        assert!(error.to_string().contains("payload"), "{error}");
        // 无法关联 transfer，不上报
        assert!(harness.reporter.completed_events().is_empty());
    }

    #[test]
    fn payload_parses_contract_fields() {
        let fixture = raw_fixture_for_job();
        let payload =
            ImagePullPayload::from_json(&payload_json(&fixture)).expect("payload 应能解析");
        assert_eq!(payload.transfer_id, "transfer-1");
        assert_eq!(payload.kind, ImageKind::VmDisk);
        assert_eq!(payload.format, ImageFormat::Raw);
        assert_eq!(
            payload.chunk_count().unwrap(),
            fixture.manifest.chunks.len()
        );
        assert_eq!(
            payload.presign_endpoint.as_deref(),
            Some("/resources/presign")
        );
    }

    #[test]
    fn payload_rejects_zero_chunk_count() {
        let fixture = raw_fixture_for_job();
        let mut value: serde_json::Value = serde_json::from_str(&payload_json(&fixture)).unwrap();
        value["chunk_count"] = 0.into();
        let payload = ImagePullPayload::from_json(&value.to_string()).unwrap();
        assert!(payload.chunk_count().is_err());
    }

    #[test]
    fn imported_name_follows_kind() {
        let mut manifest = raw_fixture_for_job().manifest;
        assert_eq!(imported_name(&manifest), "demo-1.0");

        manifest.kind = ImageKind::Docker;
        manifest.runtime.docker_image_ref = Some("registry.example/demo:1".into());
        assert_eq!(imported_name(&manifest), "registry.example/demo:1");
        manifest.runtime.docker_image_ref = None;
        assert_eq!(imported_name(&manifest), "demo:1.0");

        manifest.kind = ImageKind::LxcRootfs;
        assert_eq!(imported_name(&manifest), "demo");
    }

    #[test]
    fn staging_dir_is_sanitized() {
        let fixture = raw_fixture_for_job();
        let importer = Arc::new(MockImporter::default());
        let harness = build(fixture, importer);
        assert_eq!(
            harness.job.staging_dir("../evil/id"),
            harness.job.staging_root.join(".._evil_id")
        );
    }

    fn base64_public_key_other() -> String {
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD;
        let key = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        STANDARD.encode(key.verifying_key().as_bytes())
    }
}
