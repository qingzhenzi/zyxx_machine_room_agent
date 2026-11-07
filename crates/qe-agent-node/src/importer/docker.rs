//! Docker / OCI 导入适配器（§16.4）。
//!
//! - `format=docker_archive`：`{docker_bin} load -i <artifact_path>`；
//! - `format=oci_layout`：`{ctr_bin} images import <artifact_path>`。
//!
//! 命令路径由 [`DockerImporter::new`] 注入（默认来自 `DOCKER_BIN` / `CTR_BIN`），便于测试时指向 fake 脚本。

use std::ffi::OsStr;
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::future::BoxFuture;
use tokio::process::Command;

use crate::importer::{ImportRequest, ImportResult, TopologyImporter};

/// 错误信息里保留的 stderr/stdout 最大字节数（超出截断）。
const MAX_ERROR_OUTPUT: usize = 2048;

/// Docker / OCI 镜像导入器。
#[derive(Debug, Clone)]
pub struct DockerImporter {
    docker_bin: String,
    ctr_bin: String,
}

impl DockerImporter {
    /// `docker_bin` 用于 `docker_archive`，`ctr_bin` 用于 `oci_layout`。
    pub fn new(docker_bin: impl AsRef<str>, ctr_bin: impl AsRef<str>) -> Self {
        Self {
            docker_bin: docker_bin.as_ref().trim().to_owned(),
            ctr_bin: ctr_bin.as_ref().trim().to_owned(),
        }
    }

    async fn import_inner(&self, request: &ImportRequest) -> Result<ImportResult> {
        if !self.supports(&request.kind, &request.format) {
            bail!(
                "DockerImporter 不支持 kind={} format={}",
                request.kind,
                request.format
            );
        }

        let format = normalize(&request.format);
        let bin = match format.as_str() {
            "docker_archive" => {
                require_bin(&self.docker_bin, "docker（DOCKER_BIN）")?;
                let args: [&OsStr; 3] = [
                    OsStr::new("load"),
                    OsStr::new("-i"),
                    request.artifact_path.as_os_str(),
                ];
                run(&self.docker_bin, &args).await?;
                &self.docker_bin
            }
            "oci_layout" => {
                require_bin(&self.ctr_bin, "ctr（CTR_BIN）")?;
                let args: [&OsStr; 3] = [
                    OsStr::new("images"),
                    OsStr::new("import"),
                    request.artifact_path.as_os_str(),
                ];
                run(&self.ctr_bin, &args).await?;
                &self.ctr_bin
            }
            other => bail!("DockerImporter 不支持的 format `{other}`"),
        };

        tracing::debug!(bin = %bin, format = %format, "镜像导入完成");
        Ok(ImportResult {
            imported_ref: imported_ref(request),
        })
    }
}

impl TopologyImporter for DockerImporter {
    fn supports(&self, kind: &str, format: &str) -> bool {
        let kind = normalize(kind);
        let format = normalize(format);
        matches!(
            (kind.as_str(), format.as_str()),
            ("docker", "docker_archive") | ("oci", "oci_layout")
        )
    }

    fn import<'a>(&'a self, request: &'a ImportRequest) -> BoxFuture<'a, Result<ImportResult>> {
        Box::pin(self.import_inner(request))
    }
}

/// 本地引用：`docker:<imported_name>`；`imported_name` 为空时退回 `<name>:<version>`。
fn imported_ref(request: &ImportRequest) -> String {
    let imported_name = request.imported_name.trim();
    if !imported_name.is_empty() {
        return format!("docker:{imported_name}");
    }

    let name = request.name.trim();
    let version = request.version.trim();
    if version.is_empty() {
        format!("docker:{name}")
    } else {
        format!("docker:{name}:{version}")
    }
}

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

fn require_bin(bin: &str, what: &str) -> Result<()> {
    if bin.is_empty() {
        bail!("{what} 未配置");
    }
    Ok(())
}

