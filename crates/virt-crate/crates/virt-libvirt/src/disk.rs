//! Disk image helpers: create qcow2/raw images and parse human sizes.
//!
//! Image creation shells out to `qemu-img` for qcow2 (getting the header and
//! metadata preallocation right by hand is not worth it); raw images are just
//! sparse files. Set `QE_QEMU_IMG` to use a non-default binary.

use std::path::Path;
use std::process::Command;

use virt_core::{VirtError, VirtResult};

/// Image format to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskFormat {
    Qcow2,
    Raw,
}

impl DiskFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Qcow2 => "qcow2",
            Self::Raw => "raw",
        }
    }

    pub fn parse(value: &str) -> VirtResult<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "qcow2" => Ok(Self::Qcow2),
            "raw" => Ok(Self::Raw),
            other => Err(VirtError::InvalidConfig(format!(
                "bad disk format {other:?} (expected qcow2|raw)"
            ))),
        }
    }

    /// Guess from the file extension; anything unknown is treated as qcow2.
    pub fn for_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref()
        {
            Some("raw" | "img") => Self::Raw,
            _ => Self::Qcow2,
        }
    }
}

/// Path of the `qemu-img` binary (`QE_QEMU_IMG` overrides).
pub fn qemu_img() -> String {
    std::env::var("QE_QEMU_IMG").unwrap_or_else(|_| "qemu-img".to_owned())
}

/// Create a new disk image at `path`.
///
/// Fails with [`VirtError::AlreadyExists`] rather than overwriting: silently
/// truncating a disk that holds a student's work would be unrecoverable.
pub fn create_disk(path: &Path, size_bytes: u64, format: DiskFormat) -> VirtResult<()> {
    if path.exists() {
        return Err(VirtError::AlreadyExists(path.display().to_string()));
    }
    if size_bytes == 0 {
        return Err(VirtError::InvalidConfig(
            "disk size must be greater than zero".to_owned(),
        ));
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }

    match format {
        DiskFormat::Qcow2 => {
            let output = Command::new(qemu_img())
                .args(["create", "-f", "qcow2", "-o", "preallocation=metadata"])
                .arg(path)
                .arg(size_bytes.to_string())
                .output()
                .map_err(|e| VirtError::BackendUnavailable(format!("{}: {e}", qemu_img())))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let detail = if stderr.trim().is_empty() { stdout } else { stderr };
                return Err(VirtError::Backend {
                    backend: "qemu-img",
                    message: detail.trim().to_owned(),
                });
            }
            Ok(())
        }
        DiskFormat::Raw => {
            let file = std::fs::File::create(path)?;
            file.set_len(size_bytes)?;
            Ok(())
        }
    }
}

/// Parse `20G`, `512MiB`, `1t`, or a bare byte count.
///
/// Binary units (1 KiB = 1024 bytes). A bare number is interpreted as bytes,
/// matching `qemu-img`'s own convention.
pub fn parse_size(value: &str) -> VirtResult<u64> {
    let value = value.trim();
    if value.is_empty() {
        return Err(VirtError::InvalidConfig("empty size".to_owned()));
    }
    let split = value
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(value.len());
    let (digits, suffix) = value.split_at(split);
    let number: u64 = digits.parse().map_err(|_| {
        VirtError::InvalidConfig(format!("bad size {value:?} (expected e.g. 20G)"))
    })?;
    let suffix = suffix.trim().to_ascii_lowercase();
    let suffix = suffix.strip_suffix("ib").unwrap_or(&suffix);
    let multiplier: u64 = match suffix {
        "" | "b" => 1,
        "k" => 1 << 10,
        "m" => 1 << 20,
        "g" => 1 << 30,
        "t" => 1 << 40,
        _ => {
            return Err(VirtError::InvalidConfig(format!(
                "bad size unit in {value:?} (expected K/M/G/T)"
            )))
        }
    };
    number
        .checked_mul(multiplier)
        .ok_or_else(|| VirtError::InvalidConfig(format!("size {value:?} is too large")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("20G").unwrap(), 20 << 30);
        assert_eq!(parse_size("512MiB").unwrap(), 512 << 20);
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("1t").unwrap(), 1 << 40);
        assert!(parse_size("").is_err());
        assert!(parse_size("12X").is_err());
        assert!(parse_size("-1G").is_err());
    }

    #[test]
    fn guesses_format_from_extension() {
        assert_eq!(DiskFormat::for_path(Path::new("/a/b.qcow2")), DiskFormat::Qcow2);
        assert_eq!(DiskFormat::for_path(Path::new("/a/b.raw")), DiskFormat::Raw);
        assert_eq!(DiskFormat::for_path(Path::new("/a/b.img")), DiskFormat::Raw);
        assert_eq!(DiskFormat::for_path(Path::new("/a/b")), DiskFormat::Qcow2);
    }

    #[test]
    fn refuses_to_overwrite() {
        let dir = std::env::temp_dir().join(format!("virt-disk-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("exists.qcow2");
        std::fs::write(&path, b"keep me").unwrap();
        let err = create_disk(&path, 1 << 20, DiskFormat::Qcow2).unwrap_err();
        assert!(matches!(err, VirtError::AlreadyExists(_)));
        assert_eq!(std::fs::read(&path).unwrap(), b"keep me");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn creates_raw_sparse_file() {
        let dir = std::env::temp_dir().join(format!("virt-raw-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("disk.raw");
        create_disk(&path, 8 << 20, DiskFormat::Raw).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 8 << 20);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
