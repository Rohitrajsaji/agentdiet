//! The hook router: `agentdiet hook <event>` (SPEC §3 Tier 1, "Hook router").
//!
//! Claude Code runs this on every hooked event with one JSON object on stdin. The router always
//! fails open: whatever goes wrong (a payload that never arrives, invalid JSON, a missing field, a
//! busy lock, a panic), the hook exits 0 with nothing on stdout and the agent carries on as if it
//! were not installed. Diagnostics go to stderr only, and the payload is never echoed back.

use std::io::{IsTerminal, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};

use crate::Paths;
use crate::cas::Cas;
use crate::compress::parse_simple_command;
use crate::control;
use crate::diet::{
    Bypass, Engine, ExitStatus, Extent, Input, PANIC_NOTE, PAUSED_NOTICE, Settings, Source,
};
use crate::fsutil;
use crate::memory::MemoryDb;
use crate::session::{self, Key};
use crate::state::{self, OnSessionStart, State, Status};

/// The longest a hook waits for its payload.
pub const STDIN_TIMEOUT: Duration = Duration::from_secs(5);
/// A larger payload is not processed (the hook passes everything through).
pub const MAX_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;
/// The post-compaction snapshot never exceeds this many bytes.
pub const SNAPSHOT_MAX_BYTES: usize = 2048;
/// How many memory.db entries the snapshot lists at most.
const SNAPSHOT_ENTRIES: usize = 10;
/// Words in a prompt that bypass trimming for the rest of the turn (SPEC §2.4).
pub const ESCAPE_WORDS: [&str; 3] = ["raw", "full", "unfiltered"];
/// Tool-name prefix of AgentDiet's own MCP tools, which PostToolUse leaves alone.
pub const OWN_MCP_PREFIX: &str = "mcp__agentdiet__";
/// SessionStart's line when `on_session_start` is `auto_resume`.
pub const RESUMED_NOTICE: &str =
    "▶ AgentDiet resumed automatically (on_session_start: auto_resume).";

/// The events the router handles. Anything else (including `noop`) exits at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    PreCompact,
}

impl Event {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "SessionStart" => Event::SessionStart,
            "UserPromptSubmit" => Event::UserPromptSubmit,
            "PreToolUse" => Event::PreToolUse,
            "PostToolUse" => Event::PostToolUse,
            "PreCompact" => Event::PreCompact,
            _ => return None,
        })
    }
}

/// What the hook writes: `stdout` to its output channel, `notes` (bounded, scrubbed) to stderr.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Response {
    pub stdout: String,
    pub notes: Vec<String>,
}

/// The payload from stdin, or `None` (pass through) if stdin is a terminal, the payload does not
/// arrive within `timeout`, or it is larger than `max` bytes.
pub fn read_payload(timeout: Duration, max: usize) -> Option<Vec<u8>> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    read_bounded(std::io::stdin(), timeout, max)
}

/// Read `reader` to its end on a helper thread, waiting at most `timeout`. A reader that never
/// finishes is left behind; the process exits without waiting for it.
pub fn read_bounded(
    reader: impl Read + Send + 'static,
    timeout: Duration,
    max: usize,
) -> Option<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let read = reader.take(limit).read_to_end(&mut buf);
        let _ = tx.send(read.map(|_| buf));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(buf)) if buf.len() <= max => Some(buf),
        _ => None,
    }
}

/// Handle one event. Never panics and never fails: any problem is an empty response, plus a note
/// where one helps.
pub fn handle(event: Event, payload: &[u8], paths: &Paths, now: SystemTime) -> Response {
    catch_unwind(AssertUnwindSafe(|| dispatch(event, payload, paths, now))).unwrap_or_else(|_| {
        Response {
            stdout: String::new(),
            notes: vec![PANIC_NOTE.to_string()],
        }
    })
}

fn dispatch(event: Event, payload: &[u8], paths: &Paths, now: SystemTime) -> Response {
    let Ok(Value::Object(payload)) = serde_json::from_slice::<Value>(payload) else {
        return Response::default();
    };
    let paths = project_paths(paths, &payload);
    let key = str_field(&payload, "session_id")
        .filter(|s| !s.is_empty())
        .map(|s| Key::Session(s.to_string()));
    match event {
        Event::SessionStart => session_start(&paths, &payload, key.as_ref(), now),
        Event::UserPromptSubmit => user_prompt_submit(&paths, &payload, key.as_ref()),
        Event::PreToolUse => {
            pre_tool_use(&paths, &payload, key.as_ref());
            Response::default()
        }
        Event::PostToolUse => post_tool_use(&paths, &payload, key.as_ref()),
        // Claude Code discards PreCompact output; the snapshot comes from SessionStart(compact).
        Event::PreCompact => Response::default(),
    }
}

