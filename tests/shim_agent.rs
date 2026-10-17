//! Shims end to end: `~/.agentdiet/shims/cargo` -> the built binary, in front of a fake `cargo`
//! script. The agent path is forced with `AGENTDIET_FORCE_AGENT` (the PPID walk itself is unit
//! tested in `src/shim.rs`), and the transparent path with every rule that selects it.
#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use common::Env;
use serde_json::json;

const BIN: &str = env!("CARGO_BIN_EXE_agentdiet");

fn fixture_path(name: &str) -> String {
    format!(
        "{}/tests/fixtures/compress/{name}",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn aws_key() -> String {
    ["AK", "IA"].concat() + &"Q7".repeat(8)
}

struct Shim {
    env: Env,
    shims: PathBuf,
    real: PathBuf,
}

impl Shim {
    /// A fake `cargo` that runs `script` (sh), behind a shim.
    fn new(script: &str) -> Self {
        let env = Env::empty();
        let shims = env.paths.shims_dir();
        fs::create_dir_all(&shims).unwrap();
        std::os::unix::fs::symlink(BIN, shims.join("cargo")).unwrap();
        let real = env.dir.path().join("realbin");
        fs::create_dir_all(&real).unwrap();
        let path = real.join("cargo");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        Shim { env, shims, real }
    }

    /// `sh -c <snippet>` with `"$0"` the shim, as an agent runs a command: the shell's stdout is
    /// the agent's capture, and a command run as-is shares it. An isolated home and none of the
    /// developer's own agent markers.
    fn sh(&self, snippet: &str) -> Command {
        let path = std::env::join_paths([
            self.shims.as_path(),
            self.real.as_path(),
            std::path::Path::new("/usr/bin"),
            std::path::Path::new("/bin"),
        ])
        .unwrap();
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(snippet)
            .arg(self.shims.join("cargo"))
            .env("HOME", &self.env.paths.home)
            .env("XDG_CONFIG_HOME", &self.env.paths.config_dir)
            .env("PATH", path)
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CURSOR_TRACE_ID")
            .env_remove("GEMINI_CLI")
            .env_remove("AGENTDIET_SHIM_ACTIVE")
            .env_remove("AGENTDIET_FORCE_HUMAN")
            .current_dir(&self.env.paths.project_root)
            .stdin(Stdio::null());
        cmd
    }

    /// `cargo <args>` run by the shell. The trailing `exit` keeps the shell from exec'ing the
    /// command in its own place, as an agent's wrapper shell does not either.
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = self.sh(r#""$0" "$@"; exit $?"#);
        cmd.args(args);
        cmd
    }

    fn agent(&self, args: &[&str]) -> Output {
        self.command(args)
            .env("AGENTDIET_FORCE_AGENT", "1")
            .output()
            .unwrap()
    }

    fn show(&self, id: &str) -> Vec<u8> {
        let out = Command::new(BIN)
            .args(["show", &format!("#{id}")])
            .env("HOME", &self.env.paths.home)
            .current_dir(&self.env.paths.project_root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    fn vault_entries(&self) -> usize {
        fs::read_dir(self.env.paths.cas_dir()).map_or(0, |d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".log"))
                .count()
        })
    }
}

fn crumb_id(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.starts_with("🥗 Diet: "))?;
    let start = line.find("show '#")? + "show '#".len();
    Some(line[start..].trim_end_matches('\'').to_string())
}

/// `cargo test` output (436 lines) on stdout, a short note on stderr, exit 101.
fn failing_cargo_test() -> Shim {
    Shim::new(&format!(
        "cat '{}'\necho 'error: test failed, to rerun pass `--lib`' >&2\nexit 101",
        fixture_path("cargo_test_pass.txt")
    ))
}

#[test]
fn agent_path_diets_stores_and_keeps_the_exit_code() {
    let shim = failing_cargo_test();
    let original = fs::read(fixture_path("cargo_test_pass.txt")).unwrap();
    let out = shim.agent(&["test"]);
    assert_eq!(out.status.code(), Some(101), "the real command's exit code");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.len() < original.len());
    let id = crumb_id(&text).expect("breadcrumb");
    assert!(text.contains("exit 101"), "{text}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: test failed, to rerun pass `--lib`\n",
        "stderr has its own stream, untouched when small"
    );
    assert_eq!(shim.vault_entries(), 1);
    assert_eq!(shim.show(&id), original, "recoverable byte for byte");
    let state: serde_json::Value =
        serde_json::from_str(&shim.env.read(&shim.env.paths.state_file())).unwrap();
    assert_eq!(state["last_trimmed_id"], id.as_str());
}

#[test]
fn the_same_command_twice_in_a_row_is_raw_but_still_scrubbed() {
    let shim = Shim::new(&format!(
        "cat '{}'\necho 'deploy key {}'\nexit 0",
        fixture_path("cargo_test_pass.txt"),
        aws_key()
    ));
    let first = String::from_utf8(shim.agent(&["test"]).stdout).unwrap();
    assert!(crumb_id(&first).is_some());
    let second = shim.agent(&["test"]);
    assert_eq!(second.status.code(), Some(0));
    let second = String::from_utf8(second.stdout).unwrap();
    assert!(!second.contains("🥗"), "the retry expands");
    assert!(!second.contains(&aws_key()) && second.contains("[REDACTED:"));
    assert!(crumb_id(&String::from_utf8(shim.agent(&["test", "--lib"]).stdout).unwrap()).is_some());
}

