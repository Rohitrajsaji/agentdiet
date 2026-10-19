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
    estimate_tokens,
};
use crate::fsutil;
use crate::memory::{MemoryDb, Origin, RunKind, RunOutcome};
use crate::runlog;
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
/// Prompt tokens that bypass trimming for the next tool call (SPEC §2.4). Lowercase; matched in
/// any ASCII case.
pub const FULL_DIRECTIVES: [&str; 3] = ["@diet:full", "/diet-full", "--diet-full"];
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
    /// A tool call that failed. Record-only: it never changes what the agent sees (SPEC §3 Tier 1).
    PostToolUseFailure,
    PreCompact,
}

impl Event {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "SessionStart" => Event::SessionStart,
            "UserPromptSubmit" => Event::UserPromptSubmit,
            "PreToolUse" => Event::PreToolUse,
            "PostToolUse" => Event::PostToolUse,
            "PostToolUseFailure" => Event::PostToolUseFailure,
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
        Event::PostToolUseFailure => {
            post_tool_use_failure(&paths, &payload, key.as_ref());
            Response::default()
        }
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

/// `true` if the prompt carries one of [`FULL_DIRECTIVES`] as a whole token, in any ASCII case.
/// Prose never does: "full report", "raw" and "unfiltered" are words, not directives. A token
/// glued to a path, an address or a longer word (`docs/diet-full`, `x@diet:full`,
/// `@diet:fullscreen`) does not count.
pub fn has_full_directive(prompt: &str) -> bool {
    // ASCII lowercasing keeps every byte offset.
    let lower = prompt.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let starts_token = |b: u8| !(is_word_byte(b) || matches!(b, b'/' | b'@' | b':' | b'.'));
    FULL_DIRECTIVES.iter().any(|directive| {
        lower.match_indices(directive).any(|(at, _)| {
            let end = at + directive.len();
            (at == 0 || starts_token(bytes[at - 1]))
                && bytes.get(end).is_none_or(|b| !is_word_byte(*b))
        })
    })
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
    // Every prompt arms or disarms the flag afresh and drops a binding no call ever spent (an
    // interrupted call), so a directive covers at most one tool call.
    let armed = has_full_directive(prompt);
    if let Some(key) = key
        && let Err(e) = session::update(paths, key, |s| {
            s.raw_turn = armed;
            s.raw_tool_use = None;
        })
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
    // Best effort: a sidecar that cannot be updated only means no retry is detected and no
    // directive is bound.
    let _ = session::update(paths, key, |s| {
        if s.last_hook_sig.as_deref() == Some(sig.as_str())
            && let Some(id) = id.clone()
        {
            s.retry_tool_use = Some(id);
        }
        s.last_hook_sig = Some(sig);
        // An armed directive belongs to this call, and to no later one (SPEC §2.4).
        if s.raw_turn
            && let Some(id) = id
        {
            s.raw_tool_use = Some(id);
            s.raw_turn = false;
        }
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

/// Spend a full-output directive bound to this call, if it is. Returns whether it was: the call's
/// output is then bypassed. Every tool call spends its own binding, whether or not it has output
/// the engine would touch (SPEC §2.4).
fn spend_directive(paths: &Paths, key: Option<&Key>, id: Option<&str>) -> bool {
    let (Some(key), Some(id)) = (key, id) else {
        return false;
    };
    if session::load(paths, key).raw_tool_use.as_deref() != Some(id) {
        return false;
    }
    // The output is bypassed even if the sidecar cannot be rewritten: the binding was read, and
    // a stale one is cleared by the next prompt anyway.
    let _ = session::update(paths, key, |s| {
        if s.raw_tool_use.as_deref() == Some(id) {
            s.raw_tool_use = None;
        }
    });
    true
}

fn post_tool_use_failure(paths: &Paths, payload: &Map<String, Value>, key: Option<&Key>) {
    let id = str_field(payload, "tool_use_id").map(|id| fsutil::sha256_hex(id.as_bytes()));
    spend_directive(paths, key, id.as_deref());
    // A user's interrupt says nothing about the command.
    if payload.get("is_interrupt").and_then(Value::as_bool) == Some(true) {
        return;
    }
    let (Some(tool), input) = (str_field(payload, "tool_name"), payload.get("tool_input")) else {
        return;
    };
    let error = str_field(payload, "error").unwrap_or_default();
    let tokens = estimate_tokens(error.as_bytes()) as u64;
    let label = key.and_then(runlog::session_label);
    let (kind, family, title, outcome, exit_code) = match tool {
        "Bash" => {
            let command = input_str(input, "command");
            (
                RunKind::Shell,
                runlog::shell_family(&command),
                command,
                RunOutcome::Fail,
                exit_code_line(error),
            )
        }
        "Read" => (
            RunKind::Read,
            "Read".to_string(),
            input_str(input, "file_path"),
            RunOutcome::Miss,
            None,
        ),
        "Grep" | "Glob" => (
            RunKind::Search,
            tool.to_string(),
            search_title(input),
            RunOutcome::Miss,
            None,
        ),
        _ => return,
    };
    // Best effort, like all telemetry; a failure's own output is never stored.
    let _ = runlog::record(
        paths,
        &runlog::Run {
            origin: Origin::Hook,
            kind,
            family: &family,
            title: &title,
            outcome,
            exit_code,
            raw_tokens: tokens,
            emitted_tokens: tokens,
            cas_sha: None,
            session: label.as_ref(),
        },
    );
}

/// `N` from an error whose first line is `Exit code N` (PostToolUseFailure for a Bash command).
fn exit_code_line(error: &str) -> Option<i32> {
    error
        .lines()
        .next()?
        .trim()
        .strip_prefix("Exit code ")?
        .trim()
        .parse()
        .ok()
}

fn input_str(input: Option<&Value>, name: &str) -> String {
    input
        .and_then(|i| i.get(name))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `pattern`, or `pattern in path` when the search was scoped.
fn search_title(input: Option<&Value>) -> String {
    let pattern = input_str(input, "pattern");
    match input_str(input, "path") {
        path if path.is_empty() => pattern,
        path => format!("{pattern} in {path}"),
    }
}

/// Whether a Grep/Glob result found anything, from the count fields it carries (`numFiles`,
/// `filenames`, `numLines`, `numMatches`, `content`). `None` when it carries none of them: an
/// unrecognised shape is not recorded rather than guessed at.
fn search_found(response: &Value) -> Option<bool> {
    let mut seen = false;
    let mut found = false;
    for name in ["numFiles", "numLines", "numMatches"] {
        if let Some(n) = response.get(name).and_then(Value::as_u64) {
            seen = true;
            found |= n > 0;
        }
    }
    if let Some(files) = response.get("filenames").and_then(Value::as_array) {
        seen = true;
        found |= !files.is_empty();
    }
    if let Some(content) = response.get("content").and_then(Value::as_str) {
        seen = true;
        found |= !content.trim().is_empty();
    }
    seen.then_some(found)
}

fn post_tool_use(paths: &Paths, payload: &Map<String, Value>, key: Option<&Key>) -> Response {
    let id = str_field(payload, "tool_use_id").map(|id| fsutil::sha256_hex(id.as_bytes()));
    let full = spend_directive(paths, key, id.as_deref());
    let (Some(tool), Some(response)) = (
        str_field(payload, "tool_name"),
        payload.get("tool_response"),
    ) else {
        return Response::default();
    };
    let input = payload.get("tool_input");
    let label = key.and_then(runlog::session_label);
    if matches!(tool, "Grep" | "Glob") {
        if let Some(found) = search_found(response) {
            let _ = runlog::record(
                paths,
                &runlog::Run {
                    origin: Origin::Hook,
                    kind: RunKind::Search,
                    family: tool,
                    title: &search_title(input),
                    outcome: if found {
                        RunOutcome::Ok
                    } else {
                        RunOutcome::Miss
                    },
                    exit_code: None,
                    raw_tokens: estimate_tokens(response.to_string().as_bytes()) as u64,
                    emitted_tokens: estimate_tokens(response.to_string().as_bytes()) as u64,
                    cas_sha: None,
                    session: label.as_ref(),
                },
            );
        }
        return Response::default();
    }
    let fields = text_fields(tool, response, input);
    if fields.is_empty() {
        return Response::default();
    }

    let state = state::load(paths);
    let sidecar = key.map(|k| session::load(paths, k)).unwrap_or_default();
    let bypass = if id.is_some() && sidecar.retry_tool_use == id {
        Bypass::RepeatedCommand
    } else if full {
        Bypass::RawNextTurn
    } else {
        Bypass::None
    };
    let settings = Settings {
        mode: state.mode,
        paused: state.status == Status::Paused,
        bypass,
        head_tail: false,
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
    let (mut raw_tokens, mut emitted_tokens, mut sha) = (0usize, 0usize, None);
    let mut dieted_by_shim = false;
    for (pointer, source) in fields {
        let Some(text) = updated.pointer(&pointer).and_then(Value::as_str) else {
            continue;
        };
        if matches!(source, Source::Shell { .. }) && has_breadcrumb(text) {
            dieted_by_shim = true;
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
        raw_tokens += estimate_tokens(text.as_bytes());
        emitted_tokens += estimate_tokens(&outcome.output);
        sha = sha.or(outcome.sha);
        if outcome.output != text.as_bytes()
            && let Some(slot) = updated.pointer_mut(&pointer)
        {
            *slot = Value::String(String::from_utf8_lossy(&outcome.output).into_owned());
            changed = true;
        }
    }

    // The run log (SPEC §3 Tier 3, "Run log"): every successful Bash call (one a shim dieted was
    // logged by the shim), and Reads big enough to matter.
    let logged = match tool {
        "Bash" if !dieted_by_shim => {
            let command = input_str(input, "command");
            Some((RunKind::Shell, runlog::shell_family(&command), command))
        }
        "Read" if raw_tokens as u64 >= runlog::BLOAT_TOKENS => {
            let path = response
                .pointer("/file/filePath")
                .and_then(Value::as_str)
                .map_or_else(|| input_str(input, "file_path"), str::to_string);
            Some((RunKind::Read, "Read".to_string(), path))
        }
        _ => None,
    };
    if let Some((kind, family, title)) = logged
        && let Err(note) = runlog::record(
            paths,
            &runlog::Run {
                origin: Origin::Hook,
                kind,
                family: &family,
                title: &title,
                outcome: RunOutcome::Ok,
                exit_code: (kind == RunKind::Shell).then_some(0),
                raw_tokens: raw_tokens as u64,
                emitted_tokens: emitted_tokens as u64,
                cas_sha: sha.as_deref(),
                session: label.as_ref(),
            },
        )
    {
        notes.push(note);
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
    fn only_explicit_directive_tokens_request_full_output() {
        for yes in [
            "@diet:full",
            "rerun the build @diet:full",
            "rerun it @DIET:FULL please",
            "`@diet:full`",
            "(--diet-full)",
            "cargo test --diet-full",
            "please /diet-full the next one",
            "@diet:full.",
            "line one\n/diet-full\n",
        ] {
            assert!(has_full_directive(yes), "{yes}");
        }
        for no in [
            // The dogfooding prompt that used to switch trimming off.
            "add the full report link to the footer",
            "full",
            "raw",
            "unfiltered",
            "show me the RAW output",
            "(full) log please",
            "build the full-stack app",
            "print the full path",
            "@diet:fullscreen",
            "--diet-fully",
            "---diet-full",
            "x@diet:full",
            "see docs/diet-full for details",
            "user@diet:full",
            "@diet full",
            "@diet pause",
            "",
        ] {
            assert!(!has_full_directive(no), "{no}");
        }
    }

    mod prop {
        use proptest::prelude::*;

        use super::super::has_full_directive;

        /// Words that used to trigger the escape hatch, or look like they might.
        const PROSE_WORDS: &[&str] = &[
            "full",
            "Full",
            "FULL",
            "fully",
            "full-stack",
            "full path",
            "full report link",
            "raw",
            "unfiltered",
            "report",
            "link",
            "(full)",
            "full.",
            "@full",
            "--full",
            "/full",
            // Not a bare `diet`: `(full)` + `/` + `diet` + `-` + `full` would spell a real
            // `/diet-full` token.
            "dietary",
            "@diet",
            "pause",
        ];

        fn prose() -> impl Strategy<Value = String> {
            prop::collection::vec(
                (
                    prop::sample::select(PROSE_WORDS),
                    prop::sample::select(&[" ", "\n", ", ", ". ", ": ", "/", "-"][..]),
                ),
                0..40,
            )
            .prop_map(|parts| parts.into_iter().map(|(w, s)| format!("{w}{s}")).collect())
        }

        /// Arbitrary text that cannot spell any directive (none contains `diet`).
        fn free_text() -> impl Strategy<Value = String> {
            "[a-zA-Z0-9 ,.()'\"!?:/@_\n-]{0,120}".prop_filter("no directive spelling", |s| {
                !s.to_ascii_lowercase().contains("diet")
            })
        }

        proptest! {
            #[test]
            fn prose_never_requests_full_output(text in prose()) {
                prop_assert!(!has_full_directive(&text), "{text:?}");
            }

            #[test]
            fn free_text_never_requests_full_output(text in free_text()) {
                prop_assert!(!has_full_directive(&text), "{text:?}");
            }

            #[test]
            fn a_spaced_directive_anywhere_requests_it(
                before in free_text(),
                after in free_text(),
                directive in prop::sample::select(&["@diet:full", "/diet-full", "--diet-full", "@Diet:Full"][..]),
            ) {
                let text = format!("{before} {directive} {after}");
                prop_assert!(has_full_directive(&text), "{text:?}");
            }
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
