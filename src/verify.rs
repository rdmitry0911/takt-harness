//! `takt-harness verify`: checks a readings bundle against a trial passport and states, as JSON,
//! whether the record supports declining a trial build (TAKT trial agreement, clauses 3 and 7).
//!
//! It checks that the record is the agreed one — this tool and version, the CPU model, the run
//! protocol, exactly the two pinned builds and the pinned inputs, a complete numbered record of every
//! measured run — and computes the agreed metric as a factor baseline / trial (ratio of medians, with
//! a 95% bootstrap interval) against the agreed threshold, together with functional equivalence of
//! the outputs. It decides nothing by itself: the provider checks the record against its own run of
//! the same builds on the same input. Given that run (`--reference`), it also compares the
//! instruction count of the trial build, which is nearly deterministic for one binary and one input.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::bundle::{metric_label, Bundle, CommandReadings, METRICS};
use crate::{compare, stats};

pub const PASSPORT_SCHEMA: &str = "takt-trial-passport/1";
pub const VERDICT_SCHEMA: &str = "takt-trial-verdict/1";
/// Largest relative difference between the client's and the provider's median instruction count of
/// the trial build that still counts as the same build run on the same input.
pub const INSTRUCTIONS_TOLERANCE: f64 = 0.01;

/// One build: the label it carries in the record and, when the passport pins it, the SHA-256 of its
/// executable. An unpinned build (e.g. a program the client links with a delivered library) is
/// identified in the verdict by the SHA-256 the record shows.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Build {
    pub label: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

/// The trial's measurement passport, fixed before the trial starts (agreement clause 3).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Passport {
    pub schema: String,
    pub tool: String,
    pub version: String,
    /// A metric key of the bundle: "wall_ns", "cycles", "instructions", "user_ns", ...
    pub metric: String,
    #[serde(default = "median")]
    pub aggregation: String,
    /// A decline is supported when baseline / trial < threshold (e.g. 1.10).
    pub threshold: f64,
    pub reps: u32,
    #[serde(default)]
    pub warmup: u32,
    pub baseline: Build,
    pub trial: Build,
    #[serde(default)]
    pub inputs_sha256: Vec<String>,
    #[serde(default)]
    pub cpu_model: Option<String>,
}

fn median() -> String {
    "median".into()
}

#[derive(Serialize, Debug, Default, PartialEq)]
pub struct FailedRuns {
    pub baseline: usize,
    pub trial: usize,
}

#[derive(Serialize, Debug)]
pub struct Reference {
    pub trial_instructions_client: Option<f64>,
    pub trial_instructions_provider: Option<f64>,
    pub relative_difference: Option<f64>,
    /// None when either side has no instruction counts to compare.
    pub consistent: Option<bool>,
    pub note: String,
}

#[derive(Serialize, Debug)]
pub struct Verdict {
    pub schema: &'static str,
    pub verifier_version: String,
    pub record_valid: bool,
    pub problems: Vec<String>,
    pub metric: String,
    /// SHA-256 of the executables the record shows for the baseline and the trial build.
    pub baseline_sha256: Option<String>,
    pub trial_sha256: Option<String>,
    pub baseline_median: Option<f64>,
    pub trial_median: Option<f64>,
    /// baseline / trial, ratio of medians.
    pub factor: Option<f64>,
    pub factor_ci95: Option<[f64; 2]>,
    pub threshold: f64,
    pub threshold_met: Option<bool>,
    pub equivalent: Option<bool>,
    pub failed_runs: FailedRuns,
    pub decline_supported: bool,
    pub reference: Option<Reference>,
    pub summary: String,
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

fn find<'a>(b: &'a Bundle, label: &str) -> Option<&'a CommandReadings> {
    b.commands.iter().find(|c| c.label == label)
}

fn failed(c: &CommandReadings) -> usize {
    c.runs.iter().filter(|r| r.exit_code != Some(0) || r.signal.is_some()).count()
}

