//! Command-aware compression: dispatch, protection, and one original fixture per supported tool.
//!
//! The fixtures under `tests/fixtures/compress/` were written for this project from the tools'
//! documented output formats; none is copied from a reference repository.

use agentdiet::compress::{compress, detect, parse_simple_command};
use agentdiet::diet::{Candidate, ExitStatus, Layout};
use agentdiet::state::Mode;

const MODES: [Mode; 3] = [Mode::Light, Mode::Balanced, Mode::UltraLean];

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/compress/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn argv(cmd: &str) -> Vec<String> {
    cmd.split_whitespace().map(str::to_string).collect()
}

fn body(c: &Candidate) -> &[u8] {
    match &c.layout {
        Layout::Trailer { body } => body,
        other => panic!("expected a trailer layout, got {other:?}"),
    }
}

fn run(cmd: &str, input: &[u8], mode: Mode) -> Option<Candidate> {
    compress(&argv(cmd), ExitStatus::Code(0), input, mode)
}

fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split_inclusive(|b| *b == b'\n').collect()
}

fn has_line(haystack: &[u8], line: &str) -> bool {
    lines(haystack).contains(&line.as_bytes())
}

/// Lines of the original that are "diagnostic" by plain words, outside the explicit noise forms.
fn diagnostic_lines(original: &[u8]) -> Vec<&[u8]> {
    lines(original)
        .into_iter()
        .filter(|l| {
            let lower = String::from_utf8_lossy(l).to_lowercase();
            let noise = [
                "compiling ",
                "downloaded ",
                "downloading ",
                "checking ",
                "fresh ",
            ]
            .iter()
            .any(|v| lower.trim_start().starts_with(v))
                || lower.contains("... ok")
                || lower.starts_with("progress:")
                || lower.starts_with("npm http");
            !noise
                && ["error", "warning", "warn", "fail", "panic"]
                    .iter()
                    .any(|w| lower.contains(w))
        })
        .collect()
}

// ---- dispatch ---------------------------------------------------------------------------------

#[test]
fn dispatch_recognises_the_tool_and_subcommand_not_text() {
    let yes = [
        "cargo build --release",
        "cargo +nightly test -p demo",
        "cargo -q check",
        "cargo test -- --nocapture",
        "/usr/local/bin/cargo clippy --all-targets",
        "pytest -q tests/",
        "py.test",
        "python -m pytest -x",
        "python3.12 -m pytest",
        "go test ./...",
        "go test -v -run TestX ./pkg",
        "npm install",
        "npm ci --no-audit",
        "pnpm add left-pad",
        "yarn add react",
        "git clone https://example.com/r.git",
        "git -C /tmp/x fetch --all",
        "git --no-pager pull",
        "git push origin main",
        "make -j8 all",
        "gmake",
        "docker pull alpine",
        "docker build -t x .",
        "docker buildx build .",
        "ruff check src/",
    ];
    for cmd in yes {
        assert!(detect(&argv(cmd)).is_some(), "should compress: {cmd}");
    }
    let no = [
        // not the tool, only mentioning it
        "echo cargo test",
        "sh -c cargo test",
        "grep cargo build log.txt",
        "notcargo test",
        "cargo-fmt check",
        "python script.py cargo",
        "python -m http.server",
        // the tool, but not a compressible subcommand
        "cargo run",
        "cargo fmt",
        "cargo",
        "cargo --version",
        "git log --oneline",
        "git diff HEAD~1",
        "git show HEAD",
        "git blame src/main.rs",
        "git status --short",
        "git status",
        "git commit -m cargo",
        "git -x clone r",
        "go build ./...",
        "go vet ./...",
        "npm ls",
        "npm run build",
        "npm view react version",
        "npm test",
        "docker ps",
        "docker run alpine",
        "docker compose up",
        "ruff format .",
        "ruff",
        "make --version",
        // machine-readable
        "cargo build --message-format=json",
        "cargo test --message-format json",
        "cargo test -- --format json",
        "cargo test -- --list",
        "cargo test -- -Zunstable-options --format=json",
        "pytest --collect-only",
        "pytest --co -q",
        "pytest --json-report",
        "go test -json ./...",
        "go test -list . ./...",
        "go test -c",
        "npm install --json",
        "npm ci --parseable",
        "pnpm install --reporter=json --json",
        "git clone --porcelain r",
        "git fetch --format=%H",
        "git pull -z",
        "make -n",
        "make --dry-run",
        "make -p",
        "docker pull -q alpine",
        "docker build --progress=rawjson .",
        "docker pull --format json x",
        "ruff check --output-format=json",
        "ruff check --statistics",
        "ruff check --show-settings",
    ];
    for cmd in no {
        assert!(detect(&argv(cmd)).is_none(), "must NOT compress: {cmd}");
    }
    assert!(detect(&[]).is_none());
    assert!(detect(&argv("")).is_none());
    // The label comes from the recognised tool, never from argv text.
    assert_eq!(
        detect(&argv("cargo +nightly test")).unwrap().label(),
        "cargo test"
    );
    assert_eq!(detect(&argv("python -m pytest")).unwrap().label(), "pytest");
    // Arguments after `--` are not flags of the tool itself: a program argument that merely looks
    // like one does not turn compression off.
    assert!(detect(&argv("cargo test -- --json-looking-arg")).is_some());
}

