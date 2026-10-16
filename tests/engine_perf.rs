//! Diet Engine timings (SPEC §7, boundaries B4 and B5). Opt-in: the measurements are `#[ignore]`d.
//!
//! ```sh
//! cargo test --release --test engine_perf -- --ignored --nocapture
//! AGENTDIET_PERF_ITERS=200 AGENTDIET_PERF_OUT=/tmp/engine.json \
//!     cargo test --release --test engine_perf -- --ignored --nocapture
//! ```
//!
//! What is measured, each warm unless it says "cold":
//!
//! * **B4 pure step** — `diet::transform` (classify, scrub, candidate, pre-check; no I/O) on inputs
//!   of 1 KiB to 256 KiB: realistic cargo / pytest / npm output, a JSON document, a Rust source
//!   file for folding, and adversarial inputs (one enormous line, secrets everywhere, ANSI and
//!   carriage-return floods, diagnostics on every line, multibyte and invalid UTF-8, hostile nesting).
//! * **Larger inputs** — 1, 4 and 8 MiB, and just over the 8 MiB limit, to show behaviour past the
//!   256 KiB budget boundary. The budget is stated for 256 KiB; nothing here claims it for more.
//! * **B5 persistence** — a durable Vault `put` and the whole persisting step (`Engine::finish`:
//!   Vault, `state.json`, `memory.db`) with 0, 100 and 1000 entries already in the Vault, plus the
//!   raw cost of the `fsync`s underneath.
//! * **Cold** — the first scrub, the first parse, and one complete `Engine::run` in a fresh
//!   process (regex compilation, SQLite open, fsyncs).
//!
//! Every measurement runs in a `TempDir` home. Debug builds are refused. A miss against a budget is
//! printed as a miss; this file never fails the run on one and never moves a budget.
#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime};

use agentdiet::Paths;
use agentdiet::cas::Cas;
use agentdiet::diet::{
    Engine, ExitStatus, Extent, Input, Plan, Settings, Source, estimate_tokens, transform,
};
use agentdiet::scrubber::scrub;
use agentdiet::state::Mode;
use serde_json::{Value, json};
use tempfile::TempDir;

/// SPEC §7, B4: the pure transform of up to 256 KiB.
const B4_BUDGET_MS: f64 = 10.0;
const KIB: usize = 1024;

fn require_release() {
    if cfg!(debug_assertions) {
        panic!("these timings are meaningless in a debug build: use `cargo test --release`");
    }
}

