//! Low-level KVM VM handle: ownership of the `VmFd` plus memory-slot
//! bookkeeping.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

use virt_core::{
    DomainId, DomainState, IoHandler, RawVcpu, RawVm, RunMode, VcpuConfig, VcpuExit, VirtError,
    VirtResult, MAX_MEMORY_SLOTS,
};

use crate::KvmDomain;

fn kvm_err(e: impl std::fmt::Display) -> VirtError {
    VirtError::Backend {
        backend: "kvm",
        message: e.to_string(),
    }
}

pub(super) struct VmHandle {
    pub(super) id: DomainId,
    pub(super) name: String,
    vm: Mutex<Option<kvm_ioctls::VmFd>>,
    /// Next free KVM memory slot. Slots are per-VM, so a plain counter is
    /// correct; deriving them from the domain id (as an earlier version did)
    /// overflows the 509-slot limit after a few dozen VMs.
    next_slot: AtomicU32,
    /// Bytes actually mapped into guest physical address space.
    mapped_bytes: AtomicU64,
    /// vCPUs created so far.
    vcpu_count: AtomicU32,
}

// VmFd is a bare ioctl handle: kernel-side state is per-fd and does not care
// which thread issues the call. All access goes through the Mutex above, so
// sharing across threads is sound even though VmFd itself is !Sync.
unsafe impl Send for VmHandle {}
unsafe impl Sync for VmHandle {}

