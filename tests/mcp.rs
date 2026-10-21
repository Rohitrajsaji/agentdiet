//! End-to-end tests of `agentdiet mcp` (SPEC §3b "MCP server"): the real binary over stdio, with
//! `HOME` and the working directory in a TempDir. Every stdout line must be a JSON-RPC message.

mod common;

use std::io::Write;
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::time::Instant;
use std::time::{Duration, SystemTime};

use agentdiet::cas::{Cas, DEFAULT_TTL};
use agentdiet::memory::{Kind, MemoryDb};
use agentdiet::scrubber::scrub;
use common::Env;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_agentdiet");

/// Send `requests` (one per line) to a fresh server and return its replies, in order. Stdout must
/// hold nothing but JSON-RPC lines.
fn session_with(env: &Env, extra_env: &[(&str, &str)], requests: &[Value]) -> Vec<Value> {
    let mut cmd = Command::new(BIN);
    cmd.arg("mcp")
        .env("HOME", &env.paths.home)
        .env("USERPROFILE", &env.paths.home)
        .env("XDG_CONFIG_HOME", &env.paths.config_dir)
        .env("APPDATA", &env.paths.config_dir)
        .env("USERPROFILE", &env.paths.home)
        .current_dir(&env.paths.project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for r in requests {
            writeln!(stdin, "{r}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap_or_else(|_| panic!("not JSON: {l}"));
            assert_eq!(v["jsonrpc"], "2.0", "{l}");
            v
        })
        .collect()
}

fn session(env: &Env, requests: &[Value]) -> Vec<Value> {
    session_with(env, &[], requests)
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
           "params": {"name": name, "arguments": arguments}})
}

/// The single tool result of a one-call session.
fn call_one(env: &Env, name: &str, arguments: Value) -> Value {
    session(env, &[call(1, name, arguments)]).remove(0)["result"].clone()
}

fn text(result: &Value) -> String {
    result["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            assert_eq!(b["type"], "text");
            b["text"].as_str().unwrap().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn state(env: &Env) -> Value {
    std::fs::read(env.paths.state_file())
        .ok()
        .map_or(Value::Null, |b| serde_json::from_slice(&b).unwrap())
}

fn counter(env: &Env, name: &str) -> u64 {
    state(env)[name].as_u64().unwrap_or(0)
}

#[test]
fn tools_list_advertises_the_four_tools_with_schemas() {
    let env = Env::empty();
    let replies = session(
        &env,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        ],
    );
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["result"]["capabilities"], json!({"tools": {}}));
    let tools = replies[1]["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        ["diet_inspect", "diet_search", "diet_exec", "diet_reflect"]
    );
    let required = |i: usize| tools[i]["inputSchema"]["required"].clone();
    assert_eq!(required(0), json!(["id"]));
    assert_eq!(required(1), json!(["query"]));
    assert_eq!(required(2), json!(["command"]));
    assert_eq!(
        required(3),
        Value::Null,
        "every diet_reflect argument is optional"
    );
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object");
        assert!(!t["description"].as_str().unwrap().is_empty());
    }
}

#[test]
fn unknown_tool_is_invalid_params() {
    let env = Env::empty();
    let reply = session(&env, &[call(7, "diet_nope", json!({}))]).remove(0);
    assert_eq!(reply["id"], 7);
    assert_eq!(reply["error"]["code"], -32602);
}