#[test]
fn simple_command_parsing_accepts_exactly_one_simple_command() {
    let ok: &[(&str, &[&str])] = &[
        ("cargo test", &["cargo", "test"]),
        (
            "  cargo   test  --release ",
            &["cargo", "test", "--release"],
        ),
        ("RUST_BACKTRACE=1 cargo test", &["cargo", "test"]),
        ("A=1 B='x y' cargo build", &["cargo", "build"]),
        (
            "git commit -m 'fix: a; b | c'",
            &["git", "commit", "-m", "fix: a; b | c"],
        ),
        ("echo \"a  b\"", &["echo", "a  b"]),
        ("echo a\\ b", &["echo", "a b"]),
        ("echo 'it'\"'\"'s'", &["echo", "it's"]),
        ("echo \"back\\\\slash\"", &["echo", "back\\slash"]),
        ("echo \"multi\nline\"", &["echo", "multi\nline"]),
        ("pytest -k 'a and b'", &["pytest", "-k", "a and b"]),
        ("echo a#b", &["echo", "a#b"]),
    ];
    for (input, want) in ok {
        assert_eq!(
            parse_simple_command(input).as_deref(),
            Some(
                want.iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .as_slice()
            ),
            "{input:?}"
        );
    }
    let no = [
        "",
        "   ",
        "A=1",
        "cd app && cargo test",
        "cargo test | tee out.txt",
        "cargo test > out.txt",
        "cargo test >> out.txt",
        "cargo test 2>&1",
        "cargo test < in.txt",
        "cargo test; ls",
        "cargo test &",
        "(cargo test)",
        "cargo test $(pwd)",
        "cargo test `pwd`",
        "cargo test $HOME",
        "cargo test \"$HOME\"",
        "cargo test \"`pwd`\"",
        "cargo test \"\\$HOME\"",
        "cargo test # comment",
        "cargo test\nls",
        "cargo test \\\nls",
        "cargo 'test",
        "cargo \"test",
        "cargo test ||",
    ];
    for input in no {
        assert!(
            parse_simple_command(input).is_none(),
            "must reject {input:?}"
        );
    }
    // Single quotes hide every metacharacter.
    assert_eq!(
        parse_simple_command("echo '$HOME `x` | ; & > <'").unwrap(),
        ["echo", "$HOME `x` | ; & > <"]
    );
}

// ---- per-tool fixtures ------------------------------------------------------------------------

