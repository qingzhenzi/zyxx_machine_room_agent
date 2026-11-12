//! Serializable snapshot data model.
//!
//! Every field is `serde`-friendly so a snapshot can be shipped to the backend,
//! stored, diffed, or rendered without a second conversion layer.
//!
//! The model is the **shared contract** between collectors: each collector
//! fills exactly one domain struct (or returns nothing when unsupported), and
//! [`Snapshot`] aggregates them. Optional domains are `Option`-wrapped so a
//! feature-disabled or unsupported platform simply yields `None` instead of
//! failing the whole snapshot.

use serde::{Deserialize, Serialize};

use crate::section::SectionMeta;

/// Snapshot returned by [`crate::snapshot`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// Stable schema identifier. Bump the suffix on breaking changes.
    pub schema: String,
    /// Collection time in RFC 3339 (UTC).
    pub collected_at: String,
    /// Collection time as Unix seconds.
    pub collected_at_unix: u64,
    /// Stable machine identity.
    pub identity: Identity,
    /// Operating system and uptime facts.
    pub host: Host,
    /// Hardware identity from firmware (DMI/SMBIOS), when the platform exposes it.
    pub machine: Machine,
    /// Per-section collection timing and failure reasons.
    ///
    /// Lets a consumer distinguish "no Wi-Fi hardware" from "Wi-Fi section
    /// failed to read", and spot slow collectors in production.
    pub sections: Vec<SectionMeta>,

    pub cpu: Cpu,
    pub memory: Memory,
    pub swap: Swap,
    pub disks: Vec<Disk>,
    pub networks: Vec<Network>,
    /// Thermal / fan sensors, when the platform exposes them.
    pub components: Vec<Component>,

    // ---- Optional domains (present only when collected) ----
    /// Attached monitors. Always attempted via sysinfo.
    pub displays: Option<Vec<Display>>,
    /// Logged-in user sessions. Privacy-sensitive; see [`crate::Privacy`].
    pub users: Option<Vec<UserSession>>,
    /// GPU adapters (NVIDIA via nvml). Requires the `gpu` feature.
    pub gpus: Option<Vec<Gpu>>,
    /// Primary battery state.
    pub battery: Option<Battery>,
    /// Firmware/BIOS, Secure Boot, TPM and disk-encryption posture.
    pub firmware: Option<Firmware>,
    /// CPU cache sizes and virtualization extensions.
    pub cpu_ext: Option<CpuExt>,
    /// Physical RAM module inventory (SMBIOS). Often needs root.
    pub ram_slots: Option<Vec<RamSlot>>,
    /// EDAC / ECC memory error counters.
    pub edac: Option<Edac>,
    /// Visible Wi-Fi networks / link status.
    pub wifi: Option<Vec<WifiNetwork>>,
    /// Clock sync status and offset from the time source.
    pub clock: Option<ClockInfo>,
    /// SMART health attributes per physical disk. Requires `smart` + smartctl.
    pub smart: Option<Vec<SmartDisk>>,
    /// Security/compliance posture (listening ports, autostart, AV).
    pub security: Option<SecurityInfo>,
    /// Running containers (docker/podman) and VMs.
    pub containers: Option<ContainerInfo>,
    /// Installed package counts / recent updates.
    pub packages: Option<PackageInfo>,

    /// Present only when process collection was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processes: Option<Processes>,
    /// Verdict computed from the metrics above.
    pub health: super::health::HealthReport,
}

/// Stable, non-reversible device fingerprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    /// FNV-1a 64-bit hash (16 hex chars) of hostname + sorted MAC addresses.
    ///
    /// Stable across reboots and IP changes, changes on hardware replacement.
    /// Deliberately not cryptographic: it is an identifier, not a secret.
    pub fingerprint: String,
    pub hostname: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub long_os_version: Option<String>,
    pub kernel_version: Option<String>,
    pub distribution_id: Option<String>,
    pub arch: String,
    pub uptime_secs: u64,
    pub boot_time_unix: u64,
}

