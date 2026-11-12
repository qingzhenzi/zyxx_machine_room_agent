//! Low-level VM construction traits (KVM-style: build a VM and run guest code).
//!
//! This mirrors the `kvm-ioctls` model: create a VM, attach memory, create
//! vCPUs, then run a vCPU in a loop handling VM exits.
//!
//! Exit *data* flows through [`IoHandler`] rather than the return value,
//! because IN-type exits (port `in`, MMIO read) need the caller to write the
//! value back into the buffer KVM handed out, before the vCPU runs again.

use crate::{VirtError, VirtResult};

/// Maximum number of guest memory slots on x86 (`KVM_USER_MEM_SLOTS`).
pub const MAX_MEMORY_SLOTS: u32 = 509;

/// Configuration for a VM to be created.
#[derive(Debug, Clone)]
pub struct VmConfig {
    /// Number of vCPUs to create.
    pub vcpus: u32,
    /// Desired guest memory size in bytes.
    ///
    /// This is metadata handed to the backend constructor; it does **not**
    /// map anything by itself. Mapping is an explicit
    /// [`RawVm::set_user_memory_region`] call, because the host allocation
    /// has to outlive the VM and stay owned by the caller.
    pub memory_bytes: u64,
}

impl Default for VmConfig {
    fn default() -> Self {
        Self {
            vcpus: 1,
            memory_bytes: 256 * 1024 * 1024, // 256 MiB
        }
    }
}

/// Configuration for a single vCPU.
#[derive(Debug, Clone, Copy, Default)]
pub struct VcpuConfig {
    /// Index of this vCPU within the VM.
    pub index: u32,
}

/// How a vCPU enters the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Continue executing guest code.
    Run,
    /// Single-step one instruction (requires guest-debug support).
    Step,
}

/// Why a vCPU stopped running guest code.
///
/// Exit payloads live in [`IoHandler`]: `Io`/`Mmio` carry only the direction
/// and address, the bytes go through the handler so that reads can be
/// answered in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VcpuExit {
    /// Guest executed HLT and is idle.
    Hlt,
    /// Guest requested a shutdown (triple fault or ACPI power-off).
    Shutdown,
    /// Guest performed port I/O.
    Io { is_write: bool, port: u16 },
    /// Guest performed MMIO.
    Mmio { is_write: bool, addr: u64 },
    /// Anything else; raw backend description.
    Other(String),
}

/// Data plane for VM exits.
///
/// For `*_read` methods the slice is a buffer the handler must fill in; KVM
/// copies it back into the guest before the next run. For `*_write` methods
/// the slice holds the bytes the guest wrote.
pub trait IoHandler {
    /// Guest executed `in` on `port`; fill `data` with the value to return.
    fn io_in(&mut self, port: u16, data: &mut [u8]);
    /// Guest executed `out` on `port`.
    fn io_out(&mut self, port: u16, data: &[u8]);
    /// Guest read from `addr`; fill `data` with the value to return.
    fn mmio_read(&mut self, addr: u64, data: &mut [u8]);
    /// Guest wrote to `addr`.
    fn mmio_write(&mut self, addr: u64, data: &[u8]);
}

/// Handler that answers every read with zero and drops every write.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullIoHandler;

impl IoHandler for NullIoHandler {
    fn io_in(&mut self, _port: u16, data: &mut [u8]) {
        data.fill(0);
    }
    fn io_out(&mut self, _port: u16, _data: &[u8]) {}
    fn mmio_read(&mut self, _addr: u64, data: &mut [u8]) {
        data.fill(0);
    }
    fn mmio_write(&mut self, _addr: u64, _data: &[u8]) {}
}

/// A VM under construction or running, at the ioctl level.
pub trait RawVm: Send + Sync {
    /// Map a host memory region into guest physical address space.
    ///
    /// Returns the KVM memory slot the region was placed in.
    ///
    /// # Safety
    /// `host_addr` must point to a live, correctly sized allocation for the
    /// lifetime of this VM.
    unsafe fn set_user_memory_region(
        &self,
        guest_phys_addr: u64,
        memory_size_bytes: u64,
        host_addr: u64,
    ) -> VirtResult<u32>;

    /// Create a vCPU on this VM.
    fn create_vcpu(&self, config: VcpuConfig) -> VirtResult<Box<dyn RawVcpu>>;
}

/// A single virtual CPU.
///
/// `Send` so each vCPU can live on its own thread, which is how KVM guests
/// normally run multi-core.
pub trait RawVcpu: Send {
    /// Run the vCPU until a VM exit occurs.
    ///
    /// Exit payloads are delivered through `io`; errors map to
    /// [`VirtError::VcpuExit`] for exceptional exits.
    fn run(&mut self, mode: RunMode, io: &mut dyn IoHandler) -> VirtResult<VcpuExit>;

    /// Downcast hook for architecture-specific bring-up.
    ///
    /// The generic trait cannot express segment bases, RIP or control
    /// registers, but booting a guest needs them. Backends return their
    /// concrete vCPU type so callers can downcast to it.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// Builder for constructing a [`RawVm`].
pub struct VmBuilder {
    config: VmConfig,
}

impl Default for VmBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl VmBuilder {
    pub fn new() -> Self {
        Self {
            config: VmConfig::default(),
        }
    }

    pub fn vcpus(mut self, n: u32) -> Self {
        self.config.vcpus = n;
        self
    }

    pub fn memory_bytes(mut self, bytes: u64) -> Self {
        self.config.memory_bytes = bytes;
        self
    }

    /// Build the VM using the given low-level backend constructor.
    ///
    /// Only vCPUs are created here; guest memory still has to be mapped with
    /// [`RawVm::set_user_memory_region`] before the first [`RawVcpu::run`].
    pub fn build<F, V>(self, ctor: F) -> VirtResult<(V, Vec<Box<dyn RawVcpu>>)>
    where
        F: FnOnce(VmConfig) -> VirtResult<V>,
        V: RawVm,
    {
        if self.config.vcpus == 0 {
            return Err(VirtError::InvalidConfig("vcpus must be >= 1".into()));
        }
        if self.config.memory_bytes == 0 {
            return Err(VirtError::InvalidConfig("memory must be > 0".into()));
        }
        let vm = ctor(self.config.clone())?;
        let mut vcpus = Vec::with_capacity(self.config.vcpus as usize);
        for i in 0..self.config.vcpus {
            vcpus.push(vm.create_vcpu(VcpuConfig { index: i })?);
        }
        Ok((vm, vcpus))
    }
}