async fn run(bin: &str, args: &[&OsStr]) -> Result<()> {
    let output = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("无法启动 `{bin}`"))?;

    if output.status.success() {
        return Ok(());
    }
    Err(command_failure(bin, &output))
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
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;

    fn fake_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn recorded_args(script: &Path) -> Vec<String> {
        let args_file = PathBuf::from(format!("{}.args", script.display()));
        fs::read_to_string(args_file)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn request(artifact: &Path, kind: &str, format: &str, imported_name: &str) -> ImportRequest {
        ImportRequest {
            kind: kind.to_owned(),
            format: format.to_owned(),
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

    #[test]
    fn supports_docker_and_oci_pairs() {
        let importer = DockerImporter::new("docker", "ctr");
        assert!(importer.supports("docker", "docker_archive"));
        assert!(importer.supports("oci", "oci_layout"));
        // 大小写与分隔符归一化
        assert!(importer.supports("Docker", "docker-archive"));
        assert!(importer.supports("OCI", "oci-layout"));
        // kind/format 必须成对
        assert!(!importer.supports("docker", "oci_layout"));
        assert!(!importer.supports("oci", "docker_archive"));
        assert!(!importer.supports("lxc_rootfs", "tar"));
        assert!(!importer.supports("vm_disk", "qcow2"));
    }

    #[tokio::test]
    async fn docker_archive_runs_docker_load() {
        let temp = TempDir::new().unwrap();
        let script = fake_script(temp.path(), "docker", "printf '%s\\n' \"$@\" > \"$0.args\"");
        let artifact = temp.path().join("image.tar");
        fs::write(&artifact, b"fake docker archive").unwrap();

        let importer = DockerImporter::new(script.to_str().unwrap(), "/nonexistent/ctr");
        let import_request = request(&artifact, "docker", "docker_archive", "web:1.0");
        let result = import_with_retry(&importer, &import_request).await.unwrap();

        assert_eq!(result.imported_ref, "docker:web:1.0");
        assert_eq!(
            recorded_args(&script),
            vec![
                "load".to_owned(),
                "-i".to_owned(),
                artifact.to_string_lossy().into_owned(),
            ]
        );
    }

    #[tokio::test]
    async fn oci_layout_runs_ctr_images_import() {
        let temp = TempDir::new().unwrap();
        let script = fake_script(temp.path(), "ctr", "printf '%s\\n' \"$@\" > \"$0.args\"");
        let artifact = temp.path().join("oci");
        fs::create_dir(&artifact).unwrap();

        let importer = DockerImporter::new("/nonexistent/docker", script.to_str().unwrap());
        let import_request = request(&artifact, "oci", "oci_layout", "web-1.0");
        let result = import_with_retry(&importer, &import_request).await.unwrap();

        assert_eq!(result.imported_ref, "docker:web-1.0");
        assert_eq!(
            recorded_args(&script),
            vec![
                "images".to_owned(),
                "import".to_owned(),
                artifact.to_string_lossy().into_owned(),
            ]
        );
    }

    #[tokio::test]
    async fn failing_command_returns_error_with_stderr() {
        let temp = TempDir::new().unwrap();
        let script = fake_script(
            temp.path(),
            "docker",
            "echo 'load: not a valid archive' >&2\nexit 7",
        );
        let artifact = temp.path().join("image.tar");
        fs::write(&artifact, b"broken").unwrap();

        let importer = DockerImporter::new(script.to_str().unwrap(), "ctr");
        let import_request = request(&artifact, "docker", "docker_archive", "web:1.0");
        let err = match import_with_retry(&importer, &import_request).await {
            Ok(_) => panic!("docker load 应失败"),
            Err(err) => err,
        };
        let message = format!("{err:#}");
        assert!(message.contains("not a valid archive"), "{message}");
        assert!(message.contains("退出码 7"), "{message}");
    }

    #[tokio::test]
    async fn unsupported_kind_format_is_rejected() {
        let importer = DockerImporter::new("docker", "ctr");
        let temp = TempDir::new().unwrap();
        let artifact = temp.path().join("image.tar");
        fs::write(&artifact, b"x").unwrap();

        let import_request = request(&artifact, "docker", "oci_layout", "web:1.0");
        let err = match import_with_retry(&importer, &import_request).await {
            Ok(_) => panic!("不支持的 kind/format 应失败"),
            Err(err) => err,
        };
        assert!(format!("{err:#}").contains("不支持"));
    }
}