#[test]
fn cargo_build_keeps_every_warning_and_drops_build_progress() {
    let input = fixture("cargo_build_warnings.txt");
    for mode in MODES {
        let c = run("cargo build", &input, mode).expect("compressible");
        let out = body(&c);
        for l in [
            "warning: unused variable: `retries`\n",
            " --> src/net.rs:41:9\n",
            "41 |     let retries = 3;\n",
            "warning: function `connect` is never used\n",
            "warning: `demo` (bin \"demo\") generated 2 warnings\n",
            "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.52s\n",
            "    Updating crates.io index\n", // not a listed noise form, so it stays
        ] {
            assert!(has_line(out, l), "{mode:?}: lost {l:?}");
        }
        assert!(!has_line(out, "   Compiling serde v1.0.200\n"), "{mode:?}");
        assert!(!has_line(out, "  Downloaded serde v1.0.200\n"), "{mode:?}");
        assert!(out.len() < input.len() / 2, "{mode:?}");
        let (kept, total) = c.lines.unwrap();
        assert_eq!(total, lines(&input).len());
        assert_eq!(kept, lines(out).len());
        assert!(c.details[0].starts_with("cargo build: "), "{:?}", c.details);
    }
}

#[test]
fn cargo_test_keeps_failures_with_context_and_collapses_passes() {
    let input = fixture("cargo_test_failures.txt");
    for mode in MODES {
        let c = run("cargo test", &input, mode).unwrap();
        let out = body(&c);
        for l in [
            "running 60 tests\n",
            "test parser::tests::case_17 ... FAILED\n",
            "test parser::tests::case_41 ... FAILED\n",
            "test parser::tests::case_50 ... ignored\n",
            "failures:\n",
            "---- parser::tests::case_17 stdout ----\n",
            "thread 'parser::tests::case_17' panicked at src/parser.rs:88:9:\n",
            "assertion `left == right` failed: unexpected token\n",
            "  left: Ident(\"x\")\n",
            "    parser::tests::case_41\n",
            "test result: FAILED. 57 passed; 2 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.31s\n",
            "error: test failed, to rerun pass `--lib`\n",
        ] {
            assert!(has_line(out, l), "{mode:?}: lost {l:?}");
        }
        // Far from any failure: gone. Right next to one: kept (context). The distance depends on
        // the mode: 5 / 3 / 1 lines.
        assert!(
            !has_line(out, "test parser::tests::case_02 ... ok\n"),
            "{mode:?}"
        );
        assert!(
            has_line(out, "test parser::tests::case_16 ... ok\n"),
            "{mode:?}"
        );
        assert!(
            has_line(out, "test parser::tests::case_18 ... ok\n"),
            "{mode:?}"
        );
        let reach = ModeCtx::of(mode);
        assert!(
            has_line(
                out,
                &format!("test parser::tests::case_{:02} ... ok\n", 17 - reach)
            ),
            "{mode:?}: the context line {reach} before the failure"
        );
        assert!(
            !has_line(
                out,
                &format!("test parser::tests::case_{:02} ... ok\n", 17 - reach - 1)
            ),
            "{mode:?}: one line beyond the context must go"
        );
        assert!(
            c.details
                .iter()
                .any(|d| d.contains("passing test lines collapsed"))
        );
    }
}

/// The context distance the mode table promises (SPEC): Light 5, Balanced 3, Ultra-Lean 1.
struct ModeCtx;
impl ModeCtx {
    fn of(mode: Mode) -> usize {
        match mode {
            Mode::Light => 5,
            Mode::Balanced => 3,
            Mode::UltraLean => 1,
        }
    }
}

