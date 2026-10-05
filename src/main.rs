//! takt-harness: measures a command the way the TAKT rig does and writes a reviewable readings
//! bundle. A measurement tool only.
#![deny(unsafe_code)]

mod bundle;
mod compare;
mod digest;
mod env;
mod perf;
mod report;
mod run;
mod stats;
mod sys;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
takt-harness — measure a command: user-mode cycles, instructions, wall time, output hash

USAGE
  takt-harness run [OPTIONS] -- <command> [args...] [::: <command> [args...]]...
  takt-harness compare <baseline.json> <new.json>
  takt-harness compare <readings.json>      first command is the baseline for the others
  takt-harness show <readings.json>         print a bundle, to review it before sending
  takt-harness --version | --help

RUN OPTIONS
  --reps N            measured runs per command (default 9)
  --warmup N          unmeasured warm-up runs per command (default 1)
  --out FILE          where to write the readings bundle (default readings.json)
  --input PATH        a file or directory the command reads; only its SHA-256 is recorded
                      (repeatable)
  --stdin FILE        feed FILE to the command's stdin (default /dev/null); recorded as an input
  --label NAME        name of the next command, in order (repeatable)
  --no-counters       wall time only, without hardware counters
  --require-counters  stop with an error if hardware counters are unavailable
  --redact            store arguments and paths as SHA-256 prefixes instead of text
  --ignore-failure    keep measuring when the command exits non-zero

Several commands separated by ':::' are measured in interleaved rounds (listed order on even
rounds, reverse on odd). Per run: user-mode cycles and instructions of the command and all its
child processes, cache-misses and branch-misses where available, wall/user/sys time, peak RSS,
exit status, SHA-256 of stdout. Nothing leaves the machine; the bundle holds no code and no file
contents. Exit status: 0 ok, 1 failure or outputs differ (compare), 2 usage error.
";

fn usage(msg: &str) -> ExitCode {
    eprintln!("takt-harness: {msg}\nTry 'takt-harness --help'.");
    ExitCode::from(2)
}

fn number(name: &str, v: Option<OsString>) -> Result<u32, String> {
    let v = v.ok_or_else(|| format!("{name} needs a value"))?;
    v.to_str().and_then(|s| s.parse().ok()).ok_or_else(|| format!("{name}: not a number: {}", v.to_string_lossy()))
}

fn parse_run(args: Vec<OsString>) -> Result<run::Options, String> {
    let mut o = run::Options {
        reps: 9,
        warmup: 1,
        out: PathBuf::from("readings.json"),
        inputs: Vec::new(),
        stdin: None,
        labels: Vec::new(),
        no_counters: false,
        require_counters: false,
        redact: false,
        ignore_failure: false,
        commands: Vec::new(),
    };
    let mut it = args.into_iter();
    let mut rest: Vec<OsString> = Vec::new();
    while let Some(a) = it.next() {
        let s = a.to_string_lossy().into_owned();
        // --name=value is accepted as well as --name value.
        let (name, inline) = match s.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_string(), Some(OsString::from(v))),
            _ => (s.clone(), None),
        };
        let val = |it: &mut std::vec::IntoIter<OsString>| inline.clone().or_else(|| it.next());
        match name.as_str() {
            "--" => {
                rest.extend(it.by_ref());
                break;
            }
            "--reps" => o.reps = number("--reps", val(&mut it))?,
            "--warmup" => o.warmup = number("--warmup", val(&mut it))?,
            "--out" => o.out = val(&mut it).ok_or("--out needs a file")?.into(),
            "--input" => o.inputs.push(val(&mut it).ok_or("--input needs a path")?.into()),
            "--stdin" => o.stdin = Some(val(&mut it).ok_or("--stdin needs a file")?.into()),
            "--label" => o.labels.push(val(&mut it).ok_or("--label needs a name")?.to_string_lossy().into_owned()),
            "--no-counters" => o.no_counters = true,
            "--require-counters" => o.require_counters = true,
            "--redact" => o.redact = true,
            "--ignore-failure" => o.ignore_failure = true,
            _ if s.starts_with('-') => return Err(format!("unknown option {s}")),
            _ => {
                // The command starts at the first non-option argument.
                rest.push(a);
                rest.extend(it.by_ref());
                break;
            }
        }
    }
    for part in rest.split(|a| a == ":::") {
        if part.is_empty() {
            return Err("empty command (check the ':::' separators)".into());
        }
        o.commands.push(part.to_vec());
    }
    if o.commands.is_empty() {
        return Err("no command given: takt-harness run [OPTIONS] -- <command> [args...]".into());
    }
    if o.reps == 0 {
        return Err("--reps must be at least 1".into());
    }
    if o.labels.len() > o.commands.len() {
        return Err("more --label options than commands".into());
    }
    Ok(o)
}

fn exit(r: Result<i32, String>) -> ExitCode {
    match r {
        Ok(c) => ExitCode::from(c as u8),
        Err(e) => {
            eprintln!("takt-harness: {e}");
            ExitCode::from(1)
        }
    }
}

fn main() -> ExitCode {
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.is_empty() {
        print!("{HELP}");
        return ExitCode::from(2);
    }
    let cmd = args.remove(0).to_string_lossy().into_owned();
    match cmd.as_str() {
        "-h" | "--help" | "help" => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        "-V" | "--version" | "version" => {
            println!("takt-harness {VERSION}");
            ExitCode::SUCCESS
        }
        "run" => {
            if matches!(args.first().and_then(|a| a.to_str()), Some("-h" | "--help")) {
                print!("{HELP}");
                return ExitCode::SUCCESS;
            }
            match parse_run(args) {
                Ok(o) => exit(run::run(o)),
                Err(e) => usage(&e),
            }
        }
        "compare" => {
            if args.is_empty() || args.len() > 2 {
                return usage("compare takes <baseline.json> <new.json>, or one bundle with several commands");
            }
            let paths: Vec<&Path> = args.iter().map(Path::new).collect();
            exit(compare::compare(&paths))
        }
        "show" => {
            if args.len() != 1 {
                return usage("show takes one bundle");
            }
            exit(compare::load(Path::new(&args[0])).map(|b| {
                report::print_bundle(&b);
                0
            }))
        }
        other => usage(&format!("unknown command '{other}'")),
    }
}