/// Vendor/model/serial data used for asset management.
///
/// All fields are `Option` because DMI strings are missing on many VMs, on
/// ARM macOS, and over some hypervisors.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Machine {
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub family: Option<String>,
    pub sku: Option<String>,
    /// Serial and asset tag are masked under [`crate::Privacy::Redacted`].
    pub serial: Option<String>,
    pub asset_tag: Option<String>,
    pub motherboard_vendor: Option<String>,
    pub motherboard_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cpu {
    pub brand: String,
    pub vendor: String,
    pub physical_cores: usize,
    pub logical_cores: usize,
    /// Nominal frequency in MHz.
    pub frequency_mhz: u64,
    /// Average usage since the previous refresh, 0..=100.
    pub usage_percent: f32,
    /// Per-logical-core usage, 0..=100.
    pub per_core_usage: Vec<f32>,
    pub load_average: LoadAverage,
    /// Whether hardware virtualization (VT-x/AMD-V) is exposed and enabled.
    ///
    /// `None` when the platform doesn't report it.
    pub virtualization: Option<Virtualization>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Virtualization {
    /// CPU advertises a hypervisor flag / VMX / SVM.
    pub supported: bool,
    /// Hypervisor is currently running (we are a guest).
    pub running_hypervisor: bool,
    /// Best-guest name, e.g. `kvm`, `vmware`, `hyperv`, `xen`.
    pub hypervisor: Option<String>,
}

/// CPU cache hierarchy and micro-architecture flags.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CpuExt {
    /// L1 data + instruction cache in KiB (summed across cores when reported).
    pub l1_kib: Option<u64>,
    pub l2_kib: Option<u64>,
    pub l3_kib: Option<u64>,
    /// CPU stepping and microcode revision, when reported.
    pub stepping: Option<String>,
    pub microcode: Option<String>,
    /// Selected capability flags (avx2, avx512, aes, sse4_2, ...), sorted.
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub free_bytes: u64,
    pub usage_percent: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Swap {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub usage_percent: f32,
}

/// One physical RAM module from SMBIOS Type 17.
///
/// Requires read access to the raw DMI table (usually root on Linux).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RamSlot {
    pub slot: Option<String>,
    pub size_bytes: Option<u64>,
    pub speed_mts: Option<u32>,
    /// `DDR3`, `DDR4`, `DDR5`, `LPDDR5`, ...
    pub memory_type: Option<String>,
    pub manufacturer: Option<String>,
    pub part_number: Option<String>,
    /// Serial is masked under [`crate::Privacy::Redacted`].
    pub serial: Option<String>,
    pub ecc: Option<bool>,
}

/// EDAC (Error Detection And Correction) counters for correctable/uncorrectable
/// memory errors. Linux only; the strongest software signal for failing RAM.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Edac {
    /// Total correctable (bit-flip, recovered) errors since boot.
    pub correctable_total: u64,
    /// Total uncorrectable (fatal, data-loss) errors since boot.
    pub uncorrectable_total: u64,
    /// Number of EDAC memory controllers discovered.
    pub controllers: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Disk {
    pub name: String,
    pub mount_point: String,
    pub file_system: String,
    /// `ssd`, `hdd`, `removable`, `network` or `unknown`.
    pub kind: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub usage_percent: f32,
    pub removable: bool,
    /// Filesystem was remounted read-only (usually after an I/O error).
    pub read_only: bool,
}

/// SMART health data for one physical disk. Requires `smartctl` in PATH.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartDisk {
    pub device: String,
    /// `PASSED`, `FAILED`, `UNKNOWN`, ...
    pub health: String,
    pub temperature_c: Option<f32>,
    /// Power-on hours, when reported.
    pub power_on_hours: Option<u64>,
    /// SSD percentage used / wear level, 0..=100.
    pub percentage_used: Option<f32>,
    /// Reallocated (bad) sector count — mechanical red flag.
    pub reallocated_sectors: Option<u64>,
    /// Whether the device supports SMART at all.
    pub supported: bool,
    /// Raw error text when smartctl could not parse a clean status.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Network {
    pub name: String,
    /// Masked when [`crate::Privacy::Redacted`] is used.
    pub mac: String,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    /// Negotiated link speed in Mbps (e.g. 1000 for gigabit). `None` if unknown.
    pub link_speed_mbps: Option<u64>,
    /// Interface state: `up`, `down`, `dormant`, `unknown`, ...
    pub state: String,
    /// Maximum transmission unit in bytes.
    pub mtu: Option<u64>,
}

/// A visible or connected Wi-Fi network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WifiNetwork {
    pub interface: String,
    /// Connected SSID, when associated. Masked under redaction.
    pub ssid: Option<String>,
    /// Signal strength in dBm (more negative = weaker). Typical range -30..-90.
    pub signal_dbm: Option<i32>,
    /// Link bitrate in Mbps.
    pub link_mbps: Option<f64>,
    /// Frequency band, e.g. `2.4GHz`, `5GHz`, `6GHz`.
    pub band: Option<String>,
    pub channel: Option<u32>,
    /// BSSID (AP MAC), masked under redaction.
    pub bssid: Option<String>,
    /// Whether this is the interface we are currently associated with.
    pub connected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Component {
    pub label: String,
    /// `None` when the sensor exists but returned no readable value.
    pub temperature_c: Option<f32>,
    pub max_c: Option<f32>,
    pub critical_c: Option<f32>,
    /// Fan speed in RPM, when this sensor is a fan rather than a thermometer.
    pub fan_rpm: Option<u32>,
}

/// One attached monitor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Display {
    pub name: Option<String>,
    /// Pixel resolution.
    pub width: u32,
    pub height: u32,
    /// Physical size in millimetres, when the EDID reports it.
    pub physical_width_mm: Option<u32>,
    pub physical_height_mm: Option<u32>,
    /// Refresh rate in Hz.
    pub refresh_rate: Option<u32>,
    pub is_primary: bool,
    /// Rotation in degrees (`0`, `90`, `180`, `270`).
    pub rotation: u32,
}

