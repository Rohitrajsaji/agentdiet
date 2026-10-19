//! The stdio MCP server, `agentdiet mcp` (newline-delimited JSON-RPC 2.0, SPEC §3b "MCP server").
//!
//! Three tools: `diet_inspect` (a Vault entry, or a line slice of it), `diet_search` (BM25 over the
//! project's memory.db) and `diet_exec` (run a command once and diet its output). The server is
//! synchronous: one request at a time, no async runtime. Stdout carries JSON-RPC lines only; every
//! diagnostic goes to stderr.

use std::io::{self, BufRead, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Map, Value, json};

use crate::cas::{Cas, LineRange};
use crate::compress::parse_simple_command;
use crate::diet::{
    Bypass, Engine, ExitStatus, Input, PANIC_NOTE, Settings, Source, estimate_tokens,
};
use crate::memory::{MemoryDb, Origin, RunKind};
use crate::runlog;
use crate::session;
use crate::shim::{self, NESTED_ENV};
use crate::state::{self, Status};
use crate::{Paths, basename};

const DEFAULT_PROTOCOL: &str = "2024-11-05";

/// `diet_inspect` returns at most this many bytes, cut at a line boundary.
pub const MAX_INSPECT_BYTES: usize = 256 * 1024;
/// `diet_search` result count: default and upper bound.
pub const DEFAULT_SEARCH_LIMIT: usize = 10;
pub const MAX_SEARCH_LIMIT: usize = 50;
/// `diet_exec` kills the command's process group after this long.
pub const DEFAULT_EXEC_TIMEOUT: Duration = Duration::from_secs(120);
/// Overrides [`DEFAULT_EXEC_TIMEOUT`], in milliseconds (for tests).
pub const EXEC_TIMEOUT_ENV: &str = "AGENTDIET_MCP_EXEC_TIMEOUT_MS";
/// `diet_exec` keeps at most this much of each stream; the rest is read and discarded.
pub const MAX_CAPTURE_BYTES: usize = 16 * 1024 * 1024;
/// After the command exits, how long its pipes may stay open (a background grandchild holding
/// them) before the captured bytes are taken as they are.
const PIPE_GRACE: Duration = Duration::from_secs(2);

/// Server settings that come from the environment; built by `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub exec_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            exec_timeout: DEFAULT_EXEC_TIMEOUT,
        }
    }
}

impl Options {
    /// Defaults, with [`EXEC_TIMEOUT_ENV`] applied when it holds a number.
    pub fn from_env() -> Self {
        let exec_timeout = std::env::var(EXEC_TIMEOUT_ENV)
            .ok()
            .and_then(|ms| ms.trim().parse().ok())
            .map_or(DEFAULT_EXEC_TIMEOUT, Duration::from_millis);
        Self { exec_timeout }
    }
}

/// Serve requests from `reader` until EOF. `paths.project_root` is the project whose memory.db
/// the tools search and index.
pub fn serve(
    reader: impl BufRead,
    mut writer: impl Write,
    paths: &Paths,
    options: Options,
) -> io::Result<()> {
    let mut server = Server {
        paths,
        options,
        last_exec_sig: None,
    };
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = server.handle_line(&line) {
            writeln!(writer, "{response}")?;
            writer.flush()?;
        }
    }
    Ok(())
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A tool result: text blocks plus `isError`.
fn tool_result(texts: Vec<String>, is_error: bool) -> Value {
    let content: Vec<Value> = texts
        .into_iter()
        .map(|text| json!({ "type": "text", "text": text }))
        .collect();
    json!({ "content": content, "isError": is_error })
}

fn ok_text(text: impl Into<String>) -> Value {
    tool_result(vec![text.into()], false)
}

fn err_text(text: impl Into<String>) -> Value {
    tool_result(vec![text.into()], true)
}

