//! Human-readable output of a readings bundle.

use crate::bundle::{exit_text, metric_label, Bundle, CommandReadings, Unit, METRICS};
use crate::env;

/// Four significant digits.
fn sig4(v: f64) -> String {
    let a = v.abs();
    if a >= 1000.0 {
        format!("{v:.0}")
    } else if a >= 100.0 {
        format!("{v:.1}")
    } else if a >= 10.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.3}")
    }
}

pub fn value(x: f64, unit: Unit) -> String {
    if !x.is_finite() {
        return "-".into();
    }
    match unit {
        Unit::Count => {
            if x >= 1e9 {
                format!("{} G", sig4(x / 1e9))
            } else if x >= 1e6 {
                format!("{} M", sig4(x / 1e6))
            } else if x >= 1e4 {
                format!("{} k", sig4(x / 1e3))
            } else {
                format!("{x:.0}")
            }
        }
        Unit::Nanos => {
            if x >= 1e9 {
                format!("{} s", sig4(x / 1e9))
            } else if x >= 1e6 {
                format!("{} ms", sig4(x / 1e6))
            } else if x >= 1e3 {
                format!("{} µs", sig4(x / 1e3))
            } else {
                format!("{x:.0} ns")
            }
        }
        Unit::KiB => {
            if x >= 1024.0 * 1024.0 {
                format!("{} GiB", sig4(x / 1024.0 / 1024.0))
            } else if x >= 1024.0 {
                format!("{} MiB", sig4(x / 1024.0))
            } else {
                format!("{x:.0} KiB")
            }
        }
    }
}

pub fn bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else {
        value(n as f64 / 1024.0, Unit::KiB)
    }
}

pub fn short(h: &str) -> String {
    if h.len() > 12 {
        format!("{}…", &h[..12])
    } else {
        h.to_string()
    }
}

fn pct(x: f64) -> String {
    format!("{:.2}%", x * 100.0)
}

pub fn print_environment(b: &Bundle) {
    let e = &b.environment;
    let mut machine = vec![e.cpu.model.clone().unwrap_or_else(|| e.arch.clone())];
    if let Some(l) = &e.cpu.x86_64_level {
        machine.push(l.clone());
    }
    if let Some(n) = e.cpus_online {
        machine.push(format!("{n} CPUs online"));
    }
    println!("machine   {}", machine.join(" · "));
    let mut sw = Vec::new();
    if let Some(o) = &e.os {
        sw.push(o.clone());
    }
    if let Some(k) = &e.kernel {
        sw.push(format!("Linux {k}"));
    }
    if let Some(r) = &e.rustc {
        sw.push(r.clone());
    }
    if let Some(p) = e.perf_event_paranoid {
        sw.push(format!("perf_event_paranoid {p}"));
    }
    println!("          {}", sw.join(" · "));
    println!("checks    {}", env::checks(e).join(" · "));
    if b.counters.available {
        let ev: Vec<&str> = b.counters.events.iter().map(|k| metric_label(k)).collect();
        println!("counters  {} · user mode, child processes included", ev.join(" "));
    } else {
        println!("counters  none, wall time only: {}", b.counters.fallback_reason.as_deref().unwrap_or("unknown"));
    }
    println!(
        "protocol  {} round{} after {} warm-up{} · stdin {} · CI: percentile bootstrap, {} resamples",
        b.protocol.reps,
        if b.protocol.reps == 1 { "" } else { "s" },
        b.protocol.warmup,
        if b.protocol.warmup == 1 { "" } else { "s" },
        b.protocol.stdin,
        b.protocol.bootstrap_resamples
    );
    for i in &b.inputs {
        let files = match (i.kind.as_str(), i.files.len()) {
            ("dir", 1) => ", 1 file".to_string(),
            ("dir", n) => format!(", {n} files"),
            _ => String::new(),
        };
        println!("input     {} ({}{}, {}, sha256 {})", i.path, i.kind, files, bytes(i.bytes), short(&i.sha256));
    }
}

