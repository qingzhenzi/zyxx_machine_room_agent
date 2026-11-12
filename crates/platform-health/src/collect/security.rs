//! Security posture collector — listening sockets (`ss`), mandatory access
//! control (`getenforce`/SELinux), enabled autostart services and failed
//! systemd units.
//!
//! Everything degrades to empty/`None` when the underlying tool is absent:
//! a container or minimal chroot without systemd is still a valid host, so
//! this domain always returns `Collected::ok`.
use crate::Options;
use crate::model::{ListeningPort, SecurityInfo};
use crate::section::Collected;

#[cfg(not(target_os = "linux"))]
pub(super) fn collect(_opts: &Options) -> Collected<SecurityInfo> {
    Collected::unsupported("security posture via ss/systemctl is Linux-only")
}

#[cfg(target_os = "linux")]
pub(super) fn collect(_opts: &Options) -> Collected<SecurityInfo> {
    Collected::ok(SecurityInfo {
        listening_ports: listening_ports(),
        // Windows Security Center only; no Linux equivalent implemented.
        antivirus_healthy: None,
        autostart: autostart_units(),
        mac_status: mac_status(),
        failed_units: failed_units(),
    })
}

/// Listening sockets, preferring `ss -lntupH` (with owning process) and
/// falling back to `ss -lntuH` when `-p` is restricted to root.
#[cfg(target_os = "linux")]
fn listening_ports() -> Vec<ListeningPort> {
    let mut ports = parse_ss(&["-lntupH"]);
    if ports.is_empty() {
        ports = parse_ss(&["-lntuH"]);
    }
    ports.truncate(100);
    ports
}

/// Run `ss` with the given flags and parse each line into a [`ListeningPort`].
#[cfg(target_os = "linux")]
fn parse_ss(args: &[&str]) -> Vec<ListeningPort> {
    let Ok(out) = std::process::Command::new("ss").args(args).output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut ports = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // ss mixes families here, so the Netid column is present:
        // tcp LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:(("sshd",pid=612,fd=3))
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 5 {
            continue;
        }
        let proto = if tokens[0].starts_with("tcp") {
            "tcp"
        } else if tokens[0].starts_with("udp") {
            "udp"
        } else {
            continue;
        };
        // Local address is the 5th column; split the LAST ':' for the port so
        // IPv6 literals ([::]:22 or :::22) parse correctly.
        let endpoint = tokens[4];
        let Some((addr, port)) = endpoint.rsplit_once(':') else {
            continue;
        };
        let Ok(port) = port.parse::<u16>() else {
            continue;
        };
        let (process, pid) = parse_users_column(line);
        ports.push(ListeningPort {
            protocol: proto.to_string(),
            local_address: addr.to_string(),
            port,
            process,
            pid,
        });
    }
    ports
}

/// Extract `(process name, pid)` from a trailing `users:(("name",pid=N,...))`.
#[cfg(target_os = "linux")]
fn parse_users_column(line: &str) -> (Option<String>, Option<u32>) {
    let mut process = None;
    let mut pid = None;
    if let Some(idx) = line.find("users:((") {
        let rest = &line[idx + "users:((".len()..];
        if let Some(open) = rest.find('"')
            && let Some(close) = rest[open + 1..].find('"')
        {
            let name = &rest[open + 1..open + 1 + close];
            if !name.is_empty() {
                process = Some(name.to_string());
            }
        }
        if let Some(pidx) = rest.find("pid=") {
            let digits: String = rest[pidx + 4..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            pid = digits.parse::<u32>().ok();
        }
    }
    (process, pid)
}

/// Mandatory access control status: SELinux via `getenforce`, then the
/// kernel's `/sys/fs/selinux/enforce` flag, else unknown.
#[cfg(target_os = "linux")]
fn mac_status() -> Option<String> {
    if let Ok(out) = std::process::Command::new("getenforce").output() {
        let text = String::from_utf8_lossy(&out.stdout);
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_lowercase());
        }
    }
    // getenforce absent or silent — fall back to the raw kernel knob.
    if let Ok(flag) = std::fs::read_to_string("/sys/fs/selinux/enforce") {
        let flag = flag.trim();
        return match flag {
            "1" => Some("enforcing".to_string()),
            "0" => Some("permissive".to_string()),
            _ => None,
        };
    }
    None
}

/// Enabled systemd service units (autostart entries). Capped at 100.
#[cfg(target_os = "linux")]
fn autostart_units() -> Vec<String> {
    let out = match std::process::Command::new("systemctl")
        .args([
            "list-unit-files",
            "--type=service",
            "--state=enabled",
            "--no-legend",
            "--no-pager",
        ])
        .output()
    {
        Ok(out) => out,
        Err(_) => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .take(100)
        .collect()
}

/// Count of units systemd currently lists as failed.
#[cfg(target_os = "linux")]
fn failed_units() -> Option<u32> {
    let out = std::process::Command::new("systemctl")
        .args(["--no-legend", "--state=failed", "--no-pager", "list-units"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let count = text.lines().filter(|l| !l.trim().is_empty()).count() as u32;
    // e.g. "System has not been booted with systemd": unknown, not zero.
    if count == 0 && !out.status.success() && out.stdout.is_empty() {
        return None;
    }
    Some(count)
}
