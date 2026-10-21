//! The hook router end to end: the real binary, a realistic JSON payload on stdin, and a TempDir
//! `HOME`. Payload shapes follow SPEC §3 "Hook router" (Claude Code 2.1.302).

mod common;

use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use agentdiet::diet::PAUSED_NOTICE;
use agentdiet::memory::MemoryDb;
use agentdiet::state::{self, OnSessionStart, Status};
use common::Env;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_agentdiet");
const SESSION: &str = "4a1d1c55-0f0e-4f43-a8c4-0d6c7c1f9e10";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/compress/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn aws_key() -> String {
    ["AK", "IA"].concat() + &"Q7".repeat(8)
}

/// The binary with an isolated home. Agent markers from the developer's own session are removed so
/// nothing outside the TempDir leaks in.
fn agentdiet(env: &Env) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.env("HOME", &env.paths.home)
        .env("USERPROFILE", &env.paths.home)
        .env("XDG_CONFIG_HOME", &env.paths.config_dir)
        .env("APPDATA", &env.paths.config_dir)
        .env("USERPROFILE", &env.paths.home)
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CURSOR_TRACE_ID")
        .env_remove("GEMINI_CLI")
        .env_remove("AGENTDIET_WORKSPACE")
        .current_dir(&env.paths.project_root);
    cmd
}

fn hook_raw(env: &Env, event: &str, stdin: &[u8]) -> Output {
    let mut child = agentdiet(env)
        .args(["hook", event])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "hooks always exit 0");
    out
}

/// Run `event` with the common fields plus `extra`.
fn hook(env: &Env, event: &str, extra: Value) -> Output {
    let mut payload = json!({
        "session_id": SESSION,
        "transcript_path": "/tmp/transcript.jsonl",
        "cwd": env.paths.project_root,
        "permission_mode": "default",
        "hook_event_name": event,
    });
    for (k, v) in extra.as_object().unwrap() {
        payload[k] = v.clone();
    }
    hook_raw(env, event, payload.to_string().as_bytes())
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn prompt(env: &Env, text: &str) -> Output {
    hook(env, "UserPromptSubmit", json!({ "prompt": text }))
}

fn pre_bash(env: &Env, command: &str, id: &str) -> Output {
    hook(
        env,
        "PreToolUse",
        json!({
            "tool_name": "Bash",
            "tool_input": { "command": command, "description": "run it" },
            "tool_use_id": id,
        }),
    )
}

fn post_bash(env: &Env, command: &str, id: &str, out: &str) -> Output {
    hook(
        env,
        "PostToolUse",
        json!({
            "tool_name": "Bash",
            "tool_input": { "command": command, "description": "run it" },
            "tool_response": {
                "stdout": out,
                "stderr": "",
                "interrupted": false,
                "isImage": false,
                "noOutputExpected": false,
            },
            "tool_use_id": id,
        }),
    )
}

/// The replacement a PostToolUse printed, if any.
fn updated(out: &Output) -> Option<Value> {
    let text = stdout(out);
    if text.is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    Some(v["hookSpecificOutput"]["updatedToolOutput"].clone())
}

/// The `#id` in a breadcrumb.
fn crumb_id(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.starts_with("🥗 Diet: "))?;
    let start = line.find("show '#")? + "show '#".len();
    Some(line[start..].trim_end_matches('\'').to_string())
}

fn show(env: &Env, id: &str) -> Vec<u8> {
    let out = agentdiet(env)
        .args(["show", &format!("#{id}")])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Many lines of plain words: well over 3,000 estimated tokens, and nothing to compress.
fn big_plain() -> String {
    (0..1500)
        .map(|i| format!("plain line {i} with words\n"))
        .collect()
}

// ---- SessionStart / PreCompact ---------------------------------------------------------------

#[test]
fn session_start_while_paused_prints_exactly_the_notice() {
    let env = Env::empty();
    state::set_status(&env.paths, Status::Paused).unwrap();
    let out = hook(&env, "SessionStart", json!({ "source": "startup" }));
    assert_eq!(stdout(&out), format!("{PAUSED_NOTICE}\n"));
    assert_eq!(
        state::load(&env.paths).status,
        Status::Paused,
        "prompt never resumes"
    );
}

#[test]
fn session_start_with_auto_resume_resumes_and_says_so() {
    let env = Env::empty();
    state::update(&env.paths, |s| {
        s.status = Status::Paused;
        s.on_session_start = OnSessionStart::AutoResume;
    })
    .unwrap();
    let out = hook(&env, "SessionStart", json!({ "source": "startup" }));
    let text = stdout(&out);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("resumed"), "{text}");
    assert_eq!(state::load(&env.paths).status, Status::Active);
}

