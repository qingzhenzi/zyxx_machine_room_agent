//! VM configuration loading: parse a config file, apply rules, define it on a
//! backend.

use std::path::Path;

use crate::{VirtError, VirtResult};

/// Source format of a VM configuration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigFormat {
    /// libvirt domain XML.
    #[default]
    LibvirtXml,
    /// Simple INI/TOML-like key=value description (see [`VmSpec`]).
    VmSpec,
}

impl ConfigFormat {
    /// Guess the format from the first non-blank character.
    ///
    /// XML documents start with `<`; everything else is treated as `key=value`.
    pub fn detect(contents: &str) -> Self {
        if contents.trim_start().starts_with('<') {
            Self::LibvirtXml
        } else {
            Self::VmSpec
        }
    }

    /// Parse `contents` in this format.
    pub fn parse(self, contents: &str) -> VirtResult<VmSpec> {
        match self {
            Self::LibvirtXml => parse_domain_xml(contents),
            Self::VmSpec => parse_spec(contents),
        }
    }
}

/// Graphic console backend requested for a VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayKind {
    /// VNC server inside QEMU, port auto-allocated.
    #[default]
    Vnc,
    /// SPICE server inside QEMU.
    Spice,
    /// No graphic console (headless; use the serial console instead).
    None,
}

impl DisplayKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vnc => "vnc",
            Self::Spice => "spice",
            Self::None => "none",
        }
    }

    pub fn parse(s: &str) -> VirtResult<Self> {
        match s.trim() {
            "vnc" => Ok(Self::Vnc),
            "spice" => Ok(Self::Spice),
            "none" | "headless" => Ok(Self::None),
            other => Err(VirtError::InvalidConfig(format!(
                "bad display {other:?} (expected vnc|spice|none)"
            ))),
        }
    }
}

/// CPU acceleration mode for the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Accel {
    /// KVM when `/dev/kvm` is usable on this host, TCG emulation otherwise.
    #[default]
    Auto,
    /// Require KVM; the define succeeds but starting fails on hosts without it.
    Kvm,
    /// Force TCG emulation (slow, but works inside VMs without nested virt).
    Tcg,
}

impl Accel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Kvm => "kvm",
            Self::Tcg => "tcg",
        }
    }

    pub fn parse(s: &str) -> VirtResult<Self> {
        match s.trim() {
            "auto" => Ok(Self::Auto),
            "kvm" => Ok(Self::Kvm),
            "tcg" | "qemu" => Ok(Self::Tcg),
            other => Err(VirtError::InvalidConfig(format!(
                "bad accel {other:?} (expected auto|kvm|tcg)"
            ))),
        }
    }
}

/// Which device the firmware tries first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BootDevice {
    /// First hard disk — the normal case for an installed system.
    #[default]
    Hd,
    /// CD-ROM — used while installing from an ISO.
    Cdrom,
}

impl BootDevice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hd => "hd",
            Self::Cdrom => "cdrom",
        }
    }

    pub fn parse(s: &str) -> VirtResult<Self> {
        match s.trim() {
            "hd" | "disk" => Ok(Self::Hd),
            "cdrom" | "cd" => Ok(Self::Cdrom),
            other => Err(VirtError::InvalidConfig(format!(
                "bad boot device {other:?} (expected hd|cdrom)"
            ))),
        }
    }
}

/// Graphic console settings rendered into the domain definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayConfig {
    pub kind: DisplayKind,
    /// Optional VNC/SPICE password. `None` means no authentication.
    pub password: Option<String>,
    /// Listen address. `None` keeps the backend default (libvirt: 127.0.0.1).
    pub listen: Option<String>,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            kind: DisplayKind::Vnc,
            password: None,
            listen: None,
        }
    }
}

impl DisplayConfig {
    /// True when the console is reachable from outside the host.
    pub fn is_public(&self) -> bool {
        match self.listen.as_deref() {
            None | Some("127.0.0.1") | Some("localhost") | Some("::1") => false,
            Some(_) => true,
        }
    }
}

