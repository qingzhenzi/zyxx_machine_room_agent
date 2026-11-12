//! Full flow: parse config file -> apply rules -> load (define) -> start ->
//! list -> shutdown.
//!
//! Run with: `cargo run --all-features --example lifecycle [vm.conf|vm.xml]`

use virt_core::{load_spec_file, Domain, DomainState, Hypervisor, LoadRules, SpecLoader, VmSpec};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let spec: Option<VmSpec> = args.get(1).map(load_spec_file).transpose()?;
    let rules = LoadRules::default();

    #[cfg(feature = "libvirt-backend")]
    libvirt_flow(spec.as_ref(), &rules)?;

    #[cfg(feature = "kvm-backend")]
    kvm_flow(spec.as_ref(), &rules)?;

    #[cfg(not(any(feature = "libvirt-backend", feature = "kvm-backend")))]
    {
        let _ = (spec, rules);
        eprintln!("enable --features kvm-backend or libvirt-backend");
    }
    Ok(())
}

/// Shared flow over any Hypervisor + SpecLoader backend that can boot guests.
fn flow<H>(hv: &H, spec: Option<&VmSpec>, rules: &LoadRules) -> Result<(), Box<dyn std::error::Error>>
where
    H: Hypervisor + SpecLoader,
    <H as SpecLoader>::Domain: Domain,
{
    // 1. Load from config file: validate against rules, then define.
    if let Some(spec) = spec {
        let dom = hv.load(spec, rules)?;
        println!("loaded {:?} from config", dom.name());
        for (index, gfx) in dom.graphics()?.iter().enumerate() {
            println!(
                "  display[{index}]: {} at {} (password: {})",
                gfx.kind.as_str(),
                gfx.endpoint(),
                gfx.password
            );
        }
        if dom.state()? == DomainState::ShutOff {
            dom.start()?;
            println!("started {}", dom.name());
        }
    }

    // 2. List all VMs.
    for info in hv.list_domains()? {
        println!("- {} ({}): {:?}", info.name, info.id, info.state);
    }

    // 3. Shut down by name if running.
    if let Some(spec) = spec {
        if let Ok(dom) = hv.domain_by_name(&spec.name) {
            if dom.state()?.is_active() {
                dom.shutdown()?;
                println!("shutdown requested for {}", dom.name());
            }
        }
    }
    Ok(())
}

#[cfg(feature = "libvirt-backend")]
fn libvirt_flow(
    spec: Option<&VmSpec>,
    rules: &LoadRules,
) -> Result<(), Box<dyn std::error::Error>> {
    use virt_libvirt::LibvirtHypervisor;
    if !LibvirtHypervisor::is_available() {
        eprintln!("libvirt not available, skipping");
        return Ok(());
    }
    let hv = LibvirtHypervisor::connect()?;
    println!("== libvirt {} ==", hv.version_string()?);
    flow(&hv, spec, rules)
}

/// KVM has no boot path, so this only registers the VM and lists it.
/// Use `lowlevel-kvm` to actually run guest code.
#[cfg(feature = "kvm-backend")]
fn kvm_flow(spec: Option<&VmSpec>, rules: &LoadRules) -> Result<(), Box<dyn std::error::Error>> {
    use virt_kvm::KvmHypervisor;
    if !KvmHypervisor::is_available() {
        eprintln!("/dev/kvm not available, skipping");
        return Ok(());
    }
    let hv = KvmHypervisor::connect()?;
    println!(
        "== kvm api {} (register + list only, no boot path) ==",
        hv.version()?
    );
    if let Some(spec) = spec {
        let dom = hv.load(spec, rules)?;
        println!("registered {:?} (in-process, not bootable)", dom.name());
    }
    for info in hv.list_domains()? {
        println!(
            "- {} ({}): {:?} vcpus={} mem={} KiB",
            info.name, info.id, info.state, info.vcpus, info.memory_kib
        );
    }
    Ok(())
}
