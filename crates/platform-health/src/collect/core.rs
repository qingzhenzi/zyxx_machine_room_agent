//! Core collectors built on `sysinfo` plus a few tiny sysfs/`/proc` helpers.
//!
//! These sections are always compiled and always attempted. Anything the
//! platform can't provide degrades to `None`/empty rather than an error.

use std::collections::BTreeMap;

use sysinfo::{
    Components, CpuRefreshKind, Disks, MemoryRefreshKind, Networks, ProcessRefreshKind,
    ProcessesToUpdate, RefreshKind, System, Users,
};

use crate::Options;
use crate::model::{
    Component, Cpu, Disk, Host, Identity, LoadAverage, Machine, Memory, Network, ProcessInfo,
    Processes, Swap, UserSession, Virtualization,
};
use crate::privacy::{Privacy, fingerprint};
use crate::section::Collected;

/// Handles to the shared `sysinfo` samplers, created once per snapshot.
pub(super) struct Samplers {
    pub sys: System,
    pub disks: Disks,
    pub networks: Networks,
    pub components: Components,
}

/// Create and prime the samplers.
///
/// CPU (and per-process) usage is a delta between two samples, so this performs
/// the first read, waits the minimum interval, then reads again.
pub(super) fn setup(opts: &Options) -> Samplers {
    let mut kinds = RefreshKind::nothing()
        .with_cpu(CpuRefreshKind::everything())
        .with_memory(MemoryRefreshKind::everything());
    if opts.include_processes {
        kinds = kinds.with_processes(ProcessRefreshKind::everything());
    }

    let mut sys = System::new_with_specifics(kinds);
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_cpu_all();
    if opts.include_processes {
        sys.refresh_processes(ProcessesToUpdate::All, true);
    }

    Samplers {
        sys,
        disks: Disks::new_with_refreshed_list(),
        networks: Networks::new_with_refreshed_list(),
        components: Components::new_with_refreshed_list(),
    }
}

pub(super) fn host() -> Host {
    Host {
        os_name: System::name(),
        os_version: System::os_version(),
        long_os_version: System::long_os_version(),
        kernel_version: System::kernel_version(),
        distribution_id: Some(System::distribution_id()).filter(|s| !s.is_empty()),
        arch: System::cpu_arch(),
        uptime_secs: System::uptime(),
        boot_time_unix: System::boot_time(),
    }
}

pub(super) fn identity(privacy: Privacy, hostname: Option<&str>, networks: &Networks) -> Identity {
    let mut macs: Vec<String> = networks
        .list()
        .values()
        .map(|n| n.mac_address().to_string())
        .filter(|m| m != "00:00:00:00:00:00")
        .collect();
    macs.sort();
    macs.dedup();

    let mut parts: Vec<&str> = Vec::with_capacity(macs.len() + 1);
    // UUID takes precedence: it survives hostname changes and network churn.
    let uuid = sysinfo::Product::uuid().filter(|u| !u.trim().is_empty());
    if let Some(uuid) = &uuid {
        parts.push(uuid);
    } else {
        parts.extend(hostname);
        parts.extend(macs.iter().map(String::as_str));
    }

    Identity {
        fingerprint: fingerprint(&parts),
        hostname: hostname.map(|h| privacy.hostname(h)),
    }
}

pub(super) fn machine(privacy: Privacy) -> Machine {
    let motherboard = sysinfo::Motherboard::new();
    Machine {
        vendor: clean_dmi(motherboard.as_ref().and_then(|m| m.vendor_name())),
        model: clean_dmi(sysinfo::Product::name()),
        family: clean_dmi(sysinfo::Product::family()),
        sku: clean_dmi(sysinfo::Product::stock_keeping_unit()),
        serial: clean_dmi(sysinfo::Product::serial_number()).map(|s| privacy.secret(&s)),
        asset_tag: clean_dmi(motherboard.as_ref().and_then(|m| m.asset_tag()))
            .map(|s| privacy.secret(&s)),
        motherboard_vendor: clean_dmi(motherboard.as_ref().and_then(|m| m.vendor_name())),
        motherboard_name: clean_dmi(motherboard.as_ref().and_then(|m| m.name())),
    }
}

