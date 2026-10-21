//! Shim overhead benchmark (SPEC §7, boundaries B1-B3). Opt-in: it is `#[ignore]`d.
//!
//! ```sh
//! cargo test --release --test perf -- --ignored --nocapture
//! AGENTDIET_PERF_SAMPLES=2000 AGENTDIET_PERF_OUT=/tmp/shim.json \
//!     cargo test --release --test perf -- --ignored --nocapture
//! ```
//!
//! Four arms, all run against a throwaway `TempDir` that holds the fake home, the "real" directory
//! and the shims directory. Children get `env_clear()`, a `HOME` inside the `TempDir` and a
//! synthetic `PATH`, so nothing here reads or touches the user's real assistant configs or `PATH`.
//!
//! | Arm | Measures | Boundary |
//! |---|---|---|
//! | E0 | spawn -> wait of `real/git` (a symlink to the system `true`) | baseline |
//! | E1 | spawn -> wait of `agentdiet hook noop` | `E1 - E0` = B2, process launch |
//! | E2 | spawn -> wait of `shims/git` with `PATH=shims:real` | `E2 - E0` = B1, end to end |
//! | EF | spawn -> wait of `floor/git`, a minimal C program that only `execv`s `real/git` | `EF - E0` = F, the exec floor any PATH shim pays |
//! | R1 | in process: `shim_name` + `resolve_real` on that synthetic `PATH` | B3, routing only |
//!
//! E0/E1/E2 are interleaved per iteration (the order alternates), so drift and thermal effects hit
//! every arm alike. B1 is judged against the SPEC's requirement, F + 0.5 ms with F measured in this
//! run (SPEC §7 "B1 amendment"), and B3 against 0.2 ms; each is reported as MET or NOT MET (or
//! UNMEASURED when no C compiler is there to build the floor arm). This file never redefines a
//! requirement and never fails the run on a miss: it is a measurement.
//! Debug builds are refused. The non-ignored tests only check the fixtures and the statistics.
#![cfg(unix)]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

const CARGO_EXE: &str = env!("CARGO_BIN_EXE_agentdiet");
/// SPEC §7: B1 may exceed the exec floor F by at most this much.
const B1_ALLOWANCE_OVER_FLOOR_MS: f64 = 0.5;
/// SPEC §7: B3, in-process routing.
const B3_REQUIREMENT_MS: f64 = 0.2;
const DEFAULT_SAMPLES: usize = 1000;
const DEFAULT_WARMUP: usize = 50;
const R1_ITERATIONS: usize = 10_000;

/// The `agentdiet` executable under test. `cargo test --release` relinks `target/release/agentdiet`
/// with dev-dependency features unified, so its bytes can differ from a plain `cargo build
/// --release --locked`. Set `AGENTDIET_PERF_BIN` to time that exact artifact instead (copy it out
/// of `target/` first, because the test build overwrites `target/release/agentdiet` before it
/// runs). The report records the sha256 of whichever binary ran.
fn exe() -> PathBuf {
    match std::env::var_os("AGENTDIET_PERF_BIN") {
        Some(p) => {
            let p = PathBuf::from(p);
            assert!(
                p.is_file(),
                "AGENTDIET_PERF_BIN {} is not a file",
                p.display()
            );
            fs::canonicalize(p).expect("canonicalize AGENTDIET_PERF_BIN")
        }
        None => PathBuf::from(CARGO_EXE),
    }
}

/// Where a stock `true` lives. macOS has only `/usr/bin/true`; most Linux distributions have both.
const TRUE_CANDIDATES: [&str; 2] = ["/usr/bin/true", "/bin/true"];

struct Fixture {
    _dir: TempDir,
    home: PathBuf,
    shims: PathBuf,
    real: PathBuf,
    /// `shims:real`
    shim_path: std::ffi::OsString,
    /// `real`
    direct_path: std::ffi::OsString,
    /// `floor/git`, when a C compiler could build it.
    floor: Option<PathBuf>,
}

/// Build `<dir>/floor/git`: C that only `execv`s `real` (the cost any PATH shim pays before doing
/// anything). `None` when no C compiler works; the build needs one anyway, so CI always has it.
fn build_floor(dir: &Path, real: &Path) -> Option<PathBuf> {
    let src = dir.join("floor.c");
    let out_dir = dir.join("floor");
    fs::create_dir_all(&out_dir).ok()?;
    let real = real.to_str()?.replace('\\', "\\\\").replace('"', "\\\"");
    fs::write(
        &src,
        format!(
            "#include <unistd.h>\nint main(int argc, char **argv) {{ (void)argc; execv(\"{real}\", argv); return 127; }}\n"
        ),
    )
    .ok()?;
    let out = out_dir.join("git");
    let cc = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let ok = Command::new(cc)
        .args(["-O2", "-o"])
        .arg(&out)
        .arg(&src)
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(out)
}