fn iters(default: usize) -> usize {
    std::env::var("AGENTDIET_PERF_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

#[derive(Clone, Copy)]
struct Stats {
    p50: f64,
    p95: f64,
    p99: f64,
    max: f64,
    n: usize,
}

fn stats(mut ms: Vec<f64>) -> Stats {
    ms.sort_by(f64::total_cmp);
    Stats {
        p50: percentile(&ms, 50.0),
        p95: percentile(&ms, 95.0),
        p99: percentile(&ms, 99.0),
        max: ms[ms.len() - 1],
        n: ms.len(),
    }
}

impl Stats {
    fn json(self) -> Value {
        json!({"samples": self.n, "p50_ms": self.p50, "p95_ms": self.p95, "p99_ms": self.p99, "max_ms": self.max})
    }
}

fn measure(warmup: usize, n: usize, mut f: impl FnMut()) -> Stats {
    for _ in 0..warmup {
        f();
    }
    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        let start = Instant::now();
        f();
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    stats(samples)
}

fn verdict(s: Stats) -> &'static str {
    if s.p99 < B4_BUDGET_MS {
        "within 10 ms"
    } else if s.p50 < B4_BUDGET_MS {
        "p50 within, p99 MISSES 10 ms"
    } else {
        "MISSES 10 ms"
    }
}

// ---- inputs -----------------------------------------------------------------------------------

/// Grow `unit(i)` lines until `target` bytes.
fn grow(target: usize, head: &str, mut unit: impl FnMut(usize) -> String, tail: &str) -> Vec<u8> {
    let mut out = String::from(head);
    let mut i = 0;
    while out.len() + tail.len() < target {
        out.push_str(&unit(i));
        i += 1;
    }
    out.push_str(tail);
    out.into_bytes()
}

fn cargo_test(target: usize) -> Vec<u8> {
    grow(
        target,
        "   Compiling demo v0.3.1 (/home/dev/demo)\n    Finished `test` profile in 4.10s\nrunning 5000 tests\n",
        |i| {
            if i % 997 == 500 {
                format!("test parser::tests::case_{i:05} ... FAILED\n")
            } else {
                format!("test parser::tests::case_{i:05} ... ok\n")
            }
        },
        "\nfailures:\n\n---- parser::tests::case_00500 stdout ----\nthread 'parser::tests::case_00500' panicked at src/parser.rs:88:9:\nassertion `left == right` failed\n\ntest result: FAILED. 4999 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n",
    )
}

fn pytest_verbose(target: usize) -> Vec<u8> {
    grow(
        target,
        "============================= test session starts ==============================\ncollected 5000 items\n\n",
        |i| {
            format!(
                "tests/test_mod_{}.py::test_case_{i:05} PASSED{:>30}\n",
                i % 40,
                format!("[{:3}%]", i % 100)
            )
        },
        "\n=================================== FAILURES ===================================\nE   assert 1 == 2\n============= 1 failed, 4999 passed in 4.01s ==============\n",
    )
}

fn npm_install(target: usize) -> Vec<u8> {
    grow(
        target,
        "",
        |i| {
            if i % 3 == 0 {
                "npm warn deprecated inflight@1.0.6: This module is not supported, and leaks memory.\n".to_string()
            } else {
                format!(
                    "npm http fetch GET 200 https://registry.npmjs.org/pkg-{i} {}ms\n",
                    20 + i % 90
                )
            }
        },
        "added 312 packages, and audited 313 packages in 14s\n",
    )
}

fn json_document(target: usize) -> Vec<u8> {
    let row = |i: usize| json!({"number": 1000 + i, "title": format!("Fix flaky test number {i}"), "state": if i.is_multiple_of(3) {"open"} else {"closed"}, "author": "octocat", "comments": i % 7});
    let render = |n: usize| {
        let rows: Vec<Value> = (0..n).map(row).collect();
        serde_json::to_string_pretty(&json!({"total": n, "items": rows})).unwrap()
    };
    // Pretty printing adds a third: size the document by what it really renders to.
    let per_row = render(200).len() / 200;
    let mut n = (target / per_row).max(1);
    while render(n).len() > target && n > 1 {
        n -= 1;
    }
    render(n).into_bytes()
}

fn rust_source(target: usize) -> Vec<u8> {
    let mut src = String::from("//! Generated module.\n\nuse std::collections::BTreeMap;\n\n");
    let mut i = 0;
    while src.len() < target {
        src.push_str(&format!(
            "/// Stage {i}.\npub fn stage_{i}(input: &BTreeMap<String, u64>, factor: u64) -> Result<u64, String> {{\n    let mut checked = 0u64;\n    let mut skipped = 0u64;\n    for (key, value) in input.iter() {{\n        if key.is_empty() {{\n            skipped += 1;\n            continue;\n        }}\n        let adjusted = value.checked_mul(factor + {i}).ok_or_else(|| format!(\"overflow at {{key}}\"))?;\n        if adjusted == 0 {{\n            skipped += 1;\n        }} else {{\n            checked += adjusted % 97;\n        }}\n    }}\n    Ok(checked + skipped)\n}}\n\n"
        ));
        i += 1;
    }
    src.into_bytes()
}

fn adversarial(kind: &str, target: usize) -> Vec<u8> {
    let key = ["AK", "IA"].concat() + &"Q7".repeat(8);
    match kind {
        "one huge line" => vec![b'x'; target],
        "all spaces" => vec![b' '; target],
        "secrets everywhere" => grow(
            target,
            "",
            |i| format!("API_KEY_{i}=hunter{i}hunter{i}\nid={key} Bearer abcdefghijklmnop{i}xyz\n"),
            "",
        ),
        "diagnostic on every line" => grow(
            target,
            "",
            |i| format!("error: something failed at step {i}: warning, fatal\n"),
            "",
        ),
        "ansi flood" => grow(
            target,
            "",
            |i| format!("   \x1b[0m\x1b[1m\x1b[32mCompiling\x1b[0m pkg{i} v0.1.{i}\n"),
            "",
        ),
        "carriage-return flood" => grow(
            target,
            "",
            |i| format!("Receiving objects: {:3}% ({i}/9999)\r", i % 100),
            "",
        ),
        "multibyte text" => grow(
            target,
            "",
            |i| format!("日本語のテスト {i} — héllo wörld 😀 ok\n"),
            "",
        ),
        "invalid UTF-8" => {
            let mut v = grow(
                target,
                "",
                |i| format!("line {i} with bytes \u{fffd}\n"),
                "",
            );
            for pos in (0..v.len()).step_by(97) {
                v[pos] = 0xff;
            }
            v
        }
        "numbers and punctuation" => grow(
            target,
            "",
            |i| format!("{i}: [{}] {{{}}} ({})\n", i * 7, i * 13, i % 5),
            "",
        ),
        "deep (60) JSON, large leaf" => {
            let leaf = format!("[{}]", vec!["1"; target / 2 - 200].join(","));
            format!("{}{leaf}{}", "{\"a\":".repeat(60), "}".repeat(60)).into_bytes()
        }
        "hostile JSON nesting" => {
            let n = target / 8;
            format!("{}1{}", "{\"a\":".repeat(n), "}".repeat(n)).into_bytes()
        }
        other => panic!("unknown kind {other}"),
    }
}

fn shell(argv: &[&str]) -> Source {
    Source::Shell {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        exit: ExitStatus::Code(1),
    }
}

// ---- reporting --------------------------------------------------------------------------------

struct Report {
    rows: Vec<Value>,
}

impl Report {
    fn row(&mut self, group: &str, label: &str, bytes: usize, s: Stats, note: &str) {
        println!(
            "{group:<12} {label:<34} {:>9} B  p50 {:>8.3}  p95 {:>8.3}  p99 {:>8.3}  max {:>8.3} ms  n={:<4} {note}",
            bytes, s.p50, s.p95, s.p99, s.max, s.n
        );
        let mut v = s.json();
        v["group"] = group.into();
        v["case"] = label.into();
        v["input_bytes"] = bytes.into();
        v["note"] = note.into();
        self.rows.push(v);
    }
}

fn paths_in(dir: &TempDir) -> Paths {
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    Paths::new(
        home,
        dir.path().join("config"),
        dir.path().join("project"),
        "/x/agentdiet".into(),
    )
}

fn commit() -> String {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn uname() -> String {
    Command::new("uname")
        .arg("-a")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

// ---- the measurements -------------------------------------------------------------------------

#[test]
#[ignore = "opt-in timing run; see the file header"]
fn engine_timings() {
    require_release();
    let n = iters(60);
    let mut report = Report { rows: Vec::new() };
    let balanced = Settings::new(Mode::Balanced);
    let ultra = Settings::new(Mode::UltraLean);
    println!(
        "\n== B4: pure step (transform), warm, no I/O. Budget: < {B4_BUDGET_MS} ms at <= 256 KiB =="
    );

    let mut worst_256 = ("", 0.0f64);
    let mut worst_p99 = ("", 0.0f64);
    let bench = |group: &str,
                 label: &str,
                 bytes: Vec<u8>,
                 source: Source,
                 settings: Settings,
                 rep: &mut Report| {
        let input = Input {
            bytes: &bytes,
            source: &source,
        };
        let plan_kind = match transform(&input, &settings) {
            Plan::Done(o) => format!("{:?}", o.disposition),
            Plan::Replace(p) => format!("{:?}", p.candidate().kind),
        };
        let s = measure(
            5,
            if bytes.len() > 1024 * 1024 {
                5.max(n / 10)
            } else {
                n
            },
            || {
                std::hint::black_box(transform(&input, &settings));
            },
        );
        let tokens = estimate_tokens(&bytes);
        let note = format!(
            "-> {plan_kind}, {tokens} est. tokens{}",
            if bytes.len() <= 256 * KIB {
                format!(", {}", verdict(s))
            } else {
                String::new()
            }
        );
        rep.row(group, label, bytes.len(), s, &note);
        s
    };

    for size in [KIB, 16 * KIB, 64 * KIB, 256 * KIB] {
        for (label, bytes, source) in [
            ("cargo test", cargo_test(size), shell(&["cargo", "test"])),
            ("pytest -v", pytest_verbose(size), shell(&["pytest", "-v"])),
            ("npm install", npm_install(size), shell(&["npm", "install"])),
            (
                "JSON (ModelJson -> TOON)",
                json_document(size),
                Source::ModelJson,
            ),
            (
                "Rust source (fold)",
                rust_source(size),
                Source::Read {
                    path: "src/lib.rs".into(),
                    extent: Extent::Complete,
                },
            ),
        ] {
            let s = bench("realistic", label, bytes, source, balanced, &mut report);
            if size == 256 * KIB && s.p50 > worst_256.1 {
                worst_256 = (label, s.p50);
            }
            if size == 256 * KIB && s.p99 > worst_p99.1 {
                worst_p99 = (label, s.p99);
            }
        }
    }
    println!("\n-- adversarial, 256 KiB, Ultra-Lean --");
    for kind in [
        "one huge line",
        "all spaces",
        "secrets everywhere",
        "diagnostic on every line",
        "ansi flood",
        "carriage-return flood",
        "multibyte text",
        "invalid UTF-8",
        "numbers and punctuation",
        "hostile JSON nesting",
        "deep (60) JSON, large leaf",
    ] {
        let bytes = adversarial(kind, 256 * KIB);
        // Each of these is run as every source that could look at it.
        for (tag, source) in [
            ("shell", shell(&["cargo", "build"])),
            ("json", Source::ModelJson),
            (
                "read",
                Source::Read {
                    path: "x.rs".into(),
                    extent: Extent::Complete,
                },
            ),
        ] {
            let s = bench(
                "adversarial",
                &format!("{kind} [{tag}]"),
                bytes.clone(),
                source,
                ultra,
                &mut report,
            );
            if s.p50 > worst_256.1 {
                worst_256 = (Box::leak(format!("{kind} [{tag}]").into_boxed_str()), s.p50);
            }
            if s.p99 > worst_p99.1 {
                worst_p99 = (Box::leak(format!("{kind} [{tag}]").into_boxed_str()), s.p99);
            }
        }
    }
    println!(
        "\nB4 summary at 256 KiB: slowest p50 {:.3} ms ({}), slowest p99 {:.3} ms ({}) against {B4_BUDGET_MS} ms",
        worst_256.1, worst_256.0, worst_p99.1, worst_p99.0
    );

    println!("\n== larger inputs (the 256 KiB budget does not apply; reported, not judged) ==");
    for (label, size) in [
        ("1 MiB", KIB * KIB),
        ("4 MiB", 4 * KIB * KIB),
        ("8 MiB", 8 * KIB * KIB),
    ] {
        bench(
            "larger",
            &format!("cargo test {label}"),
            cargo_test(size),
            shell(&["cargo", "test"]),
            balanced,
            &mut report,
        );
        bench(
            "larger",
            &format!("JSON {label}"),
            json_document(size),
            Source::ModelJson,
            balanced,
            &mut report,
        );
        bench(
            "larger",
            &format!("Rust source {label}"),
            rust_source(size),
            Source::Read {
                path: "x.rs".into(),
                extent: Extent::Complete,
            },
            balanced,
            &mut report,
        );
    }
    bench(
        "larger",
        "oversize (8 MiB + 1, scrub only)",
        adversarial("one huge line", 8 * KIB * KIB + 1),
        shell(&["cargo", "test"]),
        balanced,
        &mut report,
    );

    println!("\n== tree-sitter parse alone, warm (the cost inside the fold rows) ==");
    for (label, language, make) in [(
        "Rust",
        agentdiet::folding::Language::Rust,
        rust_source as fn(usize) -> Vec<u8>,
    )] {
        for size in [64 * KIB, 128 * KIB, 256 * KIB] {
            let bytes = make(size);
            let text = std::str::from_utf8(&bytes).unwrap();
            let s = measure(3, n.min(40), || {
                std::hint::black_box(agentdiet::folding::parse(language, text).is_some());
            });
            report.row("parse", &format!("{label} source"), bytes.len(), s, "");
        }
    }

    println!("\n== scrubber alone, warm ==");
    for size in [16 * KIB, 256 * KIB] {
        let bytes = cargo_test(size);
        let s = measure(5, n, || {
            std::hint::black_box(scrub(&bytes));
        });
        report.row("scrub", "cargo test", bytes.len(), s, "");
    }

    persistence(&mut report, n);

    if let Some(out) = std::env::var_os("AGENTDIET_PERF_OUT") {
        let doc = json!({
            "commit": commit(),
            "uname": uname(),
            "rustc": Command::new("rustc").arg("--version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()),
            "b4_budget_ms": B4_BUDGET_MS,
            "rows": report.rows,
        });
        fs::write(&out, serde_json::to_string_pretty(&doc).unwrap() + "\n").unwrap();
        println!("\nwrote {}", PathBuf::from(out).display());
    }
}

fn persistence(report: &mut Report, n: usize) {
    println!("\n== B5: persistence (durable Vault write, GC, ID selection, state + index) ==");
    let n = n.min(60);

    // The floor: what the filesystem charges for making 32 KiB durable.
    let dir = TempDir::new().unwrap();
    let payload = vec![b'z'; 32 * KIB];
    let mut counter = 0u64;
    let s = measure(3, n, || {
        counter += 1;
        agentdiet::fsutil::atomic_write_private(&dir.path().join(format!("f{counter}")), &payload)
            .unwrap();
    });
    report.row(
        "fsync",
        "atomic_write_private 32 KiB (file + dir)",
        payload.len(),
        s,
        "floor for any durable write",
    );
    let s = measure(3, n, || {
        let f = fs::File::open(dir.path().join("f1")).unwrap();
        f.sync_all().unwrap();
    });
    report.row(
        "fsync",
        "File::sync_all on an existing file",
        0,
        s,
        "F_FULLFSYNC on macOS",
    );
    let s = measure(3, n, || agentdiet::fsutil::sync_dir(dir.path()).unwrap());
    report.row("fsync", "sync_dir", 0, s, "");

    for existing in [0usize, 100, 1000] {
        let tmp = TempDir::new().unwrap();
        let paths = paths_in(&tmp);
        let cas = Cas::new(paths.cas_dir());
        let now = SystemTime::now();
        // Seed without paying for 1000 fsyncs: the files are the same plain files `put` would make.
        if existing > 0 {
            cas.put(&scrub(b"seed\n"), now).unwrap();
            for i in 0..existing {
                let body = format!("seeded entry {i}\n");
                let sha = agentdiet::fsutil::sha256_hex(body.as_bytes());
                fs::write(paths.cas_dir().join(format!("{sha}.log")), body).unwrap();
            }
        }
        let mut k = 0u64;
        let payload = cargo_test(32 * KIB);
        let s = measure(3, n, || {
            k += 1;
            let mut data = payload.clone();
            data.extend_from_slice(format!("unique {k}\n").as_bytes());
            std::hint::black_box(cas.put(&scrub(&data), now).unwrap());
        });
        report.row(
            "persist",
            &format!("Cas::put, {existing} entries"),
            payload.len(),
            s,
            "durable write + GC scan + ID selection",
        );

        let src = shell(&["cargo", "test"]);
        let engine = Engine::new(&paths);
        let mut j = 0u64;
        let big = cargo_test(64 * KIB);
        let s = measure(3, n, || {
            j += 1;
            let mut data = big.clone();
            data.extend_from_slice(format!("unique run {j}\n").as_bytes());
            let out = engine.run(
                &Input {
                    bytes: &data,
                    source: &src,
                },
                &Settings::new(Mode::Balanced),
            );
            assert!(out.id.is_some(), "{:?}", out.disposition);
            assert!(out.note.is_none(), "{:?}", out.note);
        });
        report.row(
            "persist",
            &format!("Engine::run 64 KiB, {existing} entries"),
            big.len(),
            s,
            "pure step + put + state + memory.db open/index",
        );

        // The parts of the persisting step, each alone.
        let mut t = 0u64;
        let s = measure(3, n, || {
            t += 1;
            agentdiet::state::record_telemetry(&paths, Some("4f9a1b2c"), t).unwrap();
        });
        report.row(
            "persist",
            &format!("state::record_telemetry alone, {existing} entries"),
            0,
            s,
            "file fsync + rename + dir fsync",
        );
        let mut m = 0u64;
        let s = measure(3, n, || {
            m += 1;
            let db = agentdiet::memory::MemoryDb::open(&paths.memory_db()).unwrap();
            db.index(
                agentdiet::memory::Kind::Command,
                &scrub(format!("cargo test {m}").as_bytes()),
                &scrub(b"kept 20/200 lines; exit 101"),
                Some(&agentdiet::fsutil::sha256_hex(b"x")),
                None,
            )
            .unwrap();
        });
        report.row(
            "persist",
            &format!("MemoryDb::open + index alone, {existing} entries"),
            0,
            s,
            "SQLite WAL, default synchronous",
        );

        let cas_read = Cas::new(paths.cas_dir());
        let id = cas_read.put(&scrub(&big), now).unwrap().short_id;
        let s = measure(3, n, || {
            std::hint::black_box(cas_read.read(&id, None, now).unwrap());
        });
        report.row(
            "persist",
            &format!("Cas::read (verify), {existing} entries"),
            big.len(),
            s,
            "resolve + GC scan + sha256 verify",
        );
    }
}

// ---- cold measurements in a fresh process -----------------------------------------------------

/// Child: time the first calls of a fresh process and print them. Run by `engine_cold_start`.
#[test]
#[ignore = "child process helper for engine_cold_start"]
fn engine_cold_child() {
    let Some(dir) = std::env::var_os("AD_COLD_HOME") else {
        return;
    };
    let tmp = PathBuf::from(dir);
    let paths = Paths::new(
        tmp.join("home"),
        tmp.join("config"),
        tmp.join("project"),
        "/x/agentdiet".into(),
    );
    let data = cargo_test(64 * KIB);
    let src = shell(&["cargo", "test"]);

    let t = Instant::now();
    let first_scrub = scrub(b"AKIAQ7Q7Q7Q7Q7Q7Q7Q7 hello\n");
    let scrub_ms = t.elapsed().as_secs_f64() * 1000.0;
    std::hint::black_box(first_scrub);

    let rust = rust_source(64 * KIB);
    let t = Instant::now();
    let parsed = agentdiet::folding::parse(
        agentdiet::folding::Language::Rust,
        std::str::from_utf8(&rust).unwrap(),
    );
    let parse_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert!(parsed.is_some());

    let t = Instant::now();
    let out = Engine::new(&paths).run(
        &Input {
            bytes: &data,
            source: &src,
        },
        &Settings::new(Mode::Balanced),
    );
    let run_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert!(out.id.is_some());
    println!("COLD scrub_ms={scrub_ms:.3} parse_ms={parse_ms:.3} run_ms={run_ms:.3}");
}

#[test]
#[ignore = "opt-in timing run; see the file header"]
fn engine_cold_start() {
    require_release();
    let exe = std::env::current_exe().unwrap();
    let rounds = iters(30).min(60);
    let (mut scrub_ms, mut parse_ms, mut run_ms) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..rounds {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("home")).unwrap();
        let out = Command::new(&exe)
            .args(["--ignored", "--exact", "engine_cold_child", "--nocapture"])
            .env("AD_COLD_HOME", tmp.path())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let line = text
            .lines()
            .find(|l| l.starts_with("COLD "))
            .expect("child output");
        let field = |name: &str| -> f64 {
            line.split_whitespace()
                .find_map(|p| p.strip_prefix(&format!("{name}=")))
                .unwrap()
                .parse()
                .unwrap()
        };
        scrub_ms.push(field("scrub_ms"));
        parse_ms.push(field("parse_ms"));
        run_ms.push(field("run_ms"));
    }
    println!("\n== cold: first call in a fresh process ({rounds} processes) ==");
    let mut report = Report { rows: Vec::new() };
    report.row(
        "cold",
        "first scrub() (regex compilation)",
        27,
        stats(scrub_ms),
        "",
    );
    report.row(
        "cold",
        "first tree-sitter parse, 64 KiB Rust",
        64 * KIB,
        stats(parse_ms),
        "",
    );
    report.row("cold", "first Engine::run, 64 KiB cargo test", 64 * KIB, stats(run_ms), "includes the scrubber's cold start, fsyncs, SQLite open; excludes process launch (see perf.rs B2)");
}