impl VmHandle {
    pub(super) fn new(kvm: &kvm_ioctls::Kvm, id: DomainId, name: String) -> VirtResult<Self> {
        let vm = kvm.create_vm().map_err(kvm_err)?;
        Ok(Self {
            id,
            name,
            vm: Mutex::new(Some(vm)),
            next_slot: AtomicU32::new(0),
            mapped_bytes: AtomicU64::new(0),
            vcpu_count: AtomicU32::new(0),
        })
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    /// There is no guest here — a `VmHandle` is just a KVM VM object. It is
    /// either alive (vCPUs may be driven via [`RawVm`]) or destroyed.
    pub(super) fn state(&self) -> DomainState {
        DomainState::ShutOff
    }

    pub(super) fn vcpu_count(&self) -> u32 {
        self.vcpu_count.load(Ordering::Relaxed)
    }

    pub(super) fn mapped_kib(&self) -> u64 {
        self.mapped_bytes.load(Ordering::Relaxed) / 1024
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<kvm_ioctls::VmFd>> {
        // A poisoned lock only means some thread panicked while holding it;
        // the fd itself is still valid. Recover instead of killing the daemon.
        self.vm.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn fd(&self) -> VirtResult<std::sync::MutexGuard<'_, Option<kvm_ioctls::VmFd>>> {
        let guard = self.lock();
        if guard.is_none() {
            return Err(VirtError::InvalidConfig("domain destroyed".into()));
        }
        Ok(guard)
    }

    /// Dropping the `VmFd` tears the VM down; vCPUs die with it.
    pub(super) fn destroy(&self) -> VirtResult<()> {
        *self.lock() = None;
        Ok(())
    }
}

impl RawVm for KvmDomain {
    unsafe fn set_user_memory_region(
        &self,
        guest_phys_addr: u64,
        memory_size_bytes: u64,
        host_addr: u64,
    ) -> VirtResult<u32> {
        if memory_size_bytes == 0 {
            return Err(VirtError::InvalidConfig("memory region is empty".into()));
        }
        let slot = self.inner.next_slot.fetch_add(1, Ordering::Relaxed);
        if slot >= MAX_MEMORY_SLOTS {
            self.inner.next_slot.fetch_sub(1, Ordering::Relaxed);
            return Err(VirtError::InvalidConfig(format!(
                "too many guest memory regions (KVM allows {MAX_MEMORY_SLOTS})"
            )));
        }

        let region = kvm_bindings::kvm_userspace_memory_region {
            slot,
            guest_phys_addr,
            memory_size: memory_size_bytes,
            userspace_addr: host_addr,
            flags: 0,
        };
        let mut guard = self.inner.fd()?;
        let vm = guard
            .as_mut()
            .ok_or_else(|| VirtError::InvalidConfig("domain destroyed".into()))?;
        // SAFETY: caller guarantees host_addr is a live, correctly sized allocation.
        unsafe { vm.set_user_memory_region(region) }.map_err(kvm_err)?;
        self.inner
            .mapped_bytes
            .fetch_add(memory_size_bytes, Ordering::Relaxed);
        Ok(slot)
    }

    fn create_vcpu(&self, config: VcpuConfig) -> VirtResult<Box<dyn RawVcpu>> {
        let mut guard = self.inner.fd()?;
        let vm = guard
            .as_mut()
            .ok_or_else(|| VirtError::InvalidConfig("domain destroyed".into()))?;
        let vcpu = vm.create_vcpu(config.index as u64).map_err(kvm_err)?;
        self.inner.vcpu_count.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(KvmVcpu { vcpu }))
    }
}

/// A KVM vCPU handle.
///
/// Exposed so callers can perform architecture-specific bring-up through
/// [`virt_core::RawVcpu::as_any_mut`] before the first run.
pub struct KvmVcpu {
    vcpu: kvm_ioctls::VcpuFd,
}

impl KvmVcpu {
    /// Point the vCPU at `rip` in 16-bit real mode with a zero-based code
    /// segment — what a boot loader would normally arrange before handing
    /// control to the guest.
    pub fn set_real_mode_entry(&mut self, rip: u64) -> VirtResult<()> {
        let mut sregs = self.vcpu.get_sregs().map_err(kvm_err)?;
        sregs.cs.base = 0;
        sregs.cs.selector = 0;
        self.vcpu.set_sregs(&sregs).map_err(kvm_err)?;

        let mut regs = self.vcpu.get_regs().map_err(kvm_err)?;
        regs.rip = rip;
        regs.rflags = 0x2; // bit 1 is reserved and always set on hardware reset
        self.vcpu.set_regs(&regs).map_err(kvm_err)?;
        Ok(())
    }
}

// VcpuFd is an ioctl handle for one vCPU; `run` takes `&mut self`, so no two
// threads can drive the same vCPU concurrently. Moving the handle to a
// dedicated thread (the usual one-thread-per-vCPU layout) is safe.
unsafe impl Send for KvmVcpu {}

impl RawVcpu for KvmVcpu {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn run(&mut self, mode: RunMode, io: &mut dyn IoHandler) -> VirtResult<VcpuExit> {
        if mode == RunMode::Step {
            return Err(VirtError::Unsupported(
                "single-step requires KVM guest-debug support and is not implemented",
            ));
        }
        let exit = self.vcpu.run().map_err(kvm_err)?;
        Ok(match exit {
            kvm_ioctls::VcpuExit::Hlt => VcpuExit::Hlt,
            kvm_ioctls::VcpuExit::Shutdown => VcpuExit::Shutdown,
            kvm_ioctls::VcpuExit::IoIn(port, data) => {
                // `data` aliases the kernel run-struct buffer; KVM copies it
                // back into the guest on the next `run`, so it must be filled
                // here, not handed back to the caller.
                io.io_in(port, data);
                VcpuExit::Io {
                    is_write: false,
                    port,
                }
            }
            kvm_ioctls::VcpuExit::IoOut(port, data) => {
                io.io_out(port, data);
                VcpuExit::Io {
                    is_write: true,
                    port,
                }
            }
            kvm_ioctls::VcpuExit::MmioRead(addr, data) => {
                io.mmio_read(addr, data);
                VcpuExit::Mmio {
                    is_write: false,
                    addr,
                }
            }
            kvm_ioctls::VcpuExit::MmioWrite(addr, data) => {
                io.mmio_write(addr, data);
                VcpuExit::Mmio { is_write: true, addr }
            }
            other => VcpuExit::Other(format!("{other:?}")),
        })
    }
}
