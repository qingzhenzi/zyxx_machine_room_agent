//! # virt-libvirt
//!
//! [`virt_core::Hypervisor`] implementation backed by libvirt through the
//! `virt` crate (libvirtd connection).
//!
//! ```no_run
//! use virt_libvirt::LibvirtHypervisor;
//! use virt_core::Hypervisor;
//!
//! if LibvirtHypervisor::is_available() {
//!     let hv = LibvirtHypervisor::connect().unwrap();
//!     for d in hv.list_domains().unwrap() {
//!         println!("{}: {:?}", d.name, d.state);
//!     }
//! }
//! ```

use std::collections::HashSet;
use std::sync::Arc;

use virt::connect::Connect;
use virt::domain::{Domain as VirtDomain, DomainInfo as VirtDomainInfo};
use virt::stream::Stream;
use virt::sys;

use virt_core::{
    net::{NetSource, NicConfig},
    xml, Accel, BootDevice, DisplayConfig, DisplayKind, Domain, DomainId, DomainInfo, DomainState,
    GraphicsInfo, GraphicsKind, Hypervisor, SpecLoader, VirtError, VirtResult, VmSpec,
};

pub mod disk;

pub use disk::{create_disk, parse_size, DiskFormat};

/// Default URI used by [`LibvirtHypervisor::connect`].
///
/// `qemu:///system` talks to the host-wide libvirtd and therefore needs
/// privileges; see [`LibvirtHypervisor::connect_uri`] for per-user setups.
pub const DEFAULT_URI: &str = "qemu:///system";

fn be(e: virt::error::Error) -> VirtError {
    VirtError::Backend {
        backend: "libvirt",
        message: e.to_string(),
    }
}

/// libvirt-backed hypervisor connection.
///
/// `Send + Sync` come from the `virt` crate's own impls; no unsafe code here.
#[derive(Clone)]
pub struct LibvirtHypervisor {
    conn: Connect,
}

impl std::fmt::Debug for LibvirtHypervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibvirtHypervisor").finish_non_exhaustive()
    }
}

impl LibvirtHypervisor {
    /// Connect to an explicit libvirt URI (e.g. `qemu:///session`).
    pub fn connect_uri(uri: &str) -> VirtResult<Self> {
        let conn = Connect::open(Some(uri)).map_err(be)?;
        Ok(Self { conn })
    }

    /// Connect the way a room-machine agent should.
    ///
    /// Honours `QE_VIRT_URI`, then tries `qemu:///system` (the right choice for
    /// a root-owned service) and falls back to `qemu:///session` so the same
    /// binary works for a logged-in user without extra privileges.
    ///
    /// Returns the URI that actually worked alongside the connection, because
    /// `virsh` helpers and later reconnects must use the same one — domains
    /// defined in the session daemon are invisible to the system daemon.
    pub fn connect_best() -> VirtResult<(Self, String)> {
        if let Ok(uri) = std::env::var("QE_VIRT_URI") {
            if !uri.trim().is_empty() {
                return Ok((Self::connect_uri(uri.trim())?, uri));
            }
        }
        match Self::connect_uri(DEFAULT_URI) {
            Ok(hv) => Ok((hv, DEFAULT_URI.to_owned())),
            Err(system_error) => match Self::connect_uri("qemu:///session") {
                Ok(hv) => Ok((hv, "qemu:///session".to_owned())),
                Err(session_error) => Err(VirtError::BackendUnavailable(format!(
                    "qemu:///system: {system_error}; qemu:///session: {session_error}"
                ))),
            },
        }
    }
}

impl Hypervisor for LibvirtHypervisor {
    type Domain = LibvirtDomain;

    fn connect() -> VirtResult<Self> {
        Self::connect_uri(DEFAULT_URI)
    }

    /// Opens a throwaway connection to probe support.
    ///
    /// This does dial libvirtd, so do not call it in a tight loop; prefer
    /// keeping one [`LibvirtHypervisor`] around and handling connect errors.
    fn is_available() -> bool {
        Connect::open(Some(DEFAULT_URI)).is_ok()
    }

