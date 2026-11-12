//! Container / VM inventory — shells out to `docker`, `podman` and `virsh`.
//!
//! A host with no containers (or no runtimes installed) is perfectly healthy,
//! so every outcome here is `Collected::ok` with zeros / `None`; this domain
//! never reports `missing_tool`.
use crate::Options;
use crate::model::ContainerInfo;
use crate::section::Collected;

#[cfg(not(target_os = "linux"))]
pub(super) fn collect(_opts: &Options) -> Collected<ContainerInfo> {
    Collected::unsupported("container inventory is Linux-only")
}

#[cfg(target_os = "linux")]
pub(super) fn collect(_opts: &Options) -> Collected<ContainerInfo> {
    // Docker may be installed but its daemon stopped; a failed invocation then
    // counts as "nothing running" while the image count stays unknown.
    let docker_running = count_lines("docker", &["ps", "-q"]).unwrap_or(0);
    let docker_images = count_lines("docker", &["images", "-q"]);

    let podman_running = count_lines("podman", &["ps", "-q"]);

    let libvirt_total = count_lines("virsh", &["list", "--all", "--name"]);
    let libvirt_running = count_lines("virsh", &["list", "--name"]);

    Collected::ok(ContainerInfo {
        docker_running,
        docker_images,
        podman_running,
        libvirt_total,
        libvirt_running,
    })
}

/// Count non-empty stdout lines of `<cmd> <args...>`.
///
/// `None` means the binary is not in `PATH` (spawn failed); any other result —
/// including a non-zero exit — yields the number of lines it printed.
#[cfg(target_os = "linux")]
fn count_lines(cmd: &str, args: &[&str]) -> Option<usize> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count(),
    )
}
