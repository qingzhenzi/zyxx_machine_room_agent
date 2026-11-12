//! 平台层小工具：只做「读内核事实」这一件事，不含任何业务判断。
//!
//! 原则：优先读 `/proc`、`/sys`，不 shell 出去。少数只能用外部命令的
//! 场景（systemctl / nft / sshd -T）统一走 [`run`]，带超时、不经过 shell。

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 读取文本并 trim，文件不存在或无权限时返回 None。
pub fn read_trim(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn read_lines(path: impl AsRef<Path>) -> Vec<String> {
    fs::read_to_string(path)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

pub fn exists(path: impl AsRef<Path>) -> bool {
    Path::new(path.as_ref()).exists()
}

/// 文件权限位（含 setuid/setgid/sticky）。读不到返回 None。
pub fn mode(path: impl AsRef<Path>) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path.as_ref())
        .ok()
        .map(|m| m.permissions().mode() & 0o7777)
}

pub fn is_dir(path: impl AsRef<Path>) -> bool {
    fs::metadata(path.as_ref())
        .map(|m| m.is_dir())
        .unwrap_or(false)
}

/// 目录下的条目名（不递归），目录不存在返回空。
pub fn list_dir(path: impl AsRef<Path>) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(path.as_ref())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

pub fn count_dir(path: impl AsRef<Path>) -> usize {
    fs::read_dir(path.as_ref())
        .map(|rd| rd.filter_map(|e| e.ok()).count())
        .unwrap_or(0)
}

pub fn uname_release() -> String {
    read_trim("/proc/sys/kernel/osrelease").unwrap_or_else(|| "unknown".into())
}

pub fn hostname() -> String {
    read_trim("/proc/sys/kernel/hostname").unwrap_or_else(|| "unknown".into())
}

/// 读 sysctl，参数形如 `kernel.kptr_restrict`。
pub fn sysctl(key: &str) -> Option<String> {
    let path = format!("/proc/sys/{}", key.replace('.', "/"));
    read_trim(path)
}

/// 同名文件里的整数 sysctl，读不到或非数字返回 None。
pub fn sysctl_i64(key: &str) -> Option<i64> {
    sysctl(key).and_then(|v| v.parse().ok())
}

// ------------------------------------------------------------- 外部命令

