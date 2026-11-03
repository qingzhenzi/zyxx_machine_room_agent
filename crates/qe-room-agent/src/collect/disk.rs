//! 磁盘采集：`df -B1`（coreutils，带 15s 硬超时）。

use std::time::Duration;

use serde::Serialize;

use crate::process;

/// 外部命令统一超时。
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// 一个挂载点的磁盘占用。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DiskInfo {
    /// 挂载点，如 `/`。
    pub mount_point: String,
    /// 设备/文件系统名。
    pub filesystem: String,
    /// 总容量（字节）。
    pub total_bytes: u64,
    /// 已用（字节）。
    pub used_bytes: u64,
    /// 可用（字节）。
    pub available_bytes: u64,
    /// 使用率（0–100）。
    pub usage_percent: f64,
}

/// 解析 `df -B1` 输出（跳过表头）。
pub fn parse_df(output: &str) -> Vec<DiskInfo> {
    let mut disks = Vec::new();
    for line in output.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // 至少 6 列：Filesystem 1B-blocks Used Available Use% Mounted-on
        if fields.len() < 6 {
            continue;
        }
        let Some(total_bytes) = fields[1].parse::<u64>().ok() else {
            continue;
        };
        if total_bytes == 0 {
            continue;
        }
        let used_bytes = fields[2].parse::<u64>().unwrap_or(0);
        let available_bytes = fields[3].parse::<u64>().unwrap_or(0);
        let usage_percent = fields[4]
            .strip_suffix('%')
            .and_then(|raw| raw.parse::<f64>().ok())
            .unwrap_or_else(|| used_bytes as f64 / total_bytes as f64 * 100.0)
            .clamp(0.0, 100.0);
        // 挂载点理论上可能含空格，用剩余列拼回。
        let mount_point = fields[5..].join(" ");
        disks.push(DiskInfo {
            mount_point,
            filesystem: fields[0].to_owned(),
            total_bytes,
            used_bytes,
            available_bytes,
            usage_percent,
        });
    }
    disks
}

/// 采集本机磁盘占用；`df` 不存在/超时/失败时返回空集合。
pub fn collect() -> Vec<DiskInfo> {
    match process::run_with_timeout("df", &["-B1".to_owned()], COMMAND_TIMEOUT) {
        Ok(output) if output.success => parse_df(&output.stdout),
        Ok(output) => {
            tracing::debug!(
                status = output.code,
                stderr = %output.stderr.trim(),
                "df 采集失败，跳过磁盘明细"
            );
            Vec::new()
        }
        Err(error) => {
            tracing::debug!(error = %error, "df 无法执行，跳过磁盘明细");
            Vec::new()
        }
    }
}

/// 代表整机磁盘使用率：优先根分区 `/`，否则取最满的挂载点。
pub fn representative_usage(disks: &[DiskInfo]) -> Option<f64> {
    disks
        .iter()
        .find(|disk| disk.mount_point == "/")
        .map(|disk| disk.usage_percent)
        .or_else(|| disks.iter().map(|disk| disk.usage_percent).reduce(f64::max))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DF_OUTPUT: &str = "\
Filesystem     1B-blocks         Used    Available Use% Mounted on
/dev/nvme0n1p6 501437431808 488944807936 12492623872  98% /
/dev/nvme0n1p7   535805952     19800064    516005888   4% /boot/efi
tmpfs          8123456789            0   8123456789   0% /dev/shm
";

    #[test]
    fn parses_df_rows() {
        let disks = parse_df(DF_OUTPUT);
        assert_eq!(disks.len(), 3);
        assert_eq!(disks[0].mount_point, "/");
        assert_eq!(disks[0].filesystem, "/dev/nvme0n1p6");
        assert_eq!(disks[0].total_bytes, 501_437_431_808);
        assert_eq!(disks[0].usage_percent, 98.0);
        assert_eq!(disks[1].mount_point, "/boot/efi");
        assert_eq!(representative_usage(&disks), Some(98.0));
    }

    #[test]
    fn zero_total_rows_are_skipped() {
        let output = "\
Filesystem 1B-blocks Used Available Use% Mounted on
overlay 0 0 0 0% /var/lib/docker/overlayfs/abc
";
        assert!(parse_df(output).is_empty());
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let output = "\
Filesystem 1B-blocks Used Available Use% Mounted on
/dev/sda1 not-a-number 1 2 3% /
/dev/sdb1 100 50 50 50% /data
";
        let disks = parse_df(output);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].mount_point, "/data");
        assert_eq!(representative_usage(&disks), Some(50.0));
    }

    #[test]
    fn representative_usage_falls_back_to_fullest_mount() {
        let output = "\
Filesystem 1B-blocks Used Available Use% Mounted on
/dev/sda1 100 50 50 50% /data
/dev/sdb1 100 90 10 90% /srv
";
        let disks = parse_df(output);
        assert_eq!(representative_usage(&disks), Some(90.0));
        assert_eq!(representative_usage(&[]), None);
    }

    #[test]
    fn mount_point_with_spaces_is_reconstructed() {
        let output = "\
Filesystem 1B-blocks Used Available Use% Mounted on
/dev/sda1 100 50 50 50% /mnt/my disk
";
        let disks = parse_df(output);
        assert_eq!(disks[0].mount_point, "/mnt/my disk");
    }
}
