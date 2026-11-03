//! LXC / Incus 导入适配器（§16.4）。
//!
//! 优先使用 Incus：先为该 rootfs tar 生成最小 `metadata.yaml` 元数据包，再
//! `{incus_bin} image import --alias <name> --reuse <metadata.tar> <artifact_path>`；
//! 当 `incus_bin` 未配置、指向的文件不存在或命令返回 “not found” 时，退回 plain LXC：
//! 用系统 `tar -xf` 把 rootfs 归档解包到 staging 目录，再原子 rename 到
//! `{lxc_rootfs_dir}/<imported_name>/rootfs`，避免留下半成品。

use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::future::BoxFuture;
use tokio::fs;
use tokio::process::Command;
use uuid::Uuid;

use crate::importer::{ImportRequest, ImportResult, TopologyImporter};

/// 错误信息里保留的 stderr/stdout 最大字节数（超出截断）。
const MAX_ERROR_OUTPUT: usize = 2048;

/// LXC rootfs 归档的导入器（Incus 优先，plain LXC 兜底）。
#[derive(Debug, Clone)]
pub struct IncusImporter {
    incus_bin: String,
    lxc_rootfs_dir: PathBuf,
    auto_start: bool,
}

impl IncusImporter {
    /// `incus_bin` 可为空或不存在（此时直接走 plain LXC）；`lxc_rootfs_dir` 默认 `/var/lib/lxc`。
    pub fn new(incus_bin: impl AsRef<str>, lxc_rootfs_dir: impl AsRef<Path>) -> Self {
        Self::with_auto_start(incus_bin, lxc_rootfs_dir, false)
    }

    /// 同 [`Self::new`]，并控制导入成功后是否 `incus launch` 启动实例。
    pub fn with_auto_start(
        incus_bin: impl AsRef<str>,
        lxc_rootfs_dir: impl AsRef<Path>,
        auto_start: bool,
    ) -> Self {
        Self {
            incus_bin: incus_bin.as_ref().trim().to_owned(),
            lxc_rootfs_dir: lxc_rootfs_dir.as_ref().to_path_buf(),
            auto_start,
        }
    }

    async fn import_inner(&self, request: &ImportRequest) -> Result<ImportResult> {
        if !self.supports(&request.kind, &request.format) {
            bail!(
                "IncusImporter 不支持 kind={} format={}",
                request.kind,
                request.format
            );
        }

        let name = request.imported_name.trim();
        if name.is_empty() {
            bail!("LXC 导入缺少 imported_name");
        }
        validate_container_name(name)?;

        if let Some(result) = self.try_incus(request, name).await? {
            return Ok(result);
        }

        tracing::info!(
            incus_bin = %self.incus_bin,
            "incus 不可用，退回 plain LXC rootfs 解包"
        );
        self.import_plain_lxc(request, name).await
    }

    /// 尝试 Incus 导入；返回 `None` 表示应退回 plain LXC。
    async fn try_incus(&self, request: &ImportRequest, name: &str) -> Result<Option<ImportResult>> {
        if !self.incus_available() {
            return Ok(None);
        }

        let bin = self.incus_bin.as_str();
        // Incus 需要 metadata tarball + rootfs tarball 两段式导入；rootfs 产物本身不带
        // metadata.yaml，这里在 staging 内合成一个最小元数据包。
        let metadata_tar = build_metadata_tarball(request, name).await?;
        let output = match Command::new(bin)
            .arg("image")
            .arg("import")
            .arg("--alias")
            .arg(name)
            .arg("--reuse")
            .arg(&metadata_tar)
            .arg(&request.artifact_path)
            .stdin(Stdio::null())
            .output()
            .await
        {
            Ok(output) => output,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(err).with_context(|| format!("无法启动 `{bin}`"));
            }
        };

        if output.status.success() {
            if self.auto_start {
                self.launch_instance(name, name).await?;
            }
            return Ok(Some(ImportResult {
                imported_ref: format!("incus:{name}"),
            }));
        }

        let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        if output.status.code() == Some(127) || stderr.contains("not found") {
            tracing::warn!(
                bin = %bin,
                stderr = %truncate_output(&output.stderr),
                "incus image import 报告命令不存在，退回 plain LXC"
            );
            return Ok(None);
        }