    fn name() -> &'static str {
        "libvirt"
    }

    fn version(&self) -> VirtResult<u64> {
        self.conn.get_lib_version().map(|v| v as u64).map_err(be)
    }

    fn version_string(&self) -> VirtResult<String> {
        self.version().map(format_libvirt_version)
    }

    fn domain_by_name(&self, name: &str) -> VirtResult<Self::Domain> {
        let dom = VirtDomain::lookup_by_name(&self.conn, name)
            .map_err(|_| VirtError::DomainNotFound(name.to_owned()))?;
        Ok(LibvirtDomain::from_dom(dom, name.to_owned()))
    }

    fn domain_by_id(&self, id: u32) -> VirtResult<Self::Domain> {
        let dom = VirtDomain::lookup_by_id(&self.conn, id)
            .map_err(|_| VirtError::DomainNotFound(format!("id {id}")))?;
        let name = dom.get_name().unwrap_or_default();
        Ok(LibvirtDomain::from_dom(dom, name))
    }

    /// Active domains first, then defined-but-inactive ones.
    ///
    /// libvirt can report a persistent running domain from both
    /// `list_domains` and `list_defined_domains`, so results are de-duplicated
    /// by name (the same thing `virsh list --all` does).
    fn list_domains(&self) -> VirtResult<Vec<DomainInfo>> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut infos = Vec::new();

        for id in self.conn.list_domains().map_err(be)? {
            let Ok(dom) = VirtDomain::lookup_by_id(&self.conn, id) else {
                // The domain vanished between listing and lookup; skip it.
                continue;
            };
            let name = dom.get_name().unwrap_or_default();
            if name.is_empty() || !seen.insert(name.clone()) {
                continue;
            }
            infos.push(info_of(&dom, &name));
        }

        for name in self.conn.list_defined_domains().map_err(be)? {
            if !seen.insert(name.clone()) {
                continue;
            }
            let Ok(dom) = VirtDomain::lookup_by_name(&self.conn, &name) else {
                continue;
            };
            infos.push(info_of(&dom, &name));
        }

        infos.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(infos)
    }
}

/// `12006000` → `"12.6.0"` (libvirt packs major/minor/patch in one integer).
pub fn format_libvirt_version(v: u64) -> String {
    format!("{}.{}.{}", v / 1_000_000, (v / 1_000) % 1_000, v % 1_000)
}

fn info_of(dom: &VirtDomain, name: &str) -> DomainInfo {
    let id = dom.get_id().unwrap_or(0);
    let vi: Option<VirtDomainInfo> = dom.get_info().ok();
    let (state, vcpus, max_mem, mem) = match vi {
        Some(i) => (map_state(i.state), i.nr_virt_cpu, i.max_mem, i.memory),
        None => (DomainState::Unknown, 0, 0, 0),
    };
    DomainInfo {
        id: DomainId::new(id),
        name: name.to_owned(),
        state,
        vcpus,
        max_memory_kib: max_mem,
        memory_kib: mem,
    }
}

fn map_state(s: sys::virDomainState) -> DomainState {
    match s {
        sys::VIR_DOMAIN_RUNNING | sys::VIR_DOMAIN_BLOCKED => DomainState::Running,
        sys::VIR_DOMAIN_PAUSED | sys::VIR_DOMAIN_PMSUSPENDED => DomainState::Paused,
        sys::VIR_DOMAIN_SHUTDOWN => DomainState::ShuttingDown,
        sys::VIR_DOMAIN_SHUTOFF | sys::VIR_DOMAIN_NOSTATE => DomainState::ShutOff,
        sys::VIR_DOMAIN_CRASHED => DomainState::Crashed,
        _ => DomainState::Unknown,
    }
}

/// A libvirt-managed domain.
///
/// The name is cached at construction: `Domain::name` returns a borrow, and
/// re-fetching it from libvirt on every call would mean either leaking the
/// C string or holding a lock across the call.
#[derive(Clone, Debug)]
pub struct LibvirtDomain {
    dom: VirtDomain,
    name: Arc<str>,
}

impl LibvirtDomain {
    fn from_dom(dom: VirtDomain, name: String) -> Self {
        Self {
            dom,
            name: Arc::from(name.as_str()),
        }
    }
}

impl Domain for LibvirtDomain {
    fn id(&self) -> DomainId {
        DomainId::new(self.dom.get_id().unwrap_or(0))
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn state(&self) -> Result<DomainState, VirtError> {
        Ok(self
            .dom
            .get_info()
            .map(|i| map_state(i.state))
            .unwrap_or(DomainState::Unknown))
    }

    fn info(&self) -> Result<DomainInfo, VirtError> {
        Ok(info_of(&self.dom, &self.name))
    }

    fn graphics(&self) -> Result<Vec<GraphicsInfo>, VirtError> {
        let xml = self.dom.get_xml_desc(0).map_err(be)?;
        Ok(parse_graphics(&xml))
    }

    fn autostart(&self) -> Result<bool, VirtError> {
        self.dom.get_autostart().map_err(be)
    }

    fn set_autostart(&self, enabled: bool) -> Result<(), VirtError> {
        self.dom.set_autostart(enabled).map(|_| ()).map_err(be)
    }

    fn persistent(&self) -> Result<bool, VirtError> {
        self.dom.is_persistent().map_err(be)
    }

    fn disk_paths(&self) -> Result<Vec<String>, VirtError> {
        let xml = self.dom.get_xml_desc(0).map_err(be)?;
        Ok(parse_disk_paths(&xml))
    }

    /// Capture the primary display via `virDomainScreenshot`.
    ///
    /// libvirt hands the image back over a stream. QEMU ≥ 7 produces PNG,
    /// older drivers PNM (P6) — `virsh screenshot` writes the same bytes, so
    /// the format is whatever the driver chose.
    fn screenshot(&self) -> Result<Vec<u8>, VirtError> {
        let conn = self.dom.get_connect().map_err(be)?;
        let stream = Stream::new(&conn, 0).map_err(be)?;
        // Non-zero mime type means the driver returned an error string.
        self.dom.screenshot(&stream, 0, 0).map_err(be)?;

        let mut image = Vec::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            match stream.recv(&mut buf) {
                Ok(0) => break,
                Ok(n) => image.extend_from_slice(&buf[..n]),
                Err(error) => {
                    let _ = stream.abort();
                    return Err(be(error));
                }
            }
        }
        stream.finish().map_err(be)?;
        if image.is_empty() {
            return Err(VirtError::Backend {
                backend: "libvirt",
                message: "screenshot returned no data".to_owned(),
            });
        }
        Ok(image)
    }

