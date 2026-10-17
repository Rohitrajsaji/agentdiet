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
        .env("XDG_CONFIG_HOME", &env.paths.config_dir)
        .env("USERPROFILE", &env.paths.home)
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CURSOR_TRACE_ID")
        .env_remove("GEMINI_CLI")
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

#[test]
fn escape_words_bypass_the_next_turn_only_and_never_skip_scrubbing() {
    let env = Env::empty();
    let output = format!(
        "{}AWS key in use: {}\n",
        fixture("cargo_test_pass.txt"),
        aws_key()
    );

    prompt(&env, "Show me the RAW output please");
    let out = updated(&post_bash(&env, "cargo test", "toolu_a", &output)).expect("redacted");
    let text = out["stdout"].as_str().unwrap();
    assert!(!text.contains("🥗"), "bypassed: not trimmed");
    assert!(
        !text.contains(&aws_key()) && text.contains("[REDACTED:"),
        "still scrubbed"
    );
    assert_eq!(text.lines().count(), output.lines().count());
    // The whole turn is raw, not only its first tool call.
    let again = updated(&post_bash(&env, "cargo test --lib", "toolu_b", &output)).unwrap();
    assert!(!again["stdout"].as_str().unwrap().contains("🥗"));

    for not_raw in [
        "fully done? build the full-stack app",
        "rawhide, unfilteredness",
    ] {
        prompt(&env, not_raw);
        let out = updated(&post_bash(&env, "cargo test", "toolu_c", &output)).unwrap();
        assert!(
            out["stdout"].as_str().unwrap().contains("🥗 Diet: "),
            "{not_raw}"
        );
    }

    for raw in ["(full) logs", "unfiltered."] {
        prompt(&env, raw);
        let out = updated(&post_bash(&env, "cargo test", "toolu_d", &output)).unwrap();
        assert!(!out["stdout"].as_str().unwrap().contains("🥗"), "{raw}");
    }
    // A new session starts without the bypass.
    hook(&env, "SessionStart", json!({ "source": "startup" }));
    let out = updated(&post_bash(&env, "cargo test", "toolu_e", &output)).unwrap();
    assert!(out["stdout"].as_str().unwrap().contains("🥗 Diet: "));
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
    let db = MemoryDb::open(&env.paths.memory_db()).unwrap();
    let hits = db.search("cargo test", 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, "command");
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
    let start = Instant::now();
    let mut child = agentdiet(&env)
        .args(["hook", "PostToolUse"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let _held_open = child.stdin.take().unwrap(); // never written, never closed
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    let waited = start.elapsed();
    assert!(
        waited >= agentdiet::hook::STDIN_TIMEOUT - Duration::from_millis(100)
            && waited < agentdiet::hook::STDIN_TIMEOUT + Duration::from_secs(5),
        "{waited:?}"
    );
}