/// The `tools/list` entries.
fn tool_list() -> Value {
    json!([
        {
            "name": "diet_inspect",
            "description": "Fetch the full (secret-scrubbed) output that AgentDiet trimmed, by the ID \
                            in its breadcrumb (e.g. #4f9a1b2c), optionally only lines start_line..end_line \
                            (1-based, inclusive).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Vault ID, e.g. #4f9a1b2c or 4f9a" },
                    "start_line": { "type": "integer", "minimum": 1 },
                    "end_line": { "type": "integer", "minimum": 1 }
                },
                "required": ["id"]
            }
        },
        {
            "name": "diet_search",
            "description": "Search this project's AgentDiet memory (summaries of trimmed commands and \
                            reads), best match first. Each hit carries the #id to pass to diet_inspect.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_SEARCH_LIMIT, "default": DEFAULT_SEARCH_LIMIT }
                },
                "required": ["query"]
            }
        },
        {
            "name": "diet_exec",
            "description": "Run one command (an argv array, or a single simple command without pipes, \
                            redirections or &&) and return its output trimmed by AgentDiet, with a \
                            breadcrumb to recover the full output. No shell is started.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": {
                        "oneOf": [
                            { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                            { "type": "string" }
                        ]
                    },
                    "cwd": { "type": "string", "description": "Working directory (default: the project)" }
                },
                "required": ["command"]
            }
        },
        {
            "name": "diet_reflect",
            "description": "Mine this project's AgentDiet run log for repo-specific lessons (commands \
                            that failed and what worked next, outputs that keep costing over 1,000 \
                            tokens, searches that found nothing) and write them to EXPERIENCE.md at the \
                            project root (at most 2 KB, no LLM). Returns the file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "dry_run": { "type": "boolean", "default": false, "description": "Return the text without writing the file" }
                }
            }
        }
    ])
}

struct Server<'a> {
    paths: &'a Paths,
    options: Options,
    /// Signature of the previous `diet_exec` (argv + cwd), for retry detection (SPEC §2.4).
    last_exec_sig: Option<String>,
}

