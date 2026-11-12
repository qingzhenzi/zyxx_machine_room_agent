//! Firmware/BIOS posture: DMI identity, boot mode, Secure Boot, TPM and
//! full-disk-encryption detection.
//!
//! Secure Boot is read from efivarfs, which usually needs root; a failure
//! there degrades to `None` instead of failing the whole section.

use crate::Options;
use crate::model::Firmware;
use crate::section::Collected;

#[cfg(target_os = "linux")]
const DMI_DIR: &str = "/sys/class/dmi/id";
#[cfg(target_os = "linux")]
const SECUREBOOT_EFIVAR: &str =
    "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c";
#[cfg(target_os = "linux")]
const TPM_DIR: &str = "/sys/class/tpm/tpm0";

#[allow(unused_variables)]
pub(super) fn collect(opts: &Options) -> Collected<Firmware> {
    #[cfg(target_os = "linux")]
    {
        let firmware_type = if std::path::Path::new("/sys/firmware/efi").exists() {
            "uefi"
        } else {
            "bios"
        };
        let tpm_present = std::path::Path::new(TPM_DIR).exists();

        Collected::ok(Firmware {
            bios_vendor: read_trimmed(format!("{DMI_DIR}/bios_vendor")),
            bios_version: read_trimmed(format!("{DMI_DIR}/bios_version")),
            bios_date: read_trimmed(format!("{DMI_DIR}/bios_date")),
            firmware_type: Some(firmware_type.to_string()),
            secure_boot: secure_boot(),
            tpm_present: Some(tpm_present),
            tpm_version: if tpm_present { tpm_version() } else { None },
            disk_encryption: disk_encryption(),
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        Collected::unsupported("firmware posture is Linux-only for now")
    }
}

/// SecureBoot efivar: 4 LE attribute bytes + 1 value byte (1 = enabled).
/// efivarfs is typically root-only; `None` when unreadable or truncated.
#[cfg(target_os = "linux")]
fn secure_boot() -> Option<bool> {
    let bytes = std::fs::read(SECUREBOOT_EFIVAR).ok()?;
    bytes.get(4).map(|b| *b == 1)
}

/// `/sys/class/tpm/tpm0/tpm_version_major` reports `2` or `1`.
#[cfg(target_os = "linux")]
fn tpm_version() -> Option<String> {
    match read_trimmed(format!("{TPM_DIR}/tpm_version_major"))?.as_str() {
        "2" => Some("2.0".to_string()),
        "1" => Some("1.2".to_string()),
        _ => None,
    }
}

/// Look for any `lsblk` row whose TYPE column is `crypt` (LUKS/dm-crypt).
/// `None` when lsblk is missing or exits non-zero.
#[cfg(target_os = "linux")]
fn disk_encryption() -> Option<bool> {
    let out = std::process::Command::new("lsblk")
        .args(["-rno", "NAME,TYPE"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.lines()
            .any(|line| line.split_whitespace().nth(1) == Some("crypt")),
    )
}

#[cfg(target_os = "linux")]
fn read_trimmed(path: impl AsRef<std::path::Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
