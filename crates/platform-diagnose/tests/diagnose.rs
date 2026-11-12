//! Engine tests using synthetic sample windows.
//!
//! These prove the rule engine reaches the right verdict from signals alone —
//! no need for a genuinely degraded machine. Each scenario builds a window of
//! [`Sample`]s and asserts the expected cause surfaces with offenders attached.

use platform_diagnose::engine::evaluate_window;
use platform_diagnose::finding::{Cause, Dimension};
use platform_diagnose::rules::Thresholds;
use platform_diagnose::sample::{
    CpuSample, MemSample, NetSample, ProbeResult, ProcSample, Sample, ThrottleSample,
};
use platform_diagnose::symptom::Symptom;

/// A quiet, healthy baseline sample.
fn healthy_sample() -> Sample {
    Sample {
        at_unix: 1_700_000_000,
        cpu: CpuSample {
            usage_percent: 5.0,
            user_percent: 3.0,
            system_percent: 1.0,
            iowait_percent: 0.5,
            steal_percent: 0.0,
            idle_percent: 95.0,
            logical_cores: 8,
            load_one: 0.3,
            run_queue: 1,
            ctx_switches_per_sec: 500.0,
            interrupts_per_sec: 800.0,
        },
        mem: MemSample {
            total_bytes: 16 << 30,
            available_bytes: 12 << 30,
            available_percent: 75.0,
            free_bytes: 11 << 30,
            swap_total_bytes: 4 << 30,
            swap_used_bytes: 0,
            swap_out_pages_per_sec: 0.0,
            swap_in_pages_per_sec: 0.0,
            major_faults_per_sec: 5.0,
            cached_bytes: 2 << 30,
        },
        thermal: ThrottleSample {
            max_temp_c: Some(45.0),
            hottest_label: Some("cpu".into()),
            throttled: false,
            freq_mhz: Some(3200.0),
            max_freq_mhz: Some(3600.0),
        },
        procs: vec![ProcSample {
            pid: 100,
            name: "idle-app".into(),
            state: "S".into(),
            cpu_percent: 2.0,
            memory_bytes: 100 << 20,
            read_bytes_per_sec: 0.0,
            write_bytes_per_sec: 0.0,
            threads: 2,
            fds: Some(20),
            connections: Some(1),
            user: None,
        }],
        ..Default::default()
    }
}

fn window(s: Sample) -> Vec<Sample> {
    vec![s; 3]
}

#[test]
fn healthy_system_has_no_findings() {
    let w = window(healthy_sample());
    let d = evaluate_window(&w, &Symptom::Generic, None, &Thresholds::default());
    assert!(d.is_healthy(), "expected no findings, got {:?}", d.findings);
    assert!(d.summary.contains("healthy"));
}

#[test]
fn memory_thrash_detected_with_offender() {
    let mut s = healthy_sample();
    s.mem.available_percent = 3.0;
    s.mem.swap_out_pages_per_sec = 400.0;
    s.mem.major_faults_per_sec = 2000.0;
    s.procs.push(ProcSample {
        pid: 500,
        name: "leaky-service".into(),
        state: "S".into(),
        cpu_percent: 10.0,
        memory_bytes: 10 << 30, // the biggest holder
        ..ProcSample::default()
    });
    let d = evaluate_window(&window(s), &Symptom::Lag, None, &Thresholds::default());

    assert!(
        d.findings.iter().any(|f| f.cause == Cause::MemoryThrashing),
        "expected memory thrashing, got {:?}",
        d.findings
            .iter()
            .map(|f| f.cause.code())
            .collect::<Vec<_>>()
    );
    let f = d
        .findings
        .iter()
        .find(|f| f.cause == Cause::MemoryThrashing)
        .unwrap();
    assert_eq!(f.dimension, Dimension::System);
    // The memory hog should be named as the offender.
    assert!(
        f.offenders.iter().any(|o| o.name == "leaky-service"),
        "expected leaky-service as offender"
    );
}

#[test]
fn cpu_saturation_names_top_process() {
    let mut s = healthy_sample();
    s.cpu.usage_percent = 98.0;
    s.cpu.user_percent = 90.0;
    s.cpu.run_queue = 20;
    s.procs[0].cpu_percent = 95.0;
    s.procs[0].name = "mining-rig".into();
    let d = evaluate_window(&window(s), &Symptom::Lag, None, &Thresholds::default());

    let f = d
        .findings
        .iter()
        .find(|f| f.cause == Cause::CpuSaturated)
        .expect("expected cpu saturation");
    assert_eq!(f.dimension, Dimension::System);
    assert!(f.offenders.iter().any(|o| o.name == "mining-rig"));
}

#[test]
fn io_bound_detected_on_high_iowait() {
    let mut s = healthy_sample();
    s.cpu.iowait_percent = 45.0;
    s.cpu.usage_percent = 50.0;
    s.procs[0].name = "db-writer".into();
    s.procs[0].write_bytes_per_sec = 200_000_000.0;
    let d = evaluate_window(&window(s), &Symptom::Lag, None, &Thresholds::default());

    let f = d
        .findings
        .iter()
        .find(|f| f.cause == Cause::IoBound)
        .expect("expected io bound");
    assert!(f.offenders.iter().any(|o| o.name == "db-writer"));
}