fn str_field<'a>(payload: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    payload.get(name).and_then(Value::as_str)
}

/// The payload's `cwd` is the project (memory.db lives there); otherwise keep the process's own.
fn project_paths(base: &Paths, payload: &Map<String, Value>) -> Paths {
    let mut paths = base.clone();
    if let Some(cwd) = str_field(payload, "cwd").map(Path::new)
        && cwd.is_absolute()
        && cwd.is_dir()
    {
        paths.project_root = cwd.to_path_buf();
    }
    paths
}

fn note(text: String) -> String {
    crate::diet::note(&text)
}

fn lines_out(lines: Vec<String>, notes: Vec<String>) -> Response {
    let mut stdout = lines.join("\n");
    if !stdout.is_empty() {
        stdout.push('\n');
    }
    Response { stdout, notes }
}

// ---- SessionStart ------------------------------------------------------------------------------

fn session_start(
    paths: &Paths,
    payload: &Map<String, Value>,
    key: Option<&Key>,
    now: SystemTime,
) -> Response {
    let mut lines = Vec::new();
    let mut notes = Vec::new();
    if let Some(key) = key
        && let Err(e) = session::clear(paths, key)
    {
        notes.push(note(format!("agentdiet: could not reset the session: {e}")));
    }
    session::gc(paths, now);

    let state = state::load(paths);
    if state.status == Status::Paused {
        match state.on_session_start {
            OnSessionStart::Prompt => lines.push(PAUSED_NOTICE.to_string()),
            OnSessionStart::AutoResume => match state::set_status(paths, Status::Active) {
                Ok(_) => lines.push(RESUMED_NOTICE.to_string()),
                Err(e) => {
                    notes.push(note(format!("agentdiet: could not resume: {e}")));
                    lines.push(PAUSED_NOTICE.to_string());
                }
            },
        }
    }
    if str_field(payload, "source") == Some("compact")
        && let Some(snapshot) = snapshot(paths, &state, now)
    {
        lines.push(snapshot);
    }
    lines_out(lines, notes)
}

/// One line of already-scrubbed text, whitespace collapsed, at most `max` bytes.
fn one_line(text: &str, max: usize) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if out.len() + word.len() + 1 > max {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Recent state for the post-compaction context: the last trimmed ID and the newest memory.db
/// entries. Every piece of it was scrubbed before it was stored. `None` when there is nothing.
fn snapshot(paths: &Paths, state: &State, now: SystemTime) -> Option<String> {
    let db_path = paths.memory_db();
    let rows = if db_path.is_file() {
        MemoryDb::open(&db_path)
            .and_then(|db| db.recent(SNAPSHOT_ENTRIES))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let last = state
        .last_trimmed_id
        .as_deref()
        .filter(|id| id.len() >= 4 && id.bytes().all(|b| b.is_ascii_hexdigit()));
    if rows.is_empty() && last.is_none() {
        return None;
    }
    let shas: Vec<&str> = rows.iter().filter_map(|r| r.cas_sha.as_deref()).collect();
    let ids = Cas::new(paths.cas_dir()).live_ids(&shas, now);

    let mut text =
        String::from("AgentDiet memory (after compaction; full output: agentdiet show '#<id>'):");
    let mut push = |line: String| {
        if text.len() + 1 + line.len() > SNAPSHOT_MAX_BYTES {
            return false;
        }
        text.push('\n');
        text.push_str(&line);
        true
    };
    if let Some(id) = last {
        push(format!("- last trimmed output: #{id}"));
    }
    for row in &rows {
        let id = row
            .cas_sha
            .as_deref()
            .and_then(|sha| ids.get(sha))
            .map(|id| format!(" #{id}"))
            .unwrap_or_default();
        let line = format!(
            "- {}: {} — {}{id}",
            row.kind,
            one_line(&row.title, 120),
            one_line(&row.body, 160)
        );
        if !push(line) {
            break;
        }
    }
    Some(text)
}

// ---- UserPromptSubmit --------------------------------------------------------------------------

/// A `@diet pause` / `@diet resume` command in a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DietCommand {
    Pause,
    Resume,
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// The words of `text`: maximal runs of ASCII letters, digits, `_` and `-`.
fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii() && is_word_byte(c as u8)))
        .filter(|w| !w.is_empty())
}

