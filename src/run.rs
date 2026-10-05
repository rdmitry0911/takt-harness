//! `takt-harness run`: warm-up, interleaved measured rounds, statistics, the readings bundle.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::bundle::{
    self, Bundle, CommandReadings, Counters, ExitSummary, FileDigest, Harness, OutputSummary, Protocol, Quality, Run,
    METRICS,
};
use crate::digest;
use crate::env;
use crate::perf::{self, Counter, Event};
use crate::report;
use crate::stats;
use crate::sys;

pub struct Options {
    pub reps: u32,
    pub warmup: u32,
    pub out: PathBuf,
    pub inputs: Vec<PathBuf>,
    pub stdin: Option<PathBuf>,
    pub labels: Vec<String>,
    pub no_counters: bool,
    pub require_counters: bool,
    pub redact: bool,
    pub ignore_failure: bool,
    pub commands: Vec<Vec<OsString>>,
}

struct Prepared {
    label: String,
    argv: Vec<OsString>,
    exe: PathBuf,
    digest: FileDigest,
}

const COUNTER_MODE: &str =
    "perf_event hardware counters, user mode only (exclude_kernel, exclude_hv), inherited by every child \
     process of the command, enabled at exec";

/// Find the file that exec will run, the way a shell does.
fn resolve(argv0: &OsStr) -> Result<PathBuf, String> {
    let shown = argv0.to_string_lossy();
    let executable =
        |p: &Path| fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false);
    if argv0.as_bytes().contains(&b'/') {
        let p = PathBuf::from(argv0);
        return if executable(&p) { Ok(p) } else { Err(format!("{shown}: not an executable file")) };
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let dir = if dir.as_os_str().is_empty() { PathBuf::from(".") } else { dir };
        let p = dir.join(argv0);
        if executable(&p) {
            return Ok(p);
        }
    }
    Err(format!("{shown}: command not found in PATH"))
}

/// An unlinked temporary file: the command's stdout or stderr goes here, never into the bundle.
fn scratch(tag: &str) -> io::Result<File> {
    let dir = std::env::temp_dir();
    for i in 0..1000 {
        let p = dir.join(format!(".takt-harness-{}-{tag}-{i}", std::process::id()));
        match OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(&p) {
            Ok(f) => {
                let _ = fs::remove_file(&p);
                return Ok(f);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io::Error::new(e.kind(), format!("temporary file in {}: {e}", dir.display()))),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free temporary file name"))
}

fn rewind(f: &mut File) -> io::Result<()> {
    f.set_len(0)?;
    f.seek(SeekFrom::Start(0)).map(|_| ())
}

/// Last lines of the command's stderr, for an error message. Shown on the terminal only.
fn tail(f: &mut File) -> String {
    let len = f.seek(SeekFrom::End(0)).unwrap_or(0);
    let start = len.saturating_sub(4096);
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(start)).is_err() || f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(15)..].join("\n")
}

/// One execution of a command, counted.
fn execute(p: &Prepared, stdin: Option<&Path>, events: &[Event], out: &mut File, err: &mut File) -> io::Result<Run> {
    rewind(out)?;
    rewind(err)?;
    let mut counters = events.iter().map(|&e| Counter::open(e).map(|c| (e, c))).collect::<io::Result<Vec<_>>>()?;
    let mut cmd = Command::new(&p.exe);
    cmd.arg0(&p.argv[0]).args(&p.argv[1..]);
    cmd.stdin(match stdin {
        Some(f) => Stdio::from(File::open(f)?),
        None => Stdio::null(),
    });
    cmd.stdout(Stdio::from(out.try_clone()?)).stderr(Stdio::from(err.try_clone()?));
    let t0 = Instant::now();
    let child = cmd.spawn()?;
    let (raw, usage) = sys::wait4(child.id() as libc::pid_t)?;
    let wall = t0.elapsed();
    drop(child); // already reaped by wait4; this only releases the handle
    let status = ExitStatus::from_raw(raw);
    let mut run = Run {
        exit_code: status.code(),
        signal: status.signal(),
        wall_ns: wall.as_nanos() as u64,
        user_ns: usage.user_ns,
        sys_ns: usage.sys_ns,
        max_rss_kib: usage.max_rss_kib,
        ..Default::default()
    };
    for (e, c) in counters.iter_mut() {
        let est = c.read()?.estimate();
        run.set_counter(e.key(), est.map(|(v, _)| v));
        let cov = est.map_or(0.0, |(_, c)| c);
        run.counter_coverage = Some(run.counter_coverage.map_or(cov, |c| c.min(cov)));
    }
    out.seek(SeekFrom::Start(0))?;
    let (sha, n) = digest::sha256_reader(&mut *out)?;
    run.stdout_sha256 = sha;
    run.stdout_bytes = n;
    Ok(run)
}