#[test]
fn session_start_while_active_and_pre_compact_are_silent() {
    let env = Env::empty();
    for source in ["startup", "resume", "clear", "compact"] {
        let out = hook(&env, "SessionStart", json!({ "source": source }));
        assert!(out.stdout.is_empty(), "{source}: nothing to report yet");
    }
    for trigger in ["manual", "auto"] {
        let out = hook(
            &env,
            "PreCompact",
            json!({ "trigger": trigger, "custom_instructions": null }),
        );
        assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{trigger}");
    }
}

#[test]
fn session_start_after_compaction_restores_the_memory_snapshot() {
    let env = Env::empty();
    let trimmed = post_bash(
        &env,
        "cargo test",
        "toolu_01",
        &fixture("cargo_test_pass.txt"),
    );
    let new = updated(&trimmed).expect("trimmed");
    let id = crumb_id(new["stdout"].as_str().unwrap()).unwrap();

    let out = hook(&env, "SessionStart", json!({ "source": "compact" }));
    let text = stdout(&out);
    assert!(text.starts_with("AgentDiet memory"), "{text}");
    assert!(
        text.contains(&format!("- last trimmed output: #{id}")),
        "{text}"
    );
    assert!(text.contains("- command: cargo test"), "{text}");
    assert!(text.contains(&format!(" #{id}")), "{text}");
    assert!(text.len() <= agentdiet::hook::SNAPSHOT_MAX_BYTES + 1);

    // A startup SessionStart does not repeat it.
    assert!(
        hook(&env, "SessionStart", json!({ "source": "startup" }))
            .stdout
            .is_empty()
    );
}

// ---- UserPromptSubmit ------------------------------------------------------------------------

#[test]
fn diet_pause_and_resume_in_chat_flip_state_json() {
    let env = Env::empty();
    let out = prompt(&env, "ok, @diet pause for a bit");
    assert_eq!(stdout(&out).lines().count(), 1);
    assert!(stdout(&out).contains("paused"));
    assert_eq!(state::load(&env.paths).status, Status::Paused);

    let out = prompt(&env, "@DIET Resume");
    assert_eq!(stdout(&out).lines().count(), 1);
    assert!(stdout(&out).contains("resumed"));
    assert_eq!(state::load(&env.paths).status, Status::Active);

    assert!(prompt(&env, "what does @dietary mean?").stdout.is_empty());
    assert_eq!(state::load(&env.paths).status, Status::Active);
}

/// The sidecar of the test session, as the hooks left it.
fn sidecar(env: &Env) -> agentdiet::session::Sidecar {
    agentdiet::session::load(
        &env.paths,
        &agentdiet::session::Key::Session(SESSION.to_string()),
    )
}

/// One Bash call as Claude Code delivers it: PreToolUse, then PostToolUse. `true` if trimmed.
fn bash_call(env: &Env, command: &str, id: &str, output: &str) -> Option<Value> {
    pre_bash(env, command, id);
    updated(&post_bash(env, command, id, output))
}

fn is_trimmed(v: &Option<Value>) -> bool {
    v.as_ref()
        .is_some_and(|v| v["stdout"].as_str().unwrap().contains("🥗 Diet: "))
}

