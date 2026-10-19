//! Command-aware shell output compression (SPEC §3 Tier 2, "Shell output compression").
//!
//! The rule that shapes everything here: **a line is removed only if an explicit noise rule for the
//! detected tool matches it**, and never when it is within the mode's context distance of an
//! error/warning/failure line or among the last lines. Everything else stays byte for byte. Unknown
//! tools, unknown subcommands, machine-readable invocations and anything unparseable are left alone
//! (`None`), so the output is only scrubbed.
//!
//! Dispatch is on structured data, never on substrings of shell text: [`detect`] looks at the
//! executable's basename and the real subcommand (skipping a per-tool table of global options), and
//! [`parse_simple_command`] turns a hook's shell string into an argv only when it is exactly one
//! simple command.
//!
//! Everything works on bytes, so non-UTF-8 output is safe, line endings (LF, CRLF, none at the end)
//! are preserved, and a carriage-return progress bar collapses to its final state. Colour codes
//! (`ESC [ … m`) are stripped, for supported tools only.
//!
//! The noise rules were written from the tools' documented output formats and from observing them.
//! Where a rule was informed by another project it says so with `// pattern ref:` and was not
//! copied (CLAUDE.md, "Reference repos").

use std::collections::HashSet;

use crate::diet::{Candidate, ExitStatus, Layout, ModeParams, Transform};
use crate::state::Mode;

// ---- shell strings --------------------------------------------------------------------------

/// Turn a hook's shell string into an argv, **only** if it is exactly one simple command:
/// optional leading `NAME=value` assignments (skipped), then words. Quotes are honoured. Any pipe,
/// `;`, `&`, redirection, subshell, command substitution, parameter expansion, comment or newline
/// outside single quotes makes the whole string `None` (SPEC: `cd app && cargo test` is a known
/// miss).
pub fn parse_simple_command(shell: &str) -> Option<Vec<String>> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = shell.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        other => word.push(other),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '$' | '`' => return None,
                        '\\' => match chars.next()? {
                            e @ ('"' | '\\') => word.push(e),
                            // `\$` and `` \` `` escape an expansion; a backslash-newline is a
                            // continuation: all of them are beyond what we claim to understand.
                            '$' | '`' | '\n' => return None,
                            other => {
                                word.push('\\');
                                word.push(other);
                            }
                        },
                        other => word.push(other),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next()? {
                    '\n' | '\r' => return None,
                    other => word.push(other),
                }
            }
            '|' | '&' | ';' | '<' | '>' | '(' | ')' | '$' | '`' | '\n' | '\r' => return None,
            '#' if !in_word => return None,
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            other => {
                in_word = true;
                word.push(other);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    // Leading assignments are not part of the argv.
    let skip = words.iter().take_while(|w| is_assignment(w)).count();
    let argv: Vec<String> = words.into_iter().skip(skip).collect();
    (!argv.is_empty()).then_some(argv)
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ---- dispatch -------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Cargo,
    Pytest,
    GoTest,
    Npm,
    Pnpm,
    Yarn,
    /// `npm ls` / `pnpm list` / `yarn list`: a dependency tree.
    PkgList,
    Git,
    Make,
    Docker,
    Ruff,
}

/// A recognised, compressible invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    tool: Tool,
    /// `"cargo test"`: built from the recognised executable and subcommand, never from argv text.
    label: &'static str,
}

impl Detected {
    pub fn label(&self) -> &'static str {
        self.label
    }
}

fn basename(arg: &str) -> &str {
    crate::basename(arg)
}

/// Flags that mark machine-readable output, wherever they appear before a bare `--`.
fn is_machine_flag(arg: &str) -> bool {
    const PREFIXES: [&str; 7] = [
        "--porcelain",
        "--json",
        "--format",
        "--pretty",
        "--template",
        "--message-format",
        "--output-format",
    ];
    arg == "-z" || arg == "-json" || arg == "--null" || PREFIXES.iter().any(|p| arg.starts_with(p))
}

fn has_machine_flag(args: &[String]) -> bool {
    args.iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| is_machine_flag(a))
}

