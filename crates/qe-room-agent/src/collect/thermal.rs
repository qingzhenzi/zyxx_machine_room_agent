//! 温度采集：`/sys/class/hwmon/*/temp*_input`（毫摄氏度）。
//!
//! 容器/虚拟机里通常没有 hwmon，读不到就返回空集合，不影响上报。

use std::fs;
use std::path::Path;

use serde::Serialize;

/// 一个温度传感器读数。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ThermalSensor {
    /// 传感器标识，如 `nvme/temp2`、`k10temp/temp1`。
    pub sensor: String,
    /// 摄氏度。
    pub temp_c: f64,
}

/// hwmon 毫摄氏度转摄氏度，并丢弃明显不合理的读数。
pub fn parse_millidegrees(raw: &str) -> Option<f64> {
    let millidegrees = raw.trim().parse::<f64>().ok()?;
    if !millidegrees.is_finite() || millidegrees <= 0.0 {
        return None;
    }
    let temp_c = millidegrees / 1000.0;
    (temp_c <= 200.0).then_some(temp_c)
}

/// 采集所有 hwmon 温度传感器；任何目录读失败都跳过。
pub fn collect() -> Vec<ThermalSensor> {
    collect_from(Path::new("/sys/class/hwmon"))
}

/// 从指定 hwmon 根目录采集（便于测试）。
pub fn collect_from(hwmon_root: &Path) -> Vec<ThermalSensor> {
    let Ok(entries) = fs::read_dir(hwmon_root) else {
        return Vec::new();
    };
    let mut sensors = Vec::new();
    for entry in entries.flatten() {
        let hwmon_path = entry.path();
        let hwmon_name = fs::read_to_string(hwmon_path.join("name"))
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
        let Ok(files) = fs::read_dir(&hwmon_path) else {
            continue;
        };
        for file in files.flatten() {
            let file_name = file.file_name();
            let file_name = file_name.to_string_lossy();
            let Some(label) = file_name.strip_suffix("_input") else {
                continue;
            };
            if !label.starts_with("temp") {
                continue;
            }
            let Ok(raw) = fs::read_to_string(file.path()) else {
                continue;
            };
            let Some(temp_c) = parse_millidegrees(&raw) else {
                continue;
            };
            sensors.push(ThermalSensor {
                sensor: format!("{hwmon_name}/{label}"),
                temp_c,
            });
        }
    }
    sensors.sort_by(|left, right| left.sensor.cmp(&right.sensor));
    sensors
}

/// 所有传感器中的最高温度（摄氏度）。
pub fn max_temperature(sensors: &[ThermalSensor]) -> Option<f64> {
    sensors.iter().map(|sensor| sensor.temp_c).reduce(f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millidegrees_are_converted() {
        assert_eq!(parse_millidegrees("58850\n"), Some(58.85));
        assert_eq!(parse_millidegrees("0"), None);
        assert_eq!(parse_millidegrees("-5000"), None);
        assert_eq!(parse_millidegrees("500000"), None);
        assert_eq!(parse_millidegrees("garbage"), None);
    }

    #[test]
    fn max_temperature_picks_hottest_sensor() {
        let sensors = vec![
            ThermalSensor {
                sensor: "k10temp/temp1".to_owned(),
                temp_c: 81.4,
            },
            ThermalSensor {
                sensor: "nvme/temp2".to_owned(),
                temp_c: 86.9,
            },
        ];
        assert_eq!(max_temperature(&sensors), Some(86.9));
        assert_eq!(max_temperature(&[]), None);
    }

    #[test]
    fn collect_from_parses_hwmon_layout() {
        let root =
            std::env::temp_dir().join(format!("qe-room-agent-hwmon-test-{}", std::process::id()));
        let hwmon = root.join("hwmon3");
        fs::create_dir_all(&hwmon).unwrap();
        fs::write(hwmon.join("name"), "nvme\n").unwrap();
        fs::write(hwmon.join("temp1_input"), "58850\n").unwrap();
        fs::write(hwmon.join("temp2_input"), "86850\n").unwrap();
        let sensors = collect_from(&root);
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(
            sensors,
            vec![
                ThermalSensor {
                    sensor: "nvme/temp1".to_owned(),
                    temp_c: 58.85,
                },
                ThermalSensor {
                    sensor: "nvme/temp2".to_owned(),
                    temp_c: 86.85,
                },
            ]
        );
    }

    #[test]
    fn missing_root_is_empty() {
        assert!(collect_from(Path::new("/nonexistent/hwmon")).is_empty());
    }
}
