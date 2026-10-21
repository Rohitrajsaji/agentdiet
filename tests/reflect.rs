//! `agentdiet stats`, `agentdiet reflect` and the MCP tool `diet_reflect` end to end (SPEC §3b): runs
//! are logged through the real hook entry point, then mined into `EXPERIENCE.md`.

mod common;

use std::io::Write;
use std::process::{Command, Output, Stdio};

use common::Env;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_agentdiet");
const SESSION: &str = "7c0ffee0-reflect-session";

fn agentdiet(env: &Env) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.env("HOME", &env.paths.home)
        .env("USERPROFILE", &env.paths.home)
        .env("XDG_CONFIG_HOME", &env.paths.config_dir)
        .env("APPDATA", &env.paths.config_dir)
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("AGENTDIET_WORKSPACE")
        .current_dir(&env.paths.project_root);
    cmd
}

fn hook(env: &Env, event: &str, extra: Value) {
    let mut payload = json!({
        "session_id": SESSION,
        "cwd": env.paths.project_root,
        "hook_event_name": event,
    });
    for (k, v) in extra.as_object().unwrap() {
        payload[k] = v.clone();
    }
    let mut child = agentdiet(env)
        .args(["hook", event])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}

fn failed_bash(env: &Env, command: &str, id: &str, error: &str) {
    hook(
        env,
        "PostToolUseFailure",
        json!({"tool_name": "Bash", "tool_input": {"command": command}, "tool_use_id": id, "error": error}),
    );
}

fn ok_bash(env: &Env, command: &str, id: &str, stdout: &str) {
    hook(
        env,
        "PostToolUse",
        json!({
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "tool_response": {"stdout": stdout, "stderr": "", "interrupted": false, "isImage": false},
            "tool_use_id": id,
        }),
    );
}

fn run(env: &Env, args: &[&str]) -> Output {
    let out = agentdiet(env).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn aws_key() -> String {
    ["AK", "IA"].concat() + &"Q7".repeat(8)
}

/// A web session with friction: a typo'd script that failed loudly twice before the right name, a
/// dependency tree printed twice, a failing deploy with a key on its command line.
fn seed(env: &Env) {
    let npm_error: String =
        std::iter::once("Exit code 1\nnpm error Missing script: \"biuld\"\n".to_string())
            .chain((0..120).map(|i| format!("npm error   available script {i}: lint{i}\n")))
            .collect();
    failed_bash(env, "npm run biuld", "t1", &npm_error);
    ok_bash(env, "npm run build", "t2", "compiled successfully\n");
    let tree: String = (0..800)
        .map(|i| format!("├── package-{i}@1.0.{i}\n"))
        .collect();
    ok_bash(env, "npm ls --all", "t3", &tree);
    failed_bash(env, "npm run biuld", "t4", &npm_error);
    ok_bash(env, "npm run build", "t5", "compiled successfully\n");
    ok_bash(env, "git status", "t6", "clean\n");
    ok_bash(env, "npm ls --all", "t7", &tree);
    let key = aws_key();
    let deploy_error: String = std::iter::once("Exit code 2\n".to_string())
        .chain((0..200).map(|i| format!("deploy error line {i}: access denied\n")))
        .collect();
    failed_bash(
        env,
        &format!("aws s3 cp dist s3://site --access-key {key}"),
        "t8",
        &deploy_error,
    );
    ok_bash(
        env,
        &format!("aws s3 cp dist s3://site --access-key {key} --region eu-central-1"),
        "t9",
        "ok\n",
    );
}

#[test]
fn stats_counts_the_run_log_and_never_creates_memory_db() {
    let env = Env::empty();
    let empty: Value = serde_json::from_slice(&run(&env, &["stats", "--json"]).stdout).unwrap();
    assert_eq!(empty["memory_db"], Value::Null);
    assert_eq!(empty["runs"]["shim"]["runs"], 0);
    assert!(
        !env.paths.memory_db().exists(),
        "never created just to be read"
    );

    seed(&env);
    let stats: Value = serde_json::from_slice(&run(&env, &["stats", "--json"]).stdout).unwrap();
    assert_eq!(stats["runs"]["hook"]["runs"], 9, "{stats}");
    assert_eq!(stats["runs"]["hook"]["failed"], 3);
    assert_eq!(stats["runs"]["shim"]["runs"], 0);
    let text = String::from_utf8(run(&env, &["stats"]).stdout).unwrap();
    assert!(
        text.contains("tokens saved") && text.contains("hook"),
        "{text}"
    );
}

#[test]
fn reflect_writes_a_bounded_cited_experience_md_and_is_idempotent() {
    let env = Env::empty();
    seed(&env);
    let path = env.paths.project_root.join("EXPERIENCE.md");

    let dry = String::from_utf8(run(&env, &["reflect", "--dry-run"]).stdout).unwrap();
    assert!(!path.exists(), "a dry run writes nothing");
    assert!(dry.contains("<!-- agentdiet:experience:begin -->"));

    let out = String::from_utf8(run(&env, &["reflect"]).stdout).unwrap();
    assert!(out.contains("updated"), "{out}");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.len() <= 2048, "{} bytes", text.len());
    assert!(
        text.contains("- `npm run build` worked after `npm run biuld` (exit 1, 2×) — "),
        "{text}"
    );
    assert!(text.contains("`npm ls --all` emits ~"), "{text}");
    assert!(text.contains("2×"), "{text}");
    // The deploy fix is a recipe (one failure, but 200 lines of wasted output); its key is never
    // written, only its redaction.
    assert!(text.contains("; added `--region eu-central-1`"), "{text}");
    assert!(!text.contains(&aws_key()) && text.contains("[REDACTED:aws-access-key-id]"));
    // Nothing that did not meet the gate: one clean `git status`.
    assert!(!text.contains("git status"));

    let again = String::from_utf8(run(&env, &["reflect"]).stdout).unwrap();
    assert!(again.contains("unchanged"), "deterministic: {again}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

    // Hand-written notes outside the block survive a regeneration.
    std::fs::write(&path, format!("# Team notes\n\nUse pnpm.\n\n{text}")).unwrap();
    run(&env, &["reflect"]);
    let kept = std::fs::read_to_string(&path).unwrap();
    assert!(kept.starts_with("# Team notes\n\nUse pnpm.\n\n<!-- agentdiet:experience:begin -->"));
    assert_eq!(kept.matches("agentdiet:experience:begin").count(), 1);
    assert!(kept.len() <= 2048);
}

#[test]
fn diet_reflect_over_mcp_returns_the_same_file() {
    let env = Env::empty();
    seed(&env);
    let mut child = agentdiet(&env)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": {"name": "diet_reflect", "arguments": {}}});
        writeln!(stdin, "{req}").unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    let result = &reply["result"];
    assert_eq!(result["isError"], false, "{reply}");
    let file = std::fs::read_to_string(env.paths.project_root.join("EXPERIENCE.md")).unwrap();
    assert_eq!(result["content"][0]["text"], file.as_str());
    assert!(
        result["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("lesson(s) proposed")
    );
}