        Err(command_failure(bin, &output))
    }

    /// 导入成功后启动容器；已存在则 restart（幂等）。
    async fn launch_instance(&self, alias: &str, instance: &str) -> Result<()> {
        let bin = self.incus_bin.as_str();
        let output = Command::new(bin)
            .arg("launch")
            .arg(alias)
            .arg(instance)
            .stdin(Stdio::null())
            .output()
            .await
            .with_context(|| format!("无法启动 `{bin} launch`"))?;
        if output.status.success() {
            tracing::info!(instance, alias, "Incus 实例已启动");
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        if stderr.contains("already exists") {
            let restart = Command::new(bin)
                .arg("restart")
                .arg(instance)
                .stdin(Stdio::null())
                .output()
                .await
                .with_context(|| format!("无法启动 `{bin} restart`"))?;
            if restart.status.success() {
                tracing::info!(instance, "Incus 实例已存在，已 restart");
                return Ok(());
            }
            return Err(command_failure(bin, &restart));
        }
        Err(command_failure(bin, &output))
    }

    /// `incus_bin` 为裸命令名时交给 PATH；为路径时要求文件存在。
    fn incus_available(&self) -> bool {
        if self.incus_bin.is_empty() {
            return false;
        }
        let path = Path::new(&self.incus_bin);
        if path.components().count() > 1 {
            return path.is_file();
        }
        true
    }

    async fn import_plain_lxc(&self, request: &ImportRequest, name: &str) -> Result<ImportResult> {
        validate_container_name(name)?;

        fs::create_dir_all(&self.lxc_rootfs_dir)
            .await
            .with_context(|| format!("无法创建 LXC 根目录 {}", self.lxc_rootfs_dir.display()))?;

        let target = self.lxc_rootfs_dir.join(name);
        if fs::try_exists(&target).await.unwrap_or(false) {
            bail!("LXC 容器目录已存在：{}", target.display());
        }

        let staging = self
            .lxc_rootfs_dir
            .join(format!(".staging-{}", Uuid::new_v4()));
        let staging_rootfs = staging.join("rootfs");

        let outcome = async {
            fs::create_dir_all(&staging_rootfs)
                .await
                .with_context(|| format!("无法创建 staging 目录 {}", staging_rootfs.display()))?;
            extract_tar(&request.artifact_path, &staging_rootfs).await?;
            fs::rename(&staging, &target)
                .await
                .with_context(|| format!("无法把 staging 目录重命名为 {}", target.display()))?;
            Ok::<(), anyhow::Error>(())
        }
        .await;

        if let Err(err) = outcome {
            let _ = fs::remove_dir_all(&staging).await;
            return Err(err);
        }

        Ok(ImportResult {
            imported_ref: format!("lxc:{name}"),
        })
    }
}

impl TopologyImporter for IncusImporter {
    fn supports(&self, kind: &str, format: &str) -> bool {
        let kind = normalize(kind);
        let format = normalize(format);
        matches!(
            (kind.as_str(), format.as_str()),
            ("lxc_rootfs" | "lxc", "tar")
        )
    }

    fn import<'a>(&'a self, request: &'a ImportRequest) -> BoxFuture<'a, Result<ImportResult>> {
        Box::pin(self.import_inner(request))
    }
}

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

/// plain LXC 的目录名只允许单层组件，防止 `..`/分隔符逃逸出 `lxc_rootfs_dir`。
fn validate_container_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        bail!("非法的 LXC 容器名 `{name}`");
    }
    Ok(())
}

async fn extract_tar(artifact: &Path, destination: &Path) -> Result<()> {
    let bin = "tar";
    let args: [&OsStr; 4] = [
        OsStr::new("-xf"),
        artifact.as_os_str(),
        OsStr::new("-C"),
        destination.as_os_str(),
    ];
    let output = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .context("无法执行系统 `tar`（plain LXC 兜底需要 tar 命令）")?;

    if output.status.success() {
        return Ok(());
    }
    Err(command_failure(bin, &output))
}

/// 为 rootfs tar 生成 Incus 需要的最小 metadata tarball。
///
/// Incus Image Store 的导入接口要求 `metadata.yaml`（至少 architecture/creation_date），
/// 而我们的产物是纯 rootfs tar；这里在 `staging_dir` 内合成
/// `incus-metadata/metadata.yaml` 并打包为 `incus-metadata.tar`。
async fn build_metadata_tarball(request: &ImportRequest, name: &str) -> Result<PathBuf> {
    let meta_dir = request.staging_dir.join("incus-metadata");
    fs::create_dir_all(&meta_dir)
        .await
        .with_context(|| format!("无法创建 Incus metadata 目录 {}", meta_dir.display()))?;

    // Incus 使用 Rust 的 arch 命名（x86_64 / aarch64）即可。
    let arch = std::env::consts::ARCH;
    let description = yaml_escape(&format!(
        "{} {} (qe-node-agent)",
        name,
        if request.version.trim().is_empty() {
            ""
        } else {
            request.version.trim()
        }
    ));
    let metadata = format!(
        "architecture: {arch}\ncreation_date: {}\nproperties:\n  description: \"{description}\"\n  os: \"linux\"\n",
        chrono::Utc::now().timestamp()
    );
    let metadata_path = meta_dir.join("metadata.yaml");
    fs::write(&metadata_path, metadata)
        .await
        .with_context(|| format!("写入 {} 失败", metadata_path.display()))?;

    let metadata_tar = request.staging_dir.join("incus-metadata.tar");
    let output = Command::new("tar")
        .arg("-cf")
        .arg(&metadata_tar)
        .arg("-C")
        .arg(&meta_dir)
        .arg("metadata.yaml")
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| "无法启动 `tar` 生成 Incus metadata tarball")?;
    if !output.status.success() {
        return Err(command_failure("tar", &output));
    }
    Ok(metadata_tar)
}

