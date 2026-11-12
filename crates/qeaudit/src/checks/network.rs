//! 网络：网卡状态、出网连通性、监听端口、防火墙、DNS、内核网络加固参数。

use crate::check::{Check, Ctx};
use crate::model::{Finding, Severity};
use crate::util;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

pub struct NetworkCheck;

impl Check for NetworkCheck {
    fn id(&self) -> &'static str {
        "network"
    }

    fn title(&self) -> &'static str {
        "网络连接与暴露面"
    }

    fn run(&self, cx: &Ctx<'_>) -> Vec<Finding> {
        let cfg = &cx.cfg.network;
        vec![
            check_interfaces(cfg),
            check_connectivity(cfg),
            check_listen_ports(cfg),
            check_sysctl(cfg),
            check_dns(),
            check_firewall(cfg),
        ]
    }
}

fn check_interfaces(cfg: &crate::config::NetworkConfig) -> Finding {
    let f = Finding::new("NET-01", "网络接口处于工作状态", Severity::High);

    let mut up = 0usize;
    let mut evidence = Vec::new();

    for iface in util::list_dir("/sys/class/net") {
        if iface == "lo" {
            continue;
        }
        let base = format!("/sys/class/net/{iface}");
        let oper = util::read_trim(format!("{base}/operstate")).unwrap_or_else(|| "unknown".into());
        let mac = util::read_trim(format!("{base}/address")).unwrap_or_default();
        evidence.push(format!("{iface}: state={oper} mac={mac}"));
        if oper == "up" {
            up += 1;
        }
    }

    if evidence.is_empty() {
        return f
            .fail("没有任何非 loopback 网卡")
            .fix("检查网卡驱动与 rfkill");
    }
    if up < cfg.min_up_interfaces {
        f.fail(format!(
            "仅 {up} 个网卡 up，低于策略要求的 {}",
            cfg.min_up_interfaces
        ))
        .evidence_all(evidence)
        .fix("ip link set <iface> up；检查 NetworkManager/systemd-networkd 配置")
    } else {
        f.pass(format!("{up} 个网卡 up")).evidence_all(evidence)
    }
}

/// 主动出网探测：能连上控制端才有意义，否则 agent 是「哑」的。
fn check_connectivity(cfg: &crate::config::NetworkConfig) -> Finding {
    let f = Finding::new("NET-02", "可连通外部网络", Severity::High);

    if cfg.connectivity_targets.is_empty() {
        return f.skip("策略未配置探测目标");
    }

    let timeout = Duration::from_millis(cfg.connectivity_timeout_ms.max(100));
    let mut ok_any = false;
    let mut evidence = Vec::new();

    for target in &cfg.connectivity_targets {
        // 一个目标可达就够，不必为后面不可达的目标白等超时
        if ok_any {
            break;
        }
        let addrs: Vec<SocketAddr> = match target.to_socket_addrs() {
            Ok(it) => it.collect(),
            Err(e) => {
                evidence.push(format!("{target}: 解析失败 {e}"));
                continue;
            }
        };
        let mut connected = false;
        for addr in addrs.iter().take(4) {
            let started = Instant::now();
            match TcpStream::connect_timeout(addr, timeout) {
                Ok(_) => {
                    connected = true;
                    evidence.push(format!(
                        "{target} ({addr}) 连通，耗时 {} ms",
                        started.elapsed().as_millis()
                    ));
                    break;
                }
                Err(e) => evidence.push(format!("{target} ({addr}) 失败: {e}")),
            }
        }
        ok_any |= connected;
    }

    if ok_any {
        f.pass("至少一个探测目标可达").evidence_all(evidence)
    } else {
        f.fail("全部探测目标不可达，可能是断网或出向策略阻断")
            .evidence_all(evidence)
            .fix("检查默认路由、DNS 与出口防火墙策略")
    }
}

fn check_listen_ports(cfg: &crate::config::NetworkConfig) -> Finding {
    let f = Finding::new("NET-03", "监听端口受控", Severity::Medium).reference("等保 8.1.4.3");

    let mut listeners: Vec<(String, String, u16)> = Vec::new();
    listeners.extend(parse_proc_net("/proc/net/tcp", "tcp"));
    listeners.extend(parse_proc_net("/proc/net/tcp6", "tcp6"));

    if listeners.is_empty() {
        return f.skip("未解析到监听套接字（或 /proc/net/tcp 不可读）");
    }

    let forbidden: Vec<&(String, String, u16)> = listeners
        .iter()
        .filter(|(_, _, port)| cfg.forbidden_listen_ports.contains(port))
        .collect();

    let summary: Vec<String> = listeners
        .iter()
        .take(15)
        .map(|(proto, addr, port)| format!("{proto} {addr}:{port}"))
        .collect();

    if forbidden.is_empty() {
        f.pass(format!("{} 个监听端口，未命中禁用列表", listeners.len()))
            .evidence_all(summary)
    } else {
        f.fail(format!("{} 个监听端口命中禁用列表", forbidden.len()))
            .evidence_all(
                forbidden
                    .iter()
                    .map(|(p, a, port)| format!("forbidden: {p} {a}:{port}")),
            )
            .fix("定位进程：ss -lntp 'sport = :<port>'；关闭服务或改绑 127.0.0.1")
    }
}