/// A PCI address (`dddd:bb:dd.f`), the identity of a passthrough device.
///
/// Parsing is strict: well-formed hex, bus/slot must fit in a byte, function in
/// `0..=7`. This is the same spelling libvirt uses in `<hostdev><source>`
/// `<address domain='0xdddd' bus='0xbb' slot='0xdd' function='0xf'/>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PciAddress {
    pub domain: u16,
    pub bus: u8,
    pub slot: u8,
    pub function: u8,
}

impl PciAddress {
    /// Parse `dddd:bb:dd.f` (hex). Surrounding whitespace and uppercase hex are
    /// accepted; the canonical spelling is lowercase with 4/2/2/1 digits.
    pub fn parse(raw: &str) -> VirtResult<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(VirtError::InvalidConfig("empty PCI address".into()));
        }
        let mut parts = trimmed.split(':');
        let (Some(domain), Some(bus), Some(rest), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(VirtError::InvalidConfig(format!(
                "PCI address {trimmed:?} must be dddd:bb:dd.f"
            )));
        };
        let (slot, function) = rest.split_once('.').ok_or_else(|| {
            VirtError::InvalidConfig(format!(
                "PCI address {trimmed:?} missing `.function` (expected dddd:bb:dd.f)"
            ))
        })?;
        if function.contains('.') {
            return Err(VirtError::InvalidConfig(format!(
                "PCI address {trimmed:?} has too many `.` separators"
            )));
        }
        let domain = parse_hex_component(domain, "domain", 0xffff, trimmed)? as u16;
        let bus = parse_hex_component(bus, "bus", 0xff, trimmed)? as u8;
        let slot = parse_hex_component(slot, "slot", 0xff, trimmed)? as u8;
        let function = parse_hex_component(function, "function", 0x7, trimmed)? as u8;
        Ok(Self {
            domain,
            bus,
            slot,
            function,
        })
    }

    /// Canonical lowercase spelling (`dddd:bb:dd.f`).
    pub fn as_string(&self) -> String {
        format!(
            "{:04x}:{:02x}:{:02x}.{:x}",
            self.domain, self.bus, self.slot, self.function
        )
    }
}

impl std::fmt::Display for PciAddress {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.as_string())
    }
}

fn parse_hex_component(raw: &str, name: &str, max: u32, full: &str) -> VirtResult<u32> {
    if raw.is_empty() || raw.len() > 4 {
        return Err(VirtError::InvalidConfig(format!(
            "PCI address {full:?}: bad {name} {raw:?}"
        )));
    }
    if !raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(VirtError::InvalidConfig(format!(
            "PCI address {full:?}: {name} {raw:?} is not hex"
        )));
    }
    let value = u32::from_str_radix(raw, 16).map_err(|_| {
        VirtError::InvalidConfig(format!("PCI address {full:?}: bad {name} {raw:?}"))
    })?;
    if value > max {
        return Err(VirtError::InvalidConfig(format!(
            "PCI address {full:?}: {name} {raw:?} out of range 0x0..=0x{max:x}"
        )));
    }
    Ok(value)
}

/// A physical GPU passed through to a VM (`<hostdev type='pci'>`).
///
/// Phase 1 only models the PCI address: the host must already have the device
/// bound to `vfio-pci` and the IOMMU group isolated. See
/// `docs/gpu-passthrough.md`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GpuPassthrough {
    /// PCI address in `dddd:bb:dd.f` form.
    pub pci_address: String,
}

impl GpuPassthrough {
    /// Build from a PCI address, validating the format.
    pub fn new(pci_address: impl Into<String>) -> VirtResult<Self> {
        let value = pci_address.into();
        let parsed = PciAddress::parse(&value)?;
        Ok(Self {
            pci_address: parsed.as_string(),
        })
    }

    /// The parsed PCI address.
    pub fn address(&self) -> VirtResult<PciAddress> {
        PciAddress::parse(&self.pci_address)
    }
}