pub struct CmdOut {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOut {
    pub fn ok(&self) -> bool {
        self.status == Some(0)
    }
}

/// 带超时执行命令。**不经过 shell**，参数逐个传递，天然没有注入面。
/// 超时或无法执行返回 None。
pub fn run(program: &str, args: &[&str], timeout: Duration) -> Option<CmdOut> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }

    let out = child.wait_with_output().ok()?;
    Some(CmdOut {
        status: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

// ------------------------------------------------------------- 磁盘 / 挂载

#[derive(Debug, Clone, Copy)]
pub struct DiskUsage {
    pub total: u64,
    pub free: u64,
    pub avail: u64,
    pub inodes_total: u64,
    pub inodes_free: u64,
}

/// `statvfs(2)`，按路径取文件系统用量。
pub fn disk_usage(path: &str) -> Option<DiskUsage> {
    let c = CString::new(path).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let bs = st.f_frsize as u64;
    Some(DiskUsage {
        total: st.f_blocks as u64 * bs,
        free: st.f_bfree as u64 * bs,
        avail: st.f_bavail as u64 * bs,
        inodes_total: st.f_files as u64,
        inodes_free: st.f_ffree as u64,
    })
}

#[derive(Debug, Clone)]
pub struct Mount {
    pub source: String,
    pub target: String,
    pub fstype: String,
    pub options: Vec<String>,
}

impl Mount {
    pub fn has_opt(&self, name: &str) -> bool {
        self.options.iter().any(|o| o == name)
    }
}

/// 解析 `/proc/self/mountinfo`。比 `mount` 输出可靠：不经本地化、含 superblock 信息。
pub fn mounts() -> Vec<Mount> {
    fs::read_to_string("/proc/self/mountinfo")
        .map(|text| text.lines().filter_map(parse_mountinfo).collect())
        .unwrap_or_default()
}

fn parse_mountinfo(line: &str) -> Option<Mount> {
    let (pre, post) = line.split_once(" - ")?;
    let f: Vec<&str> = pre.split_whitespace().collect();
    let p: Vec<&str> = post.split_whitespace().collect();
    if f.len() < 6 || p.len() < 3 {
        return None;
    }
    Some(Mount {
        source: unescape_octal(p[1]),
        target: unescape_octal(f[4]),
        fstype: p[0].to_string(),
        options: f[5].split(',').map(str::to_string).collect(),
    })
}

/// mountinfo 里空格等字符是八进制转义的（`\040` = 空格）。
fn unescape_octal(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let oct = &s[i + 1..i + 4];
            if let Ok(v) = u8::from_str_radix(oct, 8) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ------------------------------------------------------------- 进程

/// 读 `/proc/<pid>/status` 的某个字段。
pub fn proc_status(pid: u32, key: &str) -> Option<String> {
    let text = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(':')))
        .map(|v| v.trim().to_string())
}

/// 当前所有进程的 `(pid, comm)`。
pub fn pids() -> Vec<(u32, String)> {
    list_dir("/proc")
        .into_iter()
        .filter_map(|name| name.parse::<u32>().ok())
        .filter_map(|pid| {
            let comm = read_trim(format!("/proc/{pid}/comm"))?;
            Some((pid, comm))
        })
        .collect()
}

/// 读取内核编译配置。优先发行版放的 `/boot/config-<rel>`；只有 `/proc/config.gz`
/// 时退到 `zcat`，避免为解压引入 gzip 依赖。
pub fn kernel_config() -> Option<String> {
    let rel = uname_release();
    for path in [
        format!("/boot/config-{rel}"),
        format!("/usr/lib/modules/{rel}/config"),
        format!("/lib/modules/{rel}/config"),
    ] {
        if let Ok(text) = fs::read_to_string(&path) {
            return Some(text);
        }
    }
    let out = run("zcat", &["/proc/config.gz"], Duration::from_secs(10))?;
    if out.ok() && !out.stdout.is_empty() {
        Some(out.stdout)
    } else {
        None
    }
}

// ------------------------------------------------------------- 展示辅助

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} {}", UNITS[0])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// part/total 的百分比，total 为 0 时返回 0。
pub fn pct(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 * 100.0 / total as f64
    }
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// UTC 时间戳（RFC3339 风格），避免为格式化引入 chrono。
pub fn format_utc(secs: i64) -> String {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = secs as libc::time_t;
    let ok = unsafe { libc::gmtime_r(&t, &mut tm) };
    if ok.is_null() {
        return "unknown".into();
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// 变量名 → 值 的简易解析（用于 /etc/os-release 等）。
pub fn parse_kv_file(path: impl AsRef<Path>) -> BTreeMap<String, String> {
    read_lines(path)
        .into_iter()
        .filter(|l| !l.trim_start().starts_with('#') && l.contains('='))
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((
                k.trim().to_string(),
                v.trim().trim_matches('"').trim_matches('\'').to_string(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn octal_escapes_in_mountinfo_are_decoded() {
        assert_eq!(unescape_octal("/mnt/my\\040disk"), "/mnt/my disk");
        assert_eq!(unescape_octal("/plain"), "/plain");
    }

    #[test]
    fn mountinfo_line_is_split_correctly() {
        let line = "36 35 98:0 / /mnt/data rw,noatime master:1 - ext4 /dev/sda1 rw";
        let m = parse_mountinfo(line).expect("应能解析");
        assert_eq!(m.target, "/mnt/data");
        assert_eq!(m.fstype, "ext4");
        assert_eq!(m.source, "/dev/sda1");
        assert!(m.has_opt("noatime"));
    }

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MiB");
    }

    #[test]
    fn pct_avoids_division_by_zero() {
        assert_eq!(pct(1, 0), 0.0);
        assert!((pct(1, 4) - 25.0).abs() < f64::EPSILON);
    }

    #[test]
    fn epoch_formats_as_utc() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
    }
}