pub fn print_command(c: &CommandReadings) {
    println!();
    println!("{} · {} · sha256 {}", c.label, c.executable.path, short(&c.executable.sha256));
    if c.argv.len() > 1 {
        println!("  args    {}", c.argv[1..].join(" "));
    }
    println!("  {:<16} {:>11} {:>11} {:>11} {:>8}   95% CI of median", "metric", "median", "min", "max", "spread");
    for (key, label, unit) in METRICS {
        if let Some(s) = c.stats.get(key) {
            println!(
                "  {:<16} {:>11} {:>11} {:>11} {:>8}   {} – {}",
                label,
                value(s.median, unit),
                value(s.min, unit),
                value(s.max, unit),
                pct(s.rel_spread),
                value(s.ci95[0], unit),
                value(s.ci95[1], unit)
            );
        }
    }
    let n = c.runs.len();
    println!(
        "  stdout  sha256 {} · {} · {}",
        short(&c.stdout.sha256),
        bytes(c.stdout.bytes),
        if c.stdout.identical_in_all_runs {
            format!("identical in {n}/{n} runs")
        } else {
            format!("{} different hashes in {n} runs", c.stdout.distinct_hashes)
        }
    );
    println!(
        "  exit    {}{}",
        exit_text(c.exit.exit_code, c.exit.signal),
        if c.exit.identical_in_all_runs {
            format!(" in {n}/{n} runs")
        } else {
            " in the first run; differs later".into()
        }
    );
    let split = time_split(c);
    if !split.is_empty() {
        println!("  split   {}", split.join(" · "));
    }
    let q = &c.quality;
    let label = metric_label(&q.primary_metric);
    println!(
        "  quality {}: {} runs of {}, typical deviation {} (MAD), range {}, CI of median ±{}",
        q.grade,
        q.reps,
        label,
        pct(q.rel_mad),
        pct(q.rel_spread),
        pct(q.ci95_rel_halfwidth)
    );
    for w in &q.warnings {
        println!("  warning {w}");
    }
}

/// Where the time goes, from medians: CPU vs wall, IPC, clock, miss rates.
fn time_split(c: &CommandReadings) -> Vec<String> {
    let m = |k: &str| c.stats.get(k).map(|s| s.median);
    let mut out = Vec::new();
    if let (Some(w), Some(u), Some(s)) = (m("wall_ns"), m("user_ns"), m("sys_ns")) {
        if w > 0.0 {
            let cpu = u + s;
            if cpu > w * 1.05 {
                out.push(format!("CPU time {:.2}× wall (parallel)", cpu / w));
                out.push(format!("user {:.0}% · sys {:.0}% of CPU", u / cpu * 100.0, s / cpu * 100.0));
            } else {
                out.push(format!(
                    "user {:.0}% · sys {:.0}% · waiting/other {:.0}% of wall",
                    u / w * 100.0,
                    s / w * 100.0,
                    ((w - cpu) / w * 100.0).max(0.0)
                ));
            }
        }
    }
    if let (Some(cy), Some(ins)) = (m("cycles"), m("instructions")) {
        if cy > 0.0 {
            out.push(format!("IPC {:.2}", ins / cy));
        }
        if let Some(u) = m("user_ns") {
            if u > 1e6 {
                out.push(format!("≈{:.2} GHz in user mode", cy / u));
            }
        }
        if ins > 0.0 {
            if let Some(b) = m("branch_misses") {
                out.push(format!("{:.2} branch-misses/1k instr", b / ins * 1000.0));
            }
            if let Some(cm) = m("cache_misses") {
                out.push(format!("{:.2} cache-misses/1k instr", cm / ins * 1000.0));
            }
        }
    }
    out
}

pub fn print_bundle(b: &Bundle) {
    println!(
        "takt-harness {} · readings taken {} UTC",
        b.harness.version,
        b.created_utc.replace('T', " ").trim_end_matches('Z')
    );
    print_environment(b);
    for c in &b.commands {
        print_command(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units() {
        assert_eq!(value(1_376_912_345.0, Unit::Count), "1.377 G");
        assert_eq!(value(143_300_000.0, Unit::Count), "143.3 M");
        assert_eq!(value(204_512.0, Unit::Count), "204.5 k");
        assert_eq!(value(950.0, Unit::Count), "950");
        assert_eq!(value(503_210_000.0, Unit::Nanos), "503.2 ms");
        assert_eq!(value(1_500.0, Unit::Nanos), "1.500 µs");
        assert_eq!(value(2048.0, Unit::KiB), "2.000 MiB");
        assert_eq!(bytes(100), "100 B");
        assert_eq!(short("0123456789abcdef"), "0123456789ab…");
    }
}