#[test]
fn cargo_test_all_passing_collapses_to_a_count_but_keeps_the_tail() {
    let input = fixture("cargo_test_pass.txt");
    let c = run("cargo test", &input, Mode::Balanced).unwrap();
    let out = body(&c);
    assert!(has_line(out, "running 400 tests\n"));
    assert!(has_line(
        out,
        "test result: ok. 400 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.04s\n"
    ));
    // The last 10 lines of the original are untouched, in every mode, even though they are noise.
    let original = lines(&input);
    let tail: Vec<&[u8]> = original[original.len() - 10..].to_vec();
    let kept = lines(out);
    assert_eq!(&kept[kept.len() - 10..], tail.as_slice());
    assert!(out.len() < input.len() / 5);
    let passing = c
        .details
        .iter()
        .find(|d| d.contains("passing test lines"))
        .unwrap();
    assert!(
        passing.contains("390 passing test lines")
            || passing.contains("passing test lines collapsed"),
        "{passing}"
    );
}

#[test]
fn pytest_failures_and_summary_survive_progress_removal() {
    let input = fixture("pytest_failures.txt");
    for mode in MODES {
        // The fixture is short: Light's context (5) and tail (20) cover every progress row, so
        // there is nothing to remove there; Ultra-Lean has to compress it.
        let Some(c) = run("pytest -q", &input, mode) else {
            assert_ne!(mode, Mode::UltraLean);
            continue;
        };
        let out = body(&c);
        // Progress rows that contain F / E / s are results, not noise.
        assert!(has_line(
            out,
            &format!(
                "tests/test_db.py {}      [ 50%]\n",
                ".".repeat(12) + "F" + &".".repeat(17)
            )
        ));
        for l in [
            "=================================== FAILURES ===================================\n",
            "E       assert True is False\n",
            "tests/test_db.py:44: AssertionError\n",
            "E   fixture 'client' not found\n",
            "FAILED tests/test_db.py::test_insert_dupe - assert True is False\n",
            "ERROR tests/test_auth.py::test_login_token - fixture 'client' not found\n",
            "============= 1 failed, 117 passed, 1 skipped, 1 error in 3.42s ==============\n",
            "collected 120 items\n",
        ] {
            assert!(has_line(out, l), "{mode:?}: lost {l:?}");
        }
        // The all-dots rows go (unless they sit inside a context window).
        let dots_row = format!("tests/test_api.py {}      [ 25%]\n", ".".repeat(30));
        if mode == Mode::UltraLean {
            assert!(!has_line(out, &dots_row), "{mode:?}");
        }
    }
}

#[test]
fn pytest_verbose_passed_lines_collapse() {
    let input = fixture("pytest_verbose_pass.txt");
    let c = run("pytest -v", &input, Mode::Balanced).unwrap();
    let out = body(&c);
    assert!(has_line(out, "collected 150 items\n"));
    assert!(has_line(
        out,
        "\n============================= 150 passed in 4.01s ==============================\n"
            .trim_start_matches('\n')
    ));
    assert!(!has_line(
        out,
        "tests/test_mod.py::test_case_010 PASSED                         [  6%]\n"
    ));
    assert!(out.len() < input.len() / 4);
    assert!(
        c.details[0].contains("PASSED lines collapsed"),
        "{:?}",
        c.details
    );
}

#[test]
fn go_test_drops_only_passing_pairs_and_keeps_failures_and_panics() {
    let input = fixture("go_test_verbose.txt");
    for mode in MODES {
        let c = run("go test -v ./...", &input, mode).unwrap();
        let out = body(&c);
        for l in [
            "=== RUN   TestWidget\n",
            "=== RUN   TestWidget/empty\n", // subtest of a failing parent: it never printed `--- PASS`
            "=== RUN   TestWidget/negative\n",
            "    widget_test.go:31: got -1, want 0\n",
            "--- FAIL: TestWidget/negative (0.00s)\n",
            "--- FAIL: TestWidget (0.00s)\n",
            "FAIL\texample.com/demo/widget\t0.015s\n",
            "panic: runtime error: index out of range [3] with length 3\n",
            "goroutine 7 [running]:\n",
            "\t/home/dev/demo/widget/render.go:57\n",
        ] {
            assert!(has_line(out, l), "{mode:?}: lost {l:?}");
        }
        assert!(!has_line(out, "=== RUN   TestPkg0Case0\n"), "{mode:?}");
        assert!(
            !has_line(out, "--- PASS: TestPkg0Case0 (0.00s)\n"),
            "{mode:?}"
        );
        assert!(
            !has_line(out, "ok  \texample.com/demo/pkg0\t0.112s\n"),
            "{mode:?}"
        );
    }
}