#[test]
fn vm_steal_detected() {
    let mut s = healthy_sample();
    s.cpu.steal_percent = 25.0;
    let d = evaluate_window(&window(s), &Symptom::Lag, None, &Thresholds::default());
    assert!(d.findings.iter().any(|f| f.cause == Cause::VmSteal));
}

#[test]
fn thermal_throttling_detected() {
    let mut s = healthy_sample();
    s.thermal.max_temp_c = Some(97.0);
    s.thermal.throttled = true;
    s.thermal.freq_mhz = Some(1400.0);
    s.thermal.max_freq_mhz = Some(3600.0);
    let d = evaluate_window(&window(s), &Symptom::Lag, None, &Thresholds::default());
    assert!(
        d.findings
            .iter()
            .any(|f| f.cause == Cause::ThermalThrottling)
    );
}

#[test]
fn timeout_with_clean_local_points_to_network() {
    let mut s = healthy_sample();
    // Local resources idle, but the probe target is unreachable.
    s.net = NetSample {
        probes: vec![ProbeResult {
            label: "target".into(),
            target: "10.99.99.99:443".into(),
            rtt_ms: None,
            reachable: false,
        }],
        ..NetSample::default()
    };
    let d = evaluate_window(
        &window(s),
        &Symptom::Timeout {
            target: "10.99.99.99:443".into(),
        },
        None,
        &Thresholds::default(),
    );

    assert!(
        d.findings
            .iter()
            .any(|f| f.cause == Cause::RemoteUnreachable),
        "expected remote unreachable, got {:?}",
        d.findings
            .iter()
            .map(|f| f.cause.code())
            .collect::<Vec<_>>()
    );
    // The cross-dimension triage should say "it's the network, not the box".
    let triage = d
        .findings
        .iter()
        .find(|f| f.summary.contains("local resources are idle"))
        .expect("expected net-vs-local triage finding");
    assert_eq!(triage.dimension, Dimension::Network);
}

#[test]
fn high_latency_to_target_detected() {
    let mut s = healthy_sample();
    s.net = NetSample {
        probes: vec![ProbeResult {
            label: "internet".into(),
            target: "1.1.1.1:443".into(),
            rtt_ms: Some(450.0),
            reachable: true,
        }],
        ..NetSample::default()
    };
    let d = evaluate_window(
        &window(s),
        &Symptom::SlowNetwork,
        None,
        &Thresholds::default(),
    );
    assert!(d.findings.iter().any(|f| f.cause == Cause::HighLatency));
}

#[test]
fn tcp_retransmits_flagged() {
    let mut s = healthy_sample();
    s.net.tcp_retransmits = 50;
    let d = evaluate_window(
        &window(s),
        &Symptom::SlowNetwork,
        None,
        &Thresholds::default(),
    );
    assert!(d.findings.iter().any(|f| f.cause == Cause::TcpRetransmits));
}

#[test]
fn dns_slow_detected() {
    let mut s = healthy_sample();
    s.net.dns = Some(platform_diagnose::sample::DnsProbe {
        name: "example.com".into(),
        elapsed_ms: 1500.0,
        resolved: true,
    });
    let d = evaluate_window(
        &window(s),
        &Symptom::Timeout {
            target: "example.com".into(),
        },
        None,
        &Thresholds::default(),
    );
    assert!(d.findings.iter().any(|f| f.cause == Cause::DnsSlow));
}

#[test]
fn process_blocked_detected() {
    let mut s = healthy_sample();
    // Two processes stuck in D-state across the window.
    for _ in 0..3 {
        s.procs.push(ProcSample {
            pid: 700,
            name: "stuck-io".into(),
            state: "D".into(),
            ..ProcSample::default()
        });
    }
    let d = evaluate_window(&window(s), &Symptom::Lag, None, &Thresholds::default());
    assert!(d.findings.iter().any(|f| f.cause == Cause::ProcessBlocked));
}

#[test]
fn findings_are_ranked_and_deduped() {
    let mut s = healthy_sample();
    // Trigger several causes at once.
    s.cpu.usage_percent = 99.0;
    s.cpu.run_queue = 40;
    s.cpu.steal_percent = 20.0;
    s.mem.swap_out_pages_per_sec = 500.0;
    s.mem.available_percent = 2.0;
    let d = evaluate_window(&window(s), &Symptom::Lag, None, &Thresholds::default());

    assert!(d.findings.len() >= 2, "expected multiple findings");
    // Sorted by confidence descending.
    for w in d.findings.windows(2) {
        assert!(
            w[0].confidence >= w[1].confidence,
            "not sorted by confidence"
        );
    }
    // No duplicate causes.
    let mut codes: Vec<&str> = d.findings.iter().map(|f| f.cause.code()).collect();
    let before = codes.len();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(before, codes.len(), "duplicate causes present");
}