/// 解析 /proc/net/tcp{,6}，只保留 LISTEN（st=0A）。
fn parse_proc_net(path: &str, proto: &str) -> Vec<(String, String, u16)> {
    parse_proc_net_text(&util::read_lines(path).join("\n"), proto)
}

/// 与文件读取解耦，便于用固定文本做单元测试。
fn parse_proc_net_text(text: &str, proto: &str) -> Vec<(String, String, u16)> {
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 || f[3] != "0A" {
            continue;
        }
        let Some((ip_hex, port_hex)) = f[1].split_once(':') else {
            continue;
        };
        let Ok(port) = u16::from_str_radix(port_hex, 16) else {
            continue;
        };
        let addr = if proto == "tcp" {
            u32::from_str_radix(ip_hex, 16)
                .map(|v| std::net::Ipv4Addr::from(v.to_ne_bytes()).to_string())
                .unwrap_or_else(|_| ip_hex.to_string())
        } else {
            // IPv6 十六进制是 4 个 32 位字，逐字按小端展开
            let mut buf = String::from("[");
            for chunk in ip_hex.as_bytes().chunks(8) {
                if let Ok(w) = u32::from_str_radix(std::str::from_utf8(chunk).unwrap_or("0"), 16) {
                    buf.push_str(&format!("{:08x}", w));
                }
            }
            buf.push(']');
            buf
        };
        out.push((proto.to_string(), addr, port));
    }
    out
}

/// 内核网络参数基线：`(key, 允许值, 说明)`。
const NET_SYSCTLS: &[(&str, &[&str], &str)] = &[
    ("net.ipv4.ip_forward", &["0"], "禁止转发（非路由设备）"),
    ("net.ipv4.conf.all.rp_filter", &["1"], "严格反向路径过滤"),
    (
        "net.ipv4.conf.default.rp_filter",
        &["1"],
        "默认接口反向路径过滤",
    ),
    (
        "net.ipv4.conf.all.accept_redirects",
        &["0"],
        "拒绝 ICMP 重定向",
    ),
    (
        "net.ipv4.conf.default.accept_redirects",
        &["0"],
        "默认接口拒绝重定向",
    ),
    ("net.ipv4.conf.all.send_redirects", &["0"], "不发送重定向"),
    (
        "net.ipv4.conf.all.accept_source_route",
        &["0"],
        "拒绝源路由",
    ),
    ("net.ipv4.conf.all.log_martians", &["1"], "记录异常源地址包"),
    ("net.ipv4.tcp_syncookies", &["1"], "开启 SYN cookie"),
    (
        "net.ipv6.conf.all.accept_redirects",
        &["0"],
        "IPv6 拒绝重定向",
    ),
    (
        "net.ipv6.conf.all.accept_source_route",
        &["0"],
        "IPv6 拒绝源路由",
    ),
];

fn check_sysctl(cfg: &crate::config::NetworkConfig) -> Finding {
    let f = Finding::new("NET-04", "内核网络参数已加固", Severity::Medium).reference("CIS 3.1-3.3");

    if !cfg.enforce_sysctl {
        return f.skip("策略关闭该项");
    }

    let mut bad = Vec::new();
    let mut checked = Vec::new();

    for (key, allowed, desc) in NET_SYSCTLS {
        let Some(actual) = util::sysctl(key) else {
            continue; // IPv6 关闭时相关 sysctl 不存在
        };
        if allowed.contains(&actual.as_str()) {
            checked.push(format!("{key} = {actual}"));
        } else {
            bad.push(format!(
                "{key} = {actual}（期望 {}，{desc}）",
                allowed.join("/")
            ));
        }
    }

    if bad.is_empty() {
        f.pass(format!("{} 项网络参数符合基线", checked.len()))
            .evidence_all(checked)
    } else {
        f.fail(format!("{} 项网络参数不符合基线", bad.len()))
            .evidence_all(bad)
            .fix("写入 /etc/sysctl.d/99-hardening.conf 后 sysctl --system")
    }
}

