//! Network-dimension rules: passive error/retransmit signals plus active probe
//! results (RTT, reachability, DNS timing).
//!
//! The key triage question — "is it the network or the local box?" — starts
//! here. If a probe target is unreachable or slow while the system rules found
//! no CPU/memory/IO bottleneck, the network is the prime suspect.

use super::{Ctx, Rule, confidence};
use crate::finding::{Cause, Evidence, Finding};
use crate::symptom::Symptom;

/// Passive interface errors/drops and TCP retransmissions.
pub struct PassiveNetRule;

impl Rule for PassiveNetRule {
    fn name(&self) -> &'static str {
        "network.passive"
    }

    fn evaluate(&self, ctx: &Ctx<'_>) -> Vec<Finding> {
        let mut out = Vec::new();
        let net = &ctx.latest().net;
        let t = ctx.thresholds;

        // Retransmissions suggest loss somewhere on the path.
        if net.tcp_retransmits >= t.net_retransmits {
            let sustained = ctx.sustained(|s| s.net.tcp_retransmits >= t.net_retransmits);
            out.push(
                Finding::new(
                    Cause::TcpRetransmits,
                    confidence(
                        t.net_retransmits as f64,
                        net.tcp_retransmits as f64,
                        sustained,
                        0.5,
                    ),
                    format!(
                        "{} TCP segments retransmitted (path loss)",
                        net.tcp_retransmits
                    ),
                )
                .with_evidence(Evidence {
                    metric: "net.tcp_retransmits".into(),
                    observed: net.tcp_retransmits as f64,
                    threshold: t.net_retransmits as f64,
                    unit: "segments".into(),
                    note: "retransmits since last sample (packet loss indicator)".into(),
                })
                .with_remediation(
                    "Check for packet loss on the path; test with a ping/trace to the target.",
                ),
            );
        }

        // Interface-level errors or drops (bad cable, saturated link, bad driver).
        let errors = net.rx_errors + net.tx_errors;
        let drops = net.rx_dropped + net.tx_dropped;
        if errors > 0 || drops > 0 {
            out.push(
                Finding::new(
                    Cause::LocalNetworkErrors,
                    0.45,
                    format!("{errors} interface errors, {drops} drops"),
                )
                .with_evidence(Evidence {
                    metric: "net.rx_errors+tx_errors".into(),
                    observed: errors as f64,
                    threshold: 0.0,
                    unit: "count".into(),
                    note: "cumulative NIC error counters".into(),
                })
                .with_remediation(
                    "Inspect the NIC/driver/cable; errors usually mean a physical or driver fault.",
                ),
            );
        }

        out
    }
}

/// Active-probe latency and reachability to configured targets.
pub struct ProbeRule;

impl Rule for ProbeRule {
    fn name(&self) -> &'static str {
        "network.probe"
    }

    fn evaluate(&self, ctx: &Ctx<'_>) -> Vec<Finding> {
        let mut out = Vec::new();
        let net = &ctx.latest().net;
        let t = ctx.thresholds;

        for probe in &net.probes {
            if !probe.reachable {
                // Unreachable target. If the local box is otherwise healthy,
                // this points at the destination or the path, not the host.
                out.push(
                    Finding::new(
                        Cause::RemoteUnreachable,
                        0.7,
                        format!("Cannot reach {} ({})", probe.target, probe.label),
                    )
                    .with_evidence(Evidence {
                        metric: format!("net.probe.{}.reachable", probe.label),
                        observed: 0.0,
                        threshold: 1.0,
                        unit: "bool".into(),
                        note: "TCP connect to target failed".into(),
                    })
                    .with_remediation(
                        "The target or the path is down — not a local resource issue. Trace the route / check the service.",
                    ),
                );
                continue;
            }
            if let Some(rtt) = probe.rtt_ms {
                let crit = rtt >= t.net_latency_crit_ms;
                let warn = rtt >= t.net_latency_warn_ms;
                if warn {
                    out.push(
                        Finding::new(
                            Cause::HighLatency,
                            confidence(
                                if crit { t.net_latency_crit_ms } else { t.net_latency_warn_ms },
                                rtt,
                                1.0,
                                if crit { 0.6 } else { 0.4 },
                            ),
                            format!(
                                "High latency to {} ({}): {:.0} ms",
                                probe.label, probe.target, rtt
                            ),
                        )
                        .with_evidence(Evidence {
                            metric: format!("net.probe.{}.rtt_ms", probe.label),
                            observed: rtt,
                            threshold: t.net_latency_warn_ms,
                            unit: "ms".into(),
                            note: "TCP connect round-trip".into(),
                        })
                        .with_remediation(
                            "If local CPU/mem/IO are fine, the network path or remote host is slow.",
                        ),
                    );
                }
            }
        }

        out
    }
}

