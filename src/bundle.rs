//! The readings bundle: everything `run` writes, in one JSON document (schema in README.md).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::env::Environment;
use crate::stats::Stats;

pub const SCHEMA: &str = "takt-harness/readings/v1";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Bundle {
    pub schema: String,
    pub harness: Harness,
    pub created_utc: String,
    pub environment: Environment,
    pub counters: Counters,
    pub protocol: Protocol,
    pub inputs: Vec<Input>,
    pub commands: Vec<CommandReadings>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Harness {
    pub name: String,
    pub version: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Counters {
    /// true when cycles and instructions were counted.
    pub available: bool,
    /// How counting was set up, or "wall time only".
    pub mode: String,
    /// Events that were counted (keys of the per-run fields).
    pub events: Vec<String>,
    /// Events that could not be counted, with the reason.
    pub unavailable: BTreeMap<String, String>,
    /// Why the measurement fell back to wall time only.
    pub fallback_reason: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Protocol {
    pub reps: u32,
    pub warmup: u32,
    pub order: String,
    pub stdin: String,
    pub stdout: String,
    pub bootstrap_resamples: u32,
    pub ci_level: f64,
    pub redacted: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Input {
    pub path: String,
    /// "file" or "dir".
    pub kind: String,
    pub bytes: u64,
    /// SHA-256 of the file; for a directory, SHA-256 of its `files` list in sha256sum format.
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileDigest>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FileDigest {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CommandReadings {
    pub label: String,
    pub argv: Vec<String>,
    pub executable: FileDigest,
    pub runs: Vec<Run>,
    pub stats: BTreeMap<String, Stats>,
    pub stdout: OutputSummary,
    pub exit: ExitSummary,
    pub quality: Quality,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Run {
    pub round: u32,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub wall_ns: u64,
    pub user_ns: u64,
    pub sys_ns: u64,
    pub max_rss_kib: u64,
    pub cycles: Option<u64>,
    pub instructions: Option<u64>,
    pub cache_misses: Option<u64>,
    pub branch_misses: Option<u64>,
    /// Lowest share of time any counter was on the PMU (1.0 = no multiplexing).
    pub counter_coverage: Option<f64>,
    pub stdout_sha256: String,
    pub stdout_bytes: u64,
}

impl Run {
    pub fn metric(&self, key: &str) -> Option<f64> {
        let v = match key {
            "cycles" => self.cycles?,
            "instructions" => self.instructions?,
            "cache_misses" => self.cache_misses?,
            "branch_misses" => self.branch_misses?,
            "wall_ns" => self.wall_ns,
            "user_ns" => self.user_ns,
            "sys_ns" => self.sys_ns,
            "max_rss_kib" => self.max_rss_kib,
            _ => return None,
        };
        Some(v as f64)
    }

    pub fn set_counter(&mut self, key: &str, v: Option<u64>) {
        match key {
            "cycles" => self.cycles = v,
            "instructions" => self.instructions = v,
            "cache_misses" => self.cache_misses = v,
            "branch_misses" => self.branch_misses = v,
            _ => {}
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OutputSummary {
    /// SHA-256 of stdout in the first measured run.
    pub sha256: String,
    pub bytes: u64,
    pub identical_in_all_runs: bool,
    pub distinct_hashes: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ExitSummary {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub identical_in_all_runs: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Quality {
    /// good | fair | poor (rules in README.md).
    pub grade: String,
    pub primary_metric: String,
    pub reps: usize,
    /// MAD / median of the primary metric: the typical deviation of one run.
    pub rel_mad: f64,
    /// (max − min) / median of the primary metric.
    pub rel_spread: f64,
    pub ci95_rel_halfwidth: f64,
    pub warnings: Vec<String>,
}

/// Metric keys in display order, with how to print them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Count,
    Nanos,
    KiB,
}

pub const METRICS: [(&str, &str, Unit); 8] = [
    ("cycles", "cycles:u", Unit::Count),
    ("instructions", "instructions:u", Unit::Count),
    ("wall_ns", "wall time", Unit::Nanos),
    ("user_ns", "user time", Unit::Nanos),
    ("sys_ns", "sys time", Unit::Nanos),
    ("max_rss_kib", "peak RSS", Unit::KiB),
    ("cache_misses", "cache-misses:u", Unit::Count),
    ("branch_misses", "branch-misses:u", Unit::Count),
];

/// Display label of a metric key, e.g. "cycles" -> "cycles:u".
pub fn metric_label(key: &str) -> &str {
    METRICS.iter().find(|m| m.0 == key).map(|m| m.1).unwrap_or(key)
}

impl CommandReadings {
    pub fn samples(&self, key: &str) -> Vec<f64> {
        self.runs.iter().filter_map(|r| r.metric(key)).collect()
    }
}

pub fn exit_text(code: Option<i32>, signal: Option<i32>) -> String {
    match (code, signal) {
        (Some(c), _) => format!("exit {c}"),
        (None, Some(s)) => format!("signal {s}"),
        _ => "unknown".to_string(),
    }
}