/// Recognise a compressible invocation, or `None` (leave the output alone).
pub fn detect(argv: &[String]) -> Option<Detected> {
    let exe = basename(argv.first()?);
    let args = &argv[1..];
    let found = match exe {
        "cargo" => detect_cargo(args),
        "pytest" | "py.test" => detect_pytest(args),
        e if is_python(e) => match args {
            [m, module, rest @ ..] if m == "-m" && module == "pytest" => detect_pytest(rest),
            _ => None,
        },
        "go" => detect_go(args),
        "npm" => detect_pkg(Tool::Npm, "npm install", "npm ls", args),
        "pnpm" => detect_pkg(Tool::Pnpm, "pnpm install", "pnpm ls", args),
        "yarn" => detect_pkg(Tool::Yarn, "yarn install", "yarn list", args),
        "git" => detect_git(args),
        "make" | "gmake" => detect_make(args),
        "docker" => detect_docker(args),
        "ruff" => detect_ruff(args),
        _ => None,
    }?;
    Some(found)
}

fn is_python(exe: &str) -> bool {
    exe == "python"
        || exe == "python3"
        || exe
            .strip_prefix("python3.")
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
}

fn detect_cargo(args: &[String]) -> Option<Detected> {
    let mut i = 0;
    if args.first().is_some_and(|a| a.starts_with('+')) {
        i = 1; // `+nightly`
    }
    // Global options before the subcommand; anything else means a shape we do not know.
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "-q" | "--quiet" | "-v" | "-vv" | "--verbose" | "--locked" | "--frozen"
            | "--offline" => i += 1,
            a if a.starts_with("--color=") => i += 1,
            "--color" | "-Z" | "-C" | "--config" => i += 2,
            a if a.starts_with('-') => return None,
            _ => break,
        }
    }
    let sub = args.get(i)?;
    let rest = &args[i + 1..];
    let label = match sub.as_str() {
        "build" | "b" => "cargo build",
        "check" | "c" => "cargo check",
        "clippy" => "cargo clippy",
        "test" | "t" => "cargo test",
        "bench" => "cargo bench",
        "doc" | "d" => "cargo doc",
        "install" => "cargo install",
        // `cargo run` is deliberately absent: the program's own output would be matched by rules
        // meant for cargo's.
        _ => return None,
    };
    if has_machine_flag(rest) {
        return None;
    }
    // Arguments after `--` go to libtest / the lint driver: JSON test output and test listings
    // are data.
    if let Some(pos) = rest.iter().position(|a| a == "--") {
        let after = &rest[pos + 1..];
        if after.iter().any(|a| {
            a.starts_with("--format")
                || a == "--list"
                || a == "--show-output"
                || a.starts_with("-Zunstable-options")
        }) {
            return None;
        }
    }
    Some(Detected {
        tool: Tool::Cargo,
        label,
    })
}

fn detect_pytest(args: &[String]) -> Option<Detected> {
    const NOT_RUNS: [&str; 10] = [
        "--collect-only",
        "--co",
        "--fixtures",
        "--markers",
        "--help",
        "-h",
        "--version",
        "-V",
        "--setup-plan",
        "--setup-show",
    ];
    if has_machine_flag(args) || args.iter().any(|a| NOT_RUNS.contains(&a.as_str())) {
        return None;
    }
    Some(Detected {
        tool: Tool::Pytest,
        label: "pytest",
    })
}

fn detect_go(args: &[String]) -> Option<Detected> {
    if args.first().map(String::as_str) != Some("test") {
        return None;
    }
    let rest = &args[1..];
    if has_machine_flag(rest)
        || rest
            .iter()
            .any(|a| a == "-list" || a == "-c" || a.starts_with("-test.list"))
    {
        return None;
    }
    Some(Detected {
        tool: Tool::GoTest,
        label: "go test",
    })
}

fn detect_pkg(
    tool: Tool,
    label: &'static str,
    list_label: &'static str,
    args: &[String],
) -> Option<Detected> {
    let sub = args.first()?;
    // The dependency tree: finite, often thousands of lines, mostly `deduped` repeats.
    if matches!(sub.as_str(), "ls" | "list" | "ll" | "la") {
        if has_machine_flag(&args[1..]) || args.iter().any(|a| a == "--parseable" || a == "-p") {
            return None;
        }
        return Some(Detected {
            tool: Tool::PkgList,
            label: list_label,
        });
    }
    if !matches!(
        sub.as_str(),
        "install"
            | "i"
            | "add"
            | "ci"
            | "update"
            | "upgrade"
            | "up"
            | "remove"
            | "rm"
            | "uninstall"
    ) {
        return None;
    }
    if has_machine_flag(&args[1..]) || args.iter().any(|a| a == "--parseable" || a == "-p") {
        return None;
    }
    Some(Detected { tool, label })
}