    fn start(&self) -> Result<(), VirtError> {
        self.dom.create().map(|_| ()).map_err(be)
    }

    fn shutdown(&self) -> Result<(), VirtError> {
        self.dom.shutdown().map(|_| ()).map_err(be)
    }

    fn destroy(&self) -> Result<(), VirtError> {
        self.dom.destroy().map_err(be)
    }

    fn suspend(&self) -> Result<(), VirtError> {
        self.dom.suspend().map(|_| ()).map_err(be)
    }

    fn resume(&self) -> Result<(), VirtError> {
        self.dom.resume().map(|_| ()).map_err(be)
    }

    fn reboot(&self) -> Result<(), VirtError> {
        self.dom.reboot(sys::VIR_DOMAIN_REBOOT_DEFAULT).map_err(be)
    }

    fn undefine(&self) -> Result<(), VirtError> {
        self.dom.undefine().map_err(be)
    }
}

impl SpecLoader for LibvirtHypervisor {
    type Domain = LibvirtDomain;

    fn define_spec(&self, spec: &VmSpec) -> VirtResult<Self::Domain> {
        // Prefer raw XML when provided (libvirt is the source of truth there);
        // otherwise synthesize domain XML from the normalized spec.
        let xml = match &spec.raw_xml {
            Some(raw) => raw.clone(),
            None => render_xml(spec)?,
        };
        let dom = VirtDomain::define_xml(&self.conn, &xml).map_err(be)?;
        let domain = LibvirtDomain::from_dom(dom, spec.name.clone());
        // Only touch the flag when the spec asks for it: some drivers (the
        // `test://` fake used by our unit tests) do not implement autostart,
        // and "off" is already the default for a fresh definition.
        if spec.autostart {
            domain.set_autostart(true)?;
        }
        Ok(domain)
    }
}

impl LibvirtHypervisor {
    /// Detach the CD-ROM device (typically after the OS is installed).
    ///
    /// Works on the live definition returned by libvirt, so it also copes with
    /// XML that was not generated by [`render_xml`]. Returns `true` when a
    /// device was actually removed, `false` when there was nothing to do.
    pub fn eject_cdrom(&self, domain: &LibvirtDomain) -> VirtResult<bool> {
        let xml = domain.dom.get_xml_desc(0).map_err(be)?;
        let stripped = strip_cdrom(&xml);
        if stripped == xml {
            return Ok(false);
        }
        VirtDomain::define_xml(&self.conn, &stripped).map_err(be)?;
        Ok(true)
    }
}

/// Target device name for the n-th virtio disk: `vda`, `vdb`, ... `vdz`,
/// `vdaa`, ...
fn disk_target(index: usize) -> VirtResult<String> {
    const LAST: usize = 26 * 27;
    if index >= LAST {
        return Err(VirtError::InvalidConfig(format!(
            "too many disks ({index}); libvirt names stop at vdzz"
        )));
    }
    let hi = index / 26;
    let lo = index % 26;
    let mut dev = String::from("vd");
    if hi > 0 {
        dev.push((b'a' + (hi - 1) as u8) as char);
    }
    dev.push((b'a' + lo as u8) as char);
    Ok(dev)
}

/// Driver type libvirt should expect for an image path.
fn disk_format(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".qcow2") {
        "qcow2"
    } else if lower.ends_with(".qcow") {
        "qcow"
    } else if lower.ends_with(".raw") || lower.ends_with(".img") {
        "raw"
    } else if lower.ends_with(".vmdk") {
        "vmdk"
    } else if lower.ends_with(".vdi") {
        "vdi"
    } else {
        // libvirt's own default for a file-backed disk.
        "qcow2"
    }
}

