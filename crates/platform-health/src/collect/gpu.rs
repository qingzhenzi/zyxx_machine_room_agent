//! NVIDIA GPU collector via `nvml-wrapper` (feature `gpu`).
//!
//! NVML needs the NVIDIA driver loaded; on machines without one (or without
//! the driver) `Nvml::init` fails and we report the section unsupported rather
//! than erroring. Individual metrics are best-effort so a partial driver still
//! yields a usable snapshot.

use crate::Options;
use crate::model::Gpu;
use crate::section::Collected;

#[cfg(target_os = "linux")]
pub(super) fn collect(_opts: &Options) -> Collected<Vec<Gpu>> {
    use nvml_wrapper::Nvml;
    use nvml_wrapper::enum_wrappers::device::TemperatureSensor;

    let nvml = match Nvml::init() {
        Ok(n) => n,
        Err(e) => {
            return Collected::unsupported(format!("NVML unavailable (no NVIDIA driver?): {e}"));
        }
    };

    let count = match nvml.device_count() {
        Ok(c) => c,
        Err(e) => return Collected::err(format!("device_count failed: {e}")),
    };

    let driver = nvml.sys_driver_version().ok();
    let mut gpus = Vec::with_capacity(count as usize);
    for index in 0..count {
        let Ok(device) = nvml.device_by_index(index) else {
            continue;
        };
        gpus.push(Gpu {
            index,
            name: device.name().unwrap_or_else(|_| "unknown".to_string()),
            utilization_percent: device.utilization_rates().ok().map(|u| u.gpu as f32),
            memory_total_bytes: device.memory_info().ok().map(|m| m.total),
            memory_used_bytes: device.memory_info().ok().map(|m| m.used),
            temperature_c: device
                .temperature(TemperatureSensor::Gpu)
                .ok()
                .map(|t| t as f32),
            // milliwatts → watts
            power_draw_w: device.power_usage().ok().map(|mw| mw as f32 / 1000.0),
            fan_speed_percent: device.fan_speed(0).ok().map(|f| f as f32),
            driver_version: driver.clone(),
        });
    }

    Collected::ok(gpus)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn collect(_opts: &Options) -> Collected<Vec<Gpu>> {
    Collected::unsupported("NVML GPU collection is Linux-only for now")
}