/// Firmware likes to fill missing DMI fields with human-readable placeholders;
/// reporting them as real asset data is worse than reporting nothing.
pub(crate) fn clean_dmi(value: Option<String>) -> Option<String> {
    let value = value?;
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    const PLACEHOLDERS: [&str; 8] = [
        "to be filled by o.e.m.",
        "default string",
        "base board asset tag",
        "chassis asset tag",
        "not specified",
        "none",
        "unknown",
        "system serial number",
    ];
    let lower = value.to_ascii_lowercase();
    if PLACEHOLDERS.contains(&lower.as_str()) {
        None
    } else {
        Some(value.to_string())
    }
}

pub(super) fn cpu(sys: &System) -> Cpu {
    let first = sys.cpus().first();
    let cpus = sys.cpus();
    let load = System::load_average();
    Cpu {
        brand: first
            .map(|c| c.brand().trim().to_string())
            .unwrap_or_default(),
        vendor: first
            .map(|c| c.vendor_id().trim().to_string())
            .unwrap_or_default(),
        physical_cores: System::physical_core_count().unwrap_or(cpus.len()),
        logical_cores: cpus.len(),
        frequency_mhz: first.map(|c| c.frequency()).unwrap_or(0),
        usage_percent: round1(sys.global_cpu_usage()),
        per_core_usage: cpus.iter().map(|c| round1(c.cpu_usage())).collect(),
        load_average: LoadAverage {
            one: round2(load.one),
            five: round2(load.five),
            fifteen: round2(load.fifteen),
        },
        virtualization: virtualization(),
    }
}