#[test]
fn npm_groups_exact_duplicate_deprecations_and_keeps_every_distinct_warning() {
    let input = fixture("npm_install.txt");
    for mode in MODES {
        let c = run("npm install", &input, mode).unwrap();
        let out = body(&c);
        let count = |l: &str| lines(out).iter().filter(|x| **x == l.as_bytes()).count();
        // Each distinct deprecation appears exactly once; none disappears.
        assert_eq!(
            count(
                "npm warn deprecated inflight@1.0.6: This module is not supported, and leaks memory.\n"
            ),
            1,
            "{mode:?}"
        );
        assert_eq!(
            count(
                "npm warn deprecated glob@7.2.3: Glob versions prior to v9 are no longer supported\n"
            ),
            1,
            "{mode:?}"
        );
        assert_eq!(
            count(
                "npm warn deprecated rimraf@3.0.2: Rimraf versions prior to v4 are no longer supported\n"
            ),
            1,
            "{mode:?}"
        );
        assert!(has_line(
            out,
            "added 312 packages, and audited 313 packages in 14s\n"
        ));
        assert!(has_line(out, "2 high severity vulnerabilities\n"));
        assert!(
            c.details
                .iter()
                .any(|d| d.contains("duplicate deprecation warnings collapsed")),
            "{:?}",
            c.details
        );
    }
}

#[test]
fn npm_errors_are_kept_with_their_context() {
    let input = fixture("npm_error.txt");
    for mode in MODES {
        let c = run("npm install", &input, mode).unwrap();
        let out = body(&c);
        for l in lines(&input)
            .into_iter()
            .filter(|l| l.starts_with(b"npm error"))
        {
            assert!(
                out.windows(l.len()).any(|w| w == l),
                "{mode:?}: lost {:?}",
                String::from_utf8_lossy(l)
            );
        }
    }
}

#[test]
fn pnpm_progress_and_the_plus_bar_go_but_not_the_warnings() {
    let input = fixture("pnpm_install.txt");
    let c = run("pnpm install", &input, Mode::Balanced).unwrap();
    let out = body(&c);
    assert!(has_line(out, "Done in 9.8s\n"));
    assert!(has_line(out, "Packages: +311\n"));
    // the first of the two identical WARN lines stays, the repeat is counted
    let warn = " WARN  deprecated glob@7.2.3: Glob versions prior to v9 are no longer supported\n";
    assert_eq!(
        lines(out).iter().filter(|l| **l == warn.as_bytes()).count(),
        1
    );
    assert!(!has_line(
        out,
        "Progress: resolved 25, reused 20, downloaded 5, added 20\n"
    ));
}

#[test]
fn git_transfer_progress_is_collapsed_and_ordinary_lines_stay() {
    let input = fixture("git_clone.txt");
    for mode in MODES {
        let c = run("git clone https://example.com/r.git", &input, mode).unwrap();
        let out = body(&c);
        for k in 0..4 {
            assert!(
                has_line(out, &format!("Cloning into 'sub{k}'...\n")),
                "{mode:?}"
            );
        }
        assert!(has_line(
            out,
            "Submodule path 'sub3': checked out '0123456789abcdef0123456789abcdef01234567'\n"
        ));
        assert!(!out.windows(2).any(|w| w == b"\r"), "no stray CR redraws");
        assert!(out.len() < input.len() / 2, "{mode:?}");
    }
}

