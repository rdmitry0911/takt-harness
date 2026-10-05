//! End-to-end tests: the real binary measures `true` and a small busy loop.

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const BUSY: &str = "i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; echo $i";

fn harness(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_takt-harness")).args(args).output().expect("run takt-harness")
}

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli");
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn bundle(path: &PathBuf) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

#[test]
fn version_and_help() {
    let o = harness(&["--version"]);
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), format!("takt-harness {}", env!("CARGO_PKG_VERSION")));
    let o = harness(&["--help"]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("USAGE"));
    assert_eq!(harness(&["run"]).status.code(), Some(2));
    assert_eq!(harness(&["frobnicate"]).status.code(), Some(2));
    assert_eq!(harness(&["run", "--reps", "x", "--", "true"]).status.code(), Some(2));
}

#[test]
fn measures_true() {
    let out = tmp("true.json");
    let o = harness(&["run", "--reps", "3", "--warmup", "1", "--out", out.to_str().unwrap(), "--", "true"]);
    assert!(o.status.success(), "{}", text(&o));
    let b = bundle(&out);
    assert_eq!(b["schema"], "takt-harness/readings/v1");
    assert_eq!(b["protocol"]["reps"], 3);
    let c = &b["commands"][0];
    assert_eq!(c["label"], "true");
    assert_eq!(c["argv"], serde_json::json!(["true"]));
    assert_eq!(c["executable"]["sha256"].as_str().unwrap().len(), 64);
    let runs = c["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 3);
    for r in runs {
        assert_eq!(r["exit_code"], 0);
        assert_eq!(r["stdout_sha256"], EMPTY_SHA256);
        assert!(r["wall_ns"].as_u64().unwrap() > 0);
    }
    assert!(c["stats"]["wall_ns"]["median"].as_f64().unwrap() > 0.0);
    if b["counters"]["available"] == true {
        assert_eq!(c["quality"]["primary_metric"], "cycles");
        for r in runs {
            assert!(r["cycles"].as_u64().unwrap() > 0);
            assert!(r["instructions"].as_u64().unwrap() > 1000);
        }
    } else {
        eprintln!("hardware counters unavailable here: {}", b["counters"]["fallback_reason"]);
        assert_eq!(c["quality"]["primary_metric"], "wall_ns");
    }
}

#[test]
fn busy_loop_interleaved_with_true() {
    let out = tmp("busy.json");
    let o = harness(&[
        "run",
        "--reps",
        "3",
        "--warmup",
        "0",
        "--out",
        out.to_str().unwrap(),
        "--",
        "true",
        ":::",
        "sh",
        "-c",
        BUSY,
    ]);
    assert!(o.status.success(), "{}", text(&o));
    let b = bundle(&out);
    let (t, busy) = (&b["commands"][0], &b["commands"][1]);
    assert_eq!(busy["label"], "sh");
    assert_eq!(busy["stdout"]["bytes"], 6); // "20000\n"
    assert_eq!(busy["stdout"]["sha256"], "0be508172e87a2af98f344d18610bbaaa0e6bbfcef0c7804b24457f839e129c9");
    assert_eq!(busy["stdout"]["identical_in_all_runs"], true);
    // Interleaving: round 1 runs the commands in reverse order; every round has both.
    let rounds: Vec<u64> = busy["runs"].as_array().unwrap().iter().map(|r| r["round"].as_u64().unwrap()).collect();
    assert_eq!(rounds, vec![0, 1, 2]);
    if b["counters"]["available"] == true {
        let ins = |c: &Value| c["stats"]["instructions"]["median"].as_f64().unwrap();
        assert!(ins(busy) > 10.0 * ins(t), "busy {} vs true {}", ins(busy), ins(t));
    } else {
        let wall = |c: &Value| c["stats"]["wall_ns"]["median"].as_f64().unwrap();
        assert!(wall(busy) > wall(t));
    }
    // Different stdout: compare must report it and exit 1.
    let o = harness(&["compare", out.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("MISMATCH"));
}

#[test]
fn compare_two_bundles_of_the_same_program() {
    let (a, b) = (tmp("same-a.json"), tmp("same-b.json"));
    for p in [&a, &b] {
        let o = harness(&["run", "--reps", "3", "--warmup", "0", "--out", p.to_str().unwrap(), "--", "sh", "-c", BUSY]);
        assert!(o.status.success(), "{}", text(&o));
    }
    let o = harness(&["compare", a.to_str().unwrap(), b.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let t = text(&o);
    assert!(t.contains("stdout  match"), "{t}");
    assert!(t.contains("factor base/new"), "{t}");
    let o = harness(&["show", a.to_str().unwrap()]);
    assert!(o.status.success());
    assert!(text(&o).contains("quality"));
}

#[test]
fn wall_time_only_fallback() {
    let out = tmp("wall.json");
    let o = harness(&["run", "--reps", "2", "--no-counters", "--out", out.to_str().unwrap(), "--", "true"]);
    assert!(o.status.success(), "{}", text(&o));
    let b = bundle(&out);
    assert_eq!(b["counters"]["available"], false);
    assert_eq!(b["counters"]["fallback_reason"], "disabled with --no-counters");
    let c = &b["commands"][0];
    assert!(c["runs"][0]["cycles"].is_null());
    assert!(c["stats"].get("cycles").is_none());
    assert_eq!(c["quality"]["primary_metric"], "wall_ns");
    assert_eq!(harness(&["run", "--no-counters", "--require-counters", "--", "true"]).status.code(), Some(1));
}

#[test]
fn failing_command() {
    let out = tmp("false.json");
    let o = harness(&["run", "--reps", "2", "--out", out.to_str().unwrap(), "--", "sh", "-c", "echo boom >&2; exit 3"]);
    assert_eq!(o.status.code(), Some(1));
    let t = text(&o);
    assert!(t.contains("exit 3") && t.contains("boom") && t.contains("--ignore-failure"), "{t}");
    let o = harness(&["run", "--reps", "2", "--ignore-failure", "--out", out.to_str().unwrap(), "--", "false"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(bundle(&out)["commands"][0]["exit"]["exit_code"], 1);
    assert_eq!(harness(&["run", "--", "no-such-command-xyz"]).status.code(), Some(1));
}

#[test]
fn inputs_are_hashed_and_redaction_hides_text() {
    let dir = tmp("inputs");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a.txt"), "abc").unwrap();
    std::fs::write(dir.join("sub/b.txt"), "").unwrap();
    let file = dir.join("a.txt");
    let out = tmp("inputs.json");
    let o = harness(&[
        "run",
        "--reps",
        "1",
        "--warmup",
        "0",
        "--input",
        dir.to_str().unwrap(),
        "--stdin",
        file.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--",
        "cat",
    ]);
    assert!(o.status.success(), "{}", text(&o));
    let b = bundle(&out);
    let inputs = b["inputs"].as_array().unwrap();
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0]["kind"], "dir");
    assert_eq!(inputs[0]["files"].as_array().unwrap().len(), 2);
    assert_eq!(inputs[0]["files"][0]["path"], "a.txt");
    assert_eq!(inputs[0]["files"][1]["path"], "sub/b.txt");
    let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_eq!(inputs[1]["sha256"], abc);
    // cat copies stdin to stdout: the stdout hash is that of the file, the text is not stored.
    assert_eq!(b["commands"][0]["stdout"]["sha256"], abc);
    assert!(!std::fs::read_to_string(&out).unwrap().contains("\"abc\""));

    let o = harness(&[
        "run",
        "--reps",
        "1",
        "--warmup",
        "0",
        "--redact",
        "--input",
        dir.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--",
        "sh",
        "-c",
        "true",
    ]);
    assert!(o.status.success(), "{}", text(&o));
    let raw = std::fs::read_to_string(&out).unwrap();
    assert!(!raw.contains("a.txt") && !raw.contains("\"-c\"") && !raw.contains(dir.to_str().unwrap()));
    let b = bundle(&out);
    assert_eq!(b["commands"][0]["label"], "cmd1");
    assert!(b["commands"][0]["argv"].as_array().unwrap().iter().all(|a| a.as_str().unwrap().starts_with("sha256:")));
}
