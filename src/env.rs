//! The machine the readings were taken on. Only hardware and OS facts: no host name, user name,
//! environment variables or paths.

use std::collections::BTreeSet;
use std::fs;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Environment {
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub arch: String,
    pub cpu: Cpu,
    pub cpus_online: Option<usize>,
    /// CPUs the harness (and so the command) may run on, e.g. "42" after `taskset -c 42`.
    pub cpus_allowed: Option<String>,
    pub cpus_allowed_count: Option<usize>,
    /// Distinct cpufreq governors of the allowed CPUs.
    pub governors: Vec<String>,
    /// Turbo / boost enabled (None when the kernel does not say).
    pub boost: Option<bool>,
    /// Simultaneous multithreading active.
    pub smt_active: Option<bool>,
    pub perf_event_paranoid: Option<i32>,
    pub rustc: Option<String>,
    pub cargo: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Cpu {
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub family: Option<u32>,
    pub model_id: Option<u32>,
    pub stepping: Option<u32>,
    pub microcode: Option<String>,
    /// Instruction-set extensions relevant to performance that the CPU reports.
    pub isa: Vec<String>,
    pub x86_64_level: Option<String>,
    /// The CPU reports that it runs under a hypervisor (a virtual machine).
    pub hypervisor: bool,
}

/// Extensions recorded in `cpu.isa`, in this order, when present.
const ISA: &[&str] = &[
    "sse2",
    "ssse3",
    "sse4_1",
    "sse4_2",
    "popcnt",
    "avx",
    "avx2",
    "fma",
    "f16c",
    "bmi1",
    "bmi2",
    "abm",
    "movbe",
    "adx",
    "aes",
    "pclmulqdq",
    "sha_ni",
    "vaes",
    "vpclmulqdq",
    "gfni",
    "avx_vnni",
    "avx512f",
    "avx512dq",
    "avx512cd",
    "avx512bw",
    "avx512vl",
    "avx512ifma",
    "avx512vbmi",
    "avx512_vbmi2",
    "avx512_vnni",
    "avx512_bitalg",
    "avx512_vpopcntdq",
    "avx512_bf16",
    "avx512_fp16",
    "amx_tile",
    // aarch64 feature names
    "asimd",
    "sve",
    "sve2",
    "crc32",
    "sha2",
    "sha3",
    "atomics",
    "fphp",
    "asimdhp",
    "asimddp",
    "i8mm",
    "bf16",
];

const V2: &[&str] = &["cx16", "lahf_lm", "popcnt", "pni", "sse4_1", "sse4_2", "ssse3"];
const V3: &[&str] = &["avx", "avx2", "bmi1", "bmi2", "f16c", "fma", "abm", "movbe", "xsave"];
const V4: &[&str] = &["avx512f", "avx512bw", "avx512cd", "avx512dq", "avx512vl"];

fn read_trim(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn tool_version(tool: &str) -> Option<String> {
    let out = Command::new(tool)
        .arg("--version")
        .env("RUSTUP_AUTO_INSTALL", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).lines().next().map(|l| l.trim().to_string())
}

/// Parse a kernel CPU list such as "2-4,8,10-11".
pub fn parse_cpu_list(s: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for part in s.trim().split(',').filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                    out.extend(a..=b);
                }
            }
            None => out.extend(part.trim().parse::<usize>().ok()),
        }
    }
    out
}