fn detect_git(args: &[String]) -> Option<Detected> {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--no-pager" | "--paginate" | "--no-optional-locks" | "--bare" => i += 1,
            "-C" | "-c" => i += 2,
            a if a.starts_with("--git-dir=") || a.starts_with("--work-tree=") => i += 1,
            a if a.starts_with('-') => return None,
            _ => break,
        }
    }
    let sub = args.get(i)?;
    // Only commands whose human output carries transfer progress. `diff`, `log`, `show`, `blame`
    // and `status` are never touched.
    if !matches!(sub.as_str(), "clone" | "fetch" | "pull" | "push") {
        return None;
    }
    if has_machine_flag(&args[i + 1..]) {
        return None;
    }
    Some(Detected {
        tool: Tool::Git,
        label: "git transfer",
    })
}

fn detect_make(args: &[String]) -> Option<Detected> {
    const NOT_BUILDS: [&str; 9] = [
        "-n",
        "--dry-run",
        "--just-print",
        "-p",
        "--print-data-base",
        "-q",
        "--question",
        "--version",
        "-h",
    ];
    if args.iter().any(|a| NOT_BUILDS.contains(&a.as_str())) || has_machine_flag(args) {
        return None;
    }
    Some(Detected {
        tool: Tool::Make,
        label: "make",
    })
}

fn detect_docker(args: &[String]) -> Option<Detected> {
    let (sub, rest) = match args {
        [b, s, rest @ ..] if b == "buildx" && s == "build" => ("build", rest),
        [s, rest @ ..] => (s.as_str(), rest),
        [] => return None,
    };
    if !matches!(sub, "pull" | "push" | "build") {
        return None;
    }
    if has_machine_flag(rest)
        || rest
            .iter()
            .any(|a| a == "-q" || a == "--quiet" || a.starts_with("--progress=raw"))
    {
        return None;
    }
    Some(Detected {
        tool: Tool::Docker,
        label: "docker",
    })
}

fn detect_ruff(args: &[String]) -> Option<Detected> {
    if args.first().map(String::as_str) != Some("check") {
        return None;
    }
    let rest = &args[1..];
    // An explicit output format is a choice of machine format; so are the listing modes.
    if has_machine_flag(rest)
        || rest.iter().any(|a| {
            matches!(
                a.as_str(),
                "--statistics" | "--show-settings" | "--show-files" | "--show-fixes"
            )
        })
    {
        return None;
    }
    Some(Detected {
        tool: Tool::Ruff,
        label: "ruff check",
    })
}

// ---- line classification ----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Noise {
    BuildProgress,
    PassingTest,
    PytestProgress,
    PytestPassed,
    GoRun,
    GoPass,
    GoOk,
    InstallProgress,
    DuplicateWarning,
    TransferProgress,
    MakeDirectory,
    LayerProgress,
    Snippet,
    DedupedEntry,
}

impl Noise {
    fn label(self) -> &'static str {
        match self {
            Noise::BuildProgress => "build-progress lines",
            Noise::PassingTest => "passing test lines",
            Noise::PytestProgress => "progress-dot lines",
            Noise::PytestPassed => "PASSED lines",
            Noise::GoRun => "=== RUN/CONT lines",
            Noise::GoPass => "--- PASS lines",
            Noise::GoOk => "ok-package lines",
            Noise::InstallProgress => "install-progress lines",
            Noise::DuplicateWarning => "duplicate deprecation warnings",
            Noise::TransferProgress => "transfer-progress lines",
            Noise::MakeDirectory => "directory lines",
            Noise::LayerProgress => "layer-progress lines",
            Noise::Snippet => "source-snippet lines",
            Noise::DedupedEntry => "deduped lines",
        }
    }

    /// Snippet lines are the very content to remove next to a diagnostic, and an exact repeat of
    /// a warning is removable wherever it sits (its first copy is kept and the count reported), so
    /// context protection does not apply to them.
    fn ignores_context(self) -> bool {
        matches!(self, Noise::Snippet | Noise::DuplicateWarning)
    }

    /// An exact repeat loses nothing, so even the "last lines" rule does not keep it.
    fn ignores_tail(self) -> bool {
        matches!(self, Noise::DuplicateWarning)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Removable if unprotected.
    Noise(Noise),
    /// An error, warning or failure: kept, and it protects its neighbours.
    Diagnostic,
    /// Anything else: kept.
    Other,
}

