//! VM 磁盘导入适配器（§16.4）。
//!
//! `kind=vm_disk` + `format=qcow2|raw` 时，先按文件大小创建 libvirt 存储卷：
//! `{virsh_bin} vol-create-as <pool> <vol> <size> --format <fmt>`，
//! 再上传：`{virsh_bin} vol-upload <vol> <artifact_path> --pool <pool>`。
//!
//! pool 名与 `virsh` 路径由 [`VmImporter::new`] 注入（默认来自 `LIBVIRT_POOL` / `VIRSH_BIN`），
//! 便于测试时指向 fake 脚本。

use std::ffi::OsString;
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::future::BoxFuture;
use tokio::fs;
use tokio::process::Command;

use crate::importer::{ImportRequest, ImportResult, TopologyImporter};

/// 错误信息里保留的 stderr/stdout 最大字节数（超出截断）。
const MAX_ERROR_OUTPUT: usize = 2048;

/// libvirt 存储池磁盘导入器。
#[derive(Debug, Clone)]
pub struct VmImporter {
    virsh_bin: String,
    uri: String,
    pool: String,
}

impl VmImporter {
    /// `virsh_bin` 默认 `virsh`；`pool` 默认 `default`；不显式传 `--connect`。
    pub fn new(virsh_bin: impl AsRef<str>, pool: impl AsRef<str>) -> Self {
        Self::with_uri(virsh_bin, pool, "")
    }

    /// 显式指定 libvirt 连接 URI（推荐 `qemu:///system`，否则非 root 默认走 session 连接）。
    pub fn with_uri(
        virsh_bin: impl AsRef<str>,
        pool: impl AsRef<str>,
        uri: impl AsRef<str>,
    ) -> Self {
        Self {
            virsh_bin: virsh_bin.as_ref().trim().to_owned(),
            uri: uri.as_ref().trim().to_owned(),
            pool: pool.as_ref().trim().to_owned(),
        }
    }

    fn base_args(&self) -> Vec<OsString> {
        let mut args = Vec::new();
        if !self.uri.is_empty() {
            args.push(OsString::from("--connect"));
            args.push(OsString::from(self.uri.as_str()));
        }
        args
    }

    async fn import_inner(&self, request: &ImportRequest) -> Result<ImportResult> {
        if !self.supports(&request.kind, &request.format) {
            bail!(
                "VmImporter 不支持 kind={} format={}",
                request.kind,
                request.format
            );
        }

        let bin = self.virsh_bin.as_str();
        if bin.is_empty() {
            bail!("virsh（VIRSH_BIN）未配置");
        }
        let pool = self.pool.as_str();
        if pool.is_empty() {
            bail!("libvirt storage pool（LIBVIRT_POOL）未配置");
        }

        let format = normalize(&request.format);
        let volume = volume_name(request);
        validate_volume_name(&volume)?;

        let metadata = fs::metadata(&request.artifact_path)
            .await
            .with_context(|| format!("无法读取磁盘镜像 {}", request.artifact_path.display()))?;
        if !metadata.is_file() {
            bail!("磁盘镜像不是文件：{}", request.artifact_path.display());
        }
        let size = metadata.len();
        if size == 0 {
            bail!("磁盘镜像为空：{}", request.artifact_path.display());
        }
        let size = size.to_string();

        let mut create_args = self.base_args();
        create_args.extend([
            OsString::from("vol-create-as"),
            OsString::from(pool),
            OsString::from(volume.as_str()),
            OsString::from(&size),
            OsString::from("--format"),
            OsString::from(format.as_str()),
        ]);
        run(bin, &create_args)
            .await
            .with_context(|| format!("创建 libvirt 卷 {pool}/{volume} 失败"))?;

        let mut upload_args = self.base_args();
        upload_args.extend([
            OsString::from("vol-upload"),
            OsString::from(volume.as_str()),
            request.artifact_path.as_os_str().to_owned(),
            OsString::from("--pool"),
            OsString::from(pool),
        ]);
        run(bin, &upload_args)
            .await
            .with_context(|| format!("上传 libvirt 卷 {pool}/{volume} 失败"))?;

        tracing::debug!(pool, volume, format, "磁盘镜像导入完成");
        Ok(ImportResult {
            imported_ref: format!("libvirt:{pool}/{volume}"),
        })
    }
}

