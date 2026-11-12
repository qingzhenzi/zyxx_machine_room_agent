//! High-level hypervisor connection trait.

use crate::{Domain, DomainInfo, VirtResult};

/// A connection to a hypervisor on the local (Linux) host.
///
/// Implementations: `virt_kvm` via /dev/kvm, `virt_libvirt` via libvirtd.
///
/// Constructor methods are gated on `Self: Sized`, so the trait stays
/// object-safe: `Box<dyn Hypervisor<Domain = D>>` works for runtime dispatch,
/// while `H::connect()` only makes sense on a concrete backend.
pub trait Hypervisor: Send + Sync + 'static {
    /// The domain handle type produced by this connection.
    type Domain: Domain;

    /// Open a connection. Fails with [`crate::VirtError::BackendUnavailable`]
    /// when the host lacks support (e.g. no /dev/kvm or libvirtd not running).
    fn connect() -> VirtResult<Self>
    where
        Self: Sized;

    /// Probe support without opening a full connection.
    fn is_available() -> bool
    where
        Self: Sized;

    /// Hypervisor name, e.g. "kvm" or "libvirt".
    fn name() -> &'static str
    where
        Self: Sized;

    /// Hypervisor version, backend-specific encoding.
    fn version(&self) -> VirtResult<u64>;

    /// Human-readable version, e.g. "12.6.0".
    ///
    /// Defaults to the raw encoding; backends with a packed version override
    /// this so callers never have to print bare integers.
    fn version_string(&self) -> VirtResult<String> {
        self.version().map(|v| v.to_string())
    }

    /// Look up a domain by name.
    fn domain_by_name(&self, name: &str) -> VirtResult<Self::Domain>;

    /// Look up a running domain by id.
    fn domain_by_id(&self, id: u32) -> VirtResult<Self::Domain>;

    /// List all currently defined (including inactive) domains.
    fn list_domains(&self) -> VirtResult<Vec<DomainInfo>>;
}