impl Server<'_> {
    fn handle_line(&mut self, line: &str) -> Option<Value> {
        let Ok(request) = serde_json::from_str::<Value>(line) else {
            return Some(error(Value::Null, -32700, "parse error"));
        };
        let Some(object) = request.as_object() else {
            return Some(error(Value::Null, -32600, "invalid request"));
        };
        let id = object.get("id").cloned();
        let method = object.get("method").and_then(Value::as_str).unwrap_or("");

        // Notifications (no id) never get a response.
        let id = id?;
        let params = object.get("params");

        let result = match method {
            "initialize" => {
                let version = params
                    .and_then(|p| p.get("protocolVersion"))
                    .and_then(Value::as_str)
                    .unwrap_or(DEFAULT_PROTOCOL);
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "agentdiet", "version": env!("CARGO_PKG_VERSION") },
                })
            }
            "tools/list" => json!({ "tools": tool_list() }),
            "tools/call" => {
                let name = params.and_then(|p| p.get("name")).and_then(Value::as_str);
                let empty = Map::new();
                let args = params
                    .and_then(|p| p.get("arguments"))
                    .and_then(Value::as_object)
                    .unwrap_or(&empty);
                let Some(name) = name.filter(|n| tool_names().contains(n)) else {
                    let message = match name {
                        Some(n) => format!("unknown tool: {n}"),
                        None => "tools/call needs a tool name".to_string(),
                    };
                    return Some(error(id, -32602, &message));
                };
                catch_unwind(AssertUnwindSafe(|| self.call(name, args)))
                    .unwrap_or_else(|_| err_text(PANIC_NOTE))
            }
            "ping" => json!({}),
            _ => return Some(error(id, -32601, "method not found")),
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    fn call(&mut self, name: &str, args: &Map<String, Value>) -> Value {
        match name {
            "diet_inspect" => inspect(self.paths, args, SystemTime::now()),
            "diet_search" => search(self.paths, args, SystemTime::now()),
            "diet_reflect" => reflect_tool(self.paths, args, SystemTime::now()),
            _ => self.exec(args),
        }
    }

    fn exec(&mut self, args: &Map<String, Value>) -> Value {
        let argv = match args.get("command") {
            Some(Value::Array(items)) => {
                let argv: Option<Vec<String>> = items
                    .iter()
                    .map(|v| v.as_str().map(str::to_string))
                    .collect();
                match argv {
                    Some(argv) if !argv.is_empty() && !argv[0].is_empty() => argv,
                    _ => return err_text("`command` must be a non-empty array of strings"),
                }
            }
            Some(Value::String(text)) => match parse_simple_command(text) {
                Some(argv) if !argv.is_empty() => argv,
                _ => {
                    return err_text(
                        "`command` is not a single simple command (pipes, redirections, `&&`, \
                         `;`, substitutions and expansions are not run); pass an argv array instead",
                    );
                }
            },
            _ => {
                return err_text("`command` is required: an argv array or a simple command string");
            }
        };
        let base = &self.paths.project_root;
        let cwd = match args.get("cwd") {
            None | Some(Value::Null) => base.clone(),
            Some(Value::String(dir)) => base.join(dir),
            Some(_) => return err_text("`cwd` must be a string"),
        };
        if !cwd.is_dir() {
            return err_text(format!("`cwd` is not a directory: {}", cwd.display()));
        }

        let sig = session::signature(&[
            b"mcp",
            cwd.as_os_str().as_encoded_bytes(),
            argv.join("\0").as_bytes(),
        ]);
        let repeated = self.last_exec_sig.as_deref() == Some(sig.as_str());
        self.last_exec_sig = Some(sig);

        let run = match run_command(&argv, &cwd, self.options.exec_timeout) {
            Ok(run) => run,
            Err(e) => {
                let note = crate::diet::note(&format!("could not run `{}`: {e}", argv[0]));
                return err_text(note);
            }
        };

        // The command has run; from here on nothing may run it again (P2).
        let state = state::load(self.paths);
        let settings = Settings {
            mode: state.mode,
            paused: state.status == Status::Paused,
            bypass: if repeated {
                Bypass::RepeatedCommand
            } else {
                Bypass::None
            },
            // The agent asked for a dieted run: long output of any command may be cut.
            head_tail: true,
        };
        let exit = run.exit.unwrap_or(ExitStatus::Unknown);
        let family = argv
            .first()
            .map(|a| basename(a).to_string())
            .unwrap_or_default();
        let title = argv.join(" ");
        let source = Source::Shell { argv, exit };
        let engine = Engine::new(self.paths);
        let mut sha = None;
        let mut emitted_tokens = 0;
        let mut diet = |bytes: &[u8]| {
            let outcome = engine.run(
                &Input {
                    bytes,
                    source: &source,
                },
                &settings,
            );
            if let Some(note) = outcome.note {
                eprintln!("{note}");
            }
            sha = sha.take().or(outcome.sha);
            emitted_tokens += estimate_tokens(&outcome.output);
            String::from_utf8_lossy(&outcome.output).into_owned()
        };
        let out = diet(&run.stdout.bytes);
        let err = diet(&run.stderr.bytes);

        // Logged whether or not it was trimmed (SPEC §3 Tier 3, "Run log"); best effort.
        let logged = runlog::record(
            self.paths,
            &runlog::Run {
                origin: Origin::Mcp,
                kind: RunKind::Shell,
                family: &family,
                title: &title,
                outcome: runlog::outcome_of(exit),
                exit_code: runlog::exit_code_of(exit),
                raw_tokens: (estimate_tokens(&run.stdout.bytes)
                    + estimate_tokens(&run.stderr.bytes)) as u64,
                emitted_tokens: emitted_tokens as u64,
                cas_sha: sha.as_deref(),
                session: None,
            },
        );
        if let Err(note) = logged {
            eprintln!("{note}");
        }

        let mut text = out;
        let push_line = |text: &mut String, line: &str| {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(line);
            text.push('\n');
        };
        if !err.is_empty() {
            push_line(&mut text, "--- stderr ---");
            text.push_str(&err);
        }
        for (name, stream) in [("stdout", &run.stdout), ("stderr", &run.stderr)] {
            if stream.truncated {
                push_line(
                    &mut text,
                    &format!(
                        "[{name} truncated: only the first {} MiB were kept]",
                        MAX_CAPTURE_BYTES / (1024 * 1024)
                    ),
                );
            }
        }
        match run.exit {
            Some(exit) => {
                push_line(&mut text, &format!("[{}]", exit.describe()));
                ok_text(text)
            }
            None => {
                let secs = self.options.exec_timeout.as_secs_f64();
                push_line(&mut text, &format!("[timed out after {secs}s; killed]"));
                err_text(text)
            }
        }
    }
}

fn tool_names() -> [&'static str; 4] {
    ["diet_inspect", "diet_search", "diet_exec", "diet_reflect"]
}

fn reflect_tool(paths: &Paths, args: &Map<String, Value>, now: SystemTime) -> Value {
    let dry_run = match args.get("dry_run") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return err_text("`dry_run` must be a boolean"),
    };
    match crate::reflect::reflect(paths, dry_run, now) {
        Ok(report) => tool_result(vec![report.text.clone(), report.summary()], false),
        Err(e) => err_text(crate::diet::note(&format!("reflect failed: {e}"))),
    }
}

/// An optional positive integer argument; `Err` if present but not one.
fn line_arg(args: &Map<String, Value>, name: &str) -> Result<Option<usize>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .filter(|n| *n >= 1)
            .and_then(|n| usize::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| format!("`{name}` must be an integer of at least 1")),
    }
}

