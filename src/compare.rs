//! `takt-harness compare`: speed-up factors (baseline / new) and the output check.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use crate::bundle::{exit_text, Bundle, CommandReadings, Unit, METRICS, SCHEMA};
use crate::report::{bytes, short, value};
use crate::stats;

pub fn load(path: &Path) -> Result<Bundle, String> {
    let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let b: Bundle = serde_json::from_reader(BufReader::new(f))
        .map_err(|e| format!("{}: not a readings bundle: {e}", path.display()))?;
    if b.schema != SCHEMA {
        return Err(format!("{}: schema {} is not {SCHEMA}", path.display(), b.schema));
    }
    if b.commands.is_empty() {
        return Err(format!("{}: no commands", path.display()));
    }
    Ok(b)
}

/// Facts that should be equal for two bundles to be compared, with the names of those that differ.
fn environment_differences(a: &Bundle, b: &Bundle) -> Vec<&'static str> {
    let (x, y) = (&a.environment, &b.environment);
    let mut d = Vec::new();
    if x.cpu.model != y.cpu.model {
        d.push("CPU model");
    }
    if x.cpu.microcode != y.cpu.microcode {
        d.push("microcode");
    }
    if x.kernel != y.kernel {
        d.push("kernel");
    }
    if x.os != y.os {
        d.push("OS");
    }
    if x.governors != y.governors {
        d.push("governor");
    }
    if x.boost != y.boost {
        d.push("boost");
    }
    if x.smt_active != y.smt_active {
        d.push("SMT");
    }
    if x.cpus_allowed != y.cpus_allowed {
        d.push("allowed CPUs");
    }
    if x.cpu.hypervisor != y.cpu.hypervisor {
        d.push("VM / bare metal");
    }
    if a.counters.available != b.counters.available {
        d.push("counters");
    }
    if a.harness.version != b.harness.version {
        d.push("harness version");
    }
    d
}

fn factor_cell(f: f64) -> String {
    if f.is_finite() {
        format!("{f:.3}×")
    } else {
        "-".into()
    }
}

