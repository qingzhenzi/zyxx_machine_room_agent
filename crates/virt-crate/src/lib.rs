//! # virt-crate
//!
//! One import surface for Linux virtualisation: `virt-core` traits plus a
//! runtime-selected backend.
//!
//! ```no_run
//! use virt_crate::Backend;
//!
//! let hv = Backend::auto().expect("no hypervisor on this host");
//! for vm in hv.list_domains().unwrap() {
//!     println!("{} {:?}", vm.name, vm.state);
//! }
//! ```
//!
//! Backends are compile-time features (`libvirt-backend`, `kvm-backend`), but
//! [`Backend`] picks between them at run time — that is the part a single
//! generic parameter cannot express.

pub use virt_core::*;

#[cfg(any(feature = "libvirt-backend", feature = "kvm-backend"))]
mod facade {
    use virt_core::{
        DomainInfo, DomainState, GraphicsInfo, LoadRules, VirtError, VirtResult, VmSpec,
    };

    /// A live connection to whichever hypervisor this host provides.
    ///
    /// Construct with [`Backend::auto`] (libvirt first, KVM as fallback) or an
    /// explicit constructor. Only read-mostly operations are exposed here;
    /// use the backend crates directly for lifecycle verbs.
    pub enum Backend {
        #[cfg(feature = "libvirt-backend")]
        Libvirt(virt_libvirt::LibvirtHypervisor),
        #[cfg(feature = "kvm-backend")]
        Kvm(virt_kvm::KvmHypervisor),
    }

    impl Backend {
        /// Connect to the first available backend, preferring libvirt.
        pub fn auto() -> VirtResult<Self> {
            #[cfg(feature = "libvirt-backend")]
            {
                use virt_core::Hypervisor;
                if let Ok(hv) = virt_libvirt::LibvirtHypervisor::connect() {
                    return Ok(Self::Libvirt(hv));
                }
            }
            #[cfg(feature = "kvm-backend")]
            {
                use virt_core::Hypervisor;
                if let Ok(hv) = virt_kvm::KvmHypervisor::connect() {
                    return Ok(Self::Kvm(hv));
                }
            }
            Err(VirtError::BackendUnavailable(
                "no libvirt and no /dev/kvm on this host".to_owned(),
            ))
        }

        #[cfg(feature = "libvirt-backend")]
        pub fn libvirt() -> VirtResult<Self> {
            use virt_core::Hypervisor;
            Ok(Self::Libvirt(virt_libvirt::LibvirtHypervisor::connect()?))
        }

        /// 显式 libvirt URI（`qemu:///system` / `qemu:///session`）。
        /// CLI/测试需要在不改动默认探测行为的前提下指定连接。
        #[cfg(feature = "libvirt-backend")]
        pub fn libvirt_uri(uri: &str) -> VirtResult<Self> {
            Ok(Self::Libvirt(
                virt_libvirt::LibvirtHypervisor::connect_uri(uri)?,
            ))
        }

        #[cfg(feature = "kvm-backend")]
        pub fn kvm() -> VirtResult<Self> {
            use virt_core::Hypervisor;
            Ok(Self::Kvm(virt_kvm::KvmHypervisor::connect()?))
        }

