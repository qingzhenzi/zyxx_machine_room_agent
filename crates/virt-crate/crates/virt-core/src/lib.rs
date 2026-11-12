//! # virt-core
//!
//! Hypervisor-agnostic traits and types for Linux virtualization.
//!
//! The crate defines two abstraction levels:
//!
//! 1. **Lifecycle management** ([`Hypervisor`], [`Domain`]) — start/stop/query
//!    existing virtual machines, similar to what `virsh` does.
//! 2. **Low-level VM construction** ([`VmBuilder`], [`RawVm`], [`RawVcpu`]) —
//!    build a VM from scratch and run guest code, the KVM-ioctl style.
//!
//! Configuration helpers ([`load_spec_file`], [`parse_spec`], [`SpecLoader`])
//! turn a file into a defined domain. Backend crates (`virt-kvm`,
//! `virt-libvirt`) implement these traits.

pub mod config;
pub mod domain;
pub mod error;
pub mod hypervisor;
pub mod lowlevel;
pub mod net;
pub mod xml;

pub use config::{
    load_spec_file, load_spec_file_as, parse_display, parse_domain_xml, parse_spec, Accel,
    BootDevice, ConfigFormat, DisplayConfig, DisplayKind, GpuPassthrough, LoadRules, PciAddress,
    SpecLoader, VmSpec,
};
pub use domain::{Domain, DomainId, DomainInfo, DomainState, GraphicsInfo, GraphicsKind};
pub use error::VirtError;
pub use hypervisor::Hypervisor;
pub use lowlevel::{
    IoHandler, NullIoHandler, RawVcpu, RawVm, RunMode, VcpuConfig, VcpuExit, VmBuilder, VmConfig,
    MAX_MEMORY_SLOTS,
};

/// Result alias used across all virt crates.
pub type VirtResult<T> = Result<T, VirtError>;