/// Print one baseline/new comparison; returns true when the outputs are equivalent.
pub fn print_pair(base: (&Bundle, &CommandReadings), new: (&Bundle, &CommandReadings), same_session: bool) -> bool {
    let ((ba, a), (bb, b)) = (base, new);
    println!("baseline  {} · {} · sha256 {}", a.label, a.executable.path, short(&a.executable.sha256));
    println!("new       {} · {} · sha256 {}", b.label, b.executable.path, short(&b.executable.sha256));
    println!(
        "  {:<16} {:>11} {:>11}  {:>16}   {:<19} {:>9}",
        "metric", "baseline", "new", "factor base/new", "95% CI of factor", "speed-up"
    );
    let mut primary: Option<(&str, f64, [f64; 2])> = None;
    for (key, label, unit) in METRICS {
        let (Some(sa), Some(sb)) = (a.stats.get(key), b.stats.get(key)) else { continue };
        let f = if sb.median > 0.0 { sa.median / sb.median } else { f64::NAN };
        let ci =
            stats::bootstrap_ratio_ci(&a.samples(key), &b.samples(key), stats::RESAMPLES, stats::CI_LEVEL, stats::SEED);
        let ci_cell =
            if ci[0].is_finite() && ci[1].is_finite() { format!("{:.3} – {:.3}", ci[0], ci[1]) } else { "-".into() };
        // Speed-up applies to cycles and time; counts and memory are shown as factors only.
        let timelike = unit == Unit::Nanos || key == "cycles";
        let speedup = if timelike && f.is_finite() { format!("{:+.1}%", (f - 1.0) * 100.0) } else { String::new() };
        let row = format!(
            "  {:<16} {:>11} {:>11}  {:>16}   {:<19} {:>9}",
            label,
            value(sa.median, unit),
            value(sb.median, unit),
            factor_cell(f),
            ci_cell,
            speedup
        );
        println!("{}", row.trim_end());
        if primary.is_none() && (key == "cycles" || key == "wall_ns") && f.is_finite() {
            primary = Some((label, f, ci));
        }
    }

    // Equivalence: identical stdout in every run of both, and the same exit status.
    let a_det = a.stdout.identical_in_all_runs;
    let b_det = b.stdout.identical_in_all_runs;
    let stdout_ok = a_det && b_det && a.stdout.sha256 == b.stdout.sha256 && a.stdout.bytes == b.stdout.bytes;
    let stdout_line = if stdout_ok {
        format!("match · sha256 {} · {}", short(&a.stdout.sha256), bytes(a.stdout.bytes))
    } else if !a_det || !b_det {
        format!(
            "CANNOT CHECK · stdout varies between runs of the {}",
            if !a_det && !b_det {
                "baseline and the new build"
            } else if !a_det {
                "baseline"
            } else {
                "new build"
            }
        )
    } else {
        format!(
            "MISMATCH · baseline {} ({}) vs new {} ({})",
            short(&a.stdout.sha256),
            bytes(a.stdout.bytes),
            short(&b.stdout.sha256),
            bytes(b.stdout.bytes)
        )
    };
    let exit_ok = a.exit.identical_in_all_runs
        && b.exit.identical_in_all_runs
        && (a.exit.exit_code, a.exit.signal) == (b.exit.exit_code, b.exit.signal);
    let exit_line = if exit_ok {
        format!("match · {}", exit_text(a.exit.exit_code, a.exit.signal))
    } else {
        format!(
            "MISMATCH · baseline {} vs new {}",
            exit_text(a.exit.exit_code, a.exit.signal),
            exit_text(b.exit.exit_code, b.exit.signal)
        )
    };
    println!("  stdout  {stdout_line}");
    println!("  exit    {exit_line}");
    let inputs = |x: &Bundle| x.inputs.iter().map(|i| (i.kind.clone(), i.bytes, i.sha256.clone())).collect::<Vec<_>>();
    if !same_session {
        let (ia, ib) = (inputs(ba), inputs(bb));
        if ia.is_empty() && ib.is_empty() {
            println!("  inputs  none recorded (use --input to record their hashes)");
        } else if ia == ib {
            println!("  inputs  match ({} recorded)", ia.len());
        } else {
            println!("  inputs  DIFFER: the two bundles recorded different input hashes");
        }
        let diff = environment_differences(ba, bb);
        if diff.is_empty() {
            println!("  setup   same machine facts in both bundles (separate sessions, not interleaved)");
        } else {
            println!("  setup   DIFFERS: {} (factors across setups are not comparable)", diff.join(", "));
        }
    } else {
        println!("  setup   one session, interleaved rounds");
    }
    let equivalent = stdout_ok && exit_ok;
    let mut verdict =
        vec![if equivalent { "outputs identical".to_string() } else { "outputs NOT shown identical".to_string() }];
    if let Some((label, f, ci)) = primary {
        verdict.push(format!("{label} speed-up factor {f:.3}× (95% CI {:.3} – {:.3})", ci[0], ci[1]));
        verdict.push(format!("speed-up {:+.1}%", (f - 1.0) * 100.0));
        verdict.push(format!("{:.0} basis points", (10_000.0 * f).floor() - 10_000.0));
    }
    println!("verdict   {}", verdict.join(" · "));
    println!();
    equivalent
}

pub fn compare(paths: &[&Path]) -> Result<i32, String> {
    let bundles = paths.iter().map(|p| load(p)).collect::<Result<Vec<_>, _>>()?;
    if let [b] = bundles.as_slice() {
        if b.commands.len() < 2 {
            return Err(format!(
                "{} holds one command: give two bundles, or one measured with several commands (':::')",
                paths[0].display()
            ));
        }
    }
    println!("speed-up factor = baseline / new (per metric, ratio of medians); speed-up = 100 × (factor − 1) %");
    println!();
    let mut ok = true;
    match bundles.as_slice() {
        [b] => {
            for c in &b.commands[1..] {
                ok &= print_pair((b, &b.commands[0]), (b, c), true);
            }
        }
        [a, b] => {
            if a.commands.len() > 1 || b.commands.len() > 1 {
                println!("note: comparing the first command of each bundle");
            }
            ok = print_pair((a, &a.commands[0]), (b, &b.commands[0]), false);
        }
        _ => return Err("compare takes one or two bundles".into()),
    }
    Ok(if ok { 0 } else { 1 })
}