/// Render domain XML from a normalized [`VmSpec`].
///
/// Every interpolation goes through [`xml::escape`]: names, image paths and
/// bridge names come from configuration files, and an unescaped quote would
/// let a value close its attribute and inject extra elements.
fn render_xml(spec: &VmSpec) -> VirtResult<String> {
    if !cfg!(target_arch = "x86_64") {
        // q35 + SeaBIOS assumes x86; other architectures need a firmware
        // descriptor we do not model. Callers can still pass raw_xml.
        return Err(VirtError::Unsupported(
            "generated domain XML only supports x86_64 hosts; supply raw_xml instead",
        ));
    }

    let domain_type = match spec.accel {
        Accel::Kvm => "kvm",
        Accel::Tcg => "qemu",
        Accel::Auto => {
            if kvm_usable() {
                "kvm"
            } else {
                "qemu"
            }
        }
    };

    let mut xml = String::with_capacity(1024);
    xml.push_str(&format!("<domain type='{domain_type}'>"));
    xml.push_str(&format!("<name>{}</name>", xml::escape(&spec.name)));
    xml.push_str(&format!("<memory unit='KiB'>{}</memory>", spec.memory_kib));
    xml.push_str(&format!("<vcpu>{}</vcpu>", spec.vcpus));
    xml.push_str("<os><type arch='x86_64' machine='q35'>hvm</type></os>");
    xml.push_str("<devices>");

    // Firmware boots the device with the lowest <boot order> first. When an
    // installer ISO is attached the CD-ROM wins once; `vm eject` then removes
    // it and leaves the disk as the only bootable device.
    let cdrom_boot = if spec.boot == BootDevice::Cdrom { 1 } else { 2 };
    let disk_boot = if spec.cdrom.is_some() {
        3 - cdrom_boot
    } else {
        1
    };

    for (index, path) in spec.disks.iter().enumerate() {
        let boot = if index == 0 {
            format!("<boot order='{disk_boot}'/>")
        } else {
            String::new()
        };
        xml.push_str(&format!(
            "<disk type='file' device='disk'>\
             <driver name='qemu' type='{driver}'/>\
             <source file='{source}'/>\
             <target dev='{dev}' bus='virtio'/>\
             {boot}\
             </disk>",
            driver = disk_format(path),
            source = xml::escape(path),
            dev = disk_target(index)?,
        ));
    }

    if let Some(iso) = &spec.cdrom {
        xml.push_str(&format!(
            "<disk type='file' device='cdrom'>\
             <driver name='qemu' type='raw'/>\
             <source file='{source}'/>\
             <target dev='sda' bus='sata'/>\
             <boot order='{cdrom_boot}'/>\
             <readonly/>\
             </disk>",
            source = xml::escape(iso),
        ));
    }

    for nic in &spec.nics {
        xml.push_str(&render_interface(nic));
    }

    // GPU 直通：一个 <hostdev> 对应一个物理 PCI 设备。
    // managed='yes' 让 libvirt 在启动/停止时自动绑定/解绑 vfio-pci。
    for gpu in &spec.gpus {
        let address = gpu.address()?;
        xml.push_str(&format!(
            "<hostdev mode='subsystem' type='pci' managed='yes'><source><address domain='0x{:04x}' bus='0x{:02x}' slot='0x{:02x}' function='0x{:x}'/></source></hostdev>",
            address.domain, address.bus, address.slot, address.function
        ));
    }

    if spec.display.kind != DisplayKind::None {
        xml.push_str(&render_graphics(&spec.display));
        // Absolute pointer. Without it VNC mouse movement is relative, so the
        // cursor drifts away from whatever the user aims at.
        xml.push_str("<input type='tablet' bus='usb'/>");
    }

    // A pty serial console lets `virsh console <name>` work even headless.
    xml.push_str("<serial type='pty'><target port='0'/></serial>");
    xml.push_str("<console type='pty'><target type='serial' port='0'/></console>");
    xml.push_str("</devices></domain>");
    Ok(xml)
}

/// Whether this host can actually run KVM guests.
fn kvm_usable() -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_ok()
}

/// Remove every CD-ROM disk from a domain document.
///
/// `vm eject` uses this on the live definition, so the input is usually
/// libvirt's own `dumpxml` output rather than something we rendered.
pub fn strip_cdrom(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(pos) = rest.find("<disk") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos..];
        let Some(gt) = after.find('>') else {
            out.push_str(after);
            return finish_strip(out);
        };
        let tag = &after[..=gt];
        if xml::attr(tag, "device").as_deref() != Some("cdrom") {
            out.push_str(&after[..=gt]);
            rest = &after[gt + 1..];
            continue;
        }
        if tag.ends_with("/>") {
            rest = &after[gt + 1..];
        } else {
            let end = after
                .find("</disk>")
                .map(|p| p + "</disk>".len())
                .unwrap_or(after.len());
            rest = &after[end..];
        }
    }
    out.push_str(rest);
    finish_strip(out)
}

/// Fall back to the hard disk when the document booted from CD-ROM.
fn finish_strip(mut xml: String) -> String {
    if xml.contains("dev='cdrom'") {
        xml = xml.replace("<boot dev='cdrom'/>", "<boot dev='hd'/>");
    }
    xml
}