fn cpu_info() -> Cpu {
    let text = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    // The first processor block describes the model; flags are the same on all cores we care about.
    let block = text.split("\n\n").next().unwrap_or("");
    let field = |name: &str| -> Option<String> {
        block.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == name).then(|| v.trim().to_string())
        })
    };
    let flags: BTreeSet<String> = field("flags")
        .or_else(|| field("Features"))
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let has = |f: &str| flags.contains(f);
    let level = if std::env::consts::ARCH != "x86_64" {
        None
    } else if V2.iter().all(|f| has(f)) {
        Some(if V3.iter().all(|f| has(f)) {
            if V4.iter().all(|f| has(f)) {
                "v4"
            } else {
                "v3"
            }
        } else {
            "v2"
        })
    } else {
        Some("v1")
    };
    Cpu {
        vendor: field("vendor_id").or_else(|| field("CPU implementer")),
        model: field("model name").or_else(|| field("CPU part")),
        family: field("cpu family").and_then(|v| v.parse().ok()),
        model_id: field("model").and_then(|v| v.parse().ok()),
        stepping: field("stepping").and_then(|v| v.parse().ok()),
        microcode: field("microcode"),
        isa: ISA.iter().filter(|f| has(f)).map(|f| f.to_string()).collect(),
        x86_64_level: level.map(|l| format!("x86-64-{l}")),
        hypervisor: has("hypervisor"),
    }
}

fn os_name() -> Option<String> {
    let text = fs::read_to_string("/etc/os-release").or_else(|_| fs::read_to_string("/usr/lib/os-release")).ok()?;
    text.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")).map(|v| v.trim_matches('"').to_string())
}

fn boost() -> Option<bool> {
    if let Some(v) = read_trim("/sys/devices/system/cpu/cpufreq/boost") {
        return Some(v == "1");
    }
    read_trim("/sys/devices/system/cpu/intel_pstate/no_turbo").map(|v| v == "0")
}

pub fn capture() -> Environment {
    let allowed = fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("Cpus_allowed_list:").map(|v| v.trim().to_string())));
    let allowed_cpus = allowed.as_deref().map(parse_cpu_list).unwrap_or_default();
    let governors: BTreeSet<String> = allowed_cpus
        .iter()
        .filter_map(|c| read_trim(&format!("/sys/devices/system/cpu/cpu{c}/cpufreq/scaling_governor")))
        .collect();
    Environment {
        os: os_name(),
        kernel: read_trim("/proc/sys/kernel/osrelease"),
        arch: std::env::consts::ARCH.to_string(),
        cpu: cpu_info(),
        cpus_online: read_trim("/sys/devices/system/cpu/online").map(|s| parse_cpu_list(&s).len()),
        cpus_allowed_count: allowed.as_ref().map(|_| allowed_cpus.len()),
        cpus_allowed: allowed,
        governors: governors.into_iter().collect(),
        boost: boost(),
        smt_active: read_trim("/sys/devices/system/cpu/smt/active").map(|v| v == "1"),
        perf_event_paranoid: crate::perf::paranoid(),
        rustc: tool_version("rustc"),
        cargo: tool_version("cargo"),
    }
}

/// Setup facts that make cycle counts noisier, as short notes.
pub fn checks(e: &Environment) -> Vec<String> {
    let mut out = Vec::new();
    match e.cpus_allowed_count {
        Some(1) => out.push(format!("pinned to CPU {}", e.cpus_allowed.as_deref().unwrap_or("?"))),
        Some(n) => out.push(format!("not pinned ({n} CPUs allowed; taskset -c N pins)")),
        None => {}
    }
    if !e.governors.is_empty() {
        out.push(format!("governor {}", e.governors.join("/")));
    }
    if let Some(b) = e.boost {
        out.push(format!("boost {}", if b { "on" } else { "off" }));
    }
    if let Some(s) = e.smt_active {
        out.push(format!("SMT {}", if s { "on" } else { "off" }));
    }
    out.push(if e.cpu.hypervisor { "virtual machine".to_string() } else { "bare metal".to_string() });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_lists() {
        assert_eq!(parse_cpu_list("2-4,8,10-11\n"), vec![2, 3, 4, 8, 10, 11]);
        assert_eq!(parse_cpu_list("0"), vec![0]);
        assert!(parse_cpu_list("").is_empty());
    }

    #[test]
    fn capture_has_basics() {
        let e = capture();
        assert!(!e.arch.is_empty());
        assert!(e.cpus_allowed_count.unwrap_or(1) >= 1);
    }
}