fn trim_start(line: &[u8]) -> &[u8] {
    let n = line
        .iter()
        .take_while(|b| **b == b' ' || **b == b'\t')
        .count();
    &line[n..]
}

fn trim_end(line: &[u8]) -> &[u8] {
    let n = line
        .iter()
        .rev()
        .take_while(|b| b.is_ascii_whitespace())
        .count();
    &line[..line.len() - n]
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn contains_ci(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle))
}

/// The generic notion of "this line is a problem". Deliberately wide: it only ever *protects*
/// lines, so a false positive costs a few bytes of saving and a false negative is impossible to
/// make harmful by itself, because noise needs an explicit rule to be removed at all.
fn looks_like_diagnostic(line: &[u8]) -> bool {
    const WORDS: [&[u8]; 14] = [
        b"error",
        b"warn",
        b"fail",
        b"panic",
        b"fatal",
        b"exception",
        b"traceback",
        b"denied",
        b"unauthorized",
        b"abort",
        b"critical",
        b"cannot",
        b"could not",
        b"not found",
    ];
    WORDS.iter().any(|w| contains_ci(line, w))
        || line.starts_with(b"E  ")
        || line.starts_with(b"--> ")
        || contains(line, b"*** ")
}

/// `n` ASCII digits (at least one) at the start; returns the rest.
fn skip_digits(line: &[u8]) -> Option<&[u8]> {
    let n = line.iter().take_while(|b| b.is_ascii_digit()).count();
    (n > 0).then(|| &line[n..])
}

fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit() && !b.is_ascii_uppercase()
}

/// Per-run state some tools need (dedupe, two-pass pairing).
struct Context {
    seen_warnings: HashSet<Vec<u8>>,
    go_passed: HashSet<Vec<u8>>,
    ultra: bool,
}

fn classify(tool: Tool, line: &[u8], ctx: &mut Context) -> Class {
    let body = trim_end(line);
    let noise = match tool {
        Tool::Cargo => cargo_noise(body),
        Tool::Pytest => pytest_noise(body),
        Tool::GoTest => go_noise(body, ctx),
        Tool::Npm | Tool::Pnpm | Tool::Yarn => pkg_noise(body, ctx),
        Tool::PkgList => pkg_list_noise(body),
        Tool::Git => git_noise(body),
        Tool::Make => make_noise(body),
        Tool::Docker => docker_noise(body),
        Tool::Ruff => ruff_noise(body, ctx.ultra),
    };
    // In a dependency tree only the tree's own problem markers are diagnostics: package names such
    // as `es-errors` and `UNMET OPTIONAL` lines would otherwise protect the noise around them.
    let diagnostic = if tool == Tool::PkgList {
        is_problem_line(body, true)
    } else {
        looks_like_diagnostic(body)
    };
    match noise {
        Some(n) => Class::Noise(n),
        None if diagnostic => Class::Diagnostic,
        None => Class::Other,
    }
}

// pattern ref: rtk/src (behavior only) - which kinds of lines the common build tools print as
// progress; the matching rules below are written from the tools' own output formats.
fn cargo_noise(body: &[u8]) -> Option<Noise> {
    // `   Compiling foo v0.1.0 (/path)`: cargo right-aligns a verb in 12 columns.
    if body.first() == Some(&b' ') {
        let t = trim_start(body);
        for verb in [
            &b"Compiling "[..],
            b"Checking ",
            b"Downloading ",
            b"Downloaded ",
            b"Fresh ",
        ] {
            if t.starts_with(verb) {
                return Some(Noise::BuildProgress);
            }
        }
    }
    // `test module::name ... ok` (but not `... FAILED`, `... ignored`, `test result:`).
    if body.starts_with(b"test ") && body.ends_with(b" ... ok") {
        return Some(Noise::PassingTest);
    }
    None
}

fn is_progress_percent(rest: &[u8]) -> bool {
    // ` [ 12%]` or ` [100%]`
    let r = trim_start(rest);
    let Some(r) = r.strip_prefix(b"[") else {
        return false;
    };
    let r = trim_start(r);
    skip_digits(r).is_some_and(|r| r == b"%]")
}