/// A parsed, backend-neutral VM definition.
#[derive(Debug, Clone, Default)]
pub struct VmSpec {
    /// Unique domain name (required).
    pub name: String,
    /// Number of vCPUs (default 1).
    pub vcpus: u32,
    /// Memory in KiB (default 256 MiB).
    pub memory_kib: u64,
    /// Disk image paths to attach.
    pub disks: Vec<String>,
    /// Physical GPUs passed through via `<hostdev mode='subsystem' type='pci'>`.
    /// Empty by default (backward compatible).
    pub gpus: Vec<GpuPassthrough>,
    /// Virtual NICs attached to the VM.
    pub nics: Vec<crate::net::NicConfig>,
    /// Graphic console settings.
    pub display: DisplayConfig,
    /// Optional CD-ROM image (usually an installer ISO). Attached read-only.
    pub cdrom: Option<String>,
    /// Device the firmware boots from first.
    pub boot: BootDevice,
    /// CPU acceleration to request.
    pub accel: Accel,
    /// Start this domain when the host boots.
    pub autostart: bool,
    /// Raw backend configuration (e.g. full libvirt XML) when the caller
    /// wants to bypass normalization.
    pub raw_xml: Option<String>,
}

/// Rules applied to a [`VmSpec`] before it is defined on a backend.
#[derive(Debug, Clone)]
pub struct LoadRules {
    /// Reject names not matching this glob-like pattern set (simple: `*` wildcard).
    pub allowed_name_patterns: Vec<String>,
    /// Maximum vCPUs permitted.
    pub max_vcpus: u32,
    /// Maximum memory in KiB.
    pub max_memory_kib: u64,
    /// Disk images must exist on the host filesystem.
    pub require_disks_exist: bool,
    /// Maximum number of attached disks (CD-ROM included).
    pub max_disks: usize,
    /// Maximum number of passthrough GPUs (default 2).
    pub max_gpus: usize,
    /// Reject a console bound to a non-loopback address without a password.
    pub require_display_auth: bool,
}

impl Default for LoadRules {
    fn default() -> Self {
        Self {
            allowed_name_patterns: vec!["*".into()],
            max_vcpus: 64,
            max_memory_kib: 64 * 1024 * 1024, // 64 GiB
            require_disks_exist: true,
            max_disks: 16,
            max_gpus: 2,
            require_display_auth: true,
        }
    }
}

impl LoadRules {
    /// Validate a spec against these rules.
    pub fn validate(&self, spec: &VmSpec) -> VirtResult<()> {
        if spec.name.is_empty() {
            return Err(VirtError::InvalidConfig("name is required".into()));
        }
        if !self
            .allowed_name_patterns
            .iter()
            .any(|p| glob_match(p, &spec.name))
        {
            return Err(VirtError::InvalidConfig(format!(
                "name {:?} not allowed by rules",
                spec.name
            )));
        }
        if spec.vcpus == 0 || spec.vcpus > self.max_vcpus {
            return Err(VirtError::InvalidConfig(format!(
                "vcpus {} out of range 1..={}",
                spec.vcpus, self.max_vcpus
            )));
        }
        if spec.memory_kib == 0 || spec.memory_kib > self.max_memory_kib {
            return Err(VirtError::InvalidConfig(format!(
                "memory {} KiB out of range 1..={}",
                spec.memory_kib, self.max_memory_kib
            )));
        }
        let disk_count = spec.disks.len() + usize::from(spec.cdrom.is_some());
        if disk_count > self.max_disks {
            return Err(VirtError::InvalidConfig(format!(
                "{disk_count} disks attached, at most {} allowed",
                self.max_disks
            )));
        }
        if self.require_disks_exist {
            for disk in spec.disks.iter().chain(spec.cdrom.iter()) {
                if !Path::new(disk).exists() {
                    return Err(VirtError::InvalidConfig(format!(
                        "disk image not found: {disk}"
                    )));
                }
            }
        }
        if spec.gpus.len() > self.max_gpus {
            return Err(VirtError::InvalidConfig(format!(
                "{} GPUs requested, at most {} allowed",
                spec.gpus.len(),
                self.max_gpus
            )));
        }
        for gpu in &spec.gpus {
            PciAddress::parse(&gpu.pci_address).map_err(|error| {
                VirtError::InvalidConfig(format!(
                    "invalid GPU pci_address {:?}: {error}",
                    gpu.pci_address
                ))
            })?;
        }
        if self.require_display_auth && spec.display.is_public() && spec.display.password.is_none()
        {
            return Err(VirtError::InvalidConfig(format!(
                "display listens on {:?} without a password; bind 127.0.0.1 or set a password",
                spec.display.listen.as_deref().unwrap_or("")
            )));
        }
        Ok(())
    }
}

