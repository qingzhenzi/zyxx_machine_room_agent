//! Low-level KVM demo: build a 16-bit real-mode VM that writes `A` into the
//! VGA text buffer at 0xB8000, then halts.
//!
//! Run with: `cargo run --features kvm-backend --example lowlevel-kvm`
//! Requires `/dev/kvm`; there is no QEMU involved anywhere.

#[cfg(feature = "kvm-backend")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use virt_core::{Hypervisor, RawVcpu, RawVm, RunMode, VcpuExit, VmBuilder};
    use virt_kvm::{KvmHypervisor, KvmVcpu};

    // 16-bit real mode:
    //   mov ax, 0xB800 / mov es, ax
    //   mov byte [es:0x0000], 'A'
    //   mov byte [es:0x0001], 0x07   (light grey on black)
    //   hlt
    let code: [u8; 18] = [
        0xb8, 0x00, 0xb8, // mov ax, 0xB800
        0x8e, 0xc0, // mov es, ax
        0x26, 0xc6, 0x06, 0x00, 0x00, b'A', // mov byte [es:0], 'A'
        0x26, 0xc6, 0x06, 0x01, 0x00, 0x07, // mov byte [es:1], 0x07
        0xf4, // hlt
    ];

    let hv = KvmHypervisor::connect()?;
    let domain = hv.create_vm("lowlevel-demo")?;

    let mem_size = 1 << 20; // 1 MiB, covers the VGA text buffer at 0xB8000
    let mut guest = GuestMemory::new(mem_size)?;
    guest.as_mut_slice()[..code.len()].copy_from_slice(&code);

    // SAFETY: `guest` owns a 4 KiB-aligned allocation that outlives the VM
    // (it is dropped after the run loop below).
    let slot = unsafe {
        RawVm::set_user_memory_region(&domain, 0, mem_size as u64, guest.as_ptr() as u64)?
    };
    println!("mapped {mem_size} bytes into slot {slot}");

    let (vm, mut vcpus) = VmBuilder::new()
        .vcpus(1)
        .memory_bytes(mem_size as u64)
        .build(|_cfg| Ok(domain.clone()))?;
    let _ = vm;

    let vcpu = vcpus[0]
        .as_any_mut()
        .downcast_mut::<KvmVcpu>()
        .ok_or("backend did not return a KvmVcpu")?;
    vcpu.set_real_mode_entry(0)?;

    let mut console = Console;
    for _ in 0..64 {
        match vcpu.run(RunMode::Run, &mut console)? {
            VcpuExit::Hlt => {
                println!("guest halted");
                break;
            }
            VcpuExit::Shutdown => {
                println!("guest shut down");
                break;
            }
            other => println!("exit: {other:?}"),
        }
    }

    let vga = 0xb8000;
    let cell = &guest.as_slice()[vga..vga + 2];
    println!("VGA cell: {cell:?}");
    assert_eq!(cell, b"A\x07", "guest did not write the expected text cell");
    println!("ok: guest code really ran and wrote video memory");
    Ok(())
}

/// Minimal port-I/O handler: echoes serial output, answers reads with 0xFF.
#[cfg(feature = "kvm-backend")]
struct Console;

#[cfg(feature = "kvm-backend")]
impl virt_core::IoHandler for Console {
    fn io_in(&mut self, _port: u16, data: &mut [u8]) {
        data.fill(0xff);
    }
    fn io_out(&mut self, port: u16, data: &[u8]) {
        if port == 0x3f8 {
            for byte in data {
                print!("{}", *byte as char);
            }
        }
    }
    fn mmio_read(&mut self, _addr: u64, data: &mut [u8]) {
        data.fill(0);
    }
    fn mmio_write(&mut self, _addr: u64, _data: &[u8]) {}
}

/// Page-aligned guest RAM, freed on drop.
#[cfg(feature = "kvm-backend")]
struct GuestMemory {
    ptr: *mut u8,
    layout: std::alloc::Layout,
}

#[cfg(feature = "kvm-backend")]
impl GuestMemory {
    fn new(size: usize) -> Result<Self, Box<dyn std::error::Error>> {
        let layout = std::alloc::Layout::from_size_align(size, 4096)?;
        // SAFETY: layout has non-zero size.
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            return Err("guest memory allocation failed".into());
        }
        Ok(Self { ptr, layout })
    }

    fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    fn as_slice(&self) -> &[u8] {
        // SAFETY: the allocation is `layout.size()` bytes and zero-filled.
        unsafe { std::slice::from_raw_parts(self.ptr, self.layout.size()) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, and `&mut self` guarantees exclusivity.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }
}

#[cfg(feature = "kvm-backend")]
impl Drop for GuestMemory {
    fn drop(&mut self) {
        // SAFETY: allocated with this exact layout in `new`.
        unsafe { std::alloc::dealloc(self.ptr, self.layout) }
    }
}

#[cfg(not(feature = "kvm-backend"))]
fn main() {
    eprintln!("enable --features kvm-backend");
}