fn pytest_noise(body: &[u8]) -> Option<Noise> {
    // Peel an optional trailing ` [ 42%]`.
    let core = match body.iter().rposition(|b| *b == b'[') {
        Some(i) if is_progress_percent(&body[i..]) => trim_end(&body[..i]),
        _ => body,
    };
    // `tests/test_a.py ....` or `....` : only passing dots.
    let dots_only = |s: &[u8]| !s.is_empty() && s.iter().all(|b| *b == b'.');
    if dots_only(core) {
        return Some(Noise::PytestProgress);
    }
    if let Some(sp) = core.iter().position(|b| *b == b' ')
        && core[..sp].ends_with(b".py")
        && dots_only(trim_start(&core[sp..]))
    {
        return Some(Noise::PytestProgress);
    }
    // `tests/test_a.py::test_x PASSED`
    if contains(core, b"::") && core.ends_with(b" PASSED") {
        return Some(Noise::PytestPassed);
    }
    None
}

fn go_test_name(rest: &[u8]) -> &[u8] {
    let rest = trim_start(rest);
    let n = rest.iter().take_while(|b| !b.is_ascii_whitespace()).count();
    &rest[..n]
}

fn go_noise(body: &[u8], ctx: &Context) -> Option<Noise> {
    let t = trim_start(body);
    if t.starts_with(b"--- PASS: ") {
        return Some(Noise::GoPass);
    }
    for marker in [&b"=== RUN"[..], b"=== PAUSE", b"=== CONT"] {
        if let Some(rest) = t.strip_prefix(marker)
            && ctx.go_passed.contains(go_test_name(rest))
        {
            return Some(Noise::GoRun);
        }
    }
    // `ok  \texample.com/pkg\t0.012s` (and `(cached)`).
    if body.starts_with(b"ok") && body.get(2).is_some_and(|b| b.is_ascii_whitespace()) {
        return Some(Noise::GoOk);
    }
    None
}

/// `│ ├── react@19.0.0 deduped`: a package already shown elsewhere in the tree.
fn pkg_list_noise(body: &[u8]) -> Option<Noise> {
    body.ends_with(b" deduped").then_some(Noise::DedupedEntry)
}

fn pkg_noise(body: &[u8], ctx: &mut Context) -> Option<Noise> {
    let lower = body.to_ascii_lowercase();
    let stripped = lower.strip_prefix(b"npm ").unwrap_or(&lower);
    let t = trim_start(stripped);
    // `npm warn deprecated x@1: msg` / ` WARN  deprecated x@1: msg` (pnpm): all distinct ones
    // stay; an exact repeat of a line already shown is counted instead.
    let deprecated = t
        .strip_prefix(b"warn")
        .map(trim_start)
        .and_then(|r| r.strip_prefix(b"deprecated"))
        .is_some_and(|r| r.first().is_none_or(|b| b.is_ascii_whitespace()));
    if deprecated {
        return if ctx.seen_warnings.insert(body.to_vec()) {
            None
        } else {
            Some(Noise::DuplicateWarning)
        };
    }
    if stripped.starts_with(b"http ")
        || stripped.starts_with(b"timing ")
        || stripped.starts_with(b"sill ")
    {
        return Some(Noise::InstallProgress);
    }
    // pnpm: `Progress: resolved 10, reused 8, downloaded 2, added 10` and the `++++` bar.
    if t.starts_with(b"progress: resolved ") {
        return Some(Noise::InstallProgress);
    }
    if !body.is_empty() && body.iter().all(|b| matches!(b, b'+' | b'-')) && body.len() >= 8 {
        return Some(Noise::InstallProgress);
    }
    // yarn classic: `[2/4] Fetching packages...`
    if body.first() == Some(&b'[')
        && let Some(close) = body.iter().position(|b| *b == b']')
        && let Some((a, b)) = std::str::from_utf8(&body[1..close])
            .ok()
            .and_then(|s| s.split_once('/'))
        && a.bytes().all(|c| c.is_ascii_digit())
        && b.bytes().all(|c| c.is_ascii_digit())
        && !a.is_empty()
        && !b.is_empty()
    {
        return Some(Noise::InstallProgress);
    }
    None
}

fn git_noise(body: &[u8]) -> Option<Noise> {
    let t = body.strip_prefix(b"remote: ").unwrap_or(body);
    for verb in [
        &b"Enumerating objects:"[..],
        b"Counting objects:",
        b"Compressing objects:",
        b"Receiving objects:",
        b"Resolving deltas:",
        b"Writing objects:",
        b"Unpacking objects:",
        b"Updating files:",
    ] {
        if t.starts_with(verb) {
            return Some(Noise::TransferProgress);
        }
    }
    None
}