/// Minimal `*` wildcard matcher (no external dependency).
fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    fn go(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some(a), Some(b)) if a == b => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    go(&p, &t)
}

/// Parse a config file's contents into a [`VmSpec`].
///
/// `VmSpec` format (line-based `key = value`):
///
/// ```text
/// name = demo-vm
/// vcpus = 2
/// memory_kib = 2097152
/// disk = /var/lib/vms/demo.qcow2
/// gpu = 0000:01:00.0
/// nic = virtio,bridge=br0,mac=52:54:00:12:34:56
/// display = vnc,listen=127.0.0.1
/// cdrom = /var/lib/vms/debian-13.iso
/// boot = cdrom
/// accel = auto
/// autostart = true
/// ```
///
/// `disk` and `nic` may repeat; whitespace around `=` is trimmed; `#` comments.
pub fn parse_spec(contents: &str) -> VirtResult<VmSpec> {
    let mut spec = VmSpec {
        vcpus: 1,
        memory_kib: 256 * 1024,
        ..Default::default()
    };
    for (i, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(VirtError::InvalidConfig(format!(
                "line {}: expected `key = value`, got {line:?}",
                i + 1
            )));
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "name" => spec.name = value.to_owned(),
            "vcpus" => {
                spec.vcpus = value.parse().map_err(|_| {
                    VirtError::InvalidConfig(format!("line {}: bad vcpus {value:?}", i + 1))
                })?
            }
            "memory_kib" | "memory" => {
                spec.memory_kib = value.parse().map_err(|_| {
                    VirtError::InvalidConfig(format!(
                        "line {}: bad memory {value:?} (expected an integer in KiB)",
                        i + 1
                    ))
                })?
            }
            "disk" => spec.disks.push(value.to_owned()),
            "gpu" => spec.gpus.push(GpuPassthrough::new(value).map_err(|error| {
                VirtError::InvalidConfig(format!("line {}: {error}", i + 1))
            })?),
            "cdrom" | "iso" => spec.cdrom = Some(value.to_owned()),
            "boot" => spec.boot = BootDevice::parse(value)?,
            "accel" => spec.accel = Accel::parse(value)?,
            "autostart" => {
                spec.autostart = parse_bool(value).ok_or_else(|| {
                    VirtError::InvalidConfig(format!(
                        "line {}: bad autostart {value:?} (expected true|false)",
                        i + 1
                    ))
                })?
            }
            "nic" => spec.nics.push(crate::net::parse_nic(value)?),
            "display" => spec.display = parse_display(value)?,
            other => {
                return Err(VirtError::InvalidConfig(format!(
                    "line {}: unknown key {other:?}",
                    i + 1
                )))
            }
        }
    }
    Ok(spec)
}

/// Parse the boolean spellings accepted in config files.
fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Parse a `display = ...` line.
///
/// Forms accepted: `vnc`, `spice`, `none`, plus `password=...` / `listen=...`
/// parts in any order, e.g. `vnc,listen=0.0.0.0,password=s3cret`.
pub fn parse_display(value: &str) -> VirtResult<DisplayConfig> {
    let mut display = DisplayConfig::default();
    let mut kind_set = false;
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(pw) = part.strip_prefix("password=") {
            display.password = Some(pw.to_owned());
        } else if let Some(addr) = part.strip_prefix("listen=") {
            if addr.is_empty() {
                return Err(VirtError::InvalidConfig(format!(
                    "display {value:?}: empty listen address"
                )));
            }
            display.listen = Some(addr.to_owned());
        } else if let Ok(kind) = DisplayKind::parse(part) {
            if kind_set {
                return Err(VirtError::InvalidConfig(format!(
                    "display {value:?}: duplicate kind"
                )));
            }
            display.kind = kind;
            kind_set = true;
        } else {
            return Err(VirtError::InvalidConfig(format!(
                "display {value:?}: unrecognized part {part:?}"
            )));
        }
    }
    Ok(display)
}

