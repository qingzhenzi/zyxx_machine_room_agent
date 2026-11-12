//! edac collector: correctable/uncorrectable memory-error counters from the
//! Linux EDAC sysfs tree.
//!
//! An absent tree simply means the host reports no ECC (no hardware, or the
//! `edac` module is not loaded), which yields an all-zero [`Edac`] rather
//! than an error.

use crate::Options;
use crate::model::Edac;
use crate::section::Collected;

#[allow(unused_variables)]
pub(super) fn collect(opts: &Options) -> Collected<Edac> {
    #[cfg(target_os = "linux")]
    {
        let Ok(entries) = std::fs::read_dir("/sys/devices/system/edac") else {
            // No EDAC on this host: zeros are a meaningful "no ECC reporting".
            return Collected::ok(Edac::default());
        };

        let mut edac = Edac::default();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            // Only `mcN` memory controllers (the tree also holds uevent/power).
            let index = name.strip_prefix("mc").unwrap_or_default();
            if index.is_empty() || !path.is_dir() {
                continue;
            }
            edac.controllers += 1;
            // Missing or unreadable counters simply count as zero.
            edac.correctable_total += read_counter(path.join("ce_count"));
            edac.uncorrectable_total += read_counter(path.join("ue_count"));
        }
        Collected::ok(edac)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Collected::unsupported("EDAC is Linux-only")
    }
}

/// Read one sysfs counter; absent/garbage values count as 0.
#[cfg(target_os = "linux")]
fn read_counter(path: std::path::PathBuf) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}
