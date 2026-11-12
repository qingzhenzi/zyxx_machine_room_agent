//! Domain (virtual machine) lifecycle types.

use crate::VirtError;

/// Opaque, backend-specific domain identifier.
///
/// `0` means "no id": libvirt reports `-1` for inactive domains, so a stopped
/// VM maps to `0` here. Use the name as the stable key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DomainId(pub(crate) u32);

impl DomainId {
    pub fn new(id: u32) -> Self {
        Self(id)
    }

    pub fn get(&self) -> u32 {
        self.0
    }

    /// True when the backend could not assign a runtime id (inactive domain).
    pub fn is_unset(&self) -> bool {
        self.0 == 0
    }
}

impl std::fmt::Display for DomainId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Runtime state of a domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DomainState {
    /// Not running, but defined (persisted configuration exists).
    ShutOff,
    /// Currently executing guest code.
    Running,
    /// Created but paused (not scheduled).
    Paused,
    /// In the process of shutting down.
    ShuttingDown,
    /// Crashed or in an error state.
    Crashed,
    /// State unknown / not reported by the backend.
    Unknown,
}

impl DomainState {
    /// Whether the guest currently holds CPU time.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::Paused)
    }
}

/// Snapshot of a domain's status.
#[derive(Debug, Clone)]
pub struct DomainInfo {
    pub id: DomainId,
    pub name: String,
    pub state: DomainState,
    /// Number of virtual CPUs.
    pub vcpus: u32,
    /// Maximum memory in KiB.
    pub max_memory_kib: u64,
    /// Memory currently in use, in KiB.
    pub memory_kib: u64,
}

/// Graphic console protocol exposed by a domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GraphicsKind {
    Vnc,
    Spice,
    /// Backend-specific protocol name we do not model.
    Other,
}

impl GraphicsKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vnc => "vnc",
            Self::Spice => "spice",
            Self::Other => "other",
        }
    }
}

/// A reachable graphic console endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphicsInfo {
    pub kind: GraphicsKind,
    /// Address the server listens on, as reported by the backend.
    pub host: String,
    /// TCP port. `0` means the backend has not allocated one yet.
    pub port: u16,
    /// Whether a password is required to connect.
    pub password: bool,
    /// Password value reported by the backend, when it exposes one.
    ///
    /// Kept separate from [`Self::password`] so existing callers that only need
    /// the boolean stay source-compatible.
    pub password_value: Option<String>,
}

impl GraphicsInfo {
    /// `host:port`, ready to paste into a viewer.
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// VNC display number (`:0` for port 5900), if this is a VNC endpoint.
    pub fn vnc_display(&self) -> Option<u32> {
        if self.kind != GraphicsKind::Vnc || self.port < 5900 {
            return None;
        }
        Some(u32::from(self.port - 5900))
    }
}

/// A handle to one virtual machine managed by a [`crate::Hypervisor`].
///
/// Cloning is cheap; all backends implement this with an interior handle.
pub trait Domain: Clone + Send + Sync + 'static {
    /// Backend-unique id of this domain.
    fn id(&self) -> DomainId;

    /// Human-readable name.
    fn name(&self) -> &str;

    /// Current runtime state.
    fn state(&self) -> Result<DomainState, VirtError>;

    /// Full status snapshot.
    ///
    /// Backends must report what they actually know; there is no catch-all
    /// default because filling zeros would misreport a live VM as 0 vCPU.
    fn info(&self) -> Result<DomainInfo, VirtError>;

    /// Graphic console endpoints (VNC/SPICE).
    ///
    /// Returns an empty list when the domain has no display configured.
    fn graphics(&self) -> Result<Vec<GraphicsInfo>, VirtError> {
        Ok(Vec::new())
    }

    /// Whether the domain is started automatically with the host.
    fn autostart(&self) -> Result<bool, VirtError> {
        Err(VirtError::Unsupported(
            "autostart is not supported by this backend",
        ))
    }

    /// Enable or disable starting this domain with the host.
    fn set_autostart(&self, _enabled: bool) -> Result<(), VirtError> {
        Err(VirtError::Unsupported(
            "autostart is not supported by this backend",
        ))
    }

    /// Whether a persistent definition exists (i.e. survives a host reboot).
    ///
    /// Backends with no configuration store return `false`.
    fn persistent(&self) -> Result<bool, VirtError> {
        Ok(false)
    }

    /// Backing image paths of this domain's writable disks (CD-ROMs excluded).
    ///
    /// Used by `vm info` and by `vm rm --with-disks`; backends that cannot
    /// introspect their configuration return an empty list.
    fn disk_paths(&self) -> Result<Vec<String>, VirtError> {
        Ok(Vec::new())
    }

    /// Take a screenshot of the primary display.
    ///
    /// Returns the image in the backend's native format: QEMU ≥ 7 hands back
    /// PNG, older drivers PNM (`P6`). Check the magic bytes before writing a
    /// file with a fixed extension.
    fn screenshot(&self) -> Result<Vec<u8>, VirtError> {
        Err(VirtError::Unsupported(
            "screenshots are not supported by this backend",
        ))
    }

    /// Boot the domain. No-op if already running.
    fn start(&self) -> Result<(), VirtError>;

    /// Graceful shutdown (ACPI request). May require guest cooperation.
    fn shutdown(&self) -> Result<(), VirtError>;

    /// Force power-off immediately.
    fn destroy(&self) -> Result<(), VirtError>;

    /// Pause execution.
    fn suspend(&self) -> Result<(), VirtError>;

    /// Resume from pause.
    fn resume(&self) -> Result<(), VirtError>;

    /// Reboot the guest.
    fn reboot(&self) -> Result<(), VirtError>;

    /// Remove this domain's persistent definition (if any).
    fn undefine(&self) -> Result<(), VirtError>;
}