/// Parse a libvirt domain XML into a [`VmSpec`].
///
/// Only the metadata needed for validation is extracted (`name`, `vcpu`,
/// `memory`); the original document is preserved in [`VmSpec::raw_xml`] and
/// handed to libvirt verbatim, so nothing else is normalized away.
pub fn parse_domain_xml(contents: &str) -> VirtResult<VmSpec> {
    let mut spec = VmSpec {
        vcpus: 1,
        memory_kib: 256 * 1024,
        raw_xml: Some(contents.to_owned()),
        ..Default::default()
    };
    if let Some(name) = crate::xml::text(contents, "name") {
        spec.name = name;
    }
    if spec.name.is_empty() {
        return Err(VirtError::InvalidConfig(
            "libvirt XML has no <name> element".into(),
        ));
    }
    if let Some(vcpus) = crate::xml::text(contents, "vcpu").and_then(|v| v.parse::<u32>().ok()) {
        spec.vcpus = vcpus;
    }
    if let Some(mem) = crate::xml::memory_kib(contents) {
        spec.memory_kib = mem;
    }
    Ok(spec)
}

/// Load a VM configuration from a file path, detecting the format from
/// contents (XML documents start with `<`).
pub fn load_spec_file(path: impl AsRef<Path>) -> VirtResult<VmSpec> {
    let contents = std::fs::read_to_string(path.as_ref())?;
    ConfigFormat::detect(&contents).parse(&contents)
}

/// Load a VM configuration with an explicit format, bypassing detection.
pub fn load_spec_file_as(path: impl AsRef<Path>, format: ConfigFormat) -> VirtResult<VmSpec> {
    let contents = std::fs::read_to_string(path.as_ref())?;
    format.parse(&contents)
}

/// Backend hook: turn a validated [`VmSpec`] into a defined (persistent,
/// not yet running) domain handle.
pub trait SpecLoader: Sized {
    type Domain: crate::Domain;

    /// Parse + validate + define. The domain is registered but not started;
    /// call [`crate::Domain::start`] on the returned handle.
    fn load(&self, spec: &VmSpec, rules: &LoadRules) -> VirtResult<Self::Domain> {
        rules.validate(spec)?;
        self.define_spec(spec)
    }

    /// Backend-specific define step.
    fn define_spec(&self, spec: &VmSpec) -> VirtResult<Self::Domain>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spec_format() {
        let spec = parse_spec(
            "name = a\nvcpus = 4\nmemory_kib = 1048576\ndisk = /x.qcow2\n# comment\n",
        )
        .unwrap();
        assert_eq!(spec.name, "a");
        assert_eq!(spec.vcpus, 4);
        assert_eq!(spec.memory_kib, 1_048_576);
        assert_eq!(spec.disks, ["/x.qcow2"]);
        assert_eq!(spec.nics.len(), 0);
    }

    #[test]
    fn parses_display_line() {
        let spec = parse_spec("name = a\ndisplay = vnc,listen=0.0.0.0,password=s3cret\n").unwrap();
        assert_eq!(spec.display.kind, DisplayKind::Vnc);
        assert_eq!(spec.display.listen.as_deref(), Some("0.0.0.0"));
        assert_eq!(spec.display.password.as_deref(), Some("s3cret"));
        assert!(spec.display.is_public());

        let spec = parse_spec("name = a\ndisplay = none\n").unwrap();
        assert_eq!(spec.display.kind, DisplayKind::None);
        assert!(!spec.display.is_public());
        assert!(parse_spec("name = a\ndisplay = bogus\n").is_err());
    }

    #[test]
    fn rules_reject() {
        let rules = LoadRules {
            allowed_name_patterns: vec!["prod-*".into()],
            max_vcpus: 8,
            ..Default::default()
        };
        let spec = VmSpec {
            name: "dev-box".into(),
            vcpus: 2,
            memory_kib: 1024,
            ..Default::default()
        };
        assert!(rules.validate(&spec).is_err());
        let spec = VmSpec {
            name: "prod-box".into(),
            vcpus: 16,
            ..spec
        };
        assert!(rules.validate(&spec).is_err());
        let spec = VmSpec {
            name: "prod-box".into(),
            vcpus: 2,
            ..spec
        };
        assert!(rules.validate(&spec).is_ok());
    }

    #[test]
    fn rules_reject_public_display_without_password() {
        let rules = LoadRules::default();
        let mut spec = VmSpec {
            name: "box".into(),
            vcpus: 1,
            memory_kib: 1024,
            ..Default::default()
        };
        assert!(rules.validate(&spec).is_ok());
        spec.display.listen = Some("0.0.0.0".into());
        assert!(rules.validate(&spec).is_err());
        spec.display.password = Some("pw".into());
        assert!(rules.validate(&spec).is_ok());
    }