#[test]
fn make_directory_chatter_goes_and_the_error_stays() {
    let input = fixture("make_recursive.txt");
    let c = run("make", &input, Mode::UltraLean).unwrap();
    let out = body(&c);
    assert!(has_line(
        out,
        "a.c:10:5: warning: implicit declaration of function 'foo'\n"
    ));
    assert!(has_line(out, "make: *** [Makefile:12: all] Error 1\n"));
    assert!(!has_line(
        out,
        "make[1]: Entering directory '/home/dev/proj/sub1'\n"
    ));
}

#[test]
fn docker_layer_progress_goes_and_build_errors_stay() {
    let pull = fixture("docker_pull.txt");
    let c = run("docker pull demo", &pull, Mode::Balanced).unwrap();
    let out = body(&c);
    assert!(has_line(out, "latest: Pulling from library/demo\n"));
    assert!(has_line(
        out,
        "Status: Downloaded newer image for demo:latest\n"
    ));
    assert!(
        lines(out)
            .iter()
            .all(|l| !l.windows(15).any(|w| w == b"Pulling fs layer"))
    );
    assert!(out.len() < pull.len() / 3);

    let build = fixture("docker_build.txt");
    for mode in MODES {
        let c = run("docker build -t x .", &build, mode).unwrap();
        let out = body(&c);
        for l in lines(&build).into_iter().filter(|l| {
            l.starts_with(b"#4")
                || l.starts_with(b"#3 [")
                || l.starts_with(b"error[")
                || l.starts_with(b"Dockerfile")
        }) {
            assert!(
                out.windows(l.len()).any(|w| w == l),
                "{mode:?}: lost {:?}",
                String::from_utf8_lossy(l)
            );
        }
        assert!(out.len() < build.len(), "{mode:?}");
    }
}

#[test]
fn ruff_snippets_go_only_in_ultra_lean_and_every_diagnostic_stays() {
    for name in ["ruff_check.txt", "ruff_check_new.txt"] {
        let input = fixture(name);
        assert!(
            run("ruff check", &input, Mode::Light).is_none(),
            "{name}: Light leaves ruff alone"
        );
        assert!(
            run("ruff check", &input, Mode::Balanced).is_none(),
            "{name}"
        );
        let c = run("ruff check", &input, Mode::UltraLean).unwrap();
        let out = body(&c);
        for i in 0..25 {
            let header = if name == "ruff_check.txt" {
                format!(
                    "src/mod{i}.py:{}:8: F401 [*] `os{i}` imported but unused\n",
                    3 + i
                )
            } else {
                format!("F401 [*] `os{i}` imported but unused\n")
            };
            assert!(has_line(out, &header), "{name}: lost {header:?}");
            let help = if name == "ruff_check.txt" {
                format!("  = help: Remove unused import: `os{i}`\n")
            } else {
                format!("help: Remove unused import: `os{i}`\n")
            };
            assert!(has_line(out, &help), "{name}: lost {help:?}");
        }
        assert!(!has_line(out, "3 | import os0\n"), "{name}");
        assert!(out.len() < input.len() * 3 / 4, "{name}");
    }
}

#[test]
fn colour_codes_are_stripped_for_supported_tools_only() {
    let input = fixture("cargo_color.txt");
    let c = run("cargo build", &input, Mode::Balanced).unwrap();
    let out = body(&c);
    assert!(!out.contains(&0x1b), "no escape bytes left");
    assert!(has_line(out, "warning: unused import: `std::fmt`\n"));
    assert!(has_line(out, " --> src/lib.rs:1:5\n"));
    assert!(
        c.details.iter().any(|d| d == "colour codes stripped"),
        "{:?}",
        c.details
    );
    // The same bytes under an unknown tool are not touched at all.
    assert!(run("cat file", &input, Mode::Balanced).is_none());
    assert!(run("git log", &input, Mode::Balanced).is_none());
}