fn inspect(paths: &Paths, args: &Map<String, Value>, now: SystemTime) -> Value {
    let Some(id) = args.get("id").and_then(Value::as_str) else {
        return err_text("`id` is required, e.g. \"#4f9a1b2c\"");
    };
    let (start, end) = match (line_arg(args, "start_line"), line_arg(args, "end_line")) {
        (Ok(s), Ok(e)) => (s, e),
        (Err(e), _) | (_, Err(e)) => return err_text(e),
    };
    let range = match (start, end) {
        (None, None) => None,
        (s, e) => match LineRange::new(s.unwrap_or(1), e.unwrap_or(usize::MAX)) {
            Ok(range) => Some(range),
            Err(e) => return err_text(e.to_string()),
        },
    };
    let bytes = match Cas::new(paths.cas_dir()).read(id, range, now) {
        Ok(bytes) => bytes,
        Err(e) => return err_text(e.to_string()),
    };
    // Best effort, like all telemetry (SPEC §2.3).
    let _ = state::record_dereference(paths);

    let text = String::from_utf8_lossy(&bytes);
    if text.len() <= MAX_INSPECT_BYTES {
        return ok_text(text);
    }
    let mut cut = MAX_INSPECT_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    if let Some(nl) = text[..cut].rfind('\n') {
        cut = nl + 1;
    }
    let (shown, rest) = text.split_at(cut);
    let first = start.unwrap_or(1);
    let next = first + shown.matches('\n').count();
    let left = rest.split_inclusive('\n').count();
    let note = format!(
        "[agentdiet: {left} more line(s) not shown ({} KiB limit); ask for them with \
         start_line={next}]",
        MAX_INSPECT_BYTES / 1024
    );
    tool_result(vec![shown.to_string(), note], false)
}

fn search(paths: &Paths, args: &Map<String, Value>, now: SystemTime) -> Value {
    let Some(query) = args.get("query").and_then(Value::as_str) else {
        return err_text("`query` is required");
    };
    let limit = match args.get("limit") {
        None | Some(Value::Null) => DEFAULT_SEARCH_LIMIT,
        Some(v) => match v.as_u64() {
            Some(n) => usize::try_from(n)
                .unwrap_or(MAX_SEARCH_LIMIT)
                .clamp(1, MAX_SEARCH_LIMIT),
            None => return err_text("`limit` must be an integer"),
        },
    };
    let db_path = paths.memory_db();
    // memory.db is never created just to be read.
    if !db_path.is_file() {
        return ok_text("No indexed entries yet.");
    }
    let hits = match MemoryDb::open(&db_path).and_then(|db| db.search(query, limit)) {
        Ok(hits) => hits,
        Err(e) => return err_text(format!("memory.db could not be searched: {e}")),
    };
    if hits.is_empty() {
        return ok_text("No matches.");
    }
    let shas: Vec<&str> = hits.iter().filter_map(|h| h.cas_sha.as_deref()).collect();
    let ids = Cas::new(paths.cas_dir()).live_ids(&shas, now);
    let collapse = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lines: Vec<String> = hits
        .iter()
        .map(|hit| {
            let pointer = match hit.cas_sha.as_deref() {
                Some(sha) => match ids.get(sha) {
                    Some(id) => format!("#{id} "),
                    None => "(expired) ".to_string(),
                },
                None => String::new(),
            };
            format!(
                "{pointer}{}: {} — {}",
                hit.kind,
                collapse(&hit.title),
                collapse(&hit.snippet)
            )
        })
        .collect();
    ok_text(lines.join("\n"))
}

// ---- running a command ------------------------------------------------------------------------

/// One captured stream.
#[derive(Debug, Default)]
struct Stream {
    bytes: Vec<u8>,
    /// More than [`MAX_CAPTURE_BYTES`] arrived; only the first ones were kept.
    truncated: bool,
}

#[derive(Debug)]
struct Run {
    stdout: Stream,
    stderr: Stream,
    /// `None` when the command timed out and was killed.
    exit: Option<ExitStatus>,
}

/// A pipe read on its own thread into a shared buffer, so the bytes that arrived can be taken
/// even if the pipe never reaches EOF (a background process kept it open).
struct Capture {
    stream: Arc<Mutex<Stream>>,
    done: mpsc::Receiver<()>,
}

impl Capture {
    fn start(pipe: Option<impl Read + Send + 'static>) -> Self {
        let stream = Arc::new(Mutex::new(Stream::default()));
        let (tx, done) = mpsc::channel();
        let shared = Arc::clone(&stream);
        std::thread::spawn(move || {
            if let Some(mut pipe) = pipe {
                let mut buf = [0u8; 64 * 1024];
                loop {
                    let n = match pipe.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                    let room = MAX_CAPTURE_BYTES.saturating_sub(s.bytes.len());
                    if n > room {
                        s.truncated = true;
                    }
                    s.bytes.extend_from_slice(&buf[..n.min(room)]);
                }
            }
            let _ = tx.send(());
        });
        Self { stream, done }
    }

