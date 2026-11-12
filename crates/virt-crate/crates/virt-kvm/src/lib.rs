//! # virt-kvm
//!
//! [`virt_core::RawVm`] / [`virt_core::RawVcpu`] implementation backed by
//! Linux KVM through `kvm-ioctls`, plus a thin in-process registry that lets
//! these VMs appear as [`virt_core::Domain`] handles.
//!
//! ## What this backend can and cannot do
//!
//! KVM is a hypervisor API, not a VMM: it gives you a VM object, memory slots
//! and vCPUs, and nothing else. There is no BIOS, no device model, no boot
//! loader. Consequently:
//!
//! - **Can**: create VM objects, map guest memory, create vCPUs, run guest
//!   code and service port/MMIO exits.
//! - **Cannot**: boot an OS, persist a definition, or implement the lifecycle
//!   verbs (`start`/`shutdown`/`reboot`). Those return
//!   [`VirtError::Unsupported`] instead of pretending to succeed.
//!
//! If you want managed VMs (define, start, VNC console), use `virt-libvirt`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};

use virt_core::{
    Domain, DomainId, DomainInfo, DomainState, Hypervisor, SpecLoader, VirtError, VirtResult, VmSpec,
};

mod lowlevel;

pub use lowlevel::KvmVcpu;

/// Low-level VM type (KVM ioctl surface) for use with [`virt_core::VmBuilder`].
pub type KvmRawVm = KvmDomain;

/// KVM-backed hypervisor connection.
///
/// Holds the `/dev/kvm` handle and an in-process registry of VM objects.
/// Nothing here survives the process.
#[derive(Clone)]
pub struct KvmHypervisor {
    kvm: Arc<kvm_ioctls::Kvm>,
    registry: Arc<Registry>,
}

impl std::fmt::Debug for KvmHypervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KvmHypervisor").finish_non_exhaustive()
    }
}

impl KvmHypervisor {
    /// Access the raw `kvm-ioctls::Kvm` handle for advanced use.
    pub fn kvm(&self) -> &kvm_ioctls::Kvm {
        &self.kvm
    }

    /// Create an in-process VM and register it under `name`.
    ///
    /// This is the KVM-backend way to "create a VM": there is no daemon and
    /// nothing is persisted. The returned handle supports the [`RawVm`] API
    /// ([`virt_core::RawVm::set_user_memory_region`],
    /// [`virt_core::RawVm::create_vcpu`]); lifecycle verbs are not available
    /// because KVM cannot boot anything on its own.
    pub fn create_vm(&self, name: &str) -> VirtResult<KvmDomain> {
        self.registry.create(&self.kvm, name)
    }

    /// Remove a domain from the in-process registry (like undefine + forget).
    pub fn unregister(&self, name: &str) -> VirtResult<()> {
        self.registry.remove(name)
    }
}

impl Hypervisor for KvmHypervisor {
    type Domain = KvmDomain;

    fn connect() -> VirtResult<Self> {
        let kvm = kvm_ioctls::Kvm::new()
            .map_err(|e| VirtError::BackendUnavailable(format!("/dev/kvm: {e}")))?;
        Ok(Self {
            kvm: Arc::new(kvm),
            registry: Arc::new(Registry::new()),
        })
    }

    fn is_available() -> bool {
        kvm_ioctls::Kvm::new().is_ok()
    }

    fn name() -> &'static str {
        "kvm"
    }

    fn version(&self) -> VirtResult<u64> {
        // KVM reports a non-negative API version; clamp defensively.
        Ok(u64::try_from(self.kvm.get_api_version()).unwrap_or(0))
    }

    fn domain_by_name(&self, name: &str) -> VirtResult<Self::Domain> {
        self.registry
            .get(name)
            .ok_or_else(|| VirtError::DomainNotFound(name.to_owned()))
    }

    fn domain_by_id(&self, id: u32) -> VirtResult<Self::Domain> {
        self.registry
            .get_by_id(id)
            .ok_or_else(|| VirtError::DomainNotFound(format!("id {id}")))
    }

    fn list_domains(&self) -> VirtResult<Vec<DomainInfo>> {
        Ok(self.registry.list())
    }
}

impl SpecLoader for KvmHypervisor {
    type Domain = KvmDomain;

    /// Registers an empty VM object for `spec.name`.
    ///
    /// `raw_xml` is ignored (KVM has no configuration store); use
    /// `virt-libvirt` for XML-defined VMs. The returned domain is *not*
    /// bootable — see the crate docs.
    fn define_spec(&self, spec: &VmSpec) -> VirtResult<Self::Domain> {
        self.registry.create(&self.kvm, &spec.name)
    }
}

/// A KVM VM registered as a lifecycle-managed domain.
#[derive(Clone)]
pub struct KvmDomain {
    inner: Arc<lowlevel::VmHandle>,
    /// Kept weak so a domain handle does not keep the registry (and every
    /// other domain) alive after `undefine`.
    registry: Weak<Registry>,
}