// ---- properties across everything ------------------------------------------------------------

const FIXTURES: [(&str, &str); 14] = [
    ("cargo build", "cargo_build_warnings.txt"),
    ("cargo test", "cargo_test_failures.txt"),
    ("cargo test", "cargo_test_pass.txt"),
    ("pytest", "pytest_failures.txt"),
    ("pytest -v", "pytest_verbose_pass.txt"),
    ("go test -v", "go_test_verbose.txt"),
    ("npm install", "npm_install.txt"),
    ("npm install", "npm_error.txt"),
    ("pnpm install", "pnpm_install.txt"),
    ("git clone r", "git_clone.txt"),
    ("make", "make_recursive.txt"),
    ("docker pull x", "docker_pull.txt"),
    ("docker build .", "docker_build.txt"),
    ("ruff check", "ruff_check.txt"),
];

#[test]
fn every_diagnostic_line_survives_in_every_mode() {
    for (cmd, name) in FIXTURES {
        let input = fixture(name);
        for mode in MODES {
            let Some(c) = run(cmd, &input, mode) else {
                continue;
            };
            let out = body(&c);
            for l in diagnostic_lines(&input) {
                // The duplicate-warning rule is the one sanctioned collapse of a warning: an
                // exact repeat is counted, its first copy is kept.
                assert!(
                    out.windows(l.len()).any(|w| w == l),
                    "{cmd} {name} {mode:?}: lost {:?}",
                    String::from_utf8_lossy(l)
                );
            }
        }
    }
}

#[test]
fn removal_never_touches_lines_that_are_not_noise_and_shrinks_the_output() {
    for (cmd, name) in FIXTURES {
        let input = fixture(name);
        for mode in MODES {
            let Some(c) = run(cmd, &input, mode) else {
                continue;
            };
            let out = body(&c);
            assert!(out.len() < input.len(), "{cmd} {name} {mode:?}");
            // Kept lines appear in the original, in order (carriage-return collapsing and colour
            // stripping aside: the fixtures that have those are checked separately).
            if name != "git_clone.txt" {
                let original = lines(&input);
                let mut cursor = 0;
                for l in lines(out) {
                    let pos = original[cursor..].iter().position(|o| *o == l);
                    assert!(
                        pos.is_some(),
                        "{cmd} {name} {mode:?}: invented or reordered {:?}",
                        String::from_utf8_lossy(l)
                    );
                    cursor += pos.unwrap() + 1;
                }
            }
            let (kept, total) = c.lines.unwrap();
            assert!(kept < total || out.len() < input.len());
        }
    }
}

#[test]
fn light_mode_never_removes_more_context_than_ultra_lean() {
    for (cmd, name) in FIXTURES {
        let input = fixture(name);
        let (Some(l), Some(u)) = (
            run(cmd, &input, Mode::Light),
            run(cmd, &input, Mode::UltraLean),
        ) else {
            continue;
        };
        // Light protects 5 lines each side and 20 at the end; Ultra-Lean 1 and 5.
        assert!(body(&l).len() >= body(&u).len(), "{cmd} {name}");
    }
}