fn make_noise(body: &[u8]) -> Option<Noise> {
    let rest = body.strip_prefix(b"make")?;
    // `make: Entering directory '/x'` or `make[2]: Leaving directory '/x'`
    let rest = if let Some(r) = rest.strip_prefix(b"[") {
        skip_digits(r)?.strip_prefix(b"]")?
    } else {
        rest
    };
    let rest = rest.strip_prefix(b": ")?;
    (rest.starts_with(b"Entering directory ") || rest.starts_with(b"Leaving directory "))
        .then_some(Noise::MakeDirectory)
}

fn docker_noise(body: &[u8]) -> Option<Noise> {
    // Legacy pull/push progress: `3e2a1b4c5d6e: Downloading [===>   ]  1.2MB/30MB`.
    if body.len() > 14 && body[..12].iter().all(|b| is_hex(*b)) && &body[12..14] == b": " {
        let rest = &body[14..];
        for phase in [
            &b"Pulling fs layer"[..],
            b"Waiting",
            b"Verifying Checksum",
            b"Download complete",
            b"Pull complete",
            b"Already exists",
            b"Layer already exists",
            b"Preparing",
            b"Pushed",
            b"Mounted from ",
            b"Downloading",
            b"Extracting",
            b"Pushing",
        ] {
            if rest.starts_with(phase) {
                return Some(Noise::LayerProgress);
            }
        }
    }
    // BuildKit transfer progress: `#5 sha256:ab12... 12.5MB / 30MB 1.2s`.
    if let Some(r) = body.strip_prefix(b"#")
        && let Some(r) = skip_digits(r)
        && let Some(r) = r.strip_prefix(b" sha256:")
        && contains(r, b"B / ")
    {
        return Some(Noise::LayerProgress);
    }
    None
}

fn ruff_noise(body: &[u8], ultra: bool) -> Option<Noise> {
    if !ultra {
        return None;
    }
    // The gutter of a rendered snippet: optional line number, then `|`.
    //   `  |` , `3 | import os` , `  |        ^^ F401`
    let t = trim_start(body);
    let after_num = skip_digits(t).map_or(t, trim_start);
    if after_num.first() == Some(&b'|') && (after_num.len() == 1 || after_num[1] == b' ') {
        return Some(Noise::Snippet);
    }
    None
}

// ---- the transform ----------------------------------------------------------------------------

/// Remove ESC `[` … `m` sequences. Returns `None` if there were none (no allocation).
fn strip_sgr(line: &[u8]) -> Option<Vec<u8>> {
    if !line.contains(&0x1b) {
        return None;
    }
    let mut out = Vec::with_capacity(line.len());
    let mut i = 0;
    let mut changed = false;
    while i < line.len() {
        if line[i] == 0x1b && line.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            while j < line.len() && (line[j].is_ascii_digit() || line[j] == b';' || line[j] == b':')
            {
                j += 1;
            }
            if line.get(j) == Some(&b'm') {
                i = j + 1;
                changed = true;
                continue;
            }
        }
        out.push(line[i]);
        i += 1;
    }
    changed.then_some(out)
}

/// Split a line into content and its terminator (`\r\n`, `\n` or nothing).
fn split_terminator(line: &[u8]) -> (&[u8], &[u8]) {
    if let Some(c) = line.strip_suffix(b"\r\n") {
        (c, &line[c.len()..])
    } else if let Some(c) = line.strip_suffix(b"\n") {
        (c, &line[c.len()..])
    } else {
        (line, &[])
    }
}

/// A line that was redrawn with carriage returns keeps only its final state.
fn collapse_cr(content: &[u8]) -> Option<&[u8]> {
    let end = content.len();
    let trimmed = &content[..end - content.iter().rev().take_while(|b| **b == b'\r').count()];
    let pos = trimmed.iter().rposition(|b| *b == b'\r')?;
    let last = &trimmed[pos + 1..];
    (!last.is_empty()).then_some(last)
}

struct Prepared {
    /// The line after colour stripping and progress collapsing, without its terminator.
    content: Vec<u8>,
    terminator: Vec<u8>,
}