/// DNS resolution timing/failure.
pub struct DnsRule;

impl Rule for DnsRule {
    fn name(&self) -> &'static str {
        "network.dns"
    }

    fn evaluate(&self, ctx: &Ctx<'_>) -> Vec<Finding> {
        let Some(dns) = &ctx.latest().net.dns else {
            return Vec::new();
        };
        let t = ctx.thresholds;
        if !dns.resolved {
            return vec![
                Finding::new(
                    Cause::DnsFailing,
                    0.65,
                    format!("DNS resolution failed for {}", dns.name),
                )
                .with_evidence(Evidence {
                    metric: "net.dns.resolved".into(),
                    observed: 0.0,
                    threshold: 1.0,
                    unit: "bool".into(),
                    note: "reference name did not resolve".into(),
                })
                .with_remediation(
                    "Check /etc/resolv.conf and the configured resolver reachability.",
                ),
            ];
        }
        if dns.elapsed_ms >= t.net_dns_warn_ms {
            let crit = dns.elapsed_ms >= t.net_dns_crit_ms;
            return vec![
                Finding::new(
                    Cause::DnsSlow,
                    confidence(
                        if crit {
                            t.net_dns_crit_ms
                        } else {
                            t.net_dns_warn_ms
                        },
                        dns.elapsed_ms,
                        1.0,
                        if crit { 0.6 } else { 0.4 },
                    ),
                    format!("Slow DNS: {:.0} ms to resolve {}", dns.elapsed_ms, dns.name),
                )
                .with_evidence(Evidence {
                    metric: "net.dns.elapsed_ms".into(),
                    observed: dns.elapsed_ms,
                    threshold: t.net_dns_warn_ms,
                    unit: "ms".into(),
                    note: "resolver round-trip".into(),
                })
                .with_remediation(
                    "A slow resolver makes everything look like a timeout; try a faster resolver.",
                ),
            ];
        }
        Vec::new()
    }
}

/// Cross-dimension triage: when the symptom is a timeout/lag and the local box
/// is clean but the network probes are bad, state that explicitly so the
/// operator doesn't chase local ghosts.
pub struct NetVsLocalRule;

impl Rule for NetVsLocalRule {
    fn name(&self) -> &'static str {
        "triage.net_vs_local"
    }

    fn evaluate(&self, ctx: &Ctx<'_>) -> Vec<Finding> {
        if !ctx.symptom.network_priority() && !matches!(ctx.symptom, Symptom::Lag) {
            return Vec::new();
        }
        let latest = ctx.latest();
        let t = ctx.thresholds;
        // Local resource pressure — include memory thrashing, not just the three
        // coarse levels, so this never contradicts a memory/io rule that fired.
        let local_busy = latest.cpu.usage_percent > 85.0
            || latest.mem.available_percent < 10.0
            || latest.cpu.iowait_percent > 20.0
            || latest.mem.swap_out_pages_per_sec >= t.mem_thrash_swap_out_pages
            || latest.mem.major_faults_per_sec >= t.mem_thrash_major_faults;
        // Network bad? (any probe unreachable or clearly slow, or retransmits.)
        let net_bad = latest.net.probes.iter().any(|p| !p.reachable)
            || latest
                .net
                .probes
                .iter()
                .any(|p| p.rtt_ms.unwrap_or(0.0) >= ctx.thresholds.net_latency_warn_ms)
            || latest.net.tcp_retransmits >= ctx.thresholds.net_retransmits;

        if net_bad && !local_busy {
            return vec![Finding::new(
                crate::finding::Cause::HighLatency,
                0.75,
                "Symptom points to the network: local resources are idle but probes show loss/latency",
            )
            .with_evidence(Evidence {
                metric: "triage.local_vs_net".into(),
                observed: 1.0,
                threshold: 0.0,
                unit: "verdict".into(),
                note: "no local CPU/memory/IO bottleneck; network probes degraded".into(),
            })
            .with_remediation(
                "Focus on the network path or remote service — the local host is not the bottleneck.",
            )];
        }
        Vec::new()
    }
}
