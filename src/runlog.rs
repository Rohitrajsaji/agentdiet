//! The run log's writing side (SPEC §3 Tier 3, "Run log"): one memory.db row per command or tool
//! call that a shim's agent path, `diet_exec` or a Claude Code hook saw, trimmed or not.
//!
//! Recording is telemetry: best effort, scrubbed (P1), and bounded in time (memory.db is opened
//! with [`crate::memory::RUN_BUSY_TIMEOUT`]). A failure is reported to the caller as a bounded note
//! and never changes what the agent is given. The transparent shim path never records (it `exec`s
//! before anything is opened).

use crate::Paths;
use crate::basename;
use crate::compress::parse_simple_command;
use crate::diet::ExitStatus;
use crate::memory::{self, NewRun, Origin, RunKind, RunOutcome};
use crate::scrubber::{Scrubbed, scrub, scrub_bounded};
use crate::session::{self, Key};

/// Recorded titles are cut to this many bytes (after scrubbing).
pub const MAX_TITLE_BYTES: usize = 240;
/// Output of at least this many estimated tokens is "bloat": hooks log a Read only from here on,
/// and `reflect` flags commands that keep producing it.
pub const BLOAT_TOKENS: u64 = 1000;

/// One run, before scrubbing.
#[derive(Debug, Clone, Copy)]
pub struct Run<'a> {
    pub origin: Origin,
    pub kind: RunKind,
    /// The executable's base name or the tool name.
    pub family: &'a str,
    /// The command line, path or pattern.
    pub title: &'a str,
    pub outcome: RunOutcome,
    pub exit_code: Option<i32>,
    pub raw_tokens: u64,
    pub emitted_tokens: u64,
    pub cas_sha: Option<&'a str>,
    pub session: Option<&'a Scrubbed>,
}

/// Record `run` in the workspace's memory.db. `Err` carries a bounded, scrubbed note.
pub fn record(paths: &Paths, run: &Run<'_>) -> Result<(), String> {
    let family = scrub_bounded(run.family, 64);
    let title = scrub_bounded(run.title, MAX_TITLE_BYTES);
    let row = NewRun {
        origin: run.origin,
        kind: run.kind,
        family: &family,
        title: &title,
        outcome: run.outcome,
        exit_code: run.exit_code,
        raw_tokens: run.raw_tokens,
        emitted_tokens: run.emitted_tokens,
        cas_sha: run.cas_sha,
        session: run.session,
    };
    memory::record_run(&paths.memory_db(), &row)
        .map_err(|_| crate::diet::note("agentdiet: run log not updated"))
}

/// The memory.db session label for a sidecar key: the scrubbed agent session id, or `pid:<n>` /
/// `agent:unknown` for a shim under an agent without hooks.
pub fn session_label(key: &Key) -> Option<Scrubbed> {
    match key {
        Key::Session(id) => session::label(id),
        Key::AgentPid(pid) => Some(scrub(format!("pid:{pid}").as_bytes())),
        Key::UnknownAgent => Some(scrub(b"agent:unknown")),
    }
}

/// `ok` for exit 0, `fail` for anything else (another code, a signal, unknown).
pub fn outcome_of(exit: ExitStatus) -> RunOutcome {
    if exit == ExitStatus::Code(0) {
        RunOutcome::Ok
    } else {
        RunOutcome::Fail
    }
}

/// The exit code to record: the code, `128 + n` for a signal, nothing when unknown.
pub fn exit_code_of(exit: ExitStatus) -> Option<i32> {
    match exit {
        ExitStatus::Code(c) => Some(c),
        ExitStatus::Signal(s) => Some(128 + s),
        ExitStatus::Unknown => None,
    }
}

/// Wrappers that only set up the command that follows them.
const PRELUDES: [&str; 6] = ["cd", "export", "source", ".", "set", "pushd"];

/// The command family of shell text: the executable of a simple command or, for a compound one,
/// of its first segment (split on `&&`, `||`, `;`, `|`) that is not a `cd`/`export`-style prelude
/// or an assignment. Used for grouping only, never for dispatch.
pub fn shell_family(command: &str) -> String {
    if let Some(argv) = parse_simple_command(command)
        && let Some(exe) = argv.first()
    {
        return basename(exe).to_string();
    }
    let segments = command.split(['&', '|', ';', '\n']);
    for segment in segments {
        let mut words = segment.split_whitespace().skip_while(|w| is_assignment(w));
        if let Some(word) = words.next()
            && !PRELUDES.contains(&word)
        {
            return basename(word.trim_matches(['(', ')', '"', '\''])).to_string();
        }
    }
    String::new()
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    })
}

/// Scrub the whole text first, then cut: cutting first could leave the head of a secret that the
/// cut made too short to be detected. Scrubbing a prefix of scrubbed text changes nothing; it only
/// gives the type.
#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::memory::MemoryDb;

    #[test]
    fn families_come_from_the_command_not_its_prelude() {
        assert_eq!(shell_family("npm run build"), "npm");
        assert_eq!(shell_family("/usr/bin/git status"), "git");
        assert_eq!(shell_family("cd app && npm run build"), "npm");
        assert_eq!(shell_family("FOO=1 cargo test | tail -3"), "cargo");
        assert_eq!(shell_family("export X=1; (pytest -q)"), "pytest");
        assert_eq!(shell_family(""), "");
    }

    #[test]
    fn exits_map_to_outcomes_and_codes() {
        assert_eq!(outcome_of(ExitStatus::Code(0)), RunOutcome::Ok);
        assert_eq!(outcome_of(ExitStatus::Code(1)), RunOutcome::Fail);
        assert_eq!(outcome_of(ExitStatus::Unknown), RunOutcome::Fail);
        assert_eq!(exit_code_of(ExitStatus::Signal(9)), Some(137));
        assert_eq!(exit_code_of(ExitStatus::Unknown), None);
    }

    #[test]
    fn recorded_titles_are_scrubbed_and_bounded() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::new(
            dir.path().join("home"),
            dir.path().join("config"),
            dir.path().join("project"),
            "/x/agentdiet".into(),
        );
        let key = ["AK", "IA"].concat() + &"Q7".repeat(8);
        let title = format!("aws s3 ls --key {key} {}", "x".repeat(400));
        record(
            &paths,
            &Run {
                origin: Origin::Shim,
                kind: RunKind::Shell,
                family: "aws",
                title: &title,
                outcome: RunOutcome::Fail,
                exit_code: Some(2),
                raw_tokens: 10,
                emitted_tokens: 10,
                cas_sha: None,
                session: session_label(&Key::AgentPid(42)).as_ref(),
            },
        )
        .unwrap();
        let rows = MemoryDb::open(&paths.memory_db())
            .unwrap()
            .runs(10)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].title.contains(&key) && rows[0].title.contains("[REDACTED:"));
        assert!(rows[0].title.len() <= MAX_TITLE_BYTES);

        // A secret straddling the cut is redacted, not cut to an undetectable head.
        let straddling = format!("{} {key}", "y".repeat(MAX_TITLE_BYTES - 10));
        assert!(
            !scrub_bounded(&straddling, MAX_TITLE_BYTES)
                .to_lossy_string()
                .contains(&key[..10])
        );
        assert_eq!(rows[0].session.as_deref(), Some("pid:42"));
        assert_eq!(
            (rows[0].outcome.as_str(), rows[0].exit_code),
            ("fail", Some(2))
        );
    }
}