        pub fn name(&self) -> &'static str {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(_) => "libvirt",
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(_) => "kvm",
            }
        }

        /// Human-readable version, e.g. `12.6.0`.
        pub fn version_string(&self) -> VirtResult<String> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(hv) => {
                    use virt_core::Hypervisor;
                    hv.version_string()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(hv) => {
                    use virt_core::Hypervisor;
                    hv.version_string()
                }
            }
        }

        pub fn list_domains(&self) -> VirtResult<Vec<DomainInfo>> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(hv) => {
                    use virt_core::Hypervisor;
                    hv.list_domains()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(hv) => {
                    use virt_core::Hypervisor;
                    hv.list_domains()
                }
            }
        }

        /// Look up one domain by name.
        pub fn domain(&self, name: &str) -> VirtResult<AnyDomain> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(hv) => {
                    use virt_core::Hypervisor;
                    Ok(AnyDomain::Libvirt(hv.domain_by_name(name)?))
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(hv) => {
                    use virt_core::Hypervisor;
                    Ok(AnyDomain::Kvm(hv.domain_by_name(name)?))
                }
            }
        }

        /// Graphic console endpoints of `name` (empty when headless).
        ///
        /// This is what you hand to a VNC viewer: see the crate README for the
        /// SSH-tunnel recipe used to reach a loopback-bound console.
        pub fn graphics_of(&self, name: &str) -> VirtResult<Vec<GraphicsInfo>> {
            self.domain(name)?.graphics()
        }

        /// Parse + validate + define a VM from a spec.
        ///
        /// The domain is registered but not started. On the KVM backend this
        /// only creates an in-process VM object (no boot path); libvirt is the
        /// backend that implements real definitions.
        pub fn define(&self, spec: &VmSpec, rules: &LoadRules) -> VirtResult<AnyDomain> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(hv) => {
                    use virt_core::SpecLoader;
                    Ok(AnyDomain::Libvirt(hv.load(spec, rules)?))
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(hv) => {
                    use virt_core::SpecLoader;
                    Ok(AnyDomain::Kvm(hv.load(spec, rules)?))
                }
            }
        }

        /// Screenshot of `name`'s primary display, as PNM bytes.
        pub fn screenshot_of(&self, name: &str) -> VirtResult<Vec<u8>> {
            self.domain(name)?.screenshot()
        }

        /// Detach the CD-ROM of `name`; `true` when one was actually removed.
        pub fn eject_cdrom(&self, name: &str) -> VirtResult<bool> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(hv) => {
                    use virt_core::Hypervisor;
                    let dom = hv.domain_by_name(name)?;
                    hv.eject_cdrom(&dom)
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(_) => {
                    let _ = name;
                    Err(VirtError::Unsupported(
                        "kvm backend has no CD-ROM device to eject",
                    ))
                }
            }
        }
    }

    #[cfg(feature = "libvirt-backend")]
    impl From<virt_libvirt::LibvirtHypervisor> for Backend {
        fn from(hv: virt_libvirt::LibvirtHypervisor) -> Self {
            Self::Libvirt(hv)
        }
    }

    /// A domain handle from either backend.
    ///
    /// Read-only by design: lifecycle verbs differ a lot between backends
    /// (KVM has no boot path), so they live on the concrete types.
    #[derive(Clone)]
    pub enum AnyDomain {
        #[cfg(feature = "libvirt-backend")]
        Libvirt(virt_libvirt::LibvirtDomain),
        #[cfg(feature = "kvm-backend")]
        Kvm(virt_kvm::KvmDomain),
    }

    impl AnyDomain {
        pub fn name(&self) -> &str {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.name()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.name()
                }
            }
        }

        pub fn state(&self) -> VirtResult<DomainState> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.state()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.state()
                }
            }
        }

        pub fn info(&self) -> VirtResult<DomainInfo> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.info()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.info()
                }
            }
        }

        pub fn graphics(&self) -> VirtResult<Vec<GraphicsInfo>> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.graphics()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.graphics()
                }
            }
        }

        /// First graphic console, if the domain has one.
        pub fn console(&self) -> VirtResult<Option<GraphicsInfo>> {
            Ok(self.graphics()?.into_iter().next())
        }

        pub fn start(&self) -> VirtResult<()> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.start()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.start()
                }
            }
        }

        pub fn shutdown(&self) -> VirtResult<()> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.shutdown()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.shutdown()
                }
            }
        }

        pub fn destroy(&self) -> VirtResult<()> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.destroy()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.destroy()
                }
            }
        }

        pub fn reboot(&self) -> VirtResult<()> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.reboot()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.reboot()
                }
            }
        }

        pub fn suspend(&self) -> VirtResult<()> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.suspend()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.suspend()
                }
            }
        }

        pub fn resume(&self) -> VirtResult<()> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.resume()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.resume()
                }
            }
        }

        pub fn undefine(&self) -> VirtResult<()> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.undefine()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.undefine()
                }
            }
        }

        pub fn autostart(&self) -> VirtResult<bool> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.autostart()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.autostart()
                }
            }
        }

        pub fn set_autostart(&self, enabled: bool) -> VirtResult<()> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.set_autostart(enabled)
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.set_autostart(enabled)
                }
            }
        }

        pub fn persistent(&self) -> VirtResult<bool> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.persistent()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.persistent()
                }
            }
        }

        pub fn disk_paths(&self) -> VirtResult<Vec<String>> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.disk_paths()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.disk_paths()
                }
            }
        }

        pub fn screenshot(&self) -> VirtResult<Vec<u8>> {
            match self {
                #[cfg(feature = "libvirt-backend")]
                Self::Libvirt(dom) => {
                    use virt_core::Domain;
                    dom.screenshot()
                }
                #[cfg(feature = "kvm-backend")]
                Self::Kvm(dom) => {
                    use virt_core::Domain;
                    dom.screenshot()
                }
            }
        }
    }
}

#[cfg(any(feature = "libvirt-backend", feature = "kvm-backend"))]
pub use facade::{AnyDomain, Backend};