#[test]
fn diet_inspect_returns_the_entry_or_a_slice_and_counts_dereferences() {
    let env = Env::empty();
    let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    let put = Cas::new(env.paths.cas_dir())
        .put(&scrub(body.as_bytes()), SystemTime::now())
        .unwrap();
    assert_eq!(counter(&env, "cas_dereferences"), 0);

    let full = call_one(
        &env,
        "diet_inspect",
        json!({"id": format!("#{}", put.short_id)}),
    );
    assert_eq!(full["isError"], false);
    assert_eq!(text(&full), body);
    assert_eq!(counter(&env, "cas_dereferences"), 1);

    // A bare 4-hex prefix and a line slice, 1-based and inclusive.
    let short = &put.short_id[..4];
    let slice = call_one(
        &env,
        "diet_inspect",
        json!({"id": short, "start_line": 3, "end_line": 5}),
    );
    assert_eq!(text(&slice), "line 3\nline 4\nline 5\n");
    let tail = call_one(&env, "diet_inspect", json!({"id": short, "start_line": 29}));
    assert_eq!(text(&tail), "line 29\nline 30\n");
    let head = call_one(&env, "diet_inspect", json!({"id": short, "end_line": 1}));
    assert_eq!(text(&head), "line 1\n");
    assert_eq!(counter(&env, "cas_dereferences"), 4);

    // Errors are tool results with isError, and count nothing.
    for (args, says) in [
        (json!({"id": "#beef"}), "no vault entry matches"),
        (
            json!({"id": short, "start_line": 6, "end_line": 2}),
            "invalid line range",
        ),
        (json!({"id": "#zz"}), "invalid ID"),
    ] {
        let r = call_one(&env, "diet_inspect", args.clone());
        assert_eq!(r["isError"], true, "{args}");
        assert!(text(&r).contains(says), "{args}: {}", text(&r));
    }
    assert_eq!(counter(&env, "cas_dereferences"), 4);
}

#[test]
fn diet_inspect_reports_expired_and_ambiguous_ids() {
    let env = Env::empty();
    let cas = Cas::new(env.paths.cas_dir());
    let then = SystemTime::now() - DEFAULT_TTL - Duration::from_secs(60);
    let old = cas.put(&scrub(b"old output\n"), then).unwrap();
    let r = call_one(&env, "diet_inspect", json!({"id": old.short_id}));
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("expired"), "{}", text(&r));

    // Two live entries sharing their first 4 hex: written straight into the vault (a `put` per
    // try would fsync hundreds of times), named by the sha of their bytes as `put` would.
    let mut seen: std::collections::HashMap<String, (String, String)> = Default::default();
    let ((a, a_body), (b, b_body)) = (0..1_000_000u32)
        .find_map(|i| {
            let body = format!("entry {i}\n");
            let sha = agentdiet::fsutil::sha256_hex(body.as_bytes());
            seen.insert(sha[..4].to_string(), (sha.clone(), body.clone()))
                .map(|other| (other, (sha, body)))
        })
        .unwrap();
    for (sha, body) in [(&a, &a_body), (&b, &b_body)] {
        std::fs::write(env.paths.cas_dir().join(format!("{sha}.log")), body).unwrap();
    }
    let r = call_one(&env, "diet_inspect", json!({"id": &a[..4]}));
    assert_eq!(r["isError"], true);
    let t = text(&r);
    assert!(t.contains("ambiguous"), "{t}");
    assert!(t.contains(&a[..8]) && t.contains(&b[..8]), "{t}");
}

#[test]
fn diet_search_ranks_by_bm25_and_points_at_live_entries() {
    let env = Env::empty();
    let r = call_one(&env, "diet_search", json!({"query": "cargo"}));
    assert_eq!(
        (r["isError"].clone(), text(&r)),
        (json!(false), "No indexed entries yet.".into())
    );
    assert!(
        !env.paths.memory_db().exists(),
        "search never creates memory.db"
    );

    let now = SystemTime::now();
    let cas = Cas::new(env.paths.cas_dir());
    let live = cas.put(&scrub(b"full cargo output\n"), now).unwrap();
    let gone = cas
        .put(
            &scrub(b"older output\n"),
            now - DEFAULT_TTL - Duration::from_secs(60),
        )
        .unwrap();
    let db = MemoryDb::open(&env.paths.memory_db()).unwrap();
    let index = |title: &str, body: &str, sha: Option<&str>| {
        db.index(
            Kind::Command,
            &scrub(title.as_bytes()),
            &scrub(body.as_bytes()),
            sha,
            None,
        )
        .unwrap();
    };
    index(
        "cargo test",
        "kept 42/1830 lines; cargo test: 1780 passing lines collapsed",
        Some(&live.sha),
    );
    index(
        "npm install",
        "deprecated warnings grouped; mentions cargo once",
        Some(&gone.sha),
    );
    index("git status", "nothing to commit", None);
    drop(db);

    let r = call_one(&env, "diet_search", json!({"query": "cargo"}));
    assert_eq!(r["isError"], false);
    let lines: Vec<String> = text(&r).lines().map(str::to_string).collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].starts_with(&format!("#{} command: cargo test — ", live.short_id)),
        "title matches rank first: {lines:?}"
    );
    assert!(
        lines[1].starts_with("(expired) command: npm install — "),
        "{lines:?}"
    );

    let r = call_one(&env, "diet_search", json!({"query": "cargo", "limit": 1}));
    assert_eq!(text(&r).lines().count(), 1);
    let r = call_one(&env, "diet_search", json!({"query": "zebra"}));
    assert_eq!(text(&r), "No matches.");
    let r = call_one(&env, "diet_search", json!({}));
    assert_eq!(r["isError"], true);
}

