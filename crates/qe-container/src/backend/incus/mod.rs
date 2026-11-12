//! Incus backend: maps the Docker-shaped [`Backend`] contract onto the Incus
//! REST API (`/var/lib/incus/unix.socket`, `http://incus/1.0/...`).
//!
//! The Incus model differs from Docker in a few structural ways. The mapping
//! used throughout this module (validated against a live Incus 7.5 server) is:
//!
//! | Docker concept | Incus equivalent |
//! |---|---|
//! | labels | `user.<key>` config entries |
//! | env | `environment.<key>` config entries |
//! | entrypoint + command | `raw.lxc: lxc.init.cmd` wrapped in `sh -c` |
//! | published ports | `proxy` devices (`bind: host`) |
//! | bind mounts | `disk` devices (`source`/`path`/`readonly`) |
//! | named volumes | custom storage volumes attached as `pool`/`source` disks |
//! | network attach | `nic` devices with `network` / `ipv4.address` |
//! | root read-only | root `disk` device `readonly: "true"` |
//! | tmpfs | `raw.lxc: lxc.mount.entry = tmpfs ...` |
//! | ulimits | `limits.kernel.<name> = soft:hard` |
//! | resource limits | `limits.memory` / `limits.cpu[.allowance]` / `limits.processes` |
//! | logs | wrapper files `/qe-init.out` + `/qe-init.err` via the files API (console as fallback) |
//! | exec | `POST .../exec` with `record-output`, polled output files |
//! | exit code | `/qe-exit-code` file written by the init wrapper |
//!
//! Known limitations (documented, deliberate):
//! - `security.no_new_privileges` and `log` rotation settings have no Incus
//!   equivalent and are ignored.
//! - `pull_image` cannot pull from a registry; images are imported with
//!   `incus image import` (see `qe-agent-node`'s importer).
//! - Init exit codes are only available when `command`/`entrypoint` is set (the
//!   wrapper records them); containers using the image default init report `0`.
//! - `exec` `user` must be numeric; `tty` is recorded as non-interactive output.

use std::time::Duration;

mod backend;
mod client;
mod mapping;
mod streams;
mod transport;
mod wire;

#[cfg(test)]
mod tests;

pub use self::backend::IncusBackend;

/// Default Incus unix socket.
pub const DEFAULT_INCUS_SOCKET: &str = "/var/lib/incus/unix.socket";

/// Paths (inside the container) used by the init wrapper.
const EXIT_CODE_FILE: &str = "/qe-exit-code";
const INIT_STDOUT_FILE: &str = "/qe-init.out";
const INIT_STDERR_FILE: &str = "/qe-init.err";

const POLL_INTERVAL: Duration = Duration::from_millis(300);