pub fn utc_now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Gregorian date of a day number counted from 1970-01-01 (H. Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn readings(p: &Prepared, runs: Vec<Run>, counters_ok: bool, redact: bool) -> CommandReadings {
    let mut st = BTreeMap::new();
    for (key, _, _) in METRICS {
        let v: Vec<f64> = runs.iter().filter_map(|r| r.metric(key)).collect();
        if let Some(s) = stats::summarize(&v) {
            st.insert(key.to_string(), s);
        }
    }
    let primary = if counters_ok && st.contains_key("cycles") { "cycles" } else { "wall_ns" };
    let ps = st[primary].clone();
    let hashes: BTreeSet<&str> = runs.iter().map(|r| r.stdout_sha256.as_str()).collect();
    let exits: BTreeSet<(Option<i32>, Option<i32>)> = runs.iter().map(|r| (r.exit_code, r.signal)).collect();
    let first = &runs[0];

    let mut warnings = Vec::new();
    if runs.len() < 5 {
        warnings.push(format!("only {} measured runs; 9 or more give a usable interval", runs.len()));
    }
    let rel_mad = if ps.median != 0.0 { ps.mad / ps.median } else { 0.0 };
    if rel_mad > 0.01 {
        warnings.push(format!(
            "runs of {} typically deviate {:.1}% from the median: close other programs, pin with taskset -c N, \
             use the performance governor",
            bundle::metric_label(primary),
            rel_mad * 100.0
        ));
    } else if ps.rel_spread > 0.05 {
        warnings.push(format!(
            "outliers: the range of {} is {:.1}% of the median while runs typically deviate {:.2}%",
            bundle::metric_label(primary),
            ps.rel_spread * 100.0,
            rel_mad * 100.0
        ));
    }
    if !counters_ok {
        warnings.push("hardware counters unavailable: wall time only (see counters.fallback_reason)".into());
    }
    let cov = runs.iter().filter_map(|r| r.counter_coverage).fold(1.0f64, f64::min);
    if counters_ok && cov < 0.999 {
        warnings.push(format!(
            "counters were multiplexed (lowest coverage {:.1}%): counts are scaled estimates",
            cov * 100.0
        ));
    }
    if hashes.len() > 1 {
        warnings.push(format!(
            "stdout differs between runs ({} distinct hashes): the output is not deterministic, so it cannot be \
             checked by hash",
            hashes.len()
        ));
    }
    if exits.len() > 1 {
        warnings.push("exit status differs between runs".into());
    } else if first.exit_code != Some(0) {
        warnings.push(format!("the command ended with {}", bundle::exit_text(first.exit_code, first.signal)));
    }
    let ci_half = if ps.median != 0.0 { (ps.ci95[1] - ps.ci95[0]) / 2.0 / ps.median } else { 0.0 };
    let shown = |s: String| if redact { digest::redact(&s) } else { s };
    CommandReadings {
        label: p.label.clone(),
        argv: p.argv.iter().map(|a| shown(a.to_string_lossy().into_owned())).collect(),
        executable: p.digest.clone(),
        stdout: OutputSummary {
            sha256: first.stdout_sha256.clone(),
            bytes: first.stdout_bytes,
            identical_in_all_runs: hashes.len() == 1,
            distinct_hashes: hashes.len(),
        },
        exit: ExitSummary { exit_code: first.exit_code, signal: first.signal, identical_in_all_runs: exits.len() == 1 },
        quality: Quality {
            grade: stats::grade(runs.len(), rel_mad).to_string(),
            primary_metric: primary.to_string(),
            reps: runs.len(),
            rel_mad,
            rel_spread: ps.rel_spread,
            ci95_rel_halfwidth: ci_half,
            warnings,
        },
        stats: st,
        runs,
    }
}

fn failure(p: &Prepared, what: &str, run: &Run, err: &mut File) -> String {
    let mut msg = format!(
        "{} ({what}) ended with {}; use --ignore-failure to measure it anyway",
        p.label,
        bundle::exit_text(run.exit_code, run.signal)
    );
    let t = tail(err);
    if !t.is_empty() {
        msg.push_str("\n--- last lines of its stderr (not stored) ---\n");
        msg.push_str(&t);
    }
    msg
}

pub fn run(opts: Options) -> Result<i32, String> {
    if opts.no_counters && opts.require_counters {
        return Err("--no-counters and --require-counters contradict each other".into());
    }
    let shown = |s: &str| if opts.redact { digest::redact(s) } else { s.to_string() };

    // Commands: resolve and hash what will actually be executed.
    let mut prepared = Vec::new();
    for (i, argv) in opts.commands.iter().enumerate() {
        let exe = resolve(&argv[0])?;
        let (sha, bytes) = digest::sha256_file(&exe).map_err(|e| format!("{}: {e}", exe.display()))?;
        let label = opts.labels.get(i).cloned().unwrap_or_else(|| {
            if opts.redact {
                format!("cmd{}", i + 1)
            } else {
                Path::new(&argv[0]).file_name().unwrap_or(&argv[0]).to_string_lossy().into_owned()
            }
        });
        let digest = FileDigest { path: shown(&exe.to_string_lossy()), bytes, sha256: sha };
        prepared.push(Prepared { label, argv: argv.clone(), exe, digest });
    }
    let labels: Vec<String> = prepared.iter().map(|p| p.label.clone()).collect();
    for (i, p) in prepared.iter_mut().enumerate() {
        if labels.iter().filter(|l| **l == p.label).count() > 1 {
            p.label = format!("{}#{}", p.label, i + 1);
        }
    }

    // Inputs: hashes only.
    let mut input_paths = opts.inputs.clone();
    if let Some(s) = &opts.stdin {
        if !input_paths.contains(s) {
            input_paths.push(s.clone());
        }
    }
    let mut inputs = Vec::new();
    for p in &input_paths {
        inputs.push(digest::input(p, opts.redact).map_err(|e| format!("input {}: {e}", p.display()))?);
    }

    let environment = env::capture();

    // Counters: probe every event once; cycles and instructions are required for counted mode.
    let mut events = Vec::new();
    let mut unavailable = BTreeMap::new();
    let mut fallback = None;
    if opts.no_counters {
        fallback = Some("disabled with --no-counters".to_string());
    } else {
        for ev in Event::ALL {
            match Counter::open(ev) {
                Ok(_) => events.push(ev),
                Err(e) => {
                    let why = perf::explain(ev, &e);
                    if ev.required() && fallback.is_none() {
                        fallback = Some(why.clone());
                    }
                    unavailable.insert(ev.key().to_string(), why);
                }
            }
        }
        if let Some(f) = &fallback {
            events.clear();
            eprintln!("takt-harness: hardware counters unavailable: {f}");
            if opts.require_counters {
                return Err("--require-counters: stopping".into());
            }
            eprintln!("takt-harness: measuring wall time only; the bundle records counters.available = false");
        }
    }

    let io_err = |e: io::Error| e.to_string();
    let mut out = scratch("stdout").map_err(io_err)?;
    let mut err = scratch("stderr").map_err(io_err)?;
    let stdin = opts.stdin.as_deref();
    let progress = io::stderr().is_terminal();

    for p in &prepared {
        for w in 0..opts.warmup {
            let r = execute(p, stdin, &events, &mut out, &mut err).map_err(|e| format!("{}: {e}", p.label))?;
            if r.exit_code != Some(0) && !opts.ignore_failure {
                return Err(failure(p, &format!("warm-up {}", w + 1), &r, &mut err));
            }
        }
    }

    // Interleaved rounds: every command once per round, the order reversed on odd rounds.
    let mut runs: Vec<Vec<Run>> = vec![Vec::new(); prepared.len()];
    for round in 0..opts.reps {
        if progress {
            eprint!("\r  round {}/{} ", round + 1, opts.reps);
        }
        let order: Vec<usize> =
            if round % 2 == 0 { (0..prepared.len()).collect() } else { (0..prepared.len()).rev().collect() };
        for i in order {
            let p = &prepared[i];
            let mut r = execute(p, stdin, &events, &mut out, &mut err).map_err(|e| format!("{}: {e}", p.label))?;
            if r.exit_code != Some(0) && !opts.ignore_failure {
                if progress {
                    eprintln!();
                }
                return Err(failure(p, &format!("round {}", round + 1), &r, &mut err));
            }
            r.round = round;
            runs[i].push(r);
        }
    }
    if progress {
        eprint!("\r                    \r");
    }

    // A PMU that opens but never counts (some hypervisors) is treated as unavailable.
    if fallback.is_none() && runs.iter().flatten().all(|r| r.cycles.unwrap_or(0) == 0) {
        let why = "the counters opened but counted nothing (a hypervisor that exposes a PMU without counting)";
        eprintln!("takt-harness: {why}; reporting wall time only");
        fallback = Some(why.to_string());
        for r in runs.iter_mut().flatten() {
            for ev in Event::ALL {
                r.set_counter(ev.key(), None);
            }
            r.counter_coverage = None;
        }
        events.clear();
    }
    let counters_ok = fallback.is_none();

    let commands: Vec<CommandReadings> =
        prepared.iter().zip(runs).map(|(p, r)| readings(p, r, counters_ok, opts.redact)).collect();
    let b = Bundle {
        schema: bundle::SCHEMA.to_string(),
        harness: Harness { name: "takt-harness".into(), version: env!("CARGO_PKG_VERSION").into() },
        created_utc: utc_now(),
        environment,
        counters: Counters {
            available: counters_ok,
            mode: if counters_ok { COUNTER_MODE.to_string() } else { "wall time only".to_string() },
            events: events.iter().map(|e| e.key().to_string()).collect(),
            unavailable,
            fallback_reason: fallback,
        },
        protocol: Protocol {
            reps: opts.reps,
            warmup: opts.warmup,
            order: if prepared.len() > 1 {
                "interleaved rounds: every command once per round, listed order on even rounds, reverse on odd"
            } else {
                "one command, consecutive rounds"
            }
            .into(),
            stdin: match &opts.stdin {
                Some(p) => format!("file {}", shown(&p.to_string_lossy())),
                None => "/dev/null".into(),
            },
            stdout: "written to an unlinked temporary file and hashed with SHA-256 after each run; not stored".into(),
            bootstrap_resamples: stats::RESAMPLES,
            ci_level: stats::CI_LEVEL,
            redacted: opts.redact,
        },
        inputs,
        commands,
    };

    let json = serde_json::to_string_pretty(&b).map_err(|e| e.to_string())? + "\n";
    let mut f = File::create(&opts.out).map_err(|e| format!("{}: {e}", opts.out.display()))?;
    f.write_all(json.as_bytes()).map_err(|e| format!("{}: {e}", opts.out.display()))?;

    report::print_bundle(&b);
    if b.commands.len() > 1 {
        println!();
        for c in &b.commands[1..] {
            crate::compare::print_pair((&b, &b.commands[0]), (&b, c), true);
        }
    }
    println!("readings written to {} (review it with: takt-harness show {})", opts.out.display(), opts.out.display());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(20_731), (2026, 10, 5));
    }

    #[test]
    fn resolves_from_path() {
        assert!(resolve(OsStr::new("sh")).unwrap().ends_with("sh"));
        assert!(resolve(OsStr::new("definitely-not-a-command-xyz")).is_err());
        assert!(resolve(OsStr::new("./definitely/not/here")).is_err());
    }
}