#[test]
fn a_raw_prompt_reaches_shims_through_the_claude_code_session() {
    let shim = failing_cargo_test();
    let session = "0b8f6a2e-raw-turn-session";
    let payload = json!({
        "session_id": session,
        "cwd": shim.env.paths.project_root,
        "hook_event_name": "UserPromptSubmit",
        "prompt": "rerun it and show me the full output",
    });
    let mut hook = Command::new(BIN)
        .args(["hook", "UserPromptSubmit"])
        .env("HOME", &shim.env.paths.home)
        .current_dir(&shim.env.paths.project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    std::io::Write::write_all(
        &mut hook.stdin.take().unwrap(),
        payload.to_string().as_bytes(),
    )
    .unwrap();
    assert!(hook.wait().unwrap().success());

    let original = fs::read(fixture_path("cargo_test_pass.txt")).unwrap();
    let out = shim
        .command(&["test"])
        .env("AGENTDIET_FORCE_AGENT", "1")
        .env("CLAUDE_CODE_SESSION_ID", session)
        .output()
        .unwrap();
    assert_eq!(out.stdout, original, "raw for the whole turn");
    assert_eq!(out.status.code(), Some(101));
    // Another session is not affected.
    let other = shim
        .command(&["test"])
        .env("AGENTDIET_FORCE_AGENT", "1")
        .env("CLAUDE_CODE_SESSION_ID", "another-session")
        .output()
        .unwrap();
    assert!(crumb_id(&String::from_utf8(other.stdout).unwrap()).is_some());
}

#[test]
fn transparent_cases_pass_through_byte_identical() {
    let shim = failing_cargo_test();
    let original = fs::read(fixture_path("cargo_test_pass.txt")).unwrap();

    // A shape the engine does not compress (the program's own output).
    let run = shim.agent(&["run"]);
    assert_eq!(run.stdout, original);
    assert_eq!(run.status.code(), Some(101));

    // A nested shim (a `cargo` run by another shimmed command).
    let nested = shim
        .command(&["test"])
        .env("AGENTDIET_FORCE_AGENT", "1")
        .env("AGENTDIET_SHIM_ACTIVE", "1")
        .output()
        .unwrap();
    assert_eq!(nested.stdout, original);

    // A human (forced: the test runner may itself run under an agent).
    let human = shim
        .command(&["test"])
        .env("AGENTDIET_FORCE_HUMAN", "1")
        .output()
        .unwrap();
    assert_eq!(human.stdout, original);
    assert_eq!(human.status.code(), Some(101));

    // stdout redirected to a file goes to the file, not the model.
    let file = shim.env.dir.path().join("out.txt");
    let to_file = shim
        .sh(r#""$0" test > "$1"; exit $?"#)
        .arg(&file)
        .env("AGENTDIET_FORCE_AGENT", "1")
        .output()
        .unwrap();
    assert_eq!(to_file.status.code(), Some(101));
    assert_eq!(fs::read(&file).unwrap(), original);

    // stdout piped onward goes to the next program, not the model.
    let piped = shim
        .sh(r#""$0" test | cat; exit $?"#)
        .env("AGENTDIET_FORCE_AGENT", "1")
        .output()
        .unwrap();
    assert_eq!(piped.stdout, original);

    // Spawned directly (no shell between): the shim's stdout is not its parent's.
    let direct = Command::new(shim.shims.join("cargo"))
        .arg("test")
        .env("HOME", &shim.env.paths.home)
        .env(
            "PATH",
            std::env::join_paths([
                shim.shims.as_path(),
                shim.real.as_path(),
                std::path::Path::new("/usr/bin"),
                std::path::Path::new("/bin"),
            ])
            .unwrap(),
        )
        .env("AGENTDIET_FORCE_AGENT", "1")
        .env_remove("AGENTDIET_FORCE_HUMAN")
        .env_remove("AGENTDIET_SHIM_ACTIVE")
        .current_dir(&shim.env.paths.project_root)
        .output()
        .unwrap();
    assert_eq!(direct.stdout, original);

    assert_eq!(shim.vault_entries(), 0, "nothing was stored");
}

#[test]
fn a_child_killed_by_a_signal_kills_the_shim_with_it() {
    let shim = Shim::new(&format!(
        "cat '{}'\nkill -TERM $$",
        fixture_path("cargo_test_pass.txt")
    ));
    let out = shim.agent(&["test"]);
    // The wrapper shell reports a child killed by SIGTERM as 128 + 15.
    assert_eq!(out.status.code(), Some(143), "{:?}", out.status);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("signal 15"),
        "the output still arrives, dieted: {text}"
    );
}

#[test]
fn a_paused_agent_path_passes_raw_and_counts() {
    let shim = failing_cargo_test();
    agentdiet::state::set_status(&shim.env.paths, agentdiet::state::Status::Paused).unwrap();
    let out = shim.agent(&["test"]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains("🥗"));
    assert!(agentdiet::state::load(&shim.env.paths).raw_tokens_passed_while_paused > 0);
    assert_eq!(shim.vault_entries(), 0);
}
