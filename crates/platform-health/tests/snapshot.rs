use platform_health::{Options, Privacy, SCHEMA_VERSION, snapshot_with};

fn opts_with_processes() -> Options {
    Options {
        include_processes: true,
        top_processes: 5,
        ..Options::default()
    }
}

#[test]
fn snapshot_always_has_core_fields() {
    let snap = snapshot_with(&Options::default());

    assert_eq!(snap.schema, SCHEMA_VERSION);
    assert!(snap.collected_at.ends_with('Z'));
    assert!(snap.collected_at_unix > 1_600_000_000);
    assert_eq!(snap.identity.fingerprint.len(), 16);
    assert!(!snap.host.arch.is_empty());
    assert!(snap.cpu.logical_cores > 0);
    assert_eq!(snap.cpu.per_core_usage.len(), snap.cpu.logical_cores);
    assert!(snap.memory.total_bytes > 0);
    assert!(snap.memory.usage_percent <= 100.0);
    assert!(snap.health.score <= 100);
    assert!(snap.processes.is_none(), "processes are opt-in");
}

#[test]
fn process_collection_respects_limit() {
    let snap = snapshot_with(&opts_with_processes());
    let processes = snap.processes.expect("processes requested");
    assert!(processes.total > 0);
    assert!(processes.top.len() <= 5);
    assert!(snap.health.score <= 100);
}

#[test]
fn json_roundtrip_preserves_values() {
    let snap = snapshot_with(&Options::default());
    let json = serde_json::to_string(&snap).expect("serialize");
    let back: platform_health::Snapshot = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(back.identity.fingerprint, snap.identity.fingerprint);
    assert_eq!(back.memory.total_bytes, snap.memory.total_bytes);
    assert_eq!(back.health.status, snap.health.status);
    assert_eq!(back.disks.len(), snap.disks.len());
}

#[test]
fn redaction_hides_identifying_values_but_stays_parseable() {
    let full = snapshot_with(&Options::default());
    let redacted = snapshot_with(&Options {
        privacy: Privacy::Redacted,
        ..Options::default()
    });

    if let Some(hostname) = full.identity.hostname.as_deref() {
        let masked = redacted
            .identity
            .hostname
            .as_deref()
            .expect("hostname kept");
        assert!(masked.starts_with("device-"), "got {masked}");
        assert_ne!(masked, hostname);
    }

    for net in &redacted.networks {
        assert!(
            !net.mac.contains("::") || net.mac.is_empty(),
            "unexpected mac {}",
            net.mac
        );
    }

    // Fingerprint must not change: it is computed from raw values on purpose.
    assert_eq!(redacted.identity.fingerprint, full.identity.fingerprint);
}

#[test]
fn thresholds_are_honoured() {
    let mut opts = Options::default();
    // Make every device look alarming.
    opts.thresholds.memory_usage_warn = 1.0;
    opts.thresholds.memory_usage_crit = 2.0;
    opts.thresholds.cpu_usage_warn = 0.0;
    opts.thresholds.cpu_usage_crit = 0.0;
    opts.thresholds.load_per_core_warn = 0.0;
    opts.thresholds.load_per_core_crit = 0.0;

    let snap = snapshot_with(&opts);
    assert!(!snap.health.issues.is_empty());
    assert_ne!(snap.health.status, platform_health::Status::Healthy);
}