/// A shell-free command whose output the cargo rules compress: many passing tests, one failure.
#[cfg(unix)] // used by the Unix-only diet_exec tests
fn cargo_like_output(env: &Env) -> std::path::PathBuf {
    let mut body = String::from("   Compiling demo v0.1.0\n");
    for i in 0..400 {
        body.push_str(&format!("test tests::case_{i:04} ... ok\n"));
    }
    body.push_str("test tests::broken ... FAILED\n\nfailures:\n    tests::broken\n\ntest result: FAILED. 400 passed; 1 failed\n");
    let file = env.paths.project_root.join("cargo-output.txt");
    std::fs::write(&file, body).unwrap();
    file
}

/// An executable named `cargo` that prints [`cargo_like_output`], warns on stderr and exits 101.
#[cfg(unix)] // used by the Unix-only diet_exec tests
fn fake_cargo(env: &Env) -> std::path::PathBuf {
    let file = cargo_like_output(env);
    let script = env.paths.project_root.join("cargo");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ncat '{}'\necho 'warning: on stderr' >&2\nexit 101\n",
            file.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    script
}

#[cfg(unix)] // runs Unix programs (sh, cat, seq) or Unix-only error paths
#[test]
fn diet_exec_trims_stores_and_reports_the_exit_status() {
    let env = Env::empty();
    let file = env.paths.project_root.join("cargo-output.txt");
    // The engine dispatches on argv[0]'s basename, so this script is diet as `cargo test`.
    let script = fake_cargo(&env);
    let argv = json!([script.display().to_string(), "test"]);
    let r = call_one(&env, "diet_exec", json!({"command": argv}));
    assert_eq!(
        r["isError"], false,
        "a failing command is a result, not an error"
    );
    let t = text(&r);
    assert!(t.contains("test tests::broken ... FAILED"), "{t}");
    assert!(
        !t.contains("case_0007 ... ok"),
        "passing lines collapsed: {t}"
    );
    assert!(t.contains("--- stderr ---\nwarning: on stderr\n"), "{t}");
    assert!(t.trim_end().ends_with("[exit 101]"), "{t}");
    let id = t
        .lines()
        .find_map(|l| l.split("agentdiet show '#").nth(1))
        .and_then(|rest| rest.split('\'').next())
        .expect("a breadcrumb")
        .to_string();

    let s = state(&env);
    assert_eq!(s["last_trimmed_id"], id.as_str());
    assert_eq!(s["trimmed_outputs"], 1);
    assert!(s["tokens_saved"].as_u64().unwrap() > 1000, "{s}");

    // The full output is recoverable, and the run was indexed in the project's memory.db.
    let full = call_one(&env, "diet_inspect", json!({"id": id}));
    assert_eq!(text(&full), std::fs::read_to_string(&file).unwrap());
    let found = call_one(&env, "diet_search", json!({"query": "test"}));
    assert!(
        text(&found).contains(&format!("#{id} command:")),
        "{}",
        text(&found)
    );
}

