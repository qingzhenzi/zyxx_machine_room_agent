//! Installed-package inventory — detects the host's package manager and
//! counts installed packages, with a best-effort security-update count.
//!
//! Detection is by "which binary exists", in priority order: dpkg, rpm,
//! pacman, apk. Pending security updates are only computed where the tooling
//! makes it reliable; otherwise the field stays `None`.
use crate::Options;
use crate::model::PackageInfo;
use crate::section::Collected;

#[cfg(not(target_os = "linux"))]
pub(super) fn collect(_opts: &Options) -> Collected<PackageInfo> {
    Collected::unsupported("package inventory is Linux-only")
}

#[cfg(target_os = "linux")]
pub(super) fn collect(_opts: &Options) -> Collected<PackageInfo> {
    // (manager label, binary, args) in detection priority order.
    let attempts: [(&str, &str, &[&str]); 4] = [
        ("dpkg", "dpkg-query", &["-W", "-f=${Package}\n"]),
        ("rpm", "rpm", &["-qa", "--qf", "%{NAME}\n"]),
        ("pacman", "pacman", &["-Q"]),
        ("apk", "apk", &["info"]),
    ];

    for (manager, cmd, args) in attempts {
        // `None` = binary not in PATH; keep looking.
        let Some(count) = count_lines(cmd, args) else {
            continue;
        };
        let security_updates = if manager == "dpkg" {
            dpkg_security_updates()
        } else if manager == "rpm" {
            rpm_security_updates()
        } else {
            None
        };
        return Collected::ok(PackageInfo {
            manager: manager.to_string(),
            count,
            security_updates,
        });
    }

    Collected::unsupported("no known package manager")
}

/// Count non-empty stdout lines of `<cmd> <args...>`.
///
/// `None` means the binary is not in `PATH`; a non-zero exit still counts the
/// lines that were printed (package tools warn liberally).
#[cfg(target_os = "linux")]
fn count_lines(cmd: &str, args: &[&str]) -> Option<usize> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count(),
    )
}

/// Debian/Ubuntu: pending security updates.
///
/// Prefer `apt-get -s upgrade` (no network, no changes): count `Inst` lines
/// that reference a security pocket. Fall back to `apt list --upgradable`,
/// whose descriptions mention the security pocket. Anything uncertain is `None`.
#[cfg(target_os = "linux")]
fn dpkg_security_updates() -> Option<usize> {
    if let Some(out) = run("apt-get", &["-s", "upgrade"]) {
        let text = out;
        let hits = text
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                t.starts_with("Inst ") && (t.contains(".security") || t.contains("-security"))
            })
            .count();
        // A successful simulation always prints progress lines; if it printed
        // anything at all we trust the (possibly zero) `Inst` count.
        if !text.trim().is_empty() {
            return Some(hits);
        }
    }
    if let Some(out) = run("apt", &["list", "--upgradable"]) {
        let text = out;
        let hits = text
            .lines()
            .filter(|l| !l.trim().is_empty() && l.to_lowercase().contains("security"))
            .count();
        return Some(hits);
    }
    None
}

/// RHEL/Fedora: pending security advisories via `dnf updateinfo`.
///
/// Only reported when the output clearly enumerates advisories; anything
/// ambiguous (old dnf, no metadata, plugin disabled) is `None`.
#[cfg(target_os = "linux")]
fn rpm_security_updates() -> Option<usize> {
    let out = run("dnf", &["-q", "updateinfo", "list", "security"])?;
    let text = out;
    let hits = text
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            (t.contains("CVE-") || t.contains("RHSA-") || t.contains("ALSA-"))
                && !t.starts_with("Security:")
        })
        .count();
    // No recognisable advisory lines at all → parsing was not conclusive.
    if hits == 0 && !text.contains("CVE-") && !text.contains("RHSA-") {
        return None;
    }
    Some(hits)
}

/// Run a command and return its stdout, or `None` when it is absent/fails.
#[cfg(target_os = "linux")]
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() && out.stdout.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}