fn yaml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn command_failure(bin: &str, output: &std::process::Output) -> anyhow::Error {
    let code = output
        .status
        .code()
        .map_or_else(|| "被信号终止".to_owned(), |code| code.to_string());
    let stderr = truncate_output(&output.stderr);
    let stdout = truncate_output(&output.stdout);
    let detail = if stderr.is_empty() { stdout } else { stderr };
    let detail = if detail.is_empty() {
        "（无输出）".to_owned()
    } else {
        detail
    };
    anyhow!("`{bin}` 失败（退出码 {code}）：{detail}")
}

fn truncate_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    if text.len() <= MAX_ERROR_OUTPUT {
        return text.to_owned();
    }
    let mut end = MAX_ERROR_OUTPUT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…（已截断）", &text[..end])
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs as std_fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;

    fn fake_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std_fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = std_fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std_fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn recorded_args(script: &Path) -> Vec<String> {
        let args_file = PathBuf::from(format!("{}.args", script.display()));
        std_fs::read_to_string(args_file)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn request(artifact: &Path, imported_name: &str) -> ImportRequest {
        ImportRequest {
            kind: "lxc_rootfs".to_owned(),
            format: "tar".to_owned(),
            name: "web".to_owned(),
            version: "1.0".to_owned(),
            staging_dir: artifact.parent().unwrap().to_path_buf(),
            artifact_path: artifact.to_path_buf(),
            imported_name: imported_name.to_owned(),
        }
    }

    /// 并行测试里 fork/exec 可能短暂继承脚本的写 fd，导致 Linux 返回 `ETXTBSY`；
    /// 这是测试夹具的竞态，仅对 ETXTBSY 做短重试（生产路径不重试）。
    async fn import_with_retry(
        importer: &impl TopologyImporter,
        request: &ImportRequest,
    ) -> anyhow::Result<ImportResult> {
        let mut last_error = None;
        for attempt in 0..8u32 {
            match importer.import(request).await {
                Ok(result) => return Ok(result),
                Err(error) => {
                    let retry = is_text_file_busy(&error) && attempt < 7;
                    last_error = Some(error);
                    if !retry {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        }
        Err(last_error.expect("重试循环至少执行一次"))
    }

    fn is_text_file_busy(error: &anyhow::Error) -> bool {
        error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::raw_os_error)
                == Some(26)
        })
    }

    /// 生成一个含 `etc/hostname` 与 `bin/sh` 的 rootfs tar。
    fn build_rootfs_tar(root: &Path, tar_path: &Path) {
        let source = root.join("source");
        std_fs::create_dir_all(source.join("etc")).unwrap();
        std_fs::create_dir_all(source.join("bin")).unwrap();
        std_fs::write(source.join("etc/hostname"), b"qe-test\n").unwrap();
        std_fs::write(source.join("bin/sh"), b"#!/bin/sh\n").unwrap();

        let status = std::process::Command::new("tar")
            .arg("-cf")
            .arg(tar_path)
            .arg("-C")
            .arg(&source)
            .arg(".")
            .status()
            .unwrap();
        assert!(status.success(), "生成测试 tar 失败");
    }

    #[test]
    fn supports_lxc_rootfs_tar() {
        let importer = IncusImporter::new("incus", "/var/lib/lxc");
        assert!(importer.supports("lxc_rootfs", "tar"));
        assert!(importer.supports("lxc-rootfs", "tar"));
        assert!(importer.supports("lxc", "tar"));
        assert!(!importer.supports("lxc_rootfs", "qcow2"));
        assert!(!importer.supports("docker", "docker_archive"));
        assert!(!importer.supports("vm_disk", "raw"));
    }

    #[tokio::test]
    async fn incus_image_import_passes_alias() {
        let temp = TempDir::new().unwrap();
        let script = fake_script(temp.path(), "incus", "printf '%s\\n' \"$@\" > \"$0.args\"");
        let artifact = temp.path().join("rootfs.tar");
        std_fs::write(&artifact, b"fake tar").unwrap();
        let lxc_dir = temp.path().join("lxc");

        let importer = IncusImporter::new(script.to_str().unwrap(), &lxc_dir);
        let import_request = request(&artifact, "web-1.0");
        let result = import_with_retry(&importer, &import_request).await.unwrap();

        assert_eq!(result.imported_ref, "incus:web-1.0");
        let args = recorded_args(&script);
        assert_eq!(args.len(), 7, "{args:?}");
        assert_eq!(args[0], "image");
        assert_eq!(args[1], "import");
        assert_eq!(args[2], "--alias");
        assert_eq!(args[3], "web-1.0");
        assert_eq!(args[4], "--reuse");
        assert!(
            args[5].ends_with("incus-metadata.tar"),
            "第五段应为 metadata tarball：{args:?}"
        );
        assert_eq!(args[6], artifact.to_string_lossy());
        // 走 incus 时不应创建 plain LXC 目录
        assert!(!lxc_dir.exists());
    }

    #[tokio::test]
    async fn missing_incus_falls_back_to_plain_lxc() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        std_fs::create_dir_all(&root).unwrap();
        let archive = temp.path().join("rootfs.tar");
        build_rootfs_tar(&root, &archive);
        let lxc_dir = temp.path().join("lxc");

        let missing = temp.path().join("missing-incus");
        let importer = IncusImporter::new(missing.to_str().unwrap(), &lxc_dir);
        let import_request = request(&archive, "web-1.0");
        let result = import_with_retry(&importer, &import_request).await.unwrap();

        assert_eq!(result.imported_ref, "lxc:web-1.0");
        let target = lxc_dir.join("web-1.0");
        assert!(target.join("rootfs").is_dir());
        assert_eq!(
            std_fs::read_to_string(target.join("rootfs/etc/hostname")).unwrap(),
            "qe-test\n"
        );
        assert!(target.join("rootfs/bin/sh").is_file());

        // staging 目录必须已被 rename 消耗掉
        let leftovers: Vec<String> = std_fs::read_dir(&lxc_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".staging-"))
            .collect();
        assert!(leftovers.is_empty(), "残留 staging：{leftovers:?}");
    }

    #[tokio::test]
    async fn command_not_found_falls_back_to_plain_lxc() {
        let temp = TempDir::new().unwrap();
        let script = fake_script(
            temp.path(),
            "incus",
            "echo 'incus: command not found' >&2\nexit 127",
        );
        let root = temp.path().join("root");
        std_fs::create_dir_all(&root).unwrap();
        let archive = temp.path().join("rootfs.tar");
        build_rootfs_tar(&root, &archive);
        let lxc_dir = temp.path().join("lxc");

        let importer = IncusImporter::new(script.to_str().unwrap(), &lxc_dir);
        let import_request = request(&archive, "bare");
        let result = import_with_retry(&importer, &import_request).await.unwrap();

        assert_eq!(result.imported_ref, "lxc:bare");
        assert!(lxc_dir.join("bare/rootfs/etc/hostname").is_file());
    }

    #[tokio::test]
    async fn incus_failure_other_than_not_found_is_reported() {
        let temp = TempDir::new().unwrap();
        let script = fake_script(
            temp.path(),
            "incus",
            "echo 'Error: permission denied' >&2\nexit 5",
        );
        let artifact = temp.path().join("rootfs.tar");
        std_fs::write(&artifact, b"tar").unwrap();
        let lxc_dir = temp.path().join("lxc");

        let importer = IncusImporter::new(script.to_str().unwrap(), &lxc_dir);
        let import_request = request(&artifact, "web-1.0");
        let err = match import_with_retry(&importer, &import_request).await {
            Ok(_) => panic!("incus 非 not found 失败不应兜底"),
            Err(err) => err,
        };
        let message = format!("{err:#}");
        assert!(message.contains("permission denied"), "{message}");
        assert!(message.contains("退出码 5"), "{message}");
        assert!(!lxc_dir.join("web-1.0").exists());
    }

    #[tokio::test]
    async fn invalid_container_name_is_rejected() {
        let temp = TempDir::new().unwrap();
        let artifact = temp.path().join("rootfs.tar");
        std_fs::write(&artifact, b"tar").unwrap();
        let lxc_dir = temp.path().join("lxc");

        let importer = IncusImporter::new("", &lxc_dir);
        for name in ["..", "a/b", ""] {
            let import_request = request(&artifact, name);
            let err = match import_with_retry(&importer, &import_request).await {
                Ok(_) => panic!("非法容器名 {name:?} 应失败"),
                Err(err) => err,
            };
            assert!(format!("{err:#}").contains("非法") || format!("{err:#}").contains("缺少"));
        }
        // 不能逃逸出 lxc_rootfs_dir
        assert!(!temp.path().join("a").exists());
    }
}
