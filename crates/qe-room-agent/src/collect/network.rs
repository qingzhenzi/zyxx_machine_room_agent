//! 网络接口采集：`/proc/net/dev` 累计流量。

use std::fs;

use serde::Serialize;

/// 一个网络接口的累计收发统计。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InterfaceStats {
    /// 接口名，如 `wlan0`。
    pub interface: String,
    /// 累计接收字节。
    pub rx_bytes: u64,
    /// 累计发送字节。
    pub tx_bytes: u64,
    /// 累计接收包数。
    pub rx_packets: u64,
    /// 累计发送包数。
    pub tx_packets: u64,
    /// 接收错误数。
    pub rx_errors: u64,
    /// 发送错误数。
    pub tx_errors: u64,
    /// 接收丢包数。
    pub rx_dropped: u64,
    /// 发送丢包数。
    pub tx_dropped: u64,
}

/// 解析 `/proc/net/dev`（跳过两行表头）。
pub fn parse_proc_net_dev(content: &str) -> Vec<InterfaceStats> {
    let mut interfaces = Vec::new();
    for line in content.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // Receive: bytes packets errs drop fifo frame compressed multicast
        // Transmit: bytes packets errs drop fifo colls carrier compressed
        if fields.len() < 16 {
            continue;
        }
        let number = |index: usize| -> u64 {
            fields
                .get(index)
                .and_then(|raw| raw.parse::<u64>().ok())
                .unwrap_or(0)
        };
        interfaces.push(InterfaceStats {
            interface: name.to_owned(),
            rx_bytes: number(0),
            rx_packets: number(1),
            rx_errors: number(2),
            rx_dropped: number(3),
            tx_bytes: number(8),
            tx_packets: number(9),
            tx_errors: number(10),
            tx_dropped: number(11),
        });
    }
    interfaces
}

/// 采集本机网络接口；失败返回空集合。
pub fn collect() -> Vec<InterfaceStats> {
    fs::read_to_string("/proc/net/dev")
        .ok()
        .map(|content| parse_proc_net_dev(&content))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROC_NET_DEV: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 196940492  245979    0    0    0     0          0         0 196940492  245979    0    0    0     0       0          0
 wlan0: 1274450820 1004759    2    7    0     0          0         0 3527655707 3224920    0    6    0     0       0          0
";
    #[test]
    fn parses_interfaces_and_counters() {
        let interfaces = parse_proc_net_dev(PROC_NET_DEV);
        assert_eq!(interfaces.len(), 2);
        assert_eq!(interfaces[0].interface, "lo");
        assert_eq!(interfaces[0].rx_bytes, 196_940_492);
        assert_eq!(interfaces[0].tx_bytes, 196_940_492);
        assert_eq!(interfaces[1].interface, "wlan0");
        assert_eq!(interfaces[1].rx_bytes, 1_274_450_820);
        assert_eq!(interfaces[1].rx_packets, 1_004_759);
        assert_eq!(interfaces[1].rx_errors, 2);
        assert_eq!(interfaces[1].rx_dropped, 7);
        assert_eq!(interfaces[1].tx_bytes, 3_527_655_707);
        assert_eq!(interfaces[1].tx_packets, 3_224_920);
        assert_eq!(interfaces[1].tx_dropped, 6);
    }

    #[test]
    fn malformed_rows_are_skipped() {
        let content = "\
Inter-|   Receive |  Transmit
 face |bytes ...
broken-line-without-colon
eth0: 1 2 3
";
        assert!(parse_proc_net_dev(content).is_empty());
    }

    #[test]
    fn header_lines_do_not_contain_colon() {
        assert_eq!(parse_proc_net_dev(PROC_NET_DEV).len(), 2);
    }
}