#[test]
fn prose_containing_full_never_switches_trimming_off() {
    let env = Env::empty();
    let output = fixture("cargo_test_pass.txt");
    for (i, text) in [
        // The dogfooding prompt that left trimming disabled.
        "add the full report link to the footer",
        "Show me the RAW output please",
        "(full) logs, unfiltered.",
        "build the full-stack app and print the full path",
    ]
    .into_iter()
    .enumerate()
    {
        prompt(&env, text);
        assert!(!sidecar(&env).raw_turn, "{text}");
        let id = format!("toolu_prose_{i}");
        assert!(
            is_trimmed(&bash_call(&env, "cargo test", &id, &output)),
            "{text}"
        );
        // Not a retry: run something else in between.
        bash_call(&env, "cargo build", &format!("{id}_b"), "ok\n");
    }
}

#[test]
fn the_full_directive_bypasses_exactly_one_tool_call_and_never_skips_scrubbing() {
    let env = Env::empty();
    let output = format!(
        "{}AWS key in use: {}\n",
        fixture("cargo_test_pass.txt"),
        aws_key()
    );

    prompt(&env, "rerun the tests @diet:full");
    assert!(sidecar(&env).raw_turn, "armed by the prompt");
    let out = bash_call(&env, "cargo test", "toolu_a", &output).expect("redacted");
    let text = out["stdout"].as_str().unwrap();
    assert!(!text.contains("🥗"), "bypassed: not trimmed");
    assert!(
        !text.contains(&aws_key()) && text.contains("[REDACTED:"),
        "still scrubbed"
    );
    assert_eq!(text.lines().count(), output.lines().count());
    let after = sidecar(&env);
    assert!(
        !after.raw_turn && after.raw_tool_use.is_none(),
        "consumed by that one call: {after:?}"
    );

    // The very next tool call in the same turn is trimmed again.
    assert!(is_trimmed(&bash_call(
        &env,
        "cargo test --lib",
        "toolu_b",
        &output
    )));
}

#[test]
fn the_directive_binds_to_the_next_call_even_when_it_is_not_bash() {
    let env = Env::empty();
    let output = fixture("cargo_test_pass.txt");
    prompt(&env, "--diet-full");
    // The first tool call is an Edit: it takes (and spends) the bypass.
    hook(
        &env,
        "PreToolUse",
        json!({"tool_name": "Edit", "tool_input": {"file_path": "/x"}, "tool_use_id": "toolu_e"}),
    );
    assert!(sidecar(&env).raw_tool_use.is_some(), "bound to the Edit");
    hook(
        &env,
        "PostToolUse",
        json!({"tool_name": "Edit", "tool_input": {"file_path": "/x"}, "tool_response": {}, "tool_use_id": "toolu_e"}),
    );
    assert_eq!(sidecar(&env).raw_tool_use, None, "spent");
    assert!(is_trimmed(&bash_call(
        &env,
        "cargo test",
        "toolu_f",
        &output
    )));
}

#[test]
fn a_failing_call_spends_the_directive_and_a_new_prompt_or_session_clears_it() {
    let env = Env::empty();
    let output = fixture("cargo_test_pass.txt");

    prompt(&env, "@diet:full");
    pre_bash(&env, "cargo test", "toolu_fail");
    let out = hook(
        &env,
        "PostToolUseFailure",
        json!({
            "tool_name": "Bash",
            "tool_input": { "command": "cargo test" },
            "tool_use_id": "toolu_fail",
            "error": "Exit code 101\nerror: test failed",
            "is_interrupt": false,
        }),
    );
    assert!(out.stdout.is_empty(), "record-only: nothing is added");
    assert_eq!(sidecar(&env).raw_tool_use, None);
    // A different argv: the same one right after would be a retry, which expands by design.
    assert!(is_trimmed(&bash_call(
        &env,
        "cargo test --lib",
        "toolu_g",
        &output
    )));

    // Bound to an interrupted call that never reports back: the next prompt clears it.
    prompt(&env, "@diet:full");
    pre_bash(&env, "cargo build", "toolu_lost");
    assert!(sidecar(&env).raw_tool_use.is_some());
    prompt(&env, "never mind");
    assert_eq!(
        sidecar(&env),
        agentdiet::session::Sidecar {
            last_hook_sig: sidecar(&env).last_hook_sig,
            ..Default::default()
        }
    );

    // Armed, then a new session: cleared.
    prompt(&env, "@diet:full");
    hook(&env, "SessionStart", json!({ "source": "startup" }));
    assert!(is_trimmed(&bash_call(
        &env,
        "cargo test",
        "toolu_h",
        &output
    )));
}

