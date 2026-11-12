//! The diagnosis engine: maintains a sliding window of samples, runs every
//! rule over it, and ranks the resulting findings into a [`Diagnosis`].
//!
//! Two entry points cover the real workflows:
//! * [`Diagnoser::observe`] + [`Diagnoser::diagnose`] — an agent samples in the
//!   background and diagnoses on demand when a symptom is reported.
//! * [`Diagnoser::sweep`] — sample for a short window, then diagnose (periodic
//!   health checks / a one-shot `diagnose` CLI run).

use serde::{Deserialize, Serialize};

use crate::finding::Finding;
use crate::rules::{self, Ctx, Rule};
use crate::sample::Sample;
use crate::sampler::{ProbeConfig, Sampler};
use crate::symptom::Symptom;

/// Engine configuration.
#[derive(Debug, Clone)]
pub struct Options {
    /// How many samples to keep for trend/persistence analysis.
    pub window_size: usize,
    pub thresholds: rules::Thresholds,
    /// Active network probes (RTT/DNS). `None` = passive only.
    pub probe: Option<ProbeConfig>,
    /// Take a `platform-health` baseline snapshot (enables host rules).
    pub collect_baseline: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            window_size: 10,
            thresholds: rules::Thresholds::default(),
            probe: None,
            collect_baseline: true,
        }
    }
}

/// The engine. Reuse one instance so the window fills across observations.
pub struct Diagnoser {
    sampler: Sampler,
    window: Vec<Sample>,
    opts: Options,
    baseline: Option<platform_health::Snapshot>,
}

impl Default for Diagnoser {
    fn default() -> Self {
        Diagnoser::new(Options::default())
    }
}

impl Diagnoser {
    pub fn new(opts: Options) -> Self {
        let sampler = match &opts.probe {
            Some(cfg) => Sampler::with_probes(cfg.clone()),
            None => Sampler::new(),
        };
        let baseline = if opts.collect_baseline {
            Some(platform_health::snapshot())
        } else {
            None
        };
        Diagnoser {
            sampler,
            window: Vec::new(),
            opts,
            baseline,
        }
    }

    /// Capture one sample and fold it into the window. Returns the sample.
    pub fn observe(&mut self) -> &Sample {
        let sample = self.sampler.sample();
        if self.window.len() >= self.opts.window_size {
            self.window.remove(0);
        }
        self.window.push(sample);
        self.window.last().unwrap()
    }

    /// Diagnose the current window against `symptom`. Captures a sample first
    /// if the window is empty so a one-shot call still works.
    pub fn diagnose(&mut self, symptom: &Symptom) -> Diagnosis {
        if self.window.is_empty() {
            self.observe();
        }
        run_rules(
            &self.window,
            symptom,
            self.baseline.as_ref(),
            &self.opts.thresholds,
        )
    }

    /// Sample every `interval` for `duration`, then diagnose. Gives the rules a
    /// real trend window (needed to tell a spike from sustained pressure).
    pub fn sweep(
        &mut self,
        symptom: &Symptom,
        duration: std::time::Duration,
        interval: std::time::Duration,
    ) -> Diagnosis {
        let start = std::time::Instant::now();
        while start.elapsed() < duration {
            self.observe();
            let remaining = duration.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                break;
            }
            std::thread::sleep(interval.min(remaining));
        }
        self.diagnose(symptom)
    }

    /// Refresh the baseline snapshot (e.g. after hardware changes).
    pub fn refresh_baseline(&mut self) {
        if self.opts.collect_baseline {
            self.baseline = Some(platform_health::snapshot());
        }
    }

    /// The samples captured so far.
    pub fn window(&self) -> &[Sample] {
        &self.window
    }
}

/// A ranked diagnosis report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnosis {
    /// The symptom that was investigated.
    pub symptom: String,
    pub at_unix: u64,
    /// Number of samples analysed.
    pub samples: usize,
    /// Ranked findings, most confident first.
    pub findings: Vec<Finding>,
    /// One-line verdict for logs / a dashboard header.
    pub summary: String,
}

impl Diagnosis {
    pub fn is_healthy(&self) -> bool {
        self.findings.is_empty()
    }

    /// The dominant finding, if any.
    pub fn primary(&self) -> Option<&Finding> {
        self.findings.first()
    }

    /// Findings in one dimension.
    pub fn in_dimension(&self, dim: crate::finding::Dimension) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(move |f| f.dimension == dim)
    }
}

fn run_rules(
    window: &[Sample],
    symptom: &Symptom,
    baseline: Option<&platform_health::Snapshot>,
    thresholds: &rules::Thresholds,
) -> Diagnosis {
    let ctx = Ctx {
        window,
        symptom,
        baseline,
        thresholds,
    };

    let mut findings: Vec<Finding> = Vec::new();
    for rule in rules::all_rules() {
        findings.extend(rule.evaluate(&ctx));
    }

    // Symptom-directed boost: findings in the dimension the symptom points at
    // get a nudge, so a timeout surfaces network causes ahead of incidental ones.
    for f in &mut findings {
        let boost = match (symptom, f.dimension) {
            (Symptom::Timeout { .. }, crate::finding::Dimension::Network) => 0.1,
            (Symptom::SlowNetwork, crate::finding::Dimension::Network) => 0.08,
            (Symptom::Lag, crate::finding::Dimension::System) => 0.08,
            (Symptom::HighResource { .. }, _) => 0.05,
            (Symptom::AppUnresponsive { .. }, crate::finding::Dimension::App) => 0.1,
            (Symptom::DiskFull { .. }, crate::finding::Dimension::Host) => 0.1,
            _ => 0.0,
        };
        f.confidence = (f.confidence + boost).clamp(0.0, 1.0);
    }

    // Rank by confidence; drop duplicate causes (keep the most confident).
    findings.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut seen: Vec<&str> = Vec::new();
    findings.retain(|f| {
        let code = f.cause.code();
        if seen.contains(&code) {
            false
        } else {
            seen.push(code);
            true
        }
    });

    let at_unix = window.last().map(|s| s.at_unix).unwrap_or(0);
    let summary = summarize(symptom, &findings);

    Diagnosis {
        symptom: symptom.label(),
        at_unix,
        samples: window.len(),
        findings,
        summary,
    }
}

fn summarize(symptom: &Symptom, findings: &[Finding]) -> String {
    if findings.is_empty() {
        return format!(
            "No diagnosed cause for {} — system looks healthy",
            symptom.label()
        );
    }
    let primary = &findings[0];
    let others = findings.len() - 1;
    let base = format!(
        "[{}] {} (confidence {:.0}%)",
        primary.dimension,
        primary.summary,
        primary.confidence * 100.0
    );
    if others > 0 {
        format!("{base}; +{others} secondary finding(s)")
    } else {
        base
    }
}

/// Evaluate a pre-built window against the built-in rules. Exposed for tests
/// and for callers that collect their own samples.
pub fn evaluate_window(
    window: &[Sample],
    symptom: &Symptom,
    baseline: Option<&platform_health::Snapshot>,
    thresholds: &rules::Thresholds,
) -> Diagnosis {
    run_rules(window, symptom, baseline, thresholds)
}

/// Convenience: the rule set (for callers that want to inspect names).
pub fn rule_names() -> Vec<&'static str> {
    rules::all_rules().iter().map(|r| r.name()).collect()
}

// Keep the `Rule` trait in scope for the `rule_names` map.
#[allow(unused_imports)]
use Rule as _Rule;