/// Checks `b` against the passport (and, if given, the provider's reference run).
pub fn check(p: &Passport, b: &Bundle, reference: Option<&Bundle>, verifier_version: &str) -> Verdict {
    let mut problems = Vec::new();
    if p.schema != PASSPORT_SCHEMA {
        problems.push(format!("passport schema is '{}', expected '{PASSPORT_SCHEMA}'", p.schema));
    }
    if b.harness.name != p.tool {
        problems.push(format!("the record was made by '{}', the passport names '{}'", b.harness.name, p.tool));
    }
    if b.harness.version != p.version {
        problems.push(format!("the record was made by version {}, the passport pins {}", b.harness.version, p.version));
    }
    if p.aggregation != "median" {
        problems.push(format!("aggregation '{}' is not supported (median only)", p.aggregation));
    }
    if !METRICS.iter().any(|m| m.0 == p.metric) {
        problems.push(format!("unknown metric '{}'", p.metric));
    }
    if let Some(cpu) = &p.cpu_model {
        let got = b.environment.cpu.model.as_deref().unwrap_or("");
        if got.trim() != cpu.trim() {
            problems.push(format!("the CPU model is '{got}', the passport pins '{cpu}'"));
        }
    }
    if b.protocol.reps != p.reps {
        problems.push(format!("{} measured runs per build, the passport pins {}", b.protocol.reps, p.reps));
    }
    if b.protocol.warmup != p.warmup {
        problems.push(format!("{} warm-up runs, the passport pins {}", b.protocol.warmup, p.warmup));
    }
    if b.commands.len() != 2 {
        problems.push(format!("the record has {} builds, the protocol measures exactly two", b.commands.len()));
    }
    if !p.inputs_sha256.is_empty() {
        let got: BTreeSet<&str> = b.inputs.iter().map(|i| i.sha256.as_str()).collect();
        let want: BTreeSet<&str> = p.inputs_sha256.iter().map(String::as_str).collect();
        for w in want.difference(&got) {
            problems.push(format!("input {} of the passport is not in the record", short(w)));
        }
        for g in got.difference(&want) {
            problems.push(format!("input {} of the record is not in the passport", short(g)));
        }
    }

    let base = find(b, &p.baseline.label);
    let trial = find(b, &p.trial.label);
    let want_rounds: BTreeSet<u32> = (0..p.reps).collect();
    for (role, build, cmd) in [("baseline", &p.baseline, base), ("trial", &p.trial, trial)] {
        let Some(c) = cmd else {
            problems.push(format!("there is no {role} build labelled '{}' in the record", build.label));
            continue;
        };
        if let Some(pinned) = build.sha256.as_deref().filter(|s| !s.is_empty()) {
            if c.executable.sha256 != pinned {
                problems.push(format!(
                    "the {role} build is {}, the passport pins {}",
                    short(&c.executable.sha256),
                    short(pinned)
                ));
            }
        }
        let rounds: BTreeSet<u32> = c.runs.iter().map(|r| r.round).collect();
        if c.runs.len() != p.reps as usize || rounds != want_rounds {
            problems.push(format!(
                "the {role} record is not complete: {} runs in rounds {:?}, expected one run in each of rounds 0..{}",
                c.runs.len(),
                rounds,
                p.reps
            ));
        }
    }

    let (mut baseline_median, mut trial_median, mut factor, mut factor_ci95) = (None, None, None, None);
    if let (Some(bc), Some(tc)) = (base, trial) {
        let (bs, ts) = (bc.samples(&p.metric), tc.samples(&p.metric));
        if bs.len() != bc.runs.len() || ts.len() != tc.runs.len() {
            problems
                .push(format!("{} is missing in some runs (hardware counters unavailable?)", metric_label(&p.metric)));
        }
        if !bs.is_empty() && !ts.is_empty() {
            let (mb, mt) = (stats::median(&bs), stats::median(&ts));
            baseline_median = Some(mb);
            trial_median = Some(mt);
            if mt > 0.0 {
                factor = Some(mb / mt);
                factor_ci95 = Some(stats::bootstrap_ratio_ci(&bs, &ts, stats::RESAMPLES, stats::CI_LEVEL, stats::SEED));
            }
        }
    }

    let equivalent = match (base, trial) {
        (Some(bc), Some(tc)) => Some(
            bc.stdout.identical_in_all_runs
                && tc.stdout.identical_in_all_runs
                && bc.stdout.sha256 == tc.stdout.sha256
                && bc.exit.identical_in_all_runs
                && tc.exit.identical_in_all_runs
                && bc.exit.exit_code == tc.exit.exit_code
                && bc.exit.signal == tc.exit.signal,
        ),
        _ => None,
    };
    let failed_runs = FailedRuns { baseline: base.map(failed).unwrap_or(0), trial: trial.map(failed).unwrap_or(0) };

    let reference = reference.map(|r| {
        let sha = trial.map(|c| c.executable.sha256.as_str());
        let theirs_cmd = r.commands.iter().find(|c| Some(c.executable.sha256.as_str()) == sha);
        let med = |c: &CommandReadings| {
            let v = c.samples("instructions");
            (!v.is_empty()).then(|| stats::median(&v))
        };
        let mine = trial.and_then(med);
        let theirs = theirs_cmd.and_then(med);
        let relative_difference = match (mine, theirs) {
            (Some(a), Some(b)) if b > 0.0 => Some((a - b).abs() / b),
            _ => None,
        };
        let consistent = relative_difference.map(|d| d <= INSTRUCTIONS_TOLERANCE);
        let note = match (theirs_cmd, mine, theirs) {
            (None, _, _) => "the reference has no run of the pinned trial build".to_string(),
            (_, None, _) | (_, _, None) => "instruction counts are not available on both sides".to_string(),
            _ => format!("tolerance {:.0}% of the provider's median", INSTRUCTIONS_TOLERANCE * 100.0),
        };
        Reference {
            trial_instructions_client: mine,
            trial_instructions_provider: theirs,
            relative_difference,
            consistent,
            note,
        }
    });
    if let Some(Reference { consistent: Some(false), relative_difference: Some(d), .. }) = &reference {
        problems.push(format!(
            "the trial build's instruction count differs from the provider's run of the same build by {:.2}%",
            d * 100.0
        ));
    }

    let record_valid = problems.is_empty();
    let threshold_met = factor.map(|f| f >= p.threshold);
    let decline_supported = record_valid && (threshold_met == Some(false) || equivalent == Some(false));
    let label = metric_label(&p.metric);
    let summary = if !record_valid {
        let more = if problems.len() > 1 { format!(" (and {} more)", problems.len() - 1) } else { String::new() };
        format!("The record does not match the passport: {}{more}.", problems[0])
    } else {
        let f = match (factor, factor_ci95) {
            (Some(f), Some([lo, hi])) => format!("{label} factor {f:.4} [95% CI {lo:.4}, {hi:.4}]"),
            _ => format!("{label} factor not available"),
        };
        match (threshold_met, equivalent) {
            (_, Some(false)) => format!(
                "The record matches the passport. The outputs of the two builds differ, so functional equivalence is not met: the record supports the decline. ({f}.)"
            ),
            (Some(false), _) => format!(
                "The record matches the passport. {f} is below the threshold {:.4}: the record supports the decline.",
                p.threshold
            ),
            (Some(true), _) => format!(
                "The record matches the passport. {f} meets the threshold {:.4} and the outputs are equivalent: the record does not support a decline.",
                p.threshold
            ),
            (None, _) => format!("The record matches the passport, but the factor could not be computed: {f}."),
        }
    };

    Verdict {
        schema: VERDICT_SCHEMA,
        verifier_version: verifier_version.to_string(),
        record_valid,
        problems,
        metric: p.metric.clone(),
        baseline_sha256: base.map(|c| c.executable.sha256.clone()),
        trial_sha256: trial.map(|c| c.executable.sha256.clone()),
        baseline_median,
        trial_median,
        factor,
        factor_ci95,
        threshold: p.threshold,
        threshold_met,
        equivalent,
        failed_runs,
        decline_supported,
        reference,
        summary,
    }
}

/// `verify --passport P [--reference R] readings.json`: prints the verdict as JSON. Exit 0 whenever a
/// verdict is produced (whatever it says); errors reading the files exit non-zero.
pub fn verify(
    passport: &Path,
    readings: &Path,
    reference: Option<&Path>,
    verifier_version: &str,
) -> Result<i32, String> {
    let text = std::fs::read_to_string(passport).map_err(|e| format!("{}: {e}", passport.display()))?;
    let p: Passport =
        serde_json::from_str(&text).map_err(|e| format!("{}: not a trial passport: {e}", passport.display()))?;
    let b = compare::load(readings)?;
    let r = reference.map(compare::load).transpose()?;
    let v = check(&p, &b, r.as_ref(), verifier_version);
    println!("{}", serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?);
    Ok(0)
}