// ---- PreToolUse ------------------------------------------------------------------------------

#[test]
fn pre_tool_use_never_emits_anything_so_thinking_blocks_are_untouched() {
    let env = Env::empty();
    let out = hook(
        &env,
        "PreToolUse",
        json!({
            "tool_name": "Bash",
            "tool_input": { "command": "cargo test" },
            "tool_use_id": "toolu_t1",
            "thinking": "secret plan: run the tests",
            "redacted_thinking": "EuYBCkQYAiJA...",
            "messages": [
                { "type": "thinking", "thinking": "step 1", "signature": "sig==" },
                { "type": "redacted_thinking", "data": "EmwKAhgB..." },
            ],
        }),
    );
    assert!(out.stdout.is_empty(), "no updatedInput, no decision");
}

#[test]
fn the_same_command_twice_in_a_row_expands_on_the_retry() {
    let env = Env::empty();
    let output = fixture("cargo_test_pass.txt");
    let run = |cmd: &str, id: &str| {
        pre_bash(&env, cmd, id);
        updated(&post_bash(&env, cmd, id, &output))
    };
    let trimmed =
        |v: Option<Value>| v.is_some_and(|v| v["stdout"].as_str().unwrap().contains("🥗"));

    assert!(trimmed(run("cargo test", "toolu_1")));
    assert!(
        run("cargo  'test'", "toolu_2").is_none(),
        "same argv: raw (identity)"
    );
    assert!(
        trimmed(run("cargo test --lib", "toolu_3")),
        "a different command"
    );
    assert!(
        trimmed(run("cargo test", "toolu_4")),
        "not *in a row* any more"
    );
    assert!(run("cargo test", "toolu_5").is_none());
}

// ---- PostToolUse -----------------------------------------------------------------------------