/// Paths of writable disks in a domain document (CD-ROMs excluded).
fn parse_disk_paths(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(pos) = rest.find("<disk") {
        let after = &rest[pos..];
        let Some(gt) = after.find('>') else { break };
        let tag = &after[..=gt];
        let end = if tag.ends_with("/>") {
            gt + 1
        } else {
            after
                .find("</disk>")
                .map(|p| p + "</disk>".len())
                .unwrap_or(after.len())
        };
        if xml::attr(tag, "device").as_deref().unwrap_or("disk") == "disk" {
            let block = &after[..end];
            if let Some(src) = block.find("<source") {
                if let Some(close) = block[src..].find('>') {
                    let source_tag = &block[src..=src + close];
                    if let Some(path) = xml::attr(source_tag, "file") {
                        out.push(path);
                    }
                }
            }
        }
        rest = &after[end..];
    }
    out
}

fn render_graphics(display: &DisplayConfig) -> String {
    let listen = display.listen.as_deref().unwrap_or("127.0.0.1");
    let address = xml::escape(listen);
    let passwd = match display.password.as_deref() {
        Some(pw) => format!(" passwd='{}'", xml::escape(pw)),
        None => String::new(),
    };
    format!(
        "<graphics type='{kind}' port='-1' autoport='yes' listen='{address}'{passwd}>\
         <listen type='address' address='{address}'/></graphics>",
        kind = display.kind.as_str(),
    )
}

fn render_interface(nic: &NicConfig) -> String {
    let (itype, source) = match &nic.source {
        NetSource::Bridge(bridge) => (
            "bridge",
            format!("<source bridge='{}'/>", xml::escape(bridge)),
        ),
        NetSource::Network(network) => (
            "network",
            format!("<source network='{}'/>", xml::escape(network)),
        ),
        NetSource::Direct { device, mode } => (
            "direct",
            format!(
                "<source dev='{}' mode='{}'/>",
                xml::escape(device),
                mode.as_str()
            ),
        ),
        NetSource::User => ("user", String::new()),
        // No connectivity: omitting the interface is how you express that.
        NetSource::None => return String::new(),
        // #[non_exhaustive] upstream: unknown future variants fall back to user-mode.
        _ => ("user", String::new()),
    };
    let mac = nic
        .mac
        .as_ref()
        .map(|m| format!("<mac address='{}'/>", xml::escape(m)))
        .unwrap_or_default();
    let target = nic
        .target_dev
        .as_ref()
        .map(|t| format!("<target dev='{}'/>", xml::escape(t)))
        .unwrap_or_default();
    format!(
        "<interface type='{itype}'>{mac}{source}<model type='{}'/>{target}</interface>",
        nic.model.as_str(),
    )
}