    #[test]
    fn parses_install_options() {
        let spec = parse_spec(
            "name = box\ncdrom = /tmp/install.iso\nboot = cdrom\naccel = tcg\nautostart = yes\n",
        )
        .unwrap();
        assert_eq!(spec.cdrom.as_deref(), Some("/tmp/install.iso"));
        assert_eq!(spec.boot, BootDevice::Cdrom);
        assert_eq!(spec.accel, Accel::Tcg);
        assert!(spec.autostart);
        assert!(parse_spec("name = a\nboot = floppy\n").is_err());
        assert!(parse_spec("name = a\nautostart = maybe\n").is_err());
    }

    #[test]
    fn rules_count_cdrom_as_a_disk() {
        let mut spec = VmSpec {
            name: "box".into(),
            vcpus: 1,
            memory_kib: 1024,
            ..Default::default()
        };
        spec.disks = vec!["/tmp/a".into(); 16];
        let rules = LoadRules {
            require_disks_exist: false,
            ..Default::default()
        };
        assert!(rules.validate(&spec).is_ok());
        spec.cdrom = Some("/tmp/b".into());
        assert!(rules.validate(&spec).is_err());
    }

    #[test]
    fn parses_pci_addresses() {
        let address = PciAddress::parse("0000:01:00.0").unwrap();
        assert_eq!(
            (address.domain, address.bus, address.slot, address.function),
            (0, 1, 0, 0)
        );
        assert_eq!(address.as_string(), "0000:01:00.0");
        assert_eq!(
            PciAddress::parse(" 0000:3B:1F.7 ").unwrap().as_string(),
            "0000:3b:1f.7"
        );
        assert_eq!(
            PciAddress::parse("0:1:0.0").unwrap().as_string(),
            "0000:01:00.0"
        );
        for bad in [
            "",
            "0000:01:00",
            "0000:01:00.0.1",
            "0000:01:00.8",
            "0000:100:00.0",
            "0000:01:00.z",
            "0000-01-00.0",
        ] {
            assert!(PciAddress::parse(bad).is_err(), "{bad:?} 应被拒绝");
        }
    }

    #[test]
    fn gpu_spec_parsing_and_rules() {
        let spec = parse_spec(
            "name = gpu-vm\nvcpus = 2\nmemory_kib = 1024\ngpu = 0000:01:00.0\ngpu = 0000:02:00.0\n",
        )
        .unwrap();
        assert_eq!(spec.gpus.len(), 2);
        assert_eq!(spec.gpus[0].pci_address, "0000:01:00.0");

        let rules = LoadRules::default();
        assert!(rules.validate(&spec).is_ok());
        let mut three = spec.clone();
        three
            .gpus
            .push(GpuPassthrough::new("0000:03:00.0").unwrap());
        assert!(rules.validate(&three).is_err());

        let mut bad = spec;
        bad.gpus[0].pci_address = "not-a-pci".into();
        assert!(rules.validate(&bad).is_err());
        assert!(parse_spec("name = a\ngpu = nope\n").is_err());
    }

    #[test]
    fn detects_and_parses_xml() {
        let spec = load_spec_file("tests/fixtures/demo.xml").unwrap();
        assert!(spec.raw_xml.is_some());
        assert_eq!(spec.name, "demo-xml");
        assert_eq!(spec.vcpus, 2);
        assert_eq!(spec.memory_kib, 1_048_576);
    }

    #[test]
    fn xml_entities_roundtrip() {
        let xml =
            "<domain><name>a&amp;b&lt;c&gt;</name><vcpu>3</vcpu><memory unit='MiB'>2</memory></domain>";
        let spec = parse_domain_xml(xml).unwrap();
        assert_eq!(spec.name, "a&b<c>");
        assert_eq!(spec.vcpus, 3);
        assert_eq!(spec.memory_kib, 2048);
    }

    #[test]
    fn missing_xml_name_is_rejected() {
        let xml = "<domain><memory unit='KiB'>1024</memory></domain>";
        assert!(parse_domain_xml(xml).is_err());
    }
}
