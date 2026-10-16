//! Spawns the real binary. `HOME` (and friends) are set on the child only, to
//! a TempDir, so nothing touches the developer's real `~/`.

mod common;

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use common::Env;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_agentdiet");

fn agentdiet(env: &Env) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.env("HOME", &env.paths.home)
        .env("XDG_CONFIG_HOME", &env.paths.config_dir)
        .env("USERPROFILE", &env.paths.home)
        .current_dir(&env.paths.project_root)
        .stdin(Stdio::null());
    cmd
}

fn run(env: &Env, args: &[&str]) -> Output {
    agentdiet(env).args(args).output().unwrap()
}

fn state(env: &Env) -> Value {
    serde_json::from_str(&env.read(&env.paths.state_file())).unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn pause_and_resume_flip_state_json() {
    let env = Env::empty();

    let out = run(&env, &["pause"]);
    assert!(out.status.success());
    assert_eq!(state(&env)["status"], "paused");

    // Paused stays paused across repeated commands.
    assert!(stdout(&run(&env, &["pause"])).contains("already paused"));
    assert_eq!(state(&env)["status"], "paused");

    let out = run(&env, &["resume"]);
    assert!(out.status.success());
    assert_eq!(state(&env)["status"], "active");
    assert!(stdout(&run(&env, &["resume"])).contains("already active"));

    // The file keeps the full SPEC shape.
    let s = state(&env);
    assert_eq!(s["mode"], "balanced");
    assert_eq!(s["raw_tokens_passed_while_paused"], 0);
    assert_eq!(s["on_session_start"], "prompt");
    assert!(s["last_trimmed_id"].is_null());
}

#[test]
fn bare_command_without_a_terminal_prints_status_and_exits() {
    let env = Env::empty();
    let out = run(&env, &[]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("AgentDiet is active"));

    run(&env, &["pause"]);
    assert!(stdout(&run(&env, &[])).contains("AgentDiet is paused"));
}

#[test]
fn hook_is_a_silent_no_op_that_never_reads_stdin() {
    let env = Env::empty();
    for event in ["SessionStart", "PreToolUse", "NotARealEvent"] {
        // stdin is /dev/null here; an open, never-closed pipe below proves it doesn't block.
        let out = run(&env, &["hook", event]);
        assert!(out.status.success(), "{event}");
        assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{event}");
    }

    let mut child = agentdiet(&env)
        .args(["hook", "UserPromptSubmit"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let _held_open = child.stdin.take().unwrap(); // never written, never closed
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
}

#[test]
fn mcp_server_completes_the_handshake() {
    let env = Env::empty();
    let mut child = agentdiet(&env)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n\
              {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
        )
        .unwrap();
    let out = child.wait_with_output().unwrap(); // EOF on stdin ends the server
    assert!(out.status.success());
    let lines: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines[0]["result"]["serverInfo"]["name"], "agentdiet");
    assert_eq!(lines[1]["result"]["tools"], serde_json::json!([]));
}

#[test]
fn later_slice_commands_fail_cleanly() {
    let env = Env::empty();
    for args in [vec!["show", "#4f9a"], vec!["top"], vec!["scan"]] {
        let out = run(&env, &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("not available yet"));
    }
}

#[test]
fn init_without_flags_and_without_a_terminal_explains_itself() {
    let env = Env::empty();
    let out = run(&env, &["init"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--all"));
    assert!(!env.paths.agentdiet_home.exists());
}

#[test]
fn init_all_dry_run_writes_nothing() {
    let env = Env::with_existing_configs();
    let before = env.snapshot();
    let out = run(&env, &["init", "--all", "--dry-run"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout(&out).contains("would change"));
    assert_eq!(env.snapshot(), before);
}

#[test]
fn init_thrice_then_eject_via_the_binary() {
    let env = Env::with_existing_configs();
    let pristine = env.snapshot();

    assert!(run(&env, &["init", "--all"]).status.success());
    let after_first = env.snapshot();
    for _ in 0..2 {
        let out = run(&env, &["init", "--all"]);
        assert!(out.status.success());
        assert!(stdout(&out).contains("changed 0 file(s)"), "{}", stdout(&out));
        assert_eq!(env.snapshot(), after_first);
    }
    assert!(stdout(&run(&env, &["init", "--all"])).contains("export PATH="));

    // Registered commands point at the real binary and contain no shell-rc edits.
    let claude: Value = serde_json::from_str(&env.read(&env.paths.home.join(".claude.json"))).unwrap();
    assert_eq!(claude["mcpServers"]["agentdiet"]["args"][0], "mcp");

    let out = run(&env, &["eject"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let mut restored = env.snapshot();
    restored.retain(|p, _| !p.starts_with("home/.agentdiet"));
    let mut expected = pristine;
    expected.retain(|p, _| !p.starts_with("home/.agentdiet"));
    assert_eq!(restored, expected);

    assert!(stdout(&run(&env, &["uninstall"])).contains("Nothing to eject"));
}

#[test]
fn only_restricts_targets() {
    let env = Env::empty();
    let out = run(&env, &["init", "--only", "cursor,gemini"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(env.paths.home.join(".cursor/mcp.json").exists());
    assert!(env.paths.home.join(".gemini/settings.json").exists());
    assert!(!env.paths.home.join(".claude").exists());
    assert!(!env.paths.home.join(".codex").exists());
}

#[cfg(unix)]
#[test]
fn shim_passes_through_to_the_real_binary() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::empty();
    let shims = env.paths.shims_dir();
    fs::create_dir_all(&shims).unwrap();
    std::os::unix::fs::symlink(BIN, shims.join("make")).unwrap();

    let real_dir = env.dir.path().join("realbin");
    fs::create_dir_all(&real_dir).unwrap();
    let script = real_dir.join("make");
    fs::write(&script, "#!/bin/sh\necho \"real make: $@\"\nexit 3\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let path = std::env::join_paths([shims.as_path(), real_dir.as_path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
    let out = Command::new(shims.join("make"))
        .args(["-j", "4"])
        .env("HOME", &env.paths.home)
        .env("PATH", path)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(stdout(&out).trim(), "real make: -j 4");
    assert_eq!(out.status.code(), Some(3), "exit status is the real command's");
}

#[cfg(unix)]
#[test]
fn shim_without_a_real_binary_exits_127() {
    let env = Env::empty();
    let shims = env.paths.shims_dir();
    fs::create_dir_all(&shims).unwrap();
    std::os::unix::fs::symlink(BIN, shims.join("ruff")).unwrap();
    let out = Command::new(shims.join("ruff"))
        .env("HOME", &env.paths.home)
        .env("PATH", &shims)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(127));
}