/// Extract `<graphics>` endpoints from a libvirt domain document.
fn parse_graphics(xml: &str) -> Vec<GraphicsInfo> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(pos) = rest.find("<graphics") {
        rest = &rest[pos..];
        let Some(gt) = rest.find('>') else { break };
        let tag = &rest[..=gt];

        let kind = match xml::attr(tag, "type").as_deref() {
            Some("vnc") => GraphicsKind::Vnc,
            Some("spice") => GraphicsKind::Spice,
            _ => GraphicsKind::Other,
        };

        let mut host = xml::attr(tag, "listen").unwrap_or_default();
        let password_value = xml::attr(tag, "passwd").filter(|pw| !pw.is_empty());
        let password = password_value.is_some();

        // `<listen type='address' address='...'/>` wins over the legacy attribute.
        let body_end = rest.find("</graphics>").unwrap_or(gt);
        let body = &rest[..body_end];
        if let Some(open) = body.find("<listen") {
            if let Some(close) = body[open..].find('>') {
                let listen_tag = &body[open..=open + close];
                if let Some(addr) = xml::attr(listen_tag, "address") {
                    host = addr;
                }
            }
        }

        // `autoport` is resolved by libvirt in dumpxml, so the port is normally
        // concrete; keep 0 for "not allocated yet" instead of a bogus u16.
        let port = xml::attr(tag, "port")
            .and_then(|p| p.parse::<i32>().ok())
            .filter(|p| (1..=u16::MAX as i32).contains(p))
            .map(|p| p as u16)
            .unwrap_or(0);

        out.push(GraphicsInfo {
            kind,
            host: if host.is_empty() {
                "127.0.0.1".to_owned()
            } else {
                host
            },
            port,
            password,
            password_value,
        });

        rest = &rest[gt + 1..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use virt_core::{parse_spec, GpuPassthrough, LoadRules};

    fn test_hv() -> Option<LibvirtHypervisor> {
        LibvirtHypervisor::connect_uri("test:///default").ok()
    }

    fn spec_with_disk(disk: &str) -> VmSpec {
        let mut spec = parse_spec("name = disk-vm\nvcpus = 1\nmemory_kib = 262144\n").unwrap();
        spec.disks.push(disk.to_owned());
        spec
    }

    #[test]
    fn formats_packed_version() {
        assert_eq!(format_libvirt_version(12_006_000), "12.6.0");
        assert_eq!(format_libvirt_version(10_000_010), "10.0.10");
        assert_eq!(format_libvirt_version(0), "0.0.0");
    }

    #[test]
    fn disk_targets_are_valid_libvirt_names() {
        assert_eq!(disk_target(0).unwrap(), "vda");
        assert_eq!(disk_target(25).unwrap(), "vdz");
        assert_eq!(disk_target(26).unwrap(), "vdaa");
        assert_eq!(disk_target(27).unwrap(), "vdab");
        assert_eq!(disk_target(51).unwrap(), "vdaz");
        assert!(disk_target(5000).is_err());
    }

    #[test]
    fn renders_disk_with_vd_prefix() {
        let xml = render_xml(&spec_with_disk("/tmp/a.qcow2")).unwrap();
        assert!(xml.contains("<target dev='vda' bus='virtio'/>"), "{xml}");
        assert!(xml.contains("type='qcow2'"), "{xml}");
        assert!(!xml.contains("dev='v' "), "{xml}");
    }

    #[test]
    fn renders_raw_for_img_extension() {
        let xml = render_xml(&spec_with_disk("/tmp/disk.img")).unwrap();
        assert!(xml.contains("type='raw'"), "{xml}");
    }

    #[test]
    fn escapes_hostile_values() {
        let mut spec = spec_with_disk("/tmp/it's a disk.qcow2");
        spec.name = "evil</name><hostdev/>".to_owned();
        spec.nics.push(NicConfig {
            source: NetSource::Bridge("br0'/><hostdev/>".to_owned()),
            ..NicConfig::default()
        });

        let xml = render_xml(&spec).unwrap();
        assert!(!xml.contains("<hostdev/>"), "{xml}");
        assert!(xml.contains("&lt;/name&gt;&lt;hostdev/&gt;"), "{xml}");
        assert!(xml.contains("it&apos;s a disk"), "{xml}");
        assert!(xml.contains("br0&apos;/&gt;&lt;hostdev/&gt;"), "{xml}");
    }

    #[test]
    fn renders_vnc_with_autoport_and_loopback() {
        let xml = render_xml(&spec_with_disk("/tmp/a.qcow2")).unwrap();
        assert!(
            xml.contains("<graphics type='vnc' port='-1' autoport='yes' listen='127.0.0.1'>"),
            "{xml}"
        );
        assert!(
            xml.contains("<listen type='address' address='127.0.0.1'/>"),
            "{xml}"
        );
        assert!(xml.contains("<serial type='pty'>"), "{xml}");
    }

    #[test]
    fn renders_boot_order_and_cdrom() {
        let mut spec = spec_with_disk("/tmp/a.qcow2");
        spec.cdrom = Some("/tmp/install.iso".into());
        spec.boot = virt_core::BootDevice::Cdrom;
        let xml = render_xml(&spec).unwrap();
        let cdrom = xml.find("device='cdrom'").expect("cdrom present");
        let cdrom_block = &xml[cdrom..];
        assert!(cdrom_block.contains("<boot order='1'/>"), "{xml}");
        assert!(cdrom_block.contains("<readonly/>"), "{xml}");
        assert!(xml.contains("<boot order='2'/>"), "{xml}");
        assert!(!xml.contains("<boot dev="), "{xml}");
    }

    #[test]
    fn accel_selects_domain_type() {
        let mut spec = spec_with_disk("/tmp/a.qcow2");
        spec.accel = virt_core::Accel::Tcg;
        assert!(render_xml(&spec).unwrap().starts_with("<domain type='qemu'>"));
        spec.accel = virt_core::Accel::Kvm;
        assert!(render_xml(&spec).unwrap().starts_with("<domain type='kvm'>"));
    }

    #[test]
    fn tablet_input_only_with_a_display() {
        let spec = spec_with_disk("/tmp/a.qcow2");
        assert!(render_xml(&spec).unwrap().contains("<input type='tablet'"));

        let mut headless = spec.clone();
        headless.display.kind = DisplayKind::None;
        assert!(!render_xml(&headless).unwrap().contains("<input"));
    }

    #[test]
    fn strips_cdrom_from_own_output_and_libvirt_dump() {
        let mut spec = spec_with_disk("/tmp/a.qcow2");
        spec.cdrom = Some("/tmp/install.iso".into());
        let rendered = render_xml(&spec).unwrap();
        let stripped = strip_cdrom(&rendered);
        assert!(!stripped.contains("device='cdrom'"), "{stripped}");
        assert!(stripped.contains("device='disk'"), "{stripped}");
        assert!(stripped.contains("</domain>"), "{stripped}");

        // libvirt dumpxml shape, including the legacy <boot dev='cdrom'/>.
        let dumped = "<domain><os><boot dev='cdrom'/></os><devices>\
            <disk type='file' device='disk'><source file='/d.qcow2'/></disk>\
            <disk type='file' device='cdrom'><source file='/i.iso'/></disk>\
            </devices></domain>";
        let stripped = strip_cdrom(dumped);
        assert!(!stripped.contains("device='cdrom'"), "{stripped}");
        assert!(stripped.contains("<boot dev='hd'/>"), "{stripped}");
        assert!(stripped.contains("/d.qcow2"), "{stripped}");
    }

    #[test]
    fn reads_disk_paths_from_dump() {
        let dumped = "<domain><devices>\
            <disk type='file' device='disk'><source file='/a b.qcow2'/></disk>\
            <disk type='file' device='disk'><source file='/c.raw'/></disk>\
            <disk type='file' device='cdrom'><source file='/i.iso'/></disk>\
            </devices></domain>";
        assert_eq!(parse_disk_paths(dumped), ["/a b.qcow2", "/c.raw"]);
    }

    #[test]
    fn display_none_omits_graphics() {
        let mut spec = spec_with_disk("/tmp/a.qcow2");
        spec.display.kind = DisplayKind::None;
        let xml = render_xml(&spec).unwrap();
        assert!(!xml.contains("<graphics"), "{xml}");
    }

    #[test]
    fn isolated_nic_renders_nothing() {
        let mut spec = spec_with_disk("/tmp/a.qcow2");
        spec.nics.push(NicConfig {
            source: NetSource::None,
            ..NicConfig::default()
        });
        let xml = render_xml(&spec).unwrap();
        assert!(!xml.contains("<interface"), "{xml}");
    }

    #[test]
    fn parses_graphics_endpoint() {
        let xml = "<domain><devices><graphics type='vnc' port='5901' autoport='yes' \
                   listen='127.0.0.1'><listen type='address' address='127.0.0.1'/></graphics>\
                   </devices></domain>";
        let found = parse_graphics(xml);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, GraphicsKind::Vnc);
        assert_eq!(found[0].host, "127.0.0.1");
        assert_eq!(found[0].port, 5901);
        assert_eq!(found[0].vnc_display(), Some(1));
        assert!(!found[0].password);
    }

    #[test]
    fn parses_spice_with_password() {
        let xml = "<graphics type='spice' port='5900' passwd='s3cret' listen='0.0.0.0'/>";
        let found = parse_graphics(xml);
        assert_eq!(found[0].kind, GraphicsKind::Spice);
        assert!(found[0].password);
        assert_eq!(found[0].password_value.as_deref(), Some("s3cret"));
        assert_eq!(found[0].host, "0.0.0.0");
    }

    #[test]
    fn renders_single_gpu_hostdev() {
        let mut spec = spec_with_disk("/tmp/a.qcow2");
        spec.gpus = vec![GpuPassthrough::new("0000:01:00.0").unwrap()];
        let xml = render_xml(&spec).unwrap();
        assert!(
            xml.contains(
                "<hostdev mode='subsystem' type='pci' managed='yes'><source><address domain='0x0000' bus='0x01' slot='0x00' function='0x0'/></source></hostdev>"
            ),
            "{xml}"
        );
    }

    #[test]
    fn renders_multiple_gpu_hostdevs() {
        let mut spec = spec_with_disk("/tmp/a.qcow2");
        spec.gpus = vec![
            GpuPassthrough::new("0000:01:00.0").unwrap(),
            GpuPassthrough::new("0000:41:00.1").unwrap(),
        ];
        let xml = render_xml(&spec).unwrap();
        assert!(xml.contains("bus='0x01' slot='0x00' function='0x0'"), "{xml}");
        assert!(xml.contains("bus='0x41' slot='0x00' function='0x1'"), "{xml}");
        assert_eq!(xml.matches("<hostdev").count(), 2, "{xml}");
    }

    #[test]
    fn rejects_invalid_gpu_pci_address() {
        let mut spec = spec_with_disk("/tmp/a.qcow2");
        // 绕过 GpuPassthrough::new 的校验，模拟配置/反序列化注入的坏地址。
        spec.gpus = vec![GpuPassthrough {
            pci_address: "0000:zz:00.0".into(),
        }];
        assert!(render_xml(&spec).is_err());
    }

    #[test]
    fn spec_loader_roundtrip() {
        let Some(hv) = test_hv() else {
            eprintln!("跳过：test:///default 驱动不可用");
            return;
        };
        let spec = parse_spec("name = loadtest\nvcpus = 1\nmemory_kib = 262144\n").unwrap();
        let rules = LoadRules {
            require_disks_exist: false,
            ..Default::default()
        };
        let dom = hv.load(&spec, &rules).expect("load");
        assert_eq!(dom.name(), "loadtest");
        assert_eq!(dom.state().unwrap(), DomainState::ShutOff);
        dom.start().expect("start");
        assert_eq!(dom.state().unwrap(), DomainState::Running);
        dom.destroy().expect("destroy");
        dom.undefine().expect("undefine");
    }

    #[test]
    fn defines_disk_domain_for_real() {
        // Real qemu driver: this is the case that used to fail with
        // "unknown disk name 'v'". Skipped when there is no libvirtd.
        let Ok(hv) = LibvirtHypervisor::connect_uri("qemu:///session") else {
            eprintln!("跳过：qemu:///session 不可用（无 libvirtd 用户会话）");
            return;
        };
        let image = std::env::temp_dir().join("virt-crate-test-disk.qcow2");
        let _ = std::fs::write(&image, b"");
        let name = format!("virt-crate-test-{}", std::process::id());
        let mut spec = spec_with_disk(image.to_str().unwrap());
        spec.name = name.clone();

        let dom = hv.load(&spec, &LoadRules::default()).expect("define with disk");
        let info = dom.info().expect("info");
        assert_eq!(info.name, name);
        dom.undefine().expect("undefine");
        let _ = std::fs::remove_file(&image);
    }

    #[test]
    fn rules_block_bad_spec() {
        let hv = test_hv().expect("connect");
        let spec = parse_spec("name = bad\nvcpus = 999999\n").unwrap();
        let err = hv.load(&spec, &LoadRules::default()).unwrap_err();
        assert!(matches!(err, VirtError::InvalidConfig(_)));
    }

    /// End-to-end on a real QEMU driver: create an image, define, boot, take a
    /// screenshot, stop and undefine.
    ///
    /// Ignored by default because it boots a real guest; run it on a machine
    /// with libvirtd + qemu:
    /// `cargo test -p virt-libvirt -- --ignored full_lifecycle`
    #[test]
    #[ignore = "boots a real QEMU guest; needs libvirtd + qemu"]
    fn full_lifecycle_with_real_qemu() {
        use std::time::{Duration, Instant};

        /// 测试中途 panic 时也别把运行中的域和临时磁盘留在宿主机上。
        struct Cleanup {
            dom: Option<LibvirtDomain>,
            dir: std::path::PathBuf,
        }

        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Some(dom) = &self.dom {
                    let _ = dom.destroy();
                    let _ = dom.undefine();
                }
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }

        let name = format!("virt-e2e-{}", std::process::id());
        let dir = std::env::temp_dir().join(&name);
        let disk = dir.join("disk.qcow2");
        let mut cleanup = Cleanup {
            dom: None,
            dir: dir.clone(),
        };
        let hv = match LibvirtHypervisor::connect_uri("qemu:///session") {
            Ok(hv) => hv,
            Err(error) => {
                eprintln!("跳过：qemu:///session 不可用（{error}）");
                return;
            }
        };

        create_disk(&disk, 64 << 20, DiskFormat::Qcow2).expect("create qcow2");
        let mut spec = parse_spec("name = placeholder\nvcpus = 1\nmemory_kib = 262144\n").unwrap();
        spec.name = name.clone();
        spec.disks.push(disk.to_string_lossy().into_owned());
        spec.accel = virt_core::Accel::Auto;
        spec.autostart = true;

        let dom = hv
            .load(&spec, &LoadRules::default())
            .expect("define with disk");
        cleanup.dom = Some(dom.clone());
        assert!(dom.persistent().expect("persistent"));
        assert!(dom.autostart().expect("autostart"));
        dom.set_autostart(false).expect("disable autostart");
        assert_eq!(dom.disk_paths().unwrap(), [disk.to_string_lossy()]);

        dom.start().expect("start");
        assert!(dom.state().unwrap().is_active());
        assert_eq!(dom.graphics().unwrap().len(), 1, "VNC configured");

        // The port shows up once QEMU has bound it; give the guest a moment.
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut endpoint = None;
        while Instant::now() < deadline {
            if let Some(gfx) = dom.graphics().unwrap().into_iter().next() {
                if gfx.port != 0 {
                    endpoint = Some(gfx);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let endpoint = endpoint.expect("VNC port allocated");
        assert_eq!(endpoint.kind, GraphicsKind::Vnc);

        // SeaBIOS draws "no bootable device" immediately, so a screenshot
        // should succeed within a few seconds.
        let mut image = Vec::new();
        while Instant::now() < deadline && image.is_empty() {
            match dom.screenshot() {
                Ok(bytes) => image = bytes,
                Err(error) => {
                    eprintln!("截图暂不可用：{error}");
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
        let is_image = image.starts_with(b"P6") || image.starts_with(&[0x89, b'P', b'N', b'G']);
        assert!(
            is_image,
            "expected PNG or PNM, got {:?} ({} bytes)",
            &image[..image.len().min(16)],
            image.len()
        );

        dom.destroy().expect("destroy");
        dom.undefine().expect("undefine");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