fn check_dns() -> Finding {
    let f = Finding::new("NET-05", "DNS 配置有效且未被篡改", Severity::Low);

    let path = "/etc/resolv.conf";
    if !util::exists(path) {
        return f.skip("/etc/resolv.conf 不存在");
    }

    let mut evidence = Vec::new();
    if let Ok(link) = std::fs::read_link(path) {
        evidence.push(format!("符号链接 -> {}", link.display()));
    }
    if let Some(mode) = util::mode(path) {
        evidence.push(format!("权限 {mode:04o}"));
        if mode & 0o002 != 0 {
            return f
                .fail("resolv.conf 世界可写，可被劫持 DNS")
                .evidence_all(evidence)
                .fix("chmod 0644 /etc/resolv.conf；用 systemd-resolved 或 NetworkManager 管理");
        }
    }

    let nameservers: Vec<String> = util::read_lines(path)
        .into_iter()
        .filter_map(|l| {
            let t = l.trim();
            t.strip_prefix("nameserver ").map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .collect();

    if nameservers.is_empty() {
        f.fail("未配置 nameserver")
            .evidence_all(evidence)
            .fix("在 /etc/resolv.conf 或 systemd-resolved 中配置受信 DNS")
    } else {
        evidence.extend(nameservers.iter().map(|ns| format!("nameserver {ns}")));
        f.pass(format!("{} 个 DNS 服务器", nameservers.len()))
            .evidence_all(evidence)
    }
}

fn check_firewall(cfg: &crate::config::NetworkConfig) -> Finding {
    let f = Finding::new("NET-06", "主机防火墙已启用", Severity::High).reference("等保 8.1.4.4");

    // nftables
    if let Some(o) = util::run("nft", &["list", "ruleset"], Duration::from_secs(5))
        && o.ok()
    {
        let has_rules = o.stdout.contains("chain ");
        let count = o.stdout.matches("rule ").count();
        return if has_rules && count > 0 {
            f.pass(format!("nftables 生效，{count} 条规则"))
                .evidence("nft list ruleset")
        } else if cfg.require_firewall {
            f.fail("nftables 已加载但规则为空")
                .fix("按最小暴露面编写 /etc/nftables.conf 并 systemctl enable --now nftables")
        } else {
            f.warn("nftables 规则为空（策略不强制）")
        };
    }

    // iptables
    if let Some(o) = util::run("iptables-save", &[], Duration::from_secs(5))
        && o.ok()
    {
        let rules = o.stdout.lines().filter(|l| l.starts_with("-A ")).count();
        return if rules > 0 {
            f.pass(format!("iptables 生效，{rules} 条规则"))
        } else if cfg.require_firewall {
            f.fail("iptables 无任何规则")
                .fix("配置 iptables/nftables 默认拒绝策略")
        } else {
            f.warn("iptables 无规则（策略不强制）")
        };
    }

    // firewalld / ufw 兜底
    if let Some(o) = util::run("firewall-cmd", &["--state"], Duration::from_secs(5))
        && o.ok()
        && o.stdout.contains("running")
    {
        return f.pass("firewalld running").evidence(o.stdout.trim());
    }
    if let Some(o) = util::run("ufw", &["status"], Duration::from_secs(5))
        && o.ok()
        && o.stdout.to_lowercase().contains("status: active")
    {
        return f
            .pass("ufw active")
            .evidence(o.stdout.lines().next().unwrap_or(""));
    }

    if cfg.require_firewall {
        f.fail("未检测到任何生效的主机防火墙")
            .fix("安装并启用 nftables 或 firewalld")
    } else {
        f.skip("未检测到防火墙（策略不强制）")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
       0: 0100007F:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 11111 1\n\
       1: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 22222 1\n\
       2: 0100007F:C350 0100007F:0016 01 00000000:00000000 00:00000000 00000000     0        0 33333 1";

    #[test]
    fn only_listen_sockets_are_reported() {
        let listeners = parse_proc_net_text(SAMPLE, "tcp");
        assert_eq!(listeners.len(), 2, "非 LISTEN 状态的连接必须被过滤");
    }

    #[test]
    fn ipv4_hex_is_little_endian() {
        let listeners = parse_proc_net_text(SAMPLE, "tcp");
        assert_eq!(listeners[0].1, "127.0.0.1");
        assert_eq!(listeners[0].2, 22);
        assert_eq!(listeners[1].1, "0.0.0.0");
        assert_eq!(listeners[1].2, 8080);
    }
}