#[cfg(unix)] // runs Unix programs (sh, cat, seq) or Unix-only error paths
#[test]
fn diet_exec_expands_a_repeated_command_and_never_starts_a_shell() {
    let env = Env::empty();
    let script = fake_cargo(&env);
    let cargo = json!({"command": [script.display().to_string(), "test"]});
    let replies = session(
        &env,
        &[
            call(1, "diet_exec", cargo.clone()),
            call(2, "diet_exec", cargo),
            call(3, "diet_exec", json!({"command": "echo hi && touch pwned"})),
            call(
                4,
                "diet_exec",
                json!({"command": "echo 'hello world'", "cwd": "."}),
            ),
            call(
                5,
                "diet_exec",
                json!({"command": ["true"], "cwd": "no-such-dir"}),
            ),
            call(6, "diet_exec", json!({"command": []})),
        ],
    );
    let r = |i: usize| replies[i]["result"].clone();
    assert!(
        text(&r(0)).contains("🥗 Diet: "),
        "the first run is trimmed"
    );
    let retry = text(&r(1));
    assert!(
        !retry.contains("🥗 Diet: ") && retry.contains("case_0007 ... ok"),
        "{retry}"
    );
    assert_eq!(r(2)["isError"], true);
    assert!(text(&r(2)).contains("argv array"), "{}", text(&r(2)));
    assert!(
        !env.paths.project_root.join("pwned").exists(),
        "no shell ran"
    );
    assert_eq!(text(&r(3)), "hello world\n[exit 0]\n");
    assert_eq!(r(4)["isError"], true);
    assert_eq!(r(5)["isError"], true);
}