/// Compress `scrubbed` shell output of the invocation `argv`, or `None` to leave it alone.
///
/// With `head_tail` (`diet_exec` only, SPEC §3 Tier 2 "Head/tail fallback"), output that no tool
/// rule brings under [`head_tail_min_lines`] is cut to its head, its tail and its problem lines
/// with their context, unless it is machine-readable.
pub fn compress(
    argv: &[String],
    _exit: ExitStatus,
    scrubbed: &[u8],
    mode: Mode,
    head_tail: bool,
) -> Option<Candidate> {
    let detected = detect(argv);
    let by_tool = detected
        .as_ref()
        .and_then(|d| compress_detected(d, scrubbed, mode));
    if !head_tail || is_machine_readable(argv) {
        return by_tool;
    }
    let limit = head_tail_min_lines(mode);
    if by_tool
        .as_ref()
        .is_some_and(|c| c.lines.is_some_and(|(kept, _)| kept <= limit))
    {
        return by_tool;
    }
    let label = match &detected {
        Some(d) => d.label.to_string(),
        None => argv
            .first()
            .map(|a| basename(a).chars().take(40).collect())
            .unwrap_or_default(),
    };
    let tree = detected.as_ref().is_some_and(|d| d.tool == Tool::PkgList);
    cut_head_tail(&label, scrubbed, mode, tree).or(by_tool)
}

/// Output that is never cut by the head/tail fallback: machine-readable flags anywhere, and the
/// git commands whose output is itself the data (SPEC §3 Tier 2, "Machine-readable output").
fn is_machine_readable(argv: &[String]) -> bool {
    let args = argv.get(1..).unwrap_or_default();
    if has_machine_flag(args) {
        return true;
    }
    argv.first().is_some_and(|exe| basename(exe) == "git")
        && args.iter().any(|a| {
            matches!(a.as_str(), "diff" | "log" | "show" | "blame")
                || (a == "status" && args.iter().any(|b| b == "--short" || b == "-s"))
        })
}

/// Lines the head/tail fallback keeps. In a dependency tree (`npm ls`) only the tree's own problem
/// markers count: package names such as `es-errors` are not diagnostics, and an `UNMET OPTIONAL
/// DEPENDENCY` is another platform's optional binary. Elsewhere, the generic diagnostics plus those
/// markers.
fn is_problem_line(line: &[u8], tree: bool) -> bool {
    let tree_problem = (contains(line, b"UNMET ") && !contains(line, b"UNMET OPTIONAL"))
        || contains(line, b" invalid")
        || contains(line, b" extraneous")
        || contains(line, b" missing")
        || contains(line, b"ERR!")
        || contains_ci(line, b"npm error");
    tree_problem || (!tree && looks_like_diagnostic(line))
}

/// The head/tail fallback applies only to output longer than this many lines: four times what it
/// keeps (head and tail) before counting problem lines.
pub fn head_tail_min_lines(mode: Mode) -> usize {
    let (head, tail) = head_tail_sizes(mode);
    4 * (head + tail)
}

/// `(head, tail)` lines kept: the mode's last-lines count for the tail, twice that for the head.
fn head_tail_sizes(mode: Mode) -> (usize, usize) {
    let tail = ModeParams::for_mode(mode).last_lines;
    (2 * tail, tail)
}

/// Keep the first and last lines and every problem line with the mode's context; each elided run
/// becomes one marker line with its exact line range in the Vault copy.
fn cut_head_tail(label: &str, scrubbed: &[u8], mode: Mode, tree: bool) -> Option<Candidate> {
    let params = ModeParams::for_mode(mode);
    let (head, tail) = head_tail_sizes(mode);
    let lines: Vec<&[u8]> = scrubbed.split_inclusive(|b| *b == b'\n').collect();
    let total = lines.len();
    if total <= head_tail_min_lines(mode) {
        return None;
    }
    let mut keep = vec![false; total];
    keep[..head].iter_mut().for_each(|k| *k = true);
    keep[total - tail..].iter_mut().for_each(|k| *k = true);
    for (i, line) in lines.iter().enumerate() {
        if is_problem_line(trim_end(line), tree) {
            let from = i.saturating_sub(params.context_lines);
            let to = (i + params.context_lines).min(total - 1);
            keep[from..=to].iter_mut().for_each(|k| *k = true);
        }
    }

    let mut body = Vec::with_capacity(scrubbed.len() / 4);
    let mut kept = 0usize;
    let mut elided = 0usize;
    let mut i = 0;
    while i < total {
        if keep[i] {
            body.extend_from_slice(lines[i]);
            kept += 1;
            i += 1;
            continue;
        }
        let start = i;
        while i < total && !keep[i] {
            i += 1;
        }
        let n = i - start;
        elided += n;
        // Lines are 1-based in `agentdiet show --lines`.
        let terminator: &[u8] = if lines[start].ends_with(b"\r\n") {
            b"\r\n"
        } else {
            b"\n"
        };
        if !body.is_empty() && !body.ends_with(b"\n") {
            body.extend_from_slice(terminator);
        }
        body.extend_from_slice(format!("[… {n} lines elided: {}..{} …]", start + 1, i).as_bytes());
        body.extend_from_slice(terminator);
    }
    if elided == 0 {
        return None;
    }
    Some(Candidate {
        kind: Transform::Compressed,
        layout: Layout::Trailer { body },
        details: vec![format!(
            "{label}: {elided} lines elided (head {head}, tail {tail} and problem lines kept)"
        )],
        lines: Some((kept, total)),
    })
}