    /// The captured bytes, waiting for EOF until `deadline` at most.
    fn finish(self, deadline: Instant) -> Stream {
        let _ = self
            .done
            .recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let mut s = self.stream.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *s)
    }
}

/// Run `argv` once in `cwd`, in its own process group, with stdin `/dev/null` and both streams
/// captured. After `timeout` the whole group is killed and `exit` is `None`.
fn run_command(argv: &[String], cwd: &Path, timeout: Duration) -> io::Result<Run> {
    let mut command = Command::new(PathBuf::from(&argv[0]));
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .env(NESTED_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let out = Capture::start(child.stdout.take());
    let err = Capture::start(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => break None,
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => {
                kill_group(&mut child);
                return Err(e);
            }
        }
    };
    if status.is_none() {
        kill_group(&mut child);
    }
    let grace = Instant::now() + PIPE_GRACE;
    Ok(Run {
        stdout: out.finish(grace),
        stderr: err.finish(grace),
        exit: status.as_ref().map(shim::exit_status),
    })
}

/// SIGKILL the child's whole process group (it leads one), then reap the child.
fn kill_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Ok(pgid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: `killpg` takes plain integers and has no memory-safety preconditions. The group
        // id is our own child's pid, which leads its own group (`process_group(0)`) and has not
        // been reaped yet, so the id cannot have been reused.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn paths(dir: &TempDir) -> Paths {
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        Paths::new(
            dir.path().join("home"),
            dir.path().join("config"),
            project,
            "/x/agentdiet".into(),
        )
    }

    fn exchange(paths: &Paths, input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        serve(input.as_bytes(), &mut out, paths, Options::default()).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn handshake_and_tool_list() {
        let dir = TempDir::new().unwrap();
        let replies = exchange(
            &paths(&dir),
            concat!(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                "\n",
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
                "\n",
                r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
                "\n",
            ),
        );
        assert_eq!(replies.len(), 3, "the notification gets no reply");
        assert_eq!(replies[0]["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "agentdiet");
        let names: Vec<&str> = replies[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, tool_names());
        assert_eq!(replies[2]["result"], json!({}));
    }

    #[test]
    fn unknown_methods_tools_and_garbage_are_errors() {
        let dir = TempDir::new().unwrap();
        let replies = exchange(
            &paths(&dir),
            concat!(
                "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"nope\"}\nnot json\n",
                r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"rm_rf"}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{}}"#,
                "\n",
            ),
        );
        assert_eq!(replies[0]["error"]["code"], -32601);
        assert_eq!(replies[0]["id"], 9);
        assert_eq!(replies[1]["error"]["code"], -32700);
        assert_eq!(replies[2]["error"]["code"], -32602);
        assert_eq!(replies[3]["error"]["code"], -32602);
    }

    #[test]
    fn inspect_caps_large_entries_at_a_line_boundary() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        let now = SystemTime::now();
        let body: String = (1..=40_000)
            .map(|i| format!("line number {i:06}\n"))
            .collect();
        let put = Cas::new(p.cas_dir())
            .put(&crate::scrubber::scrub(body.as_bytes()), now)
            .unwrap();
        let args = json!({ "id": put.short_id });
        let result = inspect(&p, args.as_object().unwrap(), now);
        assert_eq!(result["isError"], false);
        let shown = result["content"][0]["text"].as_str().unwrap();
        assert!(shown.len() <= MAX_INSPECT_BYTES && shown.ends_with('\n'));
        let lines = shown.lines().count();
        let note = result["content"][1]["text"].as_str().unwrap();
        assert!(
            note.contains(&format!("{} more line(s)", 40_000 - lines)),
            "{note}"
        );
        assert!(
            note.contains(&format!("start_line={}", lines + 1)),
            "{note}"
        );
    }

    #[test]
    fn inspect_argument_errors() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        let now = SystemTime::now();
        for args in [
            json!({}),
            json!({ "id": "#4f9a", "start_line": 0 }),
            json!({ "id": "#4f9a", "end_line": "3" }),
            json!({ "id": "#4f9a", "start_line": 5, "end_line": 2 }),
            json!({ "id": "zz" }),
            json!({ "id": "#4f9a" }),
        ] {
            let result = inspect(&p, args.as_object().unwrap(), now);
            assert_eq!(result["isError"], true, "{args}");
        }
        assert_eq!(state::load(&p).cas_dereferences, 0);
    }
}