impl TopologyImporter for VmImporter {
    fn supports(&self, kind: &str, format: &str) -> bool {
        let kind = normalize(kind);
        let format = normalize(format);
        matches!(
            (kind.as_str(), format.as_str()),
            ("vm_disk" | "vm", "qcow2" | "raw")
        )
    }

    fn import<'a>(&'a self, request: &'a ImportRequest) -> BoxFuture<'a, Result<ImportResult>> {
        Box::pin(self.import_inner(request))
    }
}

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

/// 卷名：`<name>-<version>`（§16.4）；`name` 为空时退回 `imported_name`。
fn volume_name(request: &ImportRequest) -> String {
    let name = request.name.trim();
    let version = request.version.trim();
    if name.is_empty() {
        request.imported_name.trim().to_owned()
    } else if version.is_empty() {
        name.to_owned()
    } else {
        format!("{name}-{version}")
    }
}

fn validate_volume_name(volume: &str) -> Result<()> {
    if volume.is_empty() || volume.contains('/') || volume.contains('\0') {
        bail!("非法的 libvirt 卷名 `{volume}`");
    }
    Ok(())
}

async fn run(bin: &str, args: &[OsString]) -> Result<()> {
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
    use std::fs as std_fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;

    /// fake virsh：把每次调用的参数以 NUL 分隔写入 `<script>.args`。
    const RECORD_ARGS: &str = r#"{ printf 'CALL\0'; printf '%s\0' "$@"; } >> "$0.args""#;

    fn fake_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std_fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = std_fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std_fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn read_calls(script: &Path) -> Vec<Vec<String>> {
        let args_file = PathBuf::from(format!("{}.args", script.display()));
        let data = std_fs::read(args_file).unwrap();
        let mut calls: Vec<Vec<String>> = Vec::new();
        let mut current: Option<Vec<String>> = None;
        for chunk in data.split(|byte| *byte == 0) {
            let token = String::from_utf8_lossy(chunk).into_owned();
            if token == "CALL" {
                if let Some(call) = current.take() {
                    calls.push(call);
                }
                current = Some(Vec::new());
            } else if let Some(call) = current.as_mut()
                && !token.is_empty()
            {
                call.push(token);
            }
        }
        if let Some(call) = current {
            calls.push(call);
        }
        calls
    }

    fn request(artifact: &Path, format: &str) -> ImportRequest {
        ImportRequest {
            kind: "vm_disk".to_owned(),
            format: format.to_owned(),
            name: "web".to_owned(),
            version: "1.0".to_owned(),
            staging_dir: artifact.parent().unwrap().to_path_buf(),
            artifact_path: artifact.to_path_buf(),
            imported_name: "web-1.0".to_owned(),
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
    fn supports_vm_disk_formats() {
        let importer = VmImporter::new("virsh", "default");
        assert!(importer.supports("vm_disk", "qcow2"));
        assert!(importer.supports("vm_disk", "raw"));
        assert!(importer.supports("vm-disk", "qcow2"));
        assert!(!importer.supports("vm_disk", "tar"));
        assert!(!importer.supports("docker", "docker_archive"));
        assert!(!importer.supports("lxc_rootfs", "tar"));
    }

    #[tokio::test]
    async fn qcow2_creates_then_uploads_volume() {
        let temp = TempDir::new().unwrap();
        let script = fake_script(temp.path(), "virsh", RECORD_ARGS);
        let artifact = temp.path().join("disk.qcow2");
        let content = b"QFI\xfb fake qcow2 payload";
        std_fs::write(&artifact, content).unwrap();

        let importer = VmImporter::new(script.to_str().unwrap(), "default");
        let import_request = request(&artifact, "qcow2");
        let result = import_with_retry(&importer, &import_request).await.unwrap();

        assert_eq!(result.imported_ref, "libvirt:default/web-1.0");
        let calls = read_calls(&script);
        assert_eq!(calls.len(), 2, "应恰好调用 virsh 两次：{calls:?}");
        assert_eq!(
            calls[0],
            vec![
                "vol-create-as".to_owned(),
                "default".to_owned(),
                "web-1.0".to_owned(),
                content.len().to_string(),
                "--format".to_owned(),
                "qcow2".to_owned(),
            ]
        );
        assert_eq!(
            calls[1],
            vec![
                "vol-upload".to_owned(),
                "web-1.0".to_owned(),
                artifact.to_string_lossy().into_owned(),
                "--pool".to_owned(),
                "default".to_owned(),
            ]
        );
    }

    #[tokio::test]
    async fn raw_uses_raw_format() {
        let temp = TempDir::new().unwrap();
        let script = fake_script(temp.path(), "virsh", RECORD_ARGS);
        let artifact = temp.path().join("disk.raw");
        std_fs::write(&artifact, b"raw bytes").unwrap();

        let importer = VmImporter::new(script.to_str().unwrap(), "pool-a");
        let import_request = request(&artifact, "raw");
        let result = import_with_retry(&importer, &import_request).await.unwrap();

        assert_eq!(result.imported_ref, "libvirt:pool-a/web-1.0");
        let calls = read_calls(&script);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0][0], "vol-create-as");
        assert_eq!(calls[0][1], "pool-a");
        assert_eq!(calls[0][5], "raw");
        assert_eq!(calls[1][4], "pool-a");
    }

    #[tokio::test]
    async fn create_failure_returns_error_with_stderr() {
        let temp = TempDir::new().unwrap();
        let script = fake_script(
            temp.path(),
            "virsh",
            "echo 'vol-create-as: pool not found' >&2\nexit 9",
        );
        let artifact = temp.path().join("disk.qcow2");
        std_fs::write(&artifact, b"QFI\xfb").unwrap();

        let importer = VmImporter::new(script.to_str().unwrap(), "missing-pool");
        let import_request = request(&artifact, "qcow2");
        let err = match import_with_retry(&importer, &import_request).await {
            Ok(_) => panic!("vol-create-as 失败应返回 Err"),
            Err(err) => err,
        };
        let message = format!("{err:#}");
        assert!(message.contains("pool not found"), "{message}");
        assert!(message.contains("退出码 9"), "{message}");
    }

    #[tokio::test]
    async fn upload_failure_returns_error() {
        let temp = TempDir::new().unwrap();
        let script = fake_script(
            temp.path(),
            "virsh",
            "{ printf 'CALL\\0'; printf '%s\\0' \"$@\"; } >> \"$0.args\"\ncase \"$1\" in\n  vol-create-as) exit 0 ;;\n  vol-upload) echo 'upload: connection reset' >&2; exit 4 ;;\n  *) exit 2 ;;\nesac",
        );
        let artifact = temp.path().join("disk.qcow2");
        std_fs::write(&artifact, b"QFI\xfb").unwrap();

        let importer = VmImporter::new(script.to_str().unwrap(), "default");
        let import_request = request(&artifact, "qcow2");
        let err = match import_with_retry(&importer, &import_request).await {
            Ok(_) => panic!("vol-upload 失败应返回 Err"),
            Err(err) => err,
        };
        let message = format!("{err:#}");
        assert!(message.contains("connection reset"), "{message}");
        assert!(message.contains("退出码 4"), "{message}");
        assert!(message.contains("上传 libvirt 卷"), "{message}");
        assert_eq!(read_calls(&script).len(), 2);
    }
    #[tokio::test]
    async fn with_uri_prepends_connect_flag() {
        let temp = TempDir::new().unwrap();
        let script = fake_script(temp.path(), "virsh", RECORD_ARGS);
        let artifact = temp.path().join("disk.raw");
        std_fs::write(&artifact, b"raw bytes").unwrap();

        let importer = VmImporter::with_uri(script.to_str().unwrap(), "default", "qemu:///system");
        import_with_retry(&importer, &request(&artifact, "raw"))
            .await
            .unwrap();

        let calls = read_calls(&script);
        assert_eq!(calls[0][0], "--connect");
        assert_eq!(calls[0][1], "qemu:///system");
        assert_eq!(calls[0][2], "vol-create-as");
    }
}