#[cfg(unix)] // runs Unix programs (sh, cat, seq) or Unix-only error paths
#[test]
fn diet_exec_kills_the_whole_process_group_on_timeout() {
    let env = Env::empty();
    let marker = env.paths.project_root.join("survived");
    // A grandchild that would outlive its parent and keep the pipes open.
    let script = format!("(sleep 3; touch '{}') & sleep 30", marker.display());
    let started = Instant::now();
    let replies = session_with(
        &env,
        &[("AGENTDIET_MCP_EXEC_TIMEOUT_MS", "300")],
        &[call(
            1,
            "diet_exec",
            json!({"command": ["sh", "-c", script]}),
        )],
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    let r = replies[0]["result"].clone();
    assert_eq!(r["isError"], true);
    assert!(
        text(&r).contains("[timed out after 0.3s; killed]"),
        "{}",
        text(&r)
    );
    std::thread::sleep(Duration::from_secs(4));
    assert!(!marker.exists(), "the grandchild was killed with its group");
}

#[cfg(unix)] // runs Unix programs (sh, cat, seq) or Unix-only error paths
#[test]
fn diet_exec_stdin_is_not_the_json_rpc_stream() {
    let env = Env::empty();
    // `cat` with no file reads stdin: it must see /dev/null, not the next request.
    let replies = session(
        &env,
        &[
            call(1, "diet_exec", json!({"command": ["cat"]})),
            json!({"jsonrpc":"2.0","id":2,"method":"ping"}),
        ],
    );
    assert_eq!(text(&replies[0]["result"]), "[exit 0]\n");
    assert_eq!(replies[1]["id"], 2, "the ping still reached the server");
}

/// An executable named `name` that prints `body` from a file and exits `code`.
#[cfg(unix)] // used by the Unix-only diet_exec tests
fn fake_tool(env: &Env, name: &str, body: &str, code: i32) -> std::path::PathBuf {
    let file = env.paths.project_root.join(format!("{name}-output.txt"));
    std::fs::write(&file, body).unwrap();
    let script = env.paths.project_root.join(name);
    std::fs::write(
        &script,
        format!("#!/bin/sh\ncat '{}'\nexit {code}\n", file.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    script
}

/// About 2,000 lines shaped like `npm ls --all`: a dependency tree, many `deduped` lines, one
/// unmet peer dependency in the middle.
#[cfg(unix)] // used by the Unix-only diet_exec tests
fn npm_ls_tree() -> String {
    let mut out = String::from("germany-masters-2027@0.1.0 /Users/dev/germany-masters-2027\n");
    for top in 0..100 {
        out.push_str(&format!("├─┬ package-{top:03}@{top}.2.3\n"));
        for dep in 0..19 {
            let suffix = if dep % 2 == 0 { " deduped" } else { "" };
            out.push_str(&format!("│ ├── dep-{top:03}-{dep:02}@1.{dep}.0{suffix}\n"));
        }
        if top == 50 {
            out.push_str("│ ├── UNMET PEER DEPENDENCY react@^17.0.0\n");
        }
    }
    out.push_str("└── zod@3.23.8\n");
    out
}

#[cfg(unix)] // used by the Unix-only diet_exec tests
fn breadcrumb_id(t: &str) -> String {
    t.lines()
        .find_map(|l| l.split("agentdiet show '#").nth(1))
        .and_then(|rest| rest.split('\'').next())
        .expect("a breadcrumb")
        .to_string()
}

/// Dogfooding regression: `npm ls --all` (about 1,900 lines) came back whole, with no breadcrumb,
/// `trimmed_outputs` 0 and `tokens_saved` 0.
#[cfg(unix)] // runs Unix programs (sh, cat, seq) or Unix-only error paths
#[test]
fn diet_exec_trims_a_2000_line_npm_ls_stores_it_and_counts_it() {
    let env = Env::empty();
    let tree = npm_ls_tree();
    assert!(tree.lines().count() > 2000);
    let npm = fake_tool(&env, "npm", &tree, 0);
    let r = call_one(
        &env,
        "diet_exec",
        json!({"command": [npm.display().to_string(), "ls", "--all"]}),
    );
    assert_eq!(r["isError"], false);
    let t = text(&r);
    assert!(
        t.lines().count() < 120,
        "compact: {} lines\n{t}",
        t.lines().count()
    );
    assert!(
        t.starts_with("germany-masters-2027@0.1.0"),
        "the head is kept: {t}"
    );
    assert!(t.contains("└── zod@3.23.8"), "the tail is kept");
    assert!(
        t.contains("UNMET PEER DEPENDENCY react@^17.0.0"),
        "problems are kept: {t}"
    );
    assert!(t.contains("elided"), "{t}");
    assert!(t.trim_end().ends_with("[exit 0]"), "{t}");
    let id = breadcrumb_id(&t);

    // Recoverable byte for byte.
    let full = call_one(&env, "diet_inspect", json!({"id": id}));
    assert_eq!(text(&full), tree);

    // Counted in state.json and in the run log.
    assert_eq!(counter(&env, "trimmed_outputs"), 1);
    assert!(counter(&env, "tokens_saved") > 10_000, "{}", state(&env));
    let db = MemoryDb::open(&env.paths.memory_db()).unwrap();
    let runs = db.runs(10).unwrap();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(
        (
            runs[0].origin.as_str(),
            runs[0].family.as_str(),
            runs[0].outcome.as_str()
        ),
        ("mcp", "npm", "ok")
    );
    assert!(runs[0].title.ends_with("npm ls --all"), "{}", runs[0].title);
    assert!(
        runs[0]
            .cas_sha
            .as_deref()
            .is_some_and(|s| s.starts_with(&id))
    );
    assert!(runs[0].raw_tokens > 10 * runs[0].emitted_tokens);
}

/// A command no rule knows is still cut to its head, its tail and its diagnostics when it is long.
#[cfg(unix)] // runs Unix programs (sh, cat, seq) or Unix-only error paths
#[test]
fn diet_exec_cuts_any_long_output_to_head_tail_and_diagnostics() {
    let env = Env::empty();
    let mut body = String::new();
    for i in 0..1500 {
        body.push_str(&format!("row {i:04}: value={}\n", i * 7));
        if i == 700 {
            body.push_str("error: checksum mismatch in row 0700\n");
        }
    }
    let tool = fake_tool(&env, "report-gen", &body, 3);
    let r = call_one(
        &env,
        "diet_exec",
        json!({"command": [tool.display().to_string()]}),
    );
    let t = text(&r);
    assert!(t.contains("row 0000: value=0"));
    assert!(t.contains("row 1499: value=10493"));
    assert!(t.contains("error: checksum mismatch in row 0700"), "{t}");
    assert!(
        t.contains("row 0699: value=4893"),
        "context around the error"
    );
    assert!(!t.contains("row 0400: value="), "the middle is elided");
    assert!(t.trim_end().ends_with("[exit 3]"));
    let id = breadcrumb_id(&t);
    assert_eq!(
        text(&call_one(&env, "diet_inspect", json!({"id": id}))),
        body
    );

    // Machine-readable output is never cut, however long.
    let json_body: String = std::iter::once("[\n".to_string())
        .chain((0..1500).map(|i| format!("  {{\"row\": {i}}},\n")))
        .chain(std::iter::once("  {\"row\": -1}\n]\n".to_string()))
        .collect();
    let tool = fake_tool(&env, "report-json", &json_body, 0);
    let r = call_one(
        &env,
        "diet_exec",
        json!({"command": [tool.display().to_string(), "--json"]}),
    );
    assert!(text(&r).starts_with(&json_body), "untouched");
}
