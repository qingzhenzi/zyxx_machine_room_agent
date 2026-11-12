//! Snapshot orchestrator.
//!
//! Wires the always-on core collectors and every optional domain, timing each
//! section and recording why a domain is absent (feature off, unsupported
//! platform, missing tool, permission denied). The health verdict is computed
//! by the caller ([`crate::snapshot_with`]) after assembly.

mod core;

#[cfg(feature = "battery")]
mod battery;
#[cfg(feature = "clock")]
mod clock;
#[cfg(feature = "containers")]
mod containers;
#[cfg(feature = "cpu-ext")]
mod cpu_ext;
#[cfg(feature = "display")]
mod display;
#[cfg(feature = "edac")]
mod edac;
#[cfg(feature = "firmware")]
mod firmware;
#[cfg(feature = "gpu")]
mod gpu;
#[cfg(feature = "packages")]
mod packages;
#[cfg(feature = "ram")]
mod ram;
#[cfg(feature = "security")]
mod security;
#[cfg(feature = "smart")]
mod smart;
#[cfg(feature = "wifi")]
mod wifi;

use crate::model::Snapshot;
use crate::section::{Collected, SectionMeta, timed};
use crate::{Options, SCHEMA_VERSION, health, time};

/// Run an optional domain: skip it (recording `disabled`) when the feature is
/// off or the caller asked for a core-only snapshot, otherwise time it.
fn optional_domain<T>(
    name: &str,
    enabled: bool,
    sections: &mut Vec<SectionMeta>,
    f: impl FnOnce() -> Collected<T>,
) -> Option<T> {
    if !enabled {
        sections.push(SectionMeta::failed(
            name,
            0,
            "disabled",
            "feature off or core-only",
        ));
        return None;
    }
    let (value, meta) = timed(name, f);
    sections.push(meta);
    value
}

/// Wire one optional domain behind its feature gate. When the feature is not
/// compiled, record a `disabled` section and yield `None`.
macro_rules! optional {
    ($name:literal, $feature:literal, $enabled:expr, $sections:expr, $call:expr) => {{
        #[cfg(feature = $feature)]
        {
            optional_domain($name, $enabled, $sections, $call)
        }
        #[cfg(not(feature = $feature))]
        {
            let _ = $enabled;
            $sections.push(SectionMeta::failed(
                $name,
                0,
                "disabled",
                concat!("feature ", $feature, " off"),
            ));
            None
        }
    }};
}

/// Build a snapshot. The caller fills in `health` afterwards; the placeholder
/// here is never surfaced.
pub(crate) fn collect(opts: &Options) -> Snapshot {
    let s = core::setup(opts);
    let hostname = sysinfo::System::host_name();
    let machine = core::machine(opts.privacy);
    let identity = core::identity(opts.privacy, hostname.as_deref(), &s.networks);
    let now = time::now_unix();

    let mut sections: Vec<SectionMeta> = Vec::new();

    // ---- Always-on core domains (timed for parity with optional sections) ----
    // These always produce a value, so time them directly rather than through
    // the `Option`-returning optional path.
    macro_rules! core_domain {
        ($name:literal, $call:expr) => {{
            let start = std::time::Instant::now();
            let value = $call;
            sections.push(SectionMeta::ok($name, start.elapsed().as_millis() as u64));
            value
        }};
    }

    let cpu = core_domain!("cpu", core::cpu(&s.sys));
    let memory = core_domain!("memory", core::memory(&s.sys));
    let swap = core_domain!("swap", core::swap(&s.sys));
    let disks = core_domain!("disks", core::disks(&s.disks));
    let networks = core_domain!("networks", core::networks(opts.privacy, &s.networks));
    let components = core_domain!("components", core::components(&s.components));

    let optional = opts.include_optional;

    // ---- Optional inventory domains ----
    let users = optional!("users", "users", optional, &mut sections, || {
        core::users(opts.privacy)
    });
    let displays = optional!("display", "display", optional, &mut sections, || {
        display::collect(opts)
    });
    let cpu_ext = optional!("cpu-ext", "cpu-ext", optional, &mut sections, || {
        cpu_ext::collect(opts)
    });
    let battery = optional!("battery", "battery", optional, &mut sections, || {
        battery::collect(opts)
    });
    let firmware = optional!("firmware", "firmware", optional, &mut sections, || {
        firmware::collect(opts)
    });
    let ram_slots = optional!("ram", "ram", optional, &mut sections, || {
        ram::collect(opts)
    });
    let edac = optional!("edac", "edac", optional, &mut sections, || {
        edac::collect(opts)
    });
    let wifi = optional!("wifi", "wifi", optional, &mut sections, || {
        wifi::collect(opts)
    });
    let clock = optional!("clock", "clock", optional, &mut sections, || {
        clock::collect(opts)
    });
    let gpus = optional!("gpu", "gpu", optional, &mut sections, || {
        gpu::collect(opts)
    });
    let smart = optional!("smart", "smart", optional, &mut sections, || {
        smart::collect(opts)
    });
    let security = optional!("security", "security", optional, &mut sections, || {
        security::collect(opts)
    });
    let containers = optional!("containers", "containers", optional, &mut sections, || {
        containers::collect(opts)
    });
    let packages = optional!("packages", "packages", optional, &mut sections, || {
        packages::collect(opts)
    });

    // ---- Processes (opt-in) ----
    let processes = if opts.include_processes {
        let start = std::time::Instant::now();
        let p = core::processes(&s.sys, opts.top_processes, opts.privacy);
        sections.push(SectionMeta::ok(
            "processes",
            start.elapsed().as_millis() as u64,
        ));
        Some(p)
    } else {
        None
    };

    Snapshot {
        schema: SCHEMA_VERSION.to_string(),
        collected_at: time::rfc3339(now),
        collected_at_unix: now,
        identity,
        host: core::host(),
        machine,
        sections,
        cpu,
        memory,
        swap,
        disks,
        networks,
        components,
        displays,
        users,
        gpus,
        battery,
        firmware,
        cpu_ext,
        ram_slots,
        edac,
        wifi,
        clock,
        smart,
        security,
        containers,
        packages,
        processes,
        health: health::HealthReport {
            status: health::Status::Healthy,
            score: 100,
            issues: Vec::new(),
        },
    }
}