impl Domain for KvmDomain {
    fn id(&self) -> DomainId {
        self.inner.id.clone()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn state(&self) -> Result<DomainState, VirtError> {
        Ok(self.inner.state())
    }

    fn info(&self) -> Result<DomainInfo, VirtError> {
        let mapped = self.inner.mapped_kib();
        Ok(DomainInfo {
            id: self.inner.id.clone(),
            name: self.inner.name().to_owned(),
            state: self.inner.state(),
            vcpus: self.inner.vcpu_count(),
            // No ballooning: the max is whatever was actually mapped.
            max_memory_kib: mapped,
            memory_kib: mapped,
        })
    }

    fn start(&self) -> Result<(), VirtError> {
        Err(VirtError::Unsupported(
            "kvm backend has no boot path; drive RawVm/RawVcpu directly, or use virt-libvirt",
        ))
    }

    fn shutdown(&self) -> Result<(), VirtError> {
        Err(VirtError::Unsupported(
            "kvm backend has no guest-facing shutdown; call destroy",
        ))
    }

    fn destroy(&self) -> Result<(), VirtError> {
        self.inner.destroy()
    }

    fn suspend(&self) -> Result<(), VirtError> {
        Err(VirtError::Unsupported(
            "kvm backend does not suspend guests; stop calling run on the vCPU",
        ))
    }

    fn resume(&self) -> Result<(), VirtError> {
        Err(VirtError::Unsupported("kvm backend does not pause guests"))
    }

    fn reboot(&self) -> Result<(), VirtError> {
        Err(VirtError::Unsupported("kvm backend cannot reboot a guest"))
    }

    /// Tears the VM down and drops it from the registry.
    fn undefine(&self) -> Result<(), VirtError> {
        self.inner.destroy()?;
        if let Some(registry) = self.registry.upgrade() {
            registry.forget(&self.inner.name);
        }
        Ok(())
    }
}

/// Process-local name → domain map.
struct Registry {
    next_id: AtomicU32,
    domains: Mutex<HashMap<String, KvmDomain>>,
}

impl Registry {
    fn new() -> Self {
        Self {
            next_id: AtomicU32::new(1),
            domains: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, KvmDomain>> {
        // Recover from poisoning: a panicking thread must not wedge the whole
        // agent, and the map is still structurally valid.
        self.domains
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn create(self: &Arc<Self>, kvm: &kvm_ioctls::Kvm, name: &str) -> VirtResult<KvmDomain> {
        if name.is_empty() {
            return Err(VirtError::InvalidConfig("name is required".into()));
        }
        let mut guard = self.lock();
        if guard.contains_key(name) {
            return Err(VirtError::AlreadyExists(name.to_owned()));
        }
        let id = DomainId::new(self.next_id.fetch_add(1, Ordering::Relaxed));
        let vm = lowlevel::VmHandle::new(kvm, id, name.to_owned())?;
        let domain = KvmDomain {
            inner: Arc::new(vm),
            registry: Arc::downgrade(self),
        };
        guard.insert(name.to_owned(), domain.clone());
        Ok(domain)
    }

    fn get(&self, name: &str) -> Option<KvmDomain> {
        self.lock().get(name).cloned()
    }

    fn get_by_id(&self, id: u32) -> Option<KvmDomain> {
        self.lock().values().find(|d| d.id().get() == id).cloned()
    }

    fn remove(&self, name: &str) -> VirtResult<()> {
        self.lock()
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| VirtError::DomainNotFound(name.to_owned()))
    }

    /// Remove without failing when already gone (used by `undefine`).
    fn forget(&self, name: &str) {
        self.lock().remove(name);
    }

    fn list(&self) -> Vec<DomainInfo> {
        let mut infos: Vec<DomainInfo> = self
            .lock()
            .values()
            .filter_map(|d| d.info().ok())
            .collect();
        infos.sort_by(|a, b| a.name.cmp(&b.name));
        infos
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use virt_core::RawVm;

    /// These tests need a real `/dev/kvm`; they print why they skip instead of
    /// silently passing as if they had run.
    fn kvm() -> Option<KvmHypervisor> {
        if !KvmHypervisor::is_available() {
            eprintln!("跳过：/dev/kvm 不可用");
            return None;
        }
        KvmHypervisor::connect().ok()
    }

    #[test]
    #[ignore = "requires a real /dev/kvm; run with --ignored on a KVM host"]
    fn rejects_duplicate_names() {
        let Some(hv) = kvm() else { return };
        hv.create_vm("dup").expect("create");
        assert!(matches!(
            hv.create_vm("dup"),
            Err(VirtError::AlreadyExists(_))
        ));
    }

    #[test]
    #[ignore = "requires a real /dev/kvm; run with --ignored on a KVM host"]
    fn undefine_drops_from_registry() {
        let Some(hv) = kvm() else { return };
        let dom = hv.create_vm("gone").expect("create");
        dom.undefine().expect("undefine");
        assert!(matches!(
            hv.domain_by_name("gone"),
            Err(VirtError::DomainNotFound(_))
        ));
        assert!(hv.list_domains().unwrap().iter().all(|d| d.name != "gone"));
    }

    #[test]
    #[ignore = "requires a real /dev/kvm; run with --ignored on a KVM host"]
    fn lifecycle_verbs_refuse_instead_of_pretending() {
        let Some(hv) = kvm() else { return };
        let dom = hv.create_vm("honest").expect("create");
        assert!(matches!(dom.start(), Err(VirtError::Unsupported(_))));
        assert!(matches!(dom.shutdown(), Err(VirtError::Unsupported(_))));
        assert!(matches!(dom.reboot(), Err(VirtError::Unsupported(_))));
        assert!(matches!(dom.suspend(), Err(VirtError::Unsupported(_))));
        // destroy is real: it closes the VM fd.
        dom.destroy().expect("destroy");
        assert!(matches!(
            RawVm::create_vcpu(&dom, virt_core::VcpuConfig { index: 0 }),
            Err(VirtError::InvalidConfig(_))
        ));
    }

    #[test]
    #[ignore = "requires a real /dev/kvm; run with --ignored on a KVM host"]
    fn memory_slots_are_per_vm() {        let Some(hv) = kvm() else { return };
        let a = hv.create_vm("slot-a").expect("create a");
        let b = hv.create_vm("slot-b").expect("create b");

        // Page-aligned host buffer; leaked because the VM outlives the test fn.
        let layout = std::alloc::Layout::from_size_align(8192, 4096).unwrap();
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "allocation failed");

        // Each VM numbers its slots from zero: slots are per-VM, not global.
        let sa = unsafe { RawVm::set_user_memory_region(&a, 0, 4096, ptr as u64) }.expect("map a");
        let sb = unsafe { RawVm::set_user_memory_region(&b, 0, 4096, ptr as u64) }.expect("map b");
        assert_eq!(sa, 0);
        assert_eq!(sb, 0);

        let second =
            unsafe { RawVm::set_user_memory_region(&a, 4096, 4096, ptr as u64) }.expect("map a2");
        assert_eq!(second, 1);
        assert_eq!(a.info().unwrap().memory_kib, 8);
    }

    /// 真正跑一段 guest 机器码：16 位实模式，往 VGA 文本缓冲写 `A`，然后 hlt。
    #[test]
    #[ignore = "requires a real /dev/kvm; run with --ignored on a KVM host"]
    fn runs_real_mode_guest_until_hlt() {
        use virt_core::{IoHandler, RawVcpu, RawVm, RunMode, VcpuExit};

        struct Sink;
        impl IoHandler for Sink {
            fn io_in(&mut self, _port: u16, data: &mut [u8]) {
                data.fill(0xff);
            }
            fn io_out(&mut self, _port: u16, _data: &[u8]) {}
            fn mmio_read(&mut self, _addr: u64, data: &mut [u8]) {
                data.fill(0);
            }
            fn mmio_write(&mut self, _addr: u64, _data: &[u8]) {}
        }

        // mov ax,0xB800 / mov es,ax / mov byte [es:0],'A' / mov byte [es:1],0x07 / hlt
        const CODE: [u8; 18] = [
            0xb8, 0x00, 0xb8, 0x8e, 0xc0, 0x26, 0xc6, 0x06, 0x00, 0x00, b'A', 0x26, 0xc6, 0x06,
            0x01, 0x00, 0x07, 0xf4,
        ];

        let Some(hv) = kvm() else { return };
        let dom = hv.create_vm("tiny").expect("create");
        let mem_size = 1 << 20;
        let layout = std::alloc::Layout::from_size_align(mem_size, 4096).unwrap();
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "allocation failed");
        // SAFETY: the allocation is mem_size bytes and we copy CODE (18 bytes).
        unsafe {
            std::slice::from_raw_parts_mut(ptr, mem_size)[..CODE.len()].copy_from_slice(&CODE);
            RawVm::set_user_memory_region(&dom, 0, mem_size as u64, ptr as u64).expect("map");
        }

        let mut vcpu_handle = dom
            .create_vcpu(virt_core::VcpuConfig { index: 0 })
            .expect("create vcpu");

        let mut halted = false;
        {
            let vcpu = vcpu_handle
                .as_any_mut()
                .downcast_mut::<KvmVcpu>()
                .expect("kvm vcpu");
            vcpu.set_real_mode_entry(0).expect("set regs");
            for _ in 0..32 {
                match vcpu.run(RunMode::Run, &mut Sink).expect("run") {
                    VcpuExit::Hlt => {
                        halted = true;
                        break;
                    }
                    VcpuExit::Shutdown => break,
                    _ => {}
                }
            }
        }
        assert!(halted, "guest did not reach hlt");

        // SAFETY: 0xB8000 lies inside the mapped, zero-filled allocation.
        let cell = unsafe { std::slice::from_raw_parts(ptr.add(0xb8000), 2) };
        assert_eq!(cell, [b'A', 0x07], "guest code did not write video memory");

        // 先放掉 vCPU 句柄和 VM，再释放 guest 内存，避免 KVM 还持有这块地址。
        drop(vcpu_handle);
        let _ = dom.destroy();
        // SAFETY: same layout used to allocate, and the VM no longer runs.
        unsafe { std::alloc::dealloc(ptr, layout) };
    }
}