/// `true` if the prompt contains `raw`, `full` or `unfiltered` as a whole word, in any case.
/// `fully` and `full-stack` are other words.
pub fn has_escape_word(prompt: &str) -> bool {
    words(prompt).any(|w| ESCAPE_WORDS.iter().any(|e| w.eq_ignore_ascii_case(e)))
}

/// The first `@diet pause` / `@diet resume` in a prompt (case-insensitive, whole words).
pub fn diet_command(prompt: &str) -> Option<DietCommand> {
    let lower = prompt.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut from = 0;
    while let Some(at) = lower[from..].find("@diet") {
        let start = from + at;
        let end = start + "@diet".len();
        from = end;
        let detached_before = start == 0 || !is_word_byte(bytes[start - 1]);
        let rest = &lower[end..];
        let trimmed = rest.trim_start();
        if !detached_before || trimmed.len() == rest.len() {
            continue; // `x@diet` or `@dietary`
        }
        match words(trimmed).next() {
            Some(w) if trimmed.starts_with(w) && w == "pause" => return Some(DietCommand::Pause),
            Some(w) if trimmed.starts_with(w) && w == "resume" => {
                return Some(DietCommand::Resume);
            }
            _ => {}
        }
    }
    None
}

fn user_prompt_submit(paths: &Paths, payload: &Map<String, Value>, key: Option<&Key>) -> Response {
    let prompt = str_field(payload, "prompt").unwrap_or_default();
    let mut lines = Vec::new();
    let mut notes = Vec::new();
    let flipped = match diet_command(prompt) {
        Some(DietCommand::Pause) => Some(control::pause(paths)),
        Some(DietCommand::Resume) => Some(control::resume(paths)),
        None => None,
    };
    match flipped {
        Some(Ok(message)) => lines.push(message),
        Some(Err(e)) => notes.push(note(format!("agentdiet: {e:#}"))),
        None => {}
    }
    // Every prompt sets the flag afresh, so a bypass lasts exactly one turn.
    let raw = has_escape_word(prompt);
    if let Some(key) = key
        && let Err(e) = session::update(paths, key, |s| s.raw_turn = raw)
    {
        notes.push(note(format!("agentdiet: {e}")));
    }
    lines_out(lines, notes)
}

// ---- PreToolUse --------------------------------------------------------------------------------

