//! Upgrade compatibility: the current build reads what the `v0.1.0` release wrote (docs/VERSIONING.md).
//! The fixture in `tests/fixtures/v0_1_0/` was produced by the `v0.1.0` binary itself; it is copied into
//! a fresh home here (so the Vault entry's age is "now", as the 7-day lifetime counts from the last use).

mod common;

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use common::Env;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_agentdiet");
const ID: &str = "8b744be8";

/// Copy the fixture. Git keeps no directory modes, so each directory gets the `0700` that v0.1.0
/// created it with (SPEC §3 Tier 3 "Private roots"; a loose one is covered in tests/hardening.rs).
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(to, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            fs::write(&dest, fs::read(entry.path()).unwrap()).unwrap();
        }
    }
}

fn v0_1_0_env() -> Env {
    let env = Env::empty();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v0_1_0");
    copy_tree(&fixture.join("home"), &env.paths.home);
    copy_tree(&fixture.join("proj"), &env.paths.project_root);
    fs::create_dir_all(env.paths.project_root.join(".git")).unwrap();
    env
}

fn run(env: &Env, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", &env.paths.home)
        .env("USERPROFILE", &env.paths.home)
        .env("XDG_CONFIG_HOME", &env.paths.config_dir)
        .env("APPDATA", &env.paths.config_dir)
        .env_remove("AGENTDIET_WORKSPACE")
        .current_dir(&env.paths.project_root)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn json(env: &Env, args: &[&str]) -> Value {
    serde_json::from_slice(&run(env, args).stdout).unwrap()
}

#[test]
fn state_vault_and_memory_written_by_v0_1_0_are_read() {
    let env = v0_1_0_env();

    // state.json: status, counters and the last ID carry over.
    let status = json(&env, &["status", "--json"]);
    assert_eq!(status["status"], "paused");
    assert_eq!(status["trimmed_outputs"], 1);
    assert_eq!(status["tokens_saved"], 4652);
    assert_eq!(status["last_trimmed_id"], ID);

    // The Vault entry, by ID and as `last`, byte for byte the same.
    let by_id = run(&env, &["show", &format!("#{ID}")]);
    assert!(
        by_id.status.success(),
        "{}",
        String::from_utf8_lossy(&by_id.stderr)
    );
    let text = String::from_utf8(by_id.stdout.clone()).unwrap();
    assert_eq!(text.lines().count(), 402);
    assert!(text.starts_with("   Compiling demo v0.1.0\n"));
    assert_eq!(run(&env, &["show", "last"]).stdout, by_id.stdout);

    // doctor sees the Vault and opens the old memory.db without calling it broken.
    let doctor = json(&env, &["doctor", "--json"]);
    let check = |name: &str| {
        doctor["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["check"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(check("vault")["level"], "ok");
    assert!(
        check("vault")["detail"]
            .as_str()
            .unwrap()
            .starts_with("1 entry")
    );
    assert_eq!(check("memory")["level"], "ok", "{}", check("memory"));
    assert_eq!(check("state")["level"], "ok");

    // The MCP server searches the v0.1.0 index and returns the entry.
    let mut child = Command::new(BIN)
        .arg("mcp")
        .env("HOME", &env.paths.home)
        .env("USERPROFILE", &env.paths.home)
        .current_dir(&env.paths.project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"diet_search","arguments":{"query":"cargo test"}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"diet_inspect","arguments":{"id":format!("#{ID}"),"start_line":402,"end_line":402}}}),
    ];
    let mut input = String::new();
    for r in requests {
        input.push_str(&format!("{r}\n"));
    }
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    let responses: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let search = responses[1]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(search.contains(&format!("#{ID}")), "{search}");
    let last_line = responses[2]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        last_line.starts_with("test result: ok. 400 passed"),
        "{last_line}"
    );

    // Resuming writes the current format, and the old counters survive it.
    assert!(run(&env, &["resume"]).status.success());
    let status = json(&env, &["status", "--json"]);
    assert_eq!(status["status"], "active");
    assert_eq!(status["tokens_saved"], 4652);
}