/// A logged-in user session. Privacy-sensitive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSession {
    /// Username. Masked under [`crate::Privacy::Redacted`].
    pub name: String,
    pub uid: Option<u32>,
    pub groups: Vec<String>,
    /// Whether this user currently has an active session (from `who`/utmp).
    pub logged_in: bool,
}

/// Primary battery state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Battery {
    /// Charge percentage 0..=100.
    pub charge_percent: Option<f32>,
    /// `charging`, `discharging`, `full`, `not_charging`, `unknown`.
    pub status: String,
    /// Health estimate as a percentage of design capacity.
    pub health_percent: Option<f32>,
    pub energy_full_wh: Option<f32>,
    pub energy_now_wh: Option<f32>,
    pub voltage_v: Option<f32>,
    /// Charge cycle count, when the firmware tracks it.
    pub cycle_count: Option<u32>,
    /// Chemistry, e.g. `Li-ion`, `Li-poly`.
    pub technology: Option<String>,
    /// Whether a battery is present at all (desktops report `false`).
    pub present: bool,
}

/// Firmware and platform-security posture.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Firmware {
    pub bios_vendor: Option<String>,
    pub bios_version: Option<String>,
    pub bios_date: Option<String>,
    /// UEFI vs legacy BIOS boot.
    pub firmware_type: Option<String>,
    pub secure_boot: Option<bool>,
    /// TPM is present and its version (`2.0`, `1.2`).
    pub tpm_present: Option<bool>,
    pub tpm_version: Option<String>,
    /// Whether system disks report full-disk encryption (LUKS/BitLocker).
    pub disk_encryption: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Processes {
    pub total: usize,
    /// Counts by coarse state (run, sleep, zombie, ...).
    pub by_state: std::collections::BTreeMap<String, usize>,
    /// Top processes by CPU usage (falls back to memory when CPU is idle).
    pub top: Vec<ProcessInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub status: String,
    /// Executable path. Masked under redaction for non-system processes.
    pub exe: Option<String>,
    /// Parent PID.
    pub parent: Option<u32>,
    /// Process start time as Unix seconds.
    pub start_time_unix: Option<u64>,
    /// Seconds since start.
    pub run_time_secs: Option<u64>,
    /// Cumulative bytes read/written, when the platform reports I/O.
    pub read_bytes: Option<u64>,
    pub write_bytes: Option<u64>,
    /// Owning user, when available.
    pub user: Option<String>,
}

/// Clock synchronisation status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClockInfo {
    /// Offset from the reference clock in milliseconds (negative = behind).
    pub offset_ms: Option<f64>,
    /// Whether an NTP/chrony/time service reports itself synchronised.
    pub synchronised: Option<bool>,
    /// Name of the reference source (e.g. `time.cloudflare.com`).
    pub source: Option<String>,
    /// How the value was obtained: `chrony`, `timedatectl`, `w32tm`.
    pub method: String,
}

/// Security/compliance posture. Opt-in; privacy-sensitive.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecurityInfo {
    /// Ports this host listens on, with owning process when known.
    pub listening_ports: Vec<ListeningPort>,
    /// Whether an antivirus/EDR reports healthy (Windows Security Center).
    pub antivirus_healthy: Option<bool>,
    /// Names of enabled autostart entries (systemd units, registry Run keys).
    pub autostart: Vec<String>,
    /// MAC/mandatory-access control status: `enforcing`, `permissive`, `disabled`.
    pub mac_status: Option<String>,
    /// Count of failed systemd units (Linux).
    pub failed_units: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListeningPort {
    pub protocol: String,
    pub local_address: String,
    pub port: u16,
    /// Owning process name/pid when resolvable.
    pub process: Option<String>,
    pub pid: Option<u32>,
}

/// Running containers and VMs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub docker_running: usize,
    pub docker_images: Option<usize>,
    pub podman_running: Option<usize>,
    /// VMs defined vs running (libvirt), when present.
    pub libvirt_total: Option<usize>,
    pub libvirt_running: Option<usize>,
}

/// Installed-package summary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackageInfo {
    pub manager: String,
    pub count: usize,
    /// Packages with security updates pending, when the manager reports it.
    pub security_updates: Option<usize>,
}

/// A GPU adapter (NVIDIA via nvml for now).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gpu {
    /// Adapter index.
    pub index: u32,
    pub name: String,
    /// Utilization 0..=100 (SM / compute).
    pub utilization_percent: Option<f32>,
    pub memory_total_bytes: Option<u64>,
    pub memory_used_bytes: Option<u64>,
    pub temperature_c: Option<f32>,
    pub power_draw_w: Option<f32>,
    pub fan_speed_percent: Option<f32>,
    pub driver_version: Option<String>,
}
