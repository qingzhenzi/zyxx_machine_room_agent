//! Virtual NIC configuration: network backend type, model, MAC, and other
//! per-interface settings shared across backends.
//!
//! Supported source backends on Linux:
//! - [`NetSource::Bridge`] — attach to an existing host bridge (e.g. `virbr0`, `br0`)
//! - [`NetSource::Network`] — named libvirt network (usually NAT, e.g. `default`)
//! - [`NetSource::Direct`] — macvtap/macvlan passthrough onto a host interface
//! - [`NetSource::User`] — slirp user-mode networking (QEMU), no root required
//! - [`NetSource::None`] — no connectivity (isolated)

use crate::{VirtError, VirtResult};

/// How the NIC connects to the outside.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NetSource {
    /// Attach to a host bridge device.
    Bridge(String),
    /// Attach to a named managed network (libvirt: NAT/routed network).
    Network(String),
    /// Direct attachment to a host NIC via macvtap/macvlan.
    Direct { device: String, mode: DirectMode },
    /// User-mode (slirp) networking — no privileges needed.
    User,
    /// Isolated network, no external connectivity.
    None,
}

impl Default for NetSource {
    fn default() -> Self {
        Self::Network("default".into())
    }
}

impl NetSource {
    /// Parse from a config-file source string, e.g.
    /// `bridge=br0`, `network=default`, `direct=eth0:bridge`, `user`, `none`.
    pub fn parse(s: &str) -> VirtResult<Self> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix("bridge=") {
            if rest.is_empty() {
                return Err(bad(s));
            }
            return Ok(Self::Bridge(rest.to_owned()));
        }
        if let Some(rest) = s.strip_prefix("network=") {
            if rest.is_empty() {
                return Err(bad(s));
            }
            return Ok(Self::Network(rest.to_owned()));
        }
        if let Some(rest) = s.strip_prefix("direct=") {
            // direct=<device> or direct=<device>:<mode>
            let (device, mode) = match rest.split_once(':') {
                Some((d, m)) => (d, DirectMode::parse(m)?),
                None => (rest, DirectMode::default()),
            };
            if device.is_empty() {
                return Err(bad(s));
            }
            return Ok(Self::Direct {
                device: device.to_owned(),
                mode,
            });
        }
        match s {
            "user" => Ok(Self::User),
            "none" | "isolated" => Ok(Self::None),
            _ => Err(bad(s)),
        }
    }
}

fn bad(input: &str) -> VirtError {
    VirtError::InvalidConfig(format!("bad nic source {input:?} (expected bridge=X, network=X, direct=X[:mode], user, none)"))
}

/// macvtap/macvlan passthrough mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DirectMode {
    /// Default VEPA-adjacent behavior.
    #[default]
    Vepa,
    /// Bridge between VMs on the same host NIC.
    Bridge,
    /// Private: no communication between VMs.
    Private,
    /// Passthrough: exclusive use of the host NIC.
    Passthrough,
}