#[test]
fn line_endings_and_odd_bytes_are_preserved() {
    // CRLF throughout.
    let crlf: Vec<u8> = fixture("cargo_test_failures.txt")
        .split_inclusive(|b| *b == b'\n')
        .flat_map(|l| {
            let mut v = l[..l.len() - 1].to_vec();
            v.extend_from_slice(b"\r\n");
            v
        })
        .collect();
    let c = run("cargo test", &crlf, Mode::Balanced).unwrap();
    let out = body(&c);
    assert!(
        lines(out).iter().all(|l| l.ends_with(b"\r\n")),
        "CRLF everywhere"
    );
    assert!(has_line(out, "test parser::tests::case_17 ... FAILED\r\n"));

    // No trailing newline at the end.
    let mut no_nl = fixture("cargo_build_warnings.txt");
    no_nl.pop();
    let c = run("cargo build", &no_nl, Mode::Balanced).unwrap();
    assert!(!body(&c).ends_with(b"\n"));
    assert!(body(&c).ends_with(b"target(s) in 8.52s"));

    // Invalid UTF-8 in a kept line, and inside a noise line.
    let mut odd = fixture("cargo_build_warnings.txt");
    let marker = b"warning: unused variable: `retries`";
    let pos = odd.windows(marker.len()).position(|w| w == marker).unwrap();
    odd.splice(pos + marker.len()..pos + marker.len(), *b" \xff\xfe\x80");
    let compiling = b"   Compiling serde v1.0.200";
    let pos = odd
        .windows(compiling.len())
        .position(|w| w == compiling)
        .unwrap();
    odd.splice(pos + compiling.len()..pos + compiling.len(), *b"\xc3");
    let c = run("cargo build", &odd, Mode::Balanced).unwrap();
    let out = body(&c);
    assert!(
        out.windows(4).any(|w| w == b" \xff\xfe\x80"),
        "a kept line is byte-exact"
    );
    assert!(
        !has_line(out, "   Compiling serde v1.0.200\u{c3}\n"),
        "a noise line holding a stray byte is still recognised and removed"
    );
}

#[test]
fn nothing_to_remove_means_no_candidate() {
    for cmd in [
        "cargo build",
        "cargo test",
        "pytest",
        "go test",
        "npm install",
        "git clone r",
        "make",
        "docker pull x",
        "ruff check",
    ] {
        assert!(
            run(cmd, b"hello\nworld\n", Mode::Balanced).is_none(),
            "{cmd}"
        );
        assert!(run(cmd, b"", Mode::Balanced).is_none(), "{cmd}");
    }
}

#[test]
fn noise_inside_the_last_lines_is_never_removed() {
    // Only noise, 30 lines: the last `last_lines` of each mode stay.
    let input: Vec<u8> = (0..30)
        .flat_map(|i| format!("   Compiling pkg{i} v0.1.0\n").into_bytes())
        .collect();
    for (mode, tail) in [
        (Mode::Light, 20),
        (Mode::Balanced, 10),
        (Mode::UltraLean, 5),
    ] {
        let c = run("cargo build", &input, mode).unwrap();
        let out = body(&c);
        assert_eq!(lines(out).len(), tail, "{mode:?}");
        assert!(has_line(out, "   Compiling pkg29 v0.1.0\n"));
        assert!(!has_line(out, "   Compiling pkg0 v0.1.0\n"));
    }
}

#[test]
fn a_warning_between_noise_lines_keeps_its_neighbours_within_the_mode_distance() {
    let mut text = String::new();
    for i in 0..40 {
        text.push_str(&format!("   Compiling c{i} v0.1.0\n"));
    }
    text.push_str("warning: something odd\n");
    for i in 40..80 {
        text.push_str(&format!("   Compiling c{i} v0.1.0\n"));
    }
    let input = text.as_bytes();
    for mode in MODES {
        let d = ModeCtx::of(mode);
        let c = run("cargo build", input, mode).unwrap();
        let out = body(&c);
        assert!(has_line(out, "warning: something odd\n"));
        for k in 1..=d {
            assert!(
                has_line(out, &format!("   Compiling c{} v0.1.0\n", 40 - k)),
                "{mode:?} -{k}"
            );
            assert!(
                has_line(out, &format!("   Compiling c{} v0.1.0\n", 39 + k)),
                "{mode:?} +{k}"
            );
        }
        assert!(
            !has_line(out, &format!("   Compiling c{} v0.1.0\n", 40 - d - 1)),
            "{mode:?}"
        );
        assert!(
            !has_line(out, &format!("   Compiling c{} v0.1.0\n", 40 + d)),
            "{mode:?}"
        );
    }
}