#[test]
fn post_tool_use_trims_stores_indexes_and_keeps_every_other_field() {
    let env = Env::empty();
    let original = fixture("cargo_test_pass.txt");
    let out = hook(
        &env,
        "PostToolUse",
        json!({
            "tool_name": "Bash",
            "tool_input": { "command": "cargo test", "description": "Run tests" },
            "tool_response": {
                "stdout": original,
                "stderr": "warning: unused import\n",
                "interrupted": false,
                "isImage": false,
                "noOutputExpected": false,
                "staleReadFileStateHint": "keep me",
            },
            "tool_use_id": "toolu_p1",
            "thinking": "private reasoning",
            "redacted_thinking": "EuYBCkQYAiJA",
        }),
    );
    let printed: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(
        printed.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["hookSpecificOutput"]
    );
    assert_eq!(
        printed["hookSpecificOutput"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["hookEventName", "updatedToolOutput"]
    );
    assert!(!stdout(&out).contains("private reasoning") && !stdout(&out).contains("EuYBCkQYAiJA"));

    let new = updated(&out).unwrap();
    let text = new["stdout"].as_str().unwrap();
    assert!(text.len() < original.len());
    let id = crumb_id(text).expect("breadcrumb");
    assert_eq!(
        new["stderr"], "warning: unused import\n",
        "too small to touch"
    );
    assert_eq!(new["interrupted"], false);
    assert_eq!(new["isImage"], false);
    assert_eq!(new["noOutputExpected"], false);
    assert_eq!(new["staleReadFileStateHint"], "keep me");

    assert_eq!(
        show(&env, &id),
        original.as_bytes(),
        "recoverable byte for byte"
    );
    let saved: Value = serde_json::from_str(&env.read(&env.paths.state_file())).unwrap();
    assert_eq!(saved["last_trimmed_id"], id.as_str());
    assert_eq!(saved["trimmed_outputs"], 1);
    let saved_tokens = agentdiet::diet::estimate_tokens(original.as_bytes())
        - agentdiet::diet::estimate_tokens(text.as_bytes());
    assert_eq!(saved["tokens_saved"], saved_tokens as u64);
    assert_eq!(saved["cas_dereferences"], 1, "the `show` above");
    let db = MemoryDb::open(&env.paths.memory_db()).unwrap();
    let hits = db.search("cargo test", 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, "command");
}

#[test]
fn agentdiets_own_mcp_results_are_left_alone() {
    let env = Env::empty();
    // A big JSON document would become TOON as any other MCP tool's result.
    let rows: Vec<Value> = (0..300)
        .map(|i| json!({"id": i, "name": format!("row {i}"), "ok": true}))
        .collect();
    let doc = serde_json::to_string_pretty(&rows).unwrap();
    let response = json!({"content": [{"type": "text", "text": doc}]});
    let post = |tool: &str| {
        hook(
            &env,
            "PostToolUse",
            json!({"tool_name": tool, "tool_input": {}, "tool_response": response, "tool_use_id": "toolu_m"}),
        )
    };
    assert!(
        !post("mcp__other__fetch").stdout.is_empty(),
        "someone else's tool is dieted"
    );
    assert!(post("mcp__agentdiet__diet_inspect").stdout.is_empty());
    assert!(post("mcp__agentdiet__diet_exec").stdout.is_empty());
}

#[test]
fn paused_output_passes_raw_counts_tokens_and_whispers_above_3000() {
    let env = Env::empty();
    state::set_status(&env.paths, Status::Paused).unwrap();

    let small = "just a few words\n";
    assert!(
        post_bash(&env, "cargo test", "toolu_s", small)
            .stdout
            .is_empty()
    );
    let after_small = state::load(&env.paths).raw_tokens_passed_while_paused;
    assert!(after_small > 0, "small output is still counted");

    let compressible = fixture("cargo_test_pass.txt");
    let out = post_bash(&env, "cargo test", "toolu_c", &compressible);
    let expected_whisper = agentdiet::diet::estimate_tokens(compressible.as_bytes())
        > agentdiet::diet::PAUSED_NOTICE_TOKENS;
    match updated(&out) {
        None => assert!(!expected_whisper),
        Some(v) => {
            assert!(expected_whisper);
            assert_eq!(
                v["stdout"],
                format!("{compressible}{PAUSED_NOTICE}\n"),
                "never trimmed"
            );
        }
    }

    let big = big_plain();
    let new = updated(&post_bash(&env, "cargo test", "toolu_b", &big)).expect("whisper");
    let text = new["stdout"].as_str().unwrap();
    assert_eq!(text, format!("{big}{PAUSED_NOTICE}\n"));
    assert!(state::load(&env.paths).raw_tokens_passed_while_paused > after_small + 3000);
    assert!(
        state::load(&env.paths).last_trimmed_id.is_none(),
        "nothing was stored"
    );
}

#[test]
fn output_a_shim_already_dieted_is_left_alone() {
    let env = Env::empty();
    let mut dieted = fixture("cargo_test_pass.txt");
    dieted.push_str("🥗 Diet: kept 30/436 lines; cargo test Run: agentdiet show '#4f9a1b2c'\n");
    assert!(
        post_bash(&env, "cargo test", "toolu_x", &dieted)
            .stdout
            .is_empty()
    );
}

#[test]
fn reads_are_scrubbed_but_never_folded() {
    let env = Env::empty();
    let source: String = (0..80)
        .map(|i| {
            format!(
                "fn f{i}() {{\n    let x = {i};\n    let y = x + 1;\n    println!(\"{{y}}\");\n}}\n"
            )
        })
        .collect();
    let read = |content: &str| {
        hook(
            &env,
            "PostToolUse",
            json!({
                "tool_name": "Read",
                "tool_input": { "file_path": "/repo/src/lib.rs" },
                "tool_response": {
                    "type": "text",
                    "file": {
                        "filePath": "/repo/src/lib.rs",
                        "content": content,
                        "numLines": content.lines().count(),
                        "startLine": 1,
                        "totalLines": content.lines().count(),
                    },
                },
                "tool_use_id": "toolu_r",
            }),
        )
    };
    assert!(source.lines().count() > 300);
    assert!(
        read(&source).stdout.is_empty(),
        "no secrets, no folding: untouched"
    );

    let with_key = format!("// key: {}\n{source}", aws_key());
    let new = updated(&read(&with_key)).expect("redacted");
    let content = new["file"]["content"].as_str().unwrap();
    assert!(!content.contains(&aws_key()) && content.contains("[REDACTED:"));
    assert!(!content.contains("folded"), "never folded");
    assert_eq!(content.lines().count(), with_key.lines().count());
    assert_eq!(new["type"], "text");
    assert_eq!(new["file"]["numLines"], with_key.lines().count());
}

#[test]
fn mcp_and_web_results_are_model_json_and_other_tools_are_untouched() {
    let env = Env::empty();
    let rows: Vec<Value> = (0..60)
        .map(|i| json!({ "id": i, "name": format!("item-{i}"), "status": "open" }))
        .collect();
    let doc = serde_json::to_string_pretty(&json!({ "items": rows })).unwrap();
    let out = hook(
        &env,
        "PostToolUse",
        json!({
            "tool_name": "mcp__tracker__list_items",
            "tool_input": {},
            "tool_response": [{ "type": "text", "text": doc }],
            "tool_use_id": "toolu_m",
        }),
    );
    let new = updated(&out).expect("TOON");
    let text = new[0]["text"].as_str().unwrap();
    assert!(text.contains("items[60]{id,name,status}:"), "{text}");
    assert!(text.contains("🥗 Diet: "));
    assert_eq!(new[0]["type"], "text");

    let edit = hook(
        &env,
        "PostToolUse",
        json!({
            "tool_name": "Edit",
            "tool_input": { "file_path": "/a" },
            "tool_response": { "filePath": "/a", "oldString": aws_key(), "newString": "x" },
            "tool_use_id": "toolu_e",
        }),
    );
    assert!(edit.stdout.is_empty());
}

#[test]
fn bad_payloads_fail_open_silently() {
    let env = Env::empty();
    for stdin in [&b""[..], b"{not json", b"[1,2,3]", b"null", b"{}"] {
        for event in [
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "PostToolUseFailure",
            "PreCompact",
        ] {
            let out = hook_raw(&env, event, stdin);
            assert!(out.stdout.is_empty(), "{event} {stdin:?}");
        }
    }
    let missing_response = hook(&env, "PostToolUse", json!({ "tool_name": "Bash" }));
    assert!(missing_response.stdout.is_empty());
}

#[test]
fn a_payload_that_never_arrives_cannot_hold_the_agent() {
    let env = Env::empty();
    let held = |event: &str| {
        let start = Instant::now();
        let mut child = agentdiet(&env)
            .args(["hook", event])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let held_open = child.stdin.take().unwrap(); // never written, never closed
        (start, child, held_open)
    };
    // `noop` exits without reading stdin: started alongside, it measures what launching costs on
    // this machine right now, so the bound is the stdin timeout plus that cost, not a guess.
    let (start, child, _stdin) = held("PostToolUse");
    let (launch_start, launch, _launch_stdin) = held("noop");
    assert!(launch.wait_with_output().unwrap().status.success());
    let launch_cost = launch_start.elapsed();
    let out = child.wait_with_output().unwrap();
    let waited = start.elapsed();
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    let timeout = agentdiet::hook::STDIN_TIMEOUT;
    assert!(
        waited >= timeout - Duration::from_millis(100),
        "gave up early: {waited:?}"
    );
    assert!(
        waited < timeout + launch_cost * 3 + Duration::from_secs(1),
        "waited {waited:?}: the {timeout:?} timeout plus a launch ({launch_cost:?} now)"
    );
}

// ---- run log ----------------------------------------------------------------------------------

fn runs(env: &Env) -> Vec<agentdiet::memory::RunRow> {
    agentdiet::memory::MemoryDb::open(&env.paths.memory_db())
        .unwrap()
        .runs(100)
        .unwrap()
}

#[test]
fn hooks_log_successes_failures_big_reads_and_search_misses() {
    let env = Env::empty();
    let big = big_plain();

    // A failing command, then the fix: both logged with their outcome.
    hook(
        &env,
        "PostToolUseFailure",
        json!({
            "tool_name": "Bash",
            "tool_input": { "command": "cd web && npm run biuld" },
            "tool_use_id": "toolu_f1",
            "error": "Exit code 1\nnpm error Missing script: \"biuld\"",
            "is_interrupt": false,
        }),
    );
    post_bash(&env, "cd web && npm run build", "toolu_ok1", "built\n");
    // A large Read is logged; a small one is not worth a row.
    hook(
        &env,
        "PostToolUse",
        json!({
            "tool_name": "Read",
            "tool_input": {"file_path": "/p/big.log"},
            "tool_response": {"type": "text", "file": {"filePath": "/p/big.log", "content": big}},
            "tool_use_id": "toolu_r1",
        }),
    );
    hook(
        &env,
        "PostToolUse",
        json!({
            "tool_name": "Read",
            "tool_input": {"file_path": "/p/small.txt"},
            "tool_response": {"type": "text", "file": {"filePath": "/p/small.txt", "content": "hi\n"}},
            "tool_use_id": "toolu_r2",
        }),
    );
    // A Glob that found nothing, then one that did; a Read of a missing file.
    hook(
        &env,
        "PostToolUse",
        json!({
            "tool_name": "Glob",
            "tool_input": {"pattern": "src/**/*.tsx"},
            "tool_response": {"filenames": [], "numFiles": 0, "durationMs": 3, "truncated": false},
            "tool_use_id": "toolu_g1",
        }),
    );
    hook(
        &env,
        "PostToolUse",
        json!({
            "tool_name": "Glob",
            "tool_input": {"pattern": "app/**/*.tsx"},
            "tool_response": {"filenames": ["/p/app/page.tsx"], "numFiles": 1},
            "tool_use_id": "toolu_g2",
        }),
    );
    hook(
        &env,
        "PostToolUseFailure",
        json!({
            "tool_name": "Read",
            "tool_input": {"file_path": "/p/src/missing.ts"},
            "tool_use_id": "toolu_r3",
            "error": "File does not exist.",
        }),
    );
    // A user interrupt is not evidence of anything.
    hook(
        &env,
        "PostToolUseFailure",
        json!({
            "tool_name": "Bash",
            "tool_input": {"command": "sleep 100"},
            "tool_use_id": "toolu_i1",
            "error": "Interrupted",
            "is_interrupt": true,
        }),
    );

    let got: Vec<(String, String, String, String, Option<i64>)> = runs(&env)
        .into_iter()
        .map(|r| (r.kind, r.family, r.title, r.outcome, r.exit_code))
        .collect();
    let row = |k: &str, f: &str, t: &str, o: &str, e: Option<i64>| {
        (
            k.to_string(),
            f.to_string(),
            t.to_string(),
            o.to_string(),
            e,
        )
    };
    assert_eq!(
        got,
        vec![
            row("shell", "npm", "cd web && npm run biuld", "fail", Some(1)),
            row("shell", "npm", "cd web && npm run build", "ok", Some(0)),
            row("read", "Read", "/p/big.log", "ok", None),
            row("search", "Glob", "src/**/*.tsx", "miss", None),
            row("search", "Glob", "app/**/*.tsx", "ok", None),
            row("read", "Read", "/p/src/missing.ts", "miss", None),
        ]
    );
    let all = runs(&env);
    assert!(
        all.iter()
            .all(|r| r.origin == "hook" && r.session.as_deref() == Some(SESSION))
    );
    assert!(all[2].raw_tokens > 1000);
}

#[test]
fn a_bash_result_a_shim_already_dieted_is_not_logged_twice() {
    let env = Env::empty();
    post_bash(
        &env,
        "cargo test",
        "toolu_s",
        "ok\n🥗 Diet: kept 2/9 lines Run: agentdiet show '#4f9a1b2c'\n",
    );
    assert!(!env.paths.memory_db().exists() || runs(&env).is_empty());
}