impl DirectMode {
    pub fn parse(s: &str) -> VirtResult<Self> {
        match s.trim() {
            "vepa" => Ok(Self::Vepa),
            "bridge" => Ok(Self::Bridge),
            "private" => Ok(Self::Private),
            "passthrough" => Ok(Self::Passthrough),
            other => Err(VirtError::InvalidConfig(format!(
                "bad direct mode {other:?} (expected vepa|bridge|private|passthrough)"
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Vepa => "vepa",
            Self::Bridge => "bridge",
            Self::Private => "private",
            Self::Passthrough => "passthrough",
        }
    }
}

/// NIC emulation model presented to the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NicModel {
    /// paravirtualized, best performance on KVM/QEMU.
    #[default]
    Virtio,
    /// Intel e1000 emulation, widest guest compatibility.
    E1000,
    /// Intel e1000e (PCIe) emulation.
    E1000e,
    /// Realtek RTL8139 emulation.
    Rtl8139,
}

impl NicModel {
    pub fn parse(s: &str) -> VirtResult<Self> {
        match s.trim() {
            "virtio" => Ok(Self::Virtio),
            "e1000" => Ok(Self::E1000),
            "e1000e" => Ok(Self::E1000e),
            "rtl8139" => Ok(Self::Rtl8139),
            other => Err(VirtError::InvalidConfig(format!(
                "bad nic model {other:?} (expected virtio|e1000|e1000e|rtl8139)"
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Virtio => "virtio",
            Self::E1000 => "e1000",
            Self::E1000e => "e1000e",
            Self::Rtl8139 => "rtl8139",
        }
    }
}

/// One virtual network interface.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NicConfig {
    /// Front-end emulation model.
    pub model: NicModel,
    /// Network source / backend.
    pub source: NetSource,
    /// Optional static MAC; generated when None.
    pub mac: Option<String>,
    /// Optional host-side tap device name override.
    pub target_dev: Option<String>,
}

impl NicConfig {
    /// Shorthand: virtio NIC on the default NAT network.
    pub fn default_nat() -> Self {
        Self::default()
    }

    /// Shorthand: virtio NIC on a host bridge.
    pub fn bridged(bridge: &str) -> Self {
        Self {
            source: NetSource::Bridge(bridge.to_owned()),
            ..Self::default()
        }
    }

    /// Validate MAC format if present (aa:bb:cc:dd:ee:ff).
    pub fn validate(&self) -> VirtResult<()> {
        if let Some(mac) = &self.mac {
            let parts: Vec<_> = mac.split(':').collect();
            if parts.len() != 6 || parts.iter().any(|p| p.len() != 2 || !p.chars().all(|c| c.is_ascii_hexdigit())) {
                return Err(VirtError::InvalidConfig(format!("bad MAC {mac:?} (expected aa:bb:cc:dd:ee:ff)")));
            }
        }
        Ok(())
    }
}

/// Parse a `nic = ...` config line into a [`NicConfig`].
///
/// Forms accepted:
/// - `virtio` — model only, default NAT source
/// - `e1000,bridge=br0` — model + source
/// - `mac=52:54:00:12:34:56,bridge=br0,virtio` — any order
pub fn parse_nic(value: &str) -> VirtResult<NicConfig> {
    let mut nic = NicConfig::default();
    let mut model_set = false;
    let mut source_set = false;
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(mac) = part.strip_prefix("mac=") {
            nic.mac = Some(mac.to_owned());
        } else if let Some(dev) = part.strip_prefix("target=") {
            nic.target_dev = Some(dev.to_owned());
        } else if let Ok(model) = NicModel::parse(part) {
            if model_set {
                return Err(VirtError::InvalidConfig(format!(
                    "nic {value:?}: duplicate model"
                )));
            }
            nic.model = model;
            model_set = true;
        } else if part.contains('=') {
            if source_set {
                return Err(VirtError::InvalidConfig(format!(
                    "nic {value:?}: duplicate source"
                )));
            }
            nic.source = NetSource::parse(part)?;
            source_set = true;
        } else if !source_set && matches!(part, "user" | "none" | "isolated") {
            nic.source = NetSource::parse(part)?;
            source_set = true;
        } else {
            return Err(VirtError::InvalidConfig(format!(
                "nic {value:?}: unrecognized part {part:?}"
            )));
        }
    }
    nic.validate()?;
    Ok(nic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sources() {
        assert_eq!(NetSource::parse("bridge=br0").unwrap(), NetSource::Bridge("br0".into()));
        assert_eq!(NetSource::parse("network=default").unwrap(), NetSource::Network("default".into()));
        assert_eq!(
            NetSource::parse("direct=eth0:bridge").unwrap(),
            NetSource::Direct { device: "eth0".into(), mode: DirectMode::Bridge }
        );
        assert_eq!(NetSource::parse("user").unwrap(), NetSource::User);
        assert!(NetSource::parse("bogus").is_err());
    }

    #[test]
    fn parses_nic_line() {
        let nic = parse_nic("e1000,bridge=br0,mac=52:54:00:12:34:56").unwrap();
        assert_eq!(nic.model, NicModel::E1000);
        assert_eq!(nic.source, NetSource::Bridge("br0".into()));
        assert_eq!(nic.mac.as_deref(), Some("52:54:00:12:34:56"));

        // bare model name
        let nic = parse_nic("virtio").unwrap();
        assert_eq!(nic.source, NetSource::Network("default".into()));

        // bad mac rejected
        assert!(parse_nic("mac=xyz,bridge=br0").is_err());
        // bad mode rejected
        assert!(parse_nic("direct=eth0:nonsense").is_err());
    }

    #[test]
    fn parses_bare_source_keywords() {
        assert_eq!(parse_nic("virtio,user").unwrap().source, NetSource::User);
        assert_eq!(parse_nic("none").unwrap().source, NetSource::None);
        assert_eq!(
            parse_nic("isolated,e1000").unwrap().source,
            NetSource::None
        );
    }

    #[test]
    fn rejects_duplicate_source() {
        assert!(parse_nic("bridge=br0,network=default").is_err());
    }
}
