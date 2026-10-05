# takt-harness

A free, open-source command-line tool that measures a program the way the TAKT measurement rig
does: user-mode CPU cycles and instructions from the hardware counters, wall time, the exit status
and a SHA-256 of the program's output. It writes everything into one JSON *readings bundle* that you
can read before you decide to send it to anyone.

It is a measurement tool only. It does not change, analyse or optimise your program.

- [What it measures and why](#what-it-measures-and-why)
- [Install](#install)
- [Usage](#usage)
- [Inside VMs, containers and CI](#inside-vms-containers-and-ci)
- [The readings bundle](#the-readings-bundle)
- [Privacy: what leaves your machine](#privacy-what-leaves-your-machine)
- [How it relates to TAKT's free estimate](#how-it-relates-to-takts-free-estimate)
- [Limits](#limits)

## What it measures and why

For every run of the command:

| Metric | Source | Why |
|---|---|---|
| `cycles:u` | hardware counter, user mode | The contract metric of TAKT (M1). Less sensitive to other load on the machine than time, and it does not include time spent in the kernel or waiting for I/O. |
| `instructions:u` | hardware counter, user mode | Nearly deterministic for a deterministic program (spread typically well below 0.1%), so it is a good regression signal even on noisy machines. |
| `cache-misses:u`, `branch-misses:u` | hardware counters, where available | Rough indicators of where the cycles go. The kernel maps these generic events to model-specific hardware events, so compare them only on the same CPU model. |
| wall time | monotonic clock around spawn and reap | What a user waits for. |
| user / sys time, peak RSS | `wait4` resource usage | How wall time splits between your code, the kernel and waiting; memory high-water mark. |
| exit status, stdout SHA-256 and size | the command's stdout is written to a temporary file and hashed | Two builds can only be compared if they produce the same output. |

How the counters are set up: the harness opens the counters on itself, disabled, with `inherit`
and `enable_on_exec`, counting user mode only (`exclude_kernel`, `exclude_hv`). The spawned command
inherits them and they switch on at its `exec`, so the count covers the program from its first
instruction, including the dynamic loader, and every child process it starts; it does not include
the harness. This is the same kind of counter the TAKT rig reads inside its measurement VM (vPMU).
If the kernel had to share the PMU between more counters than it has (multiplexing), the bundle
says so and the counts are scaled estimates.

Statistics per metric: median, min, max, mean, MAD (median absolute deviation), relative spread
`(max − min) / median`, and a 95% confidence interval of the median from a percentile bootstrap
(10,000 resamples, fixed seed, so the same samples always give the same interval).

Quality grade of a measurement, from the primary metric (`cycles:u`, or wall time without
counters):

| Grade | Rule |
|---|---|
| good | at least 9 runs and MAD / median ≤ 1% |
| fair | at least 5 runs and MAD / median ≤ 3% |
| poor | anything else |

MAD is used rather than the range so that one disturbed run does not decide the grade; outliers
get their own warning. Warnings also flag too few runs, multiplexed counters, output that differs
between runs, a non-zero exit, and missing counters.

Multiple commands separated by `:::` are measured in **interleaved rounds**: each round runs
every command once, in the listed order on even rounds and in reverse on odd rounds, so slow drift
(temperature, frequency, background load) affects all of them alike.

## Install

A ready static binary for any x86-64 Linux is attached to each
[release](https://github.com/rdmitry0911/takt-harness/releases):

```sh
curl -LO https://github.com/rdmitry0911/takt-harness/releases/latest/download/takt-harness-x86_64-linux
curl -LO https://github.com/rdmitry0911/takt-harness/releases/latest/download/SHA256SUMS
sha256sum -c SHA256SUMS && chmod +x takt-harness-x86_64-linux
```

Or build it from source, with Rust 1.74 or newer:

```sh
git clone https://github.com/rdmitry0911/takt-harness && cd takt-harness
cargo build --release
# the binary: target/release/takt-harness

# or a fully static binary that runs on any x86-64 Linux:
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
# the binary: target/x86_64-unknown-linux-musl/release/takt-harness
```

Linux only (it uses `perf_event_open`). Dependencies: `libc`, `serde`, `serde_json`, `sha2`.
The only `unsafe` code is the two system calls, in `src/sys.rs`.

## Usage

```text
takt-harness run [OPTIONS] -- <command> [args...] [::: <command> [args...]]...
takt-harness compare <baseline.json> <new.json>
takt-harness compare <readings.json>      first command is the baseline for the others
takt-harness show <readings.json>         print a bundle, to review it before sending
takt-harness --version | --help
```

Options of `run`:

| Option | Meaning |
|---|---|
| `--reps N` | measured runs per command (default 9) |
| `--warmup N` | unmeasured warm-up runs per command (default 1) |
| `--out FILE` | where to write the bundle (default `readings.json`) |
| `--input PATH` | a file or directory the command reads; only SHA-256 hashes are recorded (repeatable) |
| `--stdin FILE` | feed `FILE` to the command's stdin (default `/dev/null`); also recorded as an input |
| `--label NAME` | name of the next command, in order (repeatable; default: the executable's file name) |
| `--no-counters` | measure wall time only |
| `--require-counters` | stop with an error if the hardware counters are unavailable |
| `--redact` | store arguments and paths as SHA-256 prefixes instead of text |
| `--ignore-failure` | keep measuring when the command exits non-zero (otherwise it stops and shows the last lines of the command's stderr) |

The command inherits the environment and the working directory of the harness. Its stderr is
captured to a temporary file and shown only if it fails. Exit status of the harness: 0 success;
1 an error, a failing command, or (for `compare`) outputs that differ; 2 a usage error.

### Measure one program

Pin it to one CPU with `taskset` for stable numbers:

```sh
TAKT_INPUTS=data taskset -c 44 takt-harness run --input data --out base.json -- bin/ta-bench-base
```

Example output (the TAKT showcase `ta-indicators`, 1 M bars, an AMD Threadripper PRO 5975WX):

```text
takt-harness 0.1.0 · readings taken 2026-10-05 13:25:01 UTC
machine   AMD Ryzen Threadripper PRO 5975WX 32-Cores · x86-64-v3 · 48 CPUs online
          Ubuntu 24.04 LTS · Linux 7.0.14-14-pve · rustc 1.96.0 (ac68faa20 2026-05-25) · perf_event_paranoid 2
checks    pinned to CPU 44 · governor performance · boost on · SMT on · bare metal
counters  cycles:u instructions:u cache-misses:u branch-misses:u · user mode, child processes included
protocol  9 rounds after 1 warm-up · stdin /dev/null · CI: percentile bootstrap, 10000 resamples
input     data (dir, 1 file, 34.64 MiB, sha256 3bde6f75e6fc…)

ta-bench-base · bin/ta-bench-base · sha256 4a866b571954…
  metric                median         min         max   spread   95% CI of median
  cycles:u             1.374 G     1.369 G     1.383 G    0.99%   1.370 G – 1.379 G
  instructions:u       3.156 G     3.156 G     3.156 G    0.00%   3.156 G – 3.156 G
  wall time           490.2 ms    484.5 ms    512.8 ms    5.77%   485.8 ms – 494.1 ms
  user time           310.6 ms    306.2 ms    330.6 ms    7.86%   306.9 ms – 323.1 ms
  sys time            171.1 ms    154.8 ms    181.0 ms   15.29%   166.1 ms – 179.4 ms
  peak RSS           135.2 MiB   135.0 MiB   135.3 MiB    0.16%   135.2 MiB – 135.2 MiB
  cache-misses:u       2.219 M     2.100 M     2.319 M    9.87%   2.148 M – 2.303 M
  branch-misses:u      2.484 M     2.466 M     2.515 M    1.96%   2.474 M – 2.502 M
  stdout  sha256 ada682c61e5e… · 973 B · identical in 9/9 runs
  exit    exit 0 in 9/9 runs
  split   user 63% · sys 35% · waiting/other 2% of wall · IPC 2.30 · ≈4.42 GHz in user mode · 0.79 branch-misses/1k instr · 0.70 cache-misses/1k instr
  quality good: 9 runs of cycles:u, typical deviation 0.20% (MAD), range 0.99%, CI of median ±0.34%
readings written to base.json (review it with: takt-harness show base.json)
```

The `split` line shows where the wall time goes: your code (user), the kernel (sys: system calls,
page faults), and the rest (waiting for I/O, or other processes on the same CPU). IPC is
instructions per cycle; the clock estimate is user-mode cycles divided by user time.

### Compare a baseline and a new build

Best: both in one session, interleaved:

```sh
TAKT_INPUTS=data taskset -c 44 takt-harness run --input data --out ab.json \
    -- bin/ta-bench-base ::: bin/ta-bench-level2
takt-harness compare ab.json          # run already prints this comparison
```

Or two bundles measured separately:

```sh
takt-harness compare base.json new.json
```

```text
speed-up factor = baseline / new (per metric, ratio of medians); speed-up = 100 × (factor − 1) %

baseline  ta-bench-base · bin/ta-bench-base · sha256 4a866b571954…
new       ta-bench-level2 · bin/ta-bench-level2 · sha256 69fd4cdb378c…
  metric              baseline         new   factor base/new   95% CI of factor     speed-up
  cycles:u             1.374 G     142.6 M            9.633×   9.576 – 10.189        +863.3%
  instructions:u       3.156 G     391.7 M            8.057×   8.057 – 8.057
  wall time           490.2 ms    67.65 ms            7.247×   6.853 – 7.555         +624.7%
  user time           310.6 ms    32.17 ms            9.656×   8.827 – 10.141        +865.6%
  sys time            171.1 ms    34.70 ms            4.930×   4.719 – 5.289         +393.0%
  peak RSS           135.2 MiB   59.07 MiB            2.289×   2.288 – 2.291
  cache-misses:u       2.219 M     258.9 k            8.570×   7.557 – 10.542
  branch-misses:u      2.484 M     22.55 k          110.130×   109.688 – 110.908
  stdout  match · sha256 ada682c61e5e… · 973 B
  exit    match · exit 0
  inputs  match (1 recorded)
  setup   same machine facts in both bundles (separate sessions, not interleaved)
verdict   outputs identical · cycles:u speed-up factor 9.633× (95% CI 9.576 – 10.189) · speed-up +863.3% · 86329 basis points
```

Definitions, the same as in TAKT's Terms: for a baseline measurement B and a measurement C of the
new build, the **speed-up factor is B/C** and the **speed-up is 100 × (B/C − 1) %**. The reduction of
time, 100 × (1 − C/B) %, is a different quantity. Basis points are floor(10,000 × B/C) − 10,000 of
the median cycles. The interval of the factor is a bootstrap of the ratio of medians, resampling
both sides independently.

`compare` checks that stdout is identical in every run of both builds and that the exit status is
the same; otherwise it says MISMATCH (or CANNOT CHECK when a program's output varies between its
own runs) and exits with 1, so it can gate a CI job. A speed-up alone is not an acceptance: the
outputs must match. Comparing bundles from two different machines or settings is flagged under
`setup`.

## Inside VMs, containers and CI

The harness needs `perf_event_open` with hardware events. When they are unavailable it says why,
measures wall time only, and records `counters.available = false` and the reason in the bundle.
`--require-counters` turns that into an error. A quick check:

```sh
takt-harness run --require-counters --reps 3 -- true
```

| Situation | What to do |
|---|---|
| `kernel.perf_event_paranoid` above 2 (some distributions set 3 or 4) | `sudo sysctl kernel.perf_event_paranoid=2`, or give the binary the capability: `sudo setcap cap_perfmon+ep ./takt-harness` (kernel 5.8+) |
| Docker / Podman | the default seccomp profile usually blocks `perf_event_open` without the capability: `--cap-add PERFMON` (or `SYS_ADMIN` on older kernels), or a seccomp profile that allows the call. `perf_event_paranoid` is the host's. |
| KVM / QEMU / Proxmox VM | expose a virtual PMU: CPU type `host` (QEMU `-cpu host`; the PMU is on unless `pmu=off`) |
| VMware | enable "Virtualize CPU performance counters" for the VM |
| Cloud VMs and hosted CI runners | often have no vPMU. Bare-metal instances and self-hosted runners do. Without counters, use wall time with more runs, and treat `instructions:u` where available as the stable regression signal. |
| Hybrid CPUs (Intel P- and E-cores) | pin to one core type with `taskset`; the generic events may count on one core type only |

For the least noise: pin to one CPU (`taskset -c N`), use the `performance` governor, keep other
heavy processes off that CPU and its SMT sibling. The `checks` line shows what the harness sees
(pinning, governor, boost, SMT, VM or bare metal); it is informational and never changes settings.

## The readings bundle

One JSON document, schema `takt-harness/readings/v1`. Everything in it is listed here. Read it
with `takt-harness show FILE` or any text editor before sending it.

Top level:

| Field | Contents |
|---|---|
| `schema` | `"takt-harness/readings/v1"` |
| `harness` | `name`, `version` of the tool |
| `created_utc` | when the measurement finished, e.g. `"2026-10-05T13:25:01Z"` |
| `environment` | the machine (below) |
| `counters` | how counting was done (below) |
| `protocol` | how the runs were made (below) |
| `inputs` | hashes of `--input` and `--stdin` files (below) |
| `commands` | one entry per measured command (below) |

`environment` (hardware and OS facts only):

| Field | Contents |
|---|---|
| `os` | `PRETTY_NAME` from `/etc/os-release`, e.g. `"Ubuntu 24.04 LTS"` |
| `kernel` | kernel release, e.g. `"6.8.0-45-generic"` |
| `arch` | `"x86_64"`, `"aarch64"` |
| `cpu.vendor`, `cpu.model`, `cpu.family`, `cpu.model_id`, `cpu.stepping`, `cpu.microcode` | from `/proc/cpuinfo` |
| `cpu.isa` | performance-relevant extensions the CPU reports, e.g. `["sse4_2", "avx", "avx2", "fma", "bmi2", ...]` |
| `cpu.x86_64_level` | `"x86-64-v1"` … `"x86-64-v4"` |
| `cpu.hypervisor` | `true` inside a virtual machine |
| `cpus_online` | number of online CPUs |
| `cpus_allowed`, `cpus_allowed_count` | the CPUs the command could run on (`taskset`), e.g. `"44"`, `1` |
| `governors` | cpufreq governors of those CPUs, e.g. `["performance"]` |
| `boost` | turbo / boost on (`null` if unknown) |
| `smt_active` | SMT (hyper-threading) on (`null` if unknown) |
| `perf_event_paranoid` | the kernel setting |
| `rustc`, `cargo` | first line of `rustc --version` / `cargo --version` if they are in `PATH`, else `null` |

`counters`:

| Field | Contents |
|---|---|
| `available` | `true` when cycles and instructions were counted |
| `mode` | description of the counter setup, or `"wall time only"` |
| `events` | counted events: subset of `cycles`, `instructions`, `cache_misses`, `branch_misses` |
| `unavailable` | event → reason, for events that could not be opened |
| `fallback_reason` | why the measurement is wall time only, else `null` |

`protocol`: `reps`, `warmup`, `order` (interleaving rule), `stdin` (`"/dev/null"` or the file),
`stdout` (how output was hashed), `bootstrap_resamples`, `ci_level`, `redacted`.

`inputs[]`:

| Field | Contents |
|---|---|
| `path` | the path as given on the command line (redacted with `--redact`) |
| `kind` | `"file"` or `"dir"` |
| `bytes` | total size |
| `sha256` | of the file; for a directory, of its `files` list written as `sha256sum` output (`<hash>  <relative path>` per line, sorted by path), so it covers names and contents |
| `files[]` | directories only: `path` (relative), `bytes`, `sha256` of every regular file below it |

`commands[]`:

| Field | Contents |
|---|---|
| `label` | `--label`, or the executable's file name (`cmd1`, … with `--redact`) |
| `argv` | the command line as given (each element redacted with `--redact`) |
| `executable` | `path` (as resolved: as given, or found in `PATH`), `bytes`, `sha256` of the executable file |
| `runs[]` | one per measured run: `round`, `exit_code` / `signal`, `wall_ns`, `user_ns`, `sys_ns`, `max_rss_kib`, `cycles`, `instructions`, `cache_misses`, `branch_misses` (`null` when not counted), `counter_coverage` (lowest share of time a counter was on the PMU; 1.0 = no multiplexing), `stdout_sha256`, `stdout_bytes` |
| `stats` | metric → `n`, `median`, `min`, `max`, `mean`, `mad`, `rel_spread`, `ci95` `[low, high]`; metrics: `cycles`, `instructions`, `cache_misses`, `branch_misses`, `wall_ns`, `user_ns`, `sys_ns`, `max_rss_kib` |
| `stdout` | `sha256` and `bytes` of the first run's output, `identical_in_all_runs`, `distinct_hashes` |
| `exit` | `exit_code`, `signal`, `identical_in_all_runs` |
| `quality` | `grade`, `primary_metric`, `reps`, `rel_mad`, `rel_spread`, `ci95_rel_halfwidth`, `warnings[]` |

Warm-up runs are not stored.

## Privacy: what leaves your machine

Nothing. The harness makes no network connections and sends nothing anywhere; it only writes the
bundle file you name. The bundle leaves your machine only if you send it.

The bundle contains no source code, no program output and no file contents: output and files are
represented by SHA-256 hashes and sizes only. It does contain, in clear text:

- the command line you measured, and the paths of the executable and the input files
  (they may show directory or user names);
- the names of the files inside an `--input` directory;
- the hardware and OS facts listed above.

It does not record the host name, the user name, environment variables, the working directory or
the command's stderr. `--redact` replaces the command line, all paths and the file names inside
input directories with the first 16 hex digits of their SHA-256, so equal strings stay comparable
across bundles while the text is not stored. Labels you set with `--label` are kept as given. A hash of a short or guessable string (such as a
common file name) can be confirmed by someone who guesses it; if that matters, rename before
measuring.

## How it relates to TAKT's free estimate

- **Before any code is shared.** Measure your hot program with your real inputs and send us the
  bundle with the free-estimate request at <https://taktcycles.com>. From the readings alone, without
  source code, we give a preliminary range. Pin the run to one CPU and use at least 9 runs, so the
  grade is good.
- **The contract numbers come from the rig, not from this tool.** The order fixes a measurement
  passport (platform, CPU and frequency, inputs, tool versions, protocol), and the contract metric
  is the median of user-mode cycles on TAKT's reference rig. The harness reads the same kind of
  counter (user-mode cycles and instructions), so your numbers and the rig's should agree closely
  on the same CPU model and settings, but they are not the contract numbers.
- **Accept the work yourself.** When you receive a build, measure your baseline and the build in one
  interleaved session on your own hardware and run `compare`: it gives the speed-up factor by the
  definition in the Terms and checks that the output is identical.

## Limits

- Linux only. Counter events are the kernel's generic hardware events.
- Equivalence is checked by the hash of stdout and the exit status only. Programs that write their
  results to files need a wrapper that prints the result (or its hash) to stdout.
- Grandchildren that outlive the command (daemons) are not counted after it exits.
- No per-function profile and no deterministic (simulator) mode yet.

## License

MIT, see `LICENSE`. Copyright (c) 2026 ABX DEVELOPMENT LLP (TAKT, https://taktcycles.com).
