//! cpu_ext collector: cache sizes from sysfs plus stepping/microcode/flags
//! from `/proc/cpuinfo`.
//!
//! Linux-only; other platforms report `Collected::unsupported`.

use crate::Options;
use crate::model::CpuExt;
use crate::section::Collected;

/// Capability flags kept in the snapshot, pre-sorted for stable output.
#[cfg(target_os = "linux")]
const WANTED_FLAGS: [&str; 9] = [
    "aes",
    "avx",
    "avx2",
    "avx512f",
    "hypervisor",
    "lm",
    "sse4_2",
    "svm",
    "vmx",
];

#[allow(unused_variables)]
pub(super) fn collect(opts: &Options) -> Collected<CpuExt> {
    #[cfg(target_os = "linux")]
    {
        collect_linux()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Collected::unsupported("cpu caches/flags only read on Linux")
    }
}

/// Read caches + `/proc/cpuinfo` fields. Every read is best-effort: only an
/// unreadable `/proc/cpuinfo` makes the whole domain unavailable.
#[cfg(target_os = "linux")]
fn collect_linux() -> Collected<CpuExt> {
    let info = match std::fs::read_to_string("/proc/cpuinfo") {
        Ok(s) => s,
        Err(_) => return Collected::unsupported("no /proc/cpuinfo"),
    };

    let mut ext = CpuExt::default();

    // `/proc/cpuinfo` repeats the keys per logical CPU; the first one wins.
    let mut flags_line: Option<&str> = None;
    for line in info.lines() {
        if ext.stepping.is_none() {
            ext.stepping = cpuinfo_value(line, "stepping").map(str::to_string);
        }
        if ext.microcode.is_none() {
            ext.microcode = cpuinfo_value(line, "microcode").map(str::to_string);
        }
        // x86 labels the line `flags`; ARM labels it `Features`.
        if flags_line.is_none()
            && let Some((key, _)) = line.split_once(':')
            && matches!(key.trim(), "flags" | "Features")
        {
            flags_line = Some(line);
        }
    }

    if let Some(line) = flags_line
        && let Some((_, raw)) = line.split_once(':')
    {
        let present: Vec<&str> = raw.split_whitespace().collect();
        ext.flags = WANTED_FLAGS
            .iter()
            .copied()
            .filter(|f| present.contains(f))
            .map(str::to_string)
            .collect();
    }

    let (l1, l2, l3) = read_caches();
    ext.l1_kib = l1;
    ext.l2_kib = l2;
    ext.l3_kib = l3;

    Collected::ok(ext)
}

/// Value of a `/proc/cpuinfo` key line, trimmed; `None` when the line is not
/// that key or the value is empty.
#[cfg(target_os = "linux")]
fn cpuinfo_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (k, v) = line.split_once(':')?;
    if k.trim() != key {
        return None;
    }
    let v = v.trim();
    (!v.is_empty()).then_some(v)
}

/// Sum per-level cache sizes from `cpu0`'s sysfs index dirs.
///
/// Only cpu0 is read: index dirs are per-core, so walking every CPU would
/// multiply the totals. Returns `(l1, l2, l3)`, each `None` when unavailable.
#[cfg(target_os = "linux")]
fn read_caches() -> (Option<u64>, Option<u64>, Option<u64>) {
    let base = std::path::Path::new("/sys/devices/system/cpu/cpu0/cache");
    let Ok(entries) = std::fs::read_dir(base) else {
        return (None, None, None);
    };
    // Sort so the "first" L3 entry is deterministic across boots.
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());

    let mut l1: Option<u64> = None;
    let mut l2: Option<u64> = None;
    let mut l3: Option<u64> = None;
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("index") {
            continue;
        }
        let dir = entry.path();
        let level = std::fs::read_to_string(dir.join("level"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok());
        let size = std::fs::read_to_string(dir.join("size"))
            .ok()
            .and_then(|s| parse_cache_kib(&s));
        let (Some(level), Some(size)) = (level, size) else {
            continue;
        };
        // `type` (Data/Instruction/Unified) is deliberately not filtered on:
        // L1 reports data and instruction entries separately and both count.
        match level {
            1 => l1 = Some(l1.unwrap_or(0).saturating_add(size)),
            2 => l2 = Some(l2.unwrap_or(0).saturating_add(size)),
            // L3 is shared by every core: keep one value, never sum it.
            3 => l3 = l3.or(Some(size)),
            _ => {}
        }
    }
    (l1, l2, l3)
}

/// Parse a sysfs cache size like `32K`, `1024K` or `8M` into KiB.
#[cfg(target_os = "linux")]
fn parse_cache_kib(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    let (num, mult) = if let Some(n) = raw.strip_suffix(['K', 'k']) {
        (n, 1)
    } else if let Some(n) = raw.strip_suffix(['M', 'm']) {
        (n, 1024)
    } else if let Some(n) = raw.strip_suffix(['G', 'g']) {
        (n, 1024 * 1024)
    } else {
        (raw, 1)
    };
    num.trim()
        .parse::<u64>()
        .ok()
        .map(|v| v.saturating_mul(mult))
}