/// The retry signature of one tool call (SPEC §2.4). Hashes only; no text is kept.
pub fn tool_signature(tool: &str, input: Option<&Value>, cwd: &str) -> String {
    if tool == "Bash" {
        let command = input
            .and_then(|i| i.get("command"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let argv = parse_simple_command(command).map(|argv| argv.join("\0"));
        let shape: &[u8] = if argv.is_some() { b"argv" } else { b"text" };
        let body = argv.unwrap_or_else(|| command.to_string());
        return session::signature(&[b"Bash", cwd.as_bytes(), shape, body.as_bytes()]);
    }
    let input = input.map(Value::to_string).unwrap_or_default();
    session::signature(&[b"tool", tool.as_bytes(), cwd.as_bytes(), input.as_bytes()])
}

fn pre_tool_use(paths: &Paths, payload: &Map<String, Value>, key: Option<&Key>) {
    let (Some(key), Some(tool)) = (key, str_field(payload, "tool_name")) else {
        return;
    };
    let sig = tool_signature(
        tool,
        payload.get("tool_input"),
        str_field(payload, "cwd").unwrap_or_default(),
    );
    let id = str_field(payload, "tool_use_id").map(|id| fsutil::sha256_hex(id.as_bytes()));
    // Best effort: a sidecar that cannot be updated only means no retry is detected.
    let _ = session::update(paths, key, |s| {
        if s.last_hook_sig.as_deref() == Some(sig.as_str())
            && let Some(id) = id
        {
            s.retry_tool_use = Some(id);
        }
        s.last_hook_sig = Some(sig);
    });
}

// ---- PostToolUse -------------------------------------------------------------------------------

/// `true` if `text` already carries an AgentDiet breadcrumb line (a shim dieted it).
fn has_breadcrumb(text: &str) -> bool {
    text.lines()
        .any(|l| l.starts_with("🥗 Diet: ") && l.contains(" Run: agentdiet show '#"))
}

/// The text fields of a tool result that the engine may process, as JSON pointers into
/// `tool_response`, each with its engine source. Unknown tools have none.
fn text_fields(tool: &str, response: &Value, input: Option<&Value>) -> Vec<(String, Source)> {
    let input_str = |name: &str| {
        input
            .and_then(|i| i.get(name))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    match tool {
        "Bash" => {
            let source = Source::Shell {
                argv: parse_simple_command(&input_str("command")).unwrap_or_default(),
                // PostToolUse fires only for a command that succeeded.
                exit: ExitStatus::Code(0),
            };
            vec![
                ("/stdout".into(), source.clone()),
                ("/stderr".into(), source),
            ]
        }
        "Read" => {
            let path = response
                .pointer("/file/filePath")
                .and_then(Value::as_str)
                .map_or_else(|| input_str("file_path"), str::to_string);
            // Hooked Reads are never folded until editing after a folded Read is specified
            // (SPEC §3 Hook router): `Ranged` makes the engine scrub only.
            let source = Source::Read {
                path,
                extent: Extent::Ranged,
            };
            vec![("/file/content".into(), source)]
        }
        "WebFetch" => vec![("/result".into(), Source::ModelJson)],
        // Our own tools' results come from the engine or the Vault already (SPEC §3 Hook router).
        t if t.starts_with(OWN_MCP_PREFIX) => Vec::new(),
        t if t.starts_with("mcp__") => {
            let blocks = |prefix: &str, items: &[Value]| -> Vec<(String, Source)> {
                items
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| b.get("type").and_then(Value::as_str) == Some("text"))
                    .map(|(i, _)| (format!("{prefix}/{i}/text"), Source::ModelJson))
                    .collect()
            };
            match response {
                Value::String(_) => vec![(String::new(), Source::ModelJson)],
                Value::Array(items) => blocks("", items),
                Value::Object(obj) => match obj.get("content") {
                    Some(Value::Array(items)) => blocks("/content", items),
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

fn post_tool_use(paths: &Paths, payload: &Map<String, Value>, key: Option<&Key>) -> Response {
    let (Some(tool), Some(response)) = (
        str_field(payload, "tool_name"),
        payload.get("tool_response"),
    ) else {
        return Response::default();
    };
    let fields = text_fields(tool, response, payload.get("tool_input"));
    if fields.is_empty() {
        return Response::default();
    }

    let state = state::load(paths);
    let sidecar = key.map(|k| session::load(paths, k)).unwrap_or_default();
    let id = str_field(payload, "tool_use_id").map(|id| fsutil::sha256_hex(id.as_bytes()));
    let bypass = if id.is_some() && sidecar.retry_tool_use == id {
        Bypass::RepeatedCommand
    } else if sidecar.raw_turn {
        Bypass::RawNextTurn
    } else {
        Bypass::None
    };
    let settings = Settings {
        mode: state.mode,
        paused: state.status == Status::Paused,
        bypass,
    };
    let mut engine = Engine::new(paths);
    if let Some(Key::Session(id)) = key
        && let Some(label) = session::label(id)
    {
        engine = engine.with_session(label);
    }

    let mut updated = response.clone();
    let mut changed = false;
    let mut notes = Vec::new();
    for (pointer, source) in fields {
        let Some(text) = updated.pointer(&pointer).and_then(Value::as_str) else {
            continue;
        };
        if matches!(source, Source::Shell { .. }) && has_breadcrumb(text) {
            continue;
        }
        let outcome = engine.run(
            &Input {
                bytes: text.as_bytes(),
                source: &source,
            },
            &settings,
        );
        notes.extend(outcome.note);
        if outcome.output != text.as_bytes()
            && let Some(slot) = updated.pointer_mut(&pointer)
        {
            *slot = Value::String(String::from_utf8_lossy(&outcome.output).into_owned());
            changed = true;
        }
    }
    if !changed {
        return Response {
            stdout: String::new(),
            notes,
        };
    }
    let out = json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedToolOutput": updated,
        }
    });
    Response {
        stdout: format!("{out}\n"),
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_words_are_whole_words_in_any_case() {
        for yes in [
            "raw",
            "show me the RAW output",
            "Full.",
            "(full) log please",
            "give me unfiltered logs",
            "raw/data.json",
            "'raw'",
        ] {
            assert!(has_escape_word(yes), "{yes}");
        }
        for no in [
            "fully done",
            "full-stack app",
            "rawhide",
            "draw it",
            "unfilteredness",
            "raw_bytes",
            "",
        ] {
            assert!(!has_escape_word(no), "{no}");
        }
    }

    #[test]
    fn diet_commands_need_the_whole_words() {
        assert_eq!(diet_command("@diet pause"), Some(DietCommand::Pause));
        assert_eq!(
            diet_command("please @DIET   Resume now"),
            Some(DietCommand::Resume)
        );
        assert_eq!(diet_command("ok\n@diet\tpause."), Some(DietCommand::Pause));
        for no in [
            "@dietpause",
            "@diet pauses",
            "@diet",
            "me@diet pause",
            "@dietary pause",
            "diet pause",
            "@diet please pause",
            "@diet pause-later",
        ] {
            assert_eq!(diet_command(no), None, "{no}");
        }
        // A mention that is not a command does not hide a later one.
        assert_eq!(
            diet_command("@diet x @diet resume"),
            Some(DietCommand::Resume)
        );
    }

    #[test]
    fn bash_signatures_follow_argv_and_cwd() {
        let cmd = |c: &str| json!({ "command": c });
        let a = tool_signature("Bash", Some(&cmd("cargo test")), "/p");
        assert_eq!(a, tool_signature("Bash", Some(&cmd("cargo  'test'")), "/p"));
        assert_ne!(a, tool_signature("Bash", Some(&cmd("cargo test")), "/q"));
        assert_ne!(a, tool_signature("Bash", Some(&cmd("cargo test -q")), "/p"));
        // A compound command is compared as text.
        let c = tool_signature("Bash", Some(&cmd("cd x && cargo test")), "/p");
        assert_eq!(
            c,
            tool_signature("Bash", Some(&cmd("cd x && cargo test")), "/p")
        );
        assert_ne!(
            tool_signature("Read", Some(&json!({"file_path": "/a"})), "/p"),
            tool_signature("Read", Some(&json!({"file_path": "/b"})), "/p")
        );
    }

    #[test]
    fn breadcrumbs_are_recognised_only_as_whole_lines() {
        assert!(has_breadcrumb(
            "ok\n🥗 Diet: kept 2/9 lines Run: agentdiet show '#4f9a1b2c'\n"
        ));
        assert!(!has_breadcrumb("echo 🥗 Diet: Run: agentdiet show '#4f9a'"));
    }

    #[test]
    fn own_mcp_tools_are_left_alone() {
        let response = json!({"content": [{"type": "text", "text": "{\"a\": 1}"}]});
        assert!(text_fields("mcp__agentdiet__diet_inspect", &response, None).is_empty());
        assert_eq!(text_fields("mcp__other__x", &response, None).len(), 1);
    }

    #[test]
    fn mcp_text_blocks_are_found_in_each_result_shape() {
        let pointers = |v: Value| -> Vec<String> {
            text_fields("mcp__x__y", &v, None)
                .into_iter()
                .map(|(p, _)| p)
                .collect()
        };
        assert_eq!(pointers(json!("text")), [""]);
        assert_eq!(
            pointers(
                json!([{"type": "text", "text": "a"}, {"type": "image"}, {"type": "text", "text": "b"}])
            ),
            ["/0/text", "/2/text"]
        );
        assert_eq!(
            pointers(json!({"content": [{"type": "text", "text": "a"}]})),
            ["/content/0/text"]
        );
        assert!(text_fields("Edit", &json!({}), None).is_empty());
    }

    #[test]
    fn bounded_read_gives_up_on_a_reader_that_never_ends() {
        struct Never;
        impl Read for Never {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                std::thread::sleep(Duration::from_secs(60));
                Ok(0)
            }
        }
        let start = std::time::Instant::now();
        assert_eq!(read_bounded(Never, Duration::from_millis(50), 10), None);
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(
            read_bounded(std::io::Cursor::new(b"12345".to_vec()), STDIN_TIMEOUT, 5),
            Some(b"12345".to_vec())
        );
        assert_eq!(
            read_bounded(std::io::Cursor::new(b"123456".to_vec()), STDIN_TIMEOUT, 5),
            None,
            "oversize"
        );
    }

    #[test]
    fn snapshot_lines_stay_on_one_line_and_bounded() {
        assert_eq!(one_line("a\n  b\tc", 100), "a b c");
        assert_eq!(one_line("aaaa bbbb", 6), "aaaa");
    }
}