impl Fixture {
    /// Build `<tmp>/home/.agentdiet/shims/git -> agentdiet` and `<tmp>/real/git -> true`. The shim
    /// looks for its shims directory at `$HOME/.agentdiet/shims`, so the layout must match.
    fn new() -> Self {
        let true_bin = TRUE_CANDIDATES
            .iter()
            .map(Path::new)
            .find(|p| p.is_file())
            .unwrap_or_else(|| {
                panic!("no stock `true` found at {TRUE_CANDIDATES:?}; cannot build the fixture")
            });
        let dir = TempDir::new().expect("tempdir");
        let home = dir.path().join("home");
        let shims = home.join(".agentdiet").join("shims");
        let real = dir.path().join("real");
        fs::create_dir_all(&shims).expect("shims dir");
        fs::create_dir_all(&real).expect("real dir");
        symlink(true_bin, real.join("git")).expect("real/git");
        symlink(exe(), shims.join("git")).expect("shims/git");
        let shim_path = std::env::join_paths([&shims, &real]).expect("PATH");
        let direct_path = std::env::join_paths([&real]).expect("PATH");
        let floor = build_floor(dir.path(), &real.join("git"));
        Self {
            floor,
            _dir: dir,
            home,
            shims,
            real,
            shim_path,
            direct_path,
        }
    }

    fn command(&self, program: &Path, path_var: &OsStr) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .env("HOME", &self.home)
            .env("PATH", path_var)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd
    }

    fn e0(&self) -> Command {
        self.command(&self.real.join("git"), &self.direct_path)
    }

    fn e1(&self) -> Command {
        let mut cmd = self.command(&exe(), &self.direct_path);
        cmd.args(["hook", "noop"]);
        cmd
    }

    fn e2(&self) -> Command {
        self.command(&self.shims.join("git"), &self.shim_path)
    }

    fn ef(&self) -> Option<Command> {
        self.floor
            .as_ref()
            .map(|floor| self.command(floor, &self.direct_path))
    }
}

/// Spawn, wait, and return how long that took. Fails the run on a non-zero exit, which would mean
/// the fixture is broken (e.g. the shim could not find the real binary: exit 127).
fn time(mut cmd: Command, arm: &str) -> Duration {
    let start = Instant::now();
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("{arm}: spawn failed: {e}"));
    let elapsed = start.elapsed();
    assert!(status.success(), "{arm}: exited with {status}");
    elapsed
}