/// Detect hardware virtualization. Linux reads `/proc/cpuinfo` flags; other
/// platforms return `None` (no portable sysinfo API for this).
fn virtualization() -> Option<Virtualization> {
    #[cfg(target_os = "linux")]
    {
        let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
        let flags_line = info.lines().find(|l| l.starts_with("flags"))?;
        let flags: Vec<&str> = flags_line.split(':').nth(1)?.split_whitespace().collect();
        let supported = flags.iter().any(|f| *f == "vmx" || *f == "svm");
        let running = flags.contains(&"hypervisor");
        // On a hypervisor the guest CPUID advertises a vendor hint.
        let hypervisor = if running {
            info.lines()
                .find(|l| l.starts_with("hypervisor"))
                .and_then(|_| {
                    // Best-effort: look for known guest brand strings.
                    for (needle, name) in [
                        ("KVM", "kvm"),
                        ("VMware", "vmware"),
                        ("Hyper-V", "hyperv"),
                        ("Xen", "xen"),
                        ("QEMU", "qemu"),
                        ("VirtualBox", "virtualbox"),
                    ] {
                        if info.contains(needle) {
                            return Some(name.to_string());
                        }
                    }
                    None
                })
        } else {
            None
        };
        Some(Virtualization {
            supported,
            running_hypervisor: running,
            hypervisor,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

pub(super) fn memory(sys: &System) -> Memory {
    let total = sys.total_memory();
    let used = sys.used_memory();
    Memory {
        total_bytes: total,
        used_bytes: used,
        available_bytes: sys.available_memory(),
        free_bytes: sys.free_memory(),
        usage_percent: percent(used, total),
    }
}

pub(super) fn swap(sys: &System) -> Swap {
    let total = sys.total_swap();
    let used = sys.used_swap();
    Swap {
        total_bytes: total,
        used_bytes: used,
        free_bytes: sys.free_swap(),
        usage_percent: percent(used, total),
    }
}

/// btrfs/exotic filesystems expose every subvolume as its own mount of the
/// same device. Report one entry per backing device (root mount preferred) so
/// both the inventory and the per-disk health rules stay meaningful.
pub(super) fn disks(disks: &Disks) -> Vec<Disk> {
    let mut list: Vec<&sysinfo::Disk> = disks.list().iter().collect();
    list.sort_by_key(|d| {
        let mount = d.mount_point().to_string_lossy().to_string();
        let rank = if mount == "/" || mount.len() == 3 {
            0
        } else {
            1
        };
        (rank, mount.len())
    });

    let mut seen = std::collections::HashSet::new();
    list.into_iter()
        .filter(|d| seen.insert(d.name().to_os_string()))
        .map(collect_disk)
        .collect()
}

fn collect_disk(disk: &sysinfo::Disk) -> Disk {
    let total = disk.total_space();
    let available = disk.available_space();
    Disk {
        name: disk.name().to_string_lossy().to_string(),
        mount_point: disk.mount_point().to_string_lossy().to_string(),
        file_system: disk.file_system().to_string_lossy().to_string(),
        kind: match disk.kind() {
            sysinfo::DiskKind::HDD => "hdd",
            sysinfo::DiskKind::SSD => "ssd",
            sysinfo::DiskKind::Unknown(_) => "unknown",
        }
        .to_string(),
        total_bytes: total,
        used_bytes: total.saturating_sub(available),
        available_bytes: available,
        usage_percent: percent(total.saturating_sub(available), total),
        removable: disk.is_removable(),
        read_only: disk.is_read_only(),
    }
}

pub(super) fn networks(privacy: Privacy, networks: &Networks) -> Vec<Network> {
    networks
        .list()
        .iter()
        .map(|(name, data)| {
            let mut ipv4 = Vec::new();
            let mut ipv6 = Vec::new();
            for net in data.ip_networks() {
                match net.addr {
                    std::net::IpAddr::V4(addr) => ipv4.push(addr.to_string()),
                    std::net::IpAddr::V6(addr) => ipv6.push(addr.to_string()),
                }
            }
            Network {
                name: name.to_string(),
                mac: privacy.mac(&data.mac_address().to_string()),
                ipv4,
                ipv6,
                rx_bytes: data.total_received(),
                tx_bytes: data.total_transmitted(),
                rx_packets: data.total_packets_received(),
                tx_packets: data.total_packets_transmitted(),
                link_speed_mbps: link_speed_mbps(name),
                state: data.operational_state().to_string(),
                mtu: Some(data.mtu()).filter(|m| *m > 0),
            }
        })
        .collect()
}

/// Negotiated link speed in Mbps. sysinfo doesn't expose this in 0.39, so read
/// `/sys/class/net/<iface>/speed` on Linux (Ethernet only; Wi-Fi returns -1).
fn link_speed_mbps(iface: &str) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let path = format!("/sys/class/net/{iface}/speed");
        let raw = std::fs::read_to_string(&path).ok()?;
        let v: i64 = raw.trim().parse().ok()?;
        if v > 0 { Some(v as u64) } else { None }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = iface;
        None
    }
}

/// Temperature sensors from sysinfo, plus fan sensors read straight from
/// Linux hwmon (sysinfo 0.39 doesn't surface fans).
pub(super) fn components(components: &Components) -> Vec<Component> {
    let mut list: Vec<Component> = components
        .list()
        .iter()
        .map(|c| Component {
            label: c.label().to_string(),
            temperature_c: c.temperature().map(round1),
            max_c: c.max(),
            critical_c: c.critical(),
            fan_rpm: None,
        })
        .collect();
    list.extend(hwmon_fans());
    list
}

/// Read `fanN_input` (+ optional `fanN_label`) from `/sys/class/hwmon`.
fn hwmon_fans() -> Vec<Component> {
    #[cfg(target_os = "linux")]
    {
        let mut fans = Vec::new();
        let Ok(entries) = std::fs::read_dir("/sys/class/hwmon") else {
            return fans;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            let chip = std::fs::read_to_string(dir.join("name"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| entry.file_name().to_string_lossy().to_string());
            // fan1_input, fan2_input, ...
            if let Ok(files) = std::fs::read_dir(&dir) {
                for f in files.flatten() {
                    let fname = f.file_name().to_string_lossy().to_string();
                    if !fname.starts_with("fan") || !fname.ends_with("_input") {
                        continue;
                    }
                    let idx = fname.trim_start_matches("fan").trim_end_matches("_input");
                    let rpm: u32 = std::fs::read_to_string(f.path())
                        .ok()
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(0);
                    if rpm == 0 {
                        continue;
                    }
                    let label = std::fs::read_to_string(dir.join(format!("fan{idx}_label")))
                        .map(|s| s.trim().to_string())
                        .unwrap_or_else(|_| format!("fan{idx}"));
                    fans.push(Component {
                        label: format!("{chip} {label}"),
                        temperature_c: None,
                        max_c: None,
                        critical_c: None,
                        fan_rpm: Some(rpm),
                    });
                }
            }
        }
        fans.sort_by(|a, b| a.label.cmp(&b.label));
        fans
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

/// User inventory (sysinfo) intersected with who is actually logged in (`who`).
pub(super) fn users(privacy: Privacy) -> Collected<Vec<UserSession>> {
    let users = Users::new_with_refreshed_list();
    if users.list().is_empty() {
        return Collected::unsupported("no user database on this platform");
    }

    // Names of users with an active session, from `who` (POSIX, always present
    // on Unix; absent on Windows where we fall back to "no one logged in").
    let logged_in = logged_in_users();

    let sessions = users
        .list()
        .iter()
        .map(|u| {
            let name = u.name().to_string();
            UserSession {
                name: privacy.hostname(&name),
                uid: u.id().to_string().parse().ok(),
                groups: u.groups().iter().map(|g| g.name().to_string()).collect(),
                logged_in: logged_in.iter().any(|n| n == &name),
            }
        })
        .collect();
    Collected::ok(sessions)
}

fn logged_in_users() -> Vec<String> {
    match std::process::Command::new("who").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

pub(super) fn processes(sys: &System, limit: usize, privacy: Privacy) -> Processes {
    // Uid -> username map for attributing processes (cheap, one pass).
    let uid_names: BTreeMap<String, String> = Users::new_with_refreshed_list()
        .list()
        .iter()
        .map(|u| (u.id().to_string(), u.name().to_string()))
        .collect();

    let mut top: Vec<ProcessInfo> = sys
        .processes()
        .values()
        .map(|p| {
            let du = p.disk_usage();
            ProcessInfo {
                pid: p.pid().as_u32(),
                name: p.name().to_string_lossy().to_string(),
                cpu_percent: round1(p.cpu_usage()),
                memory_bytes: p.memory(),
                status: p.status().to_string().to_lowercase(),
                exe: p.exe().map(|e| privacy.exe(e.to_string_lossy().as_ref())),
                parent: p.parent().map(|pp| pp.as_u32()),
                start_time_unix: Some(p.start_time()).filter(|t| *t > 0),
                run_time_secs: Some(p.run_time()).filter(|t| *t > 0),
                read_bytes: Some(du.total_read_bytes).filter(|b| *b > 0),
                write_bytes: Some(du.total_written_bytes).filter(|b| *b > 0),
                user: p
                    .user_id()
                    .and_then(|u| uid_names.get(&u.to_string()).cloned()),
            }
        })
        .collect();

    top.sort_by(|a, b| {
        b.cpu_percent
            .partial_cmp(&a.cpu_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.memory_bytes.cmp(&a.memory_bytes))
    });
    top.truncate(limit);

    let mut by_state: BTreeMap<String, usize> = BTreeMap::new();
    for p in sys.processes().values() {
        *by_state
            .entry(p.status().to_string().to_lowercase())
            .or_insert(0) += 1;
    }

    Processes {
        total: sys.processes().len(),
        by_state,
        top,
    }
}

pub(super) fn percent(part: u64, total: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    round1((part as f64 / total as f64 * 100.0) as f32)
}

pub(super) fn round1(v: f32) -> f32 {
    (v * 10.0).round() / 10.0
}

pub(super) fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