fn compress_detected(detected: &Detected, scrubbed: &[u8], mode: Mode) -> Option<Candidate> {
    let params = ModeParams::for_mode(mode);
    let mut ctx = Context {
        seen_warnings: HashSet::new(),
        go_passed: HashSet::new(),
        ultra: mode == Mode::UltraLean,
    };

    // 1. Normalise every line (colour codes, carriage-return progress) and remember what changed.
    let mut ansi_lines = 0usize;
    let mut cr_lines = 0usize;
    let mut lines: Vec<Prepared> = Vec::new();
    for raw in scrubbed.split_inclusive(|b| *b == b'\n') {
        let (content, terminator) = split_terminator(raw);
        let mut content: Vec<u8> = content.to_vec();
        if let Some(plain) = strip_sgr(&content) {
            content = plain;
            ansi_lines += 1;
        }
        if let Some(last) = collapse_cr(&content) {
            content = last.to_vec();
            cr_lines += 1;
        }
        lines.push(Prepared {
            content,
            terminator: terminator.to_vec(),
        });
    }
    let total = lines.len();
    if total == 0 {
        return None;
    }

    // 2. Two-pass pairing for `go test`: remember which tests passed.
    if detected.tool == Tool::GoTest {
        for l in &lines {
            if let Some(rest) = trim_start(&l.content).strip_prefix(b"--- PASS: ") {
                ctx.go_passed.insert(go_test_name(rest).to_vec());
            }
        }
    }

    // 3. Classify.
    let classes: Vec<Class> = lines
        .iter()
        .map(|l| classify(detected.tool, &l.content, &mut ctx))
        .collect();

    // 4. Protect: context around diagnostics, and the last lines.
    let mut protected = vec![false; total];
    for (i, class) in classes.iter().enumerate() {
        if *class == Class::Diagnostic {
            let from = i.saturating_sub(params.context_lines);
            let to = (i + params.context_lines).min(total - 1);
            protected[from..=to].iter_mut().for_each(|p| *p = true);
        }
    }
    let tail_from = total.saturating_sub(params.last_lines);

    // 5. Remove unprotected noise.
    let mut kept: Vec<u8> = Vec::with_capacity(scrubbed.len());
    let mut kept_lines = 0usize;
    let mut removed: Vec<(Noise, usize)> = Vec::new();
    for (i, (line, class)) in lines.iter().zip(&classes).enumerate() {
        if let Class::Noise(kind) = class {
            let context_protected = protected[i] && !kind.ignores_context();
            let tail_protected = i >= tail_from && !kind.ignores_tail();
            if !context_protected && !tail_protected {
                match removed.iter_mut().find(|(k, _)| k == kind) {
                    Some((_, n)) => *n += 1,
                    None => removed.push((*kind, 1)),
                }
                continue;
            }
        }
        kept.extend_from_slice(&line.content);
        kept.extend_from_slice(&line.terminator);
        kept_lines += 1;
    }

    if removed.is_empty() && ansi_lines == 0 && cr_lines == 0 {
        return None;
    }

    let mut details: Vec<String> = Vec::new();
    for (i, (kind, n)) in removed.iter().enumerate() {
        details.push(if i == 0 {
            format!("{}: {n} {} collapsed", detected.label, kind.label())
        } else {
            format!("{n} {} collapsed", kind.label())
        });
    }
    if cr_lines > 0 {
        details.push(format!("{cr_lines} progress redraws collapsed"));
    }
    if ansi_lines > 0 {
        details.push("colour codes stripped".to_string());
    }
    Some(Candidate {
        kind: Transform::Compressed,
        layout: Layout::Trailer { body: kept },
        details,
        lines: Some((kept_lines, total)),
    })
}