/// Nearest-rank percentile of an ascending-sorted slice (`p` in 0..=100).
fn percentile(sorted: &[f64], p: f64) -> f64 {
    assert!(!sorted.is_empty());
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn summarize(values_ms: &[f64]) -> Value {
    let mut sorted = values_ms.to_vec();
    sorted.sort_by(f64::total_cmp);
    json!({
        "samples": sorted.len(),
        "p50_ms": percentile(&sorted, 50.0),
        "p95_ms": percentile(&sorted, 95.0),
        "p99_ms": percentile(&sorted, 99.0),
        "min_ms": sorted[0],
        "max_ms": sorted[sorted.len() - 1],
    })
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// Output of a metadata command, or `"unknown"`. Metadata is context only; it never affects timing.
fn capture(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn machine_info() -> Value {
    let exe = exe();
    // Tracked and untracked files that can change the measured binary or this benchmark; the
    // user's stray files (`.DS_Store`) are not part of the build.
    let dirty = capture(
        "git",
        &[
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--",
            "src",
            "tests",
            "scripts",
            "build.rs",
            "Cargo.toml",
            "Cargo.lock",
        ],
    );
    let signing = if cfg!(target_os = "macos") {
        // `codesign -dv` writes to stderr and fails on an unsigned binary.
        let out = Command::new("codesign").args(["-dv"]).arg(&exe).output();
        match out {
            Ok(o) if o.status.success() => "signed (ad-hoc or identity)",
            Ok(_) => "unsigned",
            Err(_) => "unknown",
        }
    } else {
        "n/a"
    };
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "os_version": capture("uname", &["-sr"]),
        "cpu": if cfg!(target_os = "macos") {
            capture("sysctl", &["-n", "machdep.cpu.brand_string"])
        } else {
            "see /proc/cpuinfo".to_string()
        },
        "rustc": capture("rustc", &["--version"]),
        "git_commit": capture("git", &["rev-parse", "HEAD"]),
        "git_dirty": dirty != "unknown" && !dirty.is_empty(),
        "binary": exe.display().to_string(),
        "binary_bytes": fs::metadata(&exe).map(|m| m.len()).unwrap_or(0),
        "binary_sha256": fs::read(&exe)
            .map(|b| agentdiet::fsutil::sha256_hex(&b))
            .unwrap_or_default(),
        "code_signing": signing,
    })
}

#[test]
#[ignore = "opt-in benchmark: cargo test --release --test perf -- --ignored --nocapture"]
fn shim_overhead() {
    if cfg!(debug_assertions) {
        panic!(
            "refusing to benchmark a debug build; run with `cargo test --release --test perf -- --ignored --nocapture`"
        );
    }
    let samples = env_usize("AGENTDIET_PERF_SAMPLES", DEFAULT_SAMPLES);
    let warmup = env_usize("AGENTDIET_PERF_WARMUP", DEFAULT_WARMUP);
    let fx = Fixture::new();

    for _ in 0..warmup {
        time(fx.e0(), "warmup E0");
        time(fx.e1(), "warmup E1");
        time(fx.e2(), "warmup E2");
        if let Some(cmd) = fx.ef() {
            time(cmd, "warmup EF");
        }
    }

    let (mut e0, mut e1, mut e2, mut ef) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for i in 0..samples {
        // Alternate the order so no arm always runs right after the same neighbour.
        if i % 2 == 0 {
            e0.push(ms(time(fx.e0(), "E0")));
            e1.push(ms(time(fx.e1(), "E1")));
            e2.push(ms(time(fx.e2(), "E2")));
            if let Some(cmd) = fx.ef() {
                ef.push(ms(time(cmd, "EF")));
            }
        } else {
            if let Some(cmd) = fx.ef() {
                ef.push(ms(time(cmd, "EF")));
            }
            e2.push(ms(time(fx.e2(), "E2")));
            e1.push(ms(time(fx.e1(), "E1")));
            e0.push(ms(time(fx.e0(), "E0")));
        }
    }

    // R1: routing only, in this process, with the same synthetic PATH and shims dir.
    let shim_argv0 = fx.shims.join("git");
    let mut r1 = Vec::with_capacity(R1_ITERATIONS);
    for _ in 0..R1_ITERATIONS {
        let start = Instant::now();
        let name = agentdiet::shim::shim_name(std::hint::black_box(shim_argv0.as_os_str()));
        let found = agentdiet::shim::resolve_real(
            name.expect("shims/git is a shim name"),
            std::hint::black_box(&fx.shim_path),
            &fx.shims,
        );
        let elapsed = start.elapsed();
        assert_eq!(
            found,
            Some(fx.real.join("git")),
            "routing resolved the wrong binary"
        );
        r1.push(ms(elapsed));
    }

    // Iteration-matched differences: E0 and E2 of the same iteration ran back to back.
    let b1: Vec<f64> = e2.iter().zip(&e0).map(|(a, b)| a - b).collect();
    let b2: Vec<f64> = e1.iter().zip(&e0).map(|(a, b)| a - b).collect();
    let b1_summary = summarize(&b1);
    let b1_median = b1_summary["p50_ms"].as_f64().unwrap_or(f64::NAN);
    // F, the exec floor, iteration-matched like B1.
    let floor: Vec<f64> = ef.iter().zip(&e0).map(|(a, b)| a - b).collect();
    let excess: Vec<f64> = e2.iter().zip(&ef).map(|(a, b)| a - b).collect();
    let floor_summary = (!floor.is_empty()).then(|| summarize(&floor));
    let floor_median = floor_summary.as_ref().and_then(|s| s["p50_ms"].as_f64());
    let b1_limit = floor_median.map(|f| f + B1_ALLOWANCE_OVER_FLOOR_MS);
    let verdict = match b1_limit {
        None => "UNMEASURED (no C compiler for the floor arm)",
        Some(limit) if b1_median <= limit => "MET",
        Some(_) => "NOT MET",
    };
    let r1_median = summarize(&r1)["p50_ms"].as_f64().unwrap_or(f64::NAN);
    let b3_verdict = if r1_median < B3_REQUIREMENT_MS {
        "MET"
    } else {
        "NOT MET"
    };

    let report = json!({
        "benchmark": "agentdiet shim overhead (SPEC §7)",
        "machine": machine_info(),
        "config": {
            "samples_per_arm": samples,
            "warmup_runs_per_arm": warmup,
            "r1_iterations": R1_ITERATIONS,
            "order": "E0,E1,E2 on even iterations; E2,E1,E0 on odd",
            "child_env": "env_clear + HOME=<TempDir>/home + synthetic PATH; stdio null",
            "baseline_program": "symlink to the system `true`",
        },
        "arms_ms": {
            "E0_direct_spawn_to_exit": summarize(&e0),
            "E1_hook_noop_spawn_to_exit": summarize(&e1),
            "E2_shim_spawn_to_exit": summarize(&e2),
            "R1_in_process_routing": summarize(&r1),
            "EF_exec_floor_spawn_to_exit": (!ef.is_empty()).then(|| summarize(&ef)),
        },
        "differences_ms_iteration_matched": {
            "B1_end_to_end_E2_minus_E0": b1_summary,
            "B2_launch_E1_minus_E0": summarize(&b2),
            "F_exec_floor_EF_minus_E0": floor_summary,
            "B1_above_floor_E2_minus_EF": (!excess.is_empty()).then(|| summarize(&excess)),
        },
        "boundaries": {
            "B1": "wall time from spawn to exit of the shimmed command minus the direct command; includes launching the agentdiet executable, routing and the second exec",
            "B2": "cost of launching agentdiet alone (hook noop exits immediately)",
            "B3": "R1; in-process routing up to, not including, exec",
        },
        "b1_requirement": format!("B1 <= F + {B1_ALLOWANCE_OVER_FLOOR_MS} ms (SPEC §7 B1 amendment)"),
        "b1_limit_ms_this_run": b1_limit,
        "b1_verdict_on_median": verdict,
        "b3_requirement_ms": B3_REQUIREMENT_MS,
        "b3_verdict_on_median": b3_verdict,
    });
    let text = serde_json::to_string_pretty(&report).expect("serialize report");
    println!("{text}");
    match (floor_median, b1_limit) {
        (Some(f), Some(limit)) => println!(
            "\nB1 (shim end-to-end overhead) median {b1_median:.3} ms; exec floor F {f:.3} ms; requirement <= F + {B1_ALLOWANCE_OVER_FLOOR_MS} = {limit:.3} ms: {verdict}"
        ),
        _ => println!("\nB1 (shim end-to-end overhead) median {b1_median:.3} ms: {verdict}"),
    }
    println!(
        "B3 (in-process routing) median {r1_median:.4} ms vs requirement < {B3_REQUIREMENT_MS} ms: {b3_verdict}"
    );
    if let Some(out) = std::env::var_os("AGENTDIET_PERF_OUT") {
        fs::write(&out, format!("{text}\n")).expect("write AGENTDIET_PERF_OUT");
    }
}

#[test]
fn fixture_routes_the_shim_to_the_real_binary() {
    let fx = Fixture::new();
    // Direct, hook and shim all exit 0 (the shim would exit 127 if it could not resolve `git`).
    time(fx.e0(), "E0");
    time(fx.e1(), "E1");
    time(fx.e2(), "E2");
    // The floor arm builds wherever a C compiler is (CI and every source build have one) and runs
    // the same `real/git`.
    if let Some(cmd) = fx.ef() {
        time(cmd, "EF");
    } else {
        eprintln!("no C compiler: the exec-floor arm is skipped");
    }

    // The shim really resolves to the fixture's own `real/git` and not something on the host PATH.
    let found = agentdiet::shim::resolve_real("git", &fx.shim_path, &fx.shims);
    assert_eq!(found, Some(fx.real.join("git")));
    // And with the real directory removed it fails (127), rather than finding a host `git`.
    fs::remove_file(fx.real.join("git")).unwrap();
    let status = fx.e2().status().unwrap();
    assert_eq!(status.code(), Some(127));
}

#[test]
fn percentile_is_nearest_rank() {
    let v: Vec<f64> = (1..=100).map(f64::from).collect();
    assert_eq!(percentile(&v, 50.0), 50.0);
    assert_eq!(percentile(&v, 95.0), 95.0);
    assert_eq!(percentile(&v, 99.0), 99.0);
    assert_eq!(percentile(&v, 100.0), 100.0);
    assert_eq!(percentile(&[7.0], 99.0), 7.0);
    assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 50.0), 2.0);
}
