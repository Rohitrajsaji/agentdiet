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

/// The row whose first column is exactly `label`, in `init`/`doctor`/`status` output.
fn row<'a>(text: &'a str, label: &str) -> &'a str {
    text.lines()
        .find(|l| {
            l.trim_start()
                .strip_prefix(label)
                .is_some_and(|rest| rest.starts_with("  "))
        })
        .unwrap_or_else(|| panic!("no {label} row in:\n{text}"))
}

/// `PATH` with the shims directory first.
fn shims_first_path(env: &Env) -> std::ffi::OsString {
    let rest = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![env.paths.shims_dir()];
    dirs.extend(std::env::split_paths(&rest));
    std::env::join_paths(dirs).unwrap()
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

/// Real events read a bounded payload (tests/hooks.rs covers them, including a stdin that never
/// closes). Unknown events, `noop` among them, never touch stdin: an open, never-closed pipe returns
/// at once.
#[test]
fn hook_with_no_payload_or_an_unknown_event_is_silent() {
    let env = Env::empty();
    for event in ["SessionStart", "PreToolUse", "NotARealEvent", "noop"] {
        // stdin is /dev/null here: an empty payload.
        let out = run(&env, &["hook", event]);
        assert!(out.status.success(), "{event}");
        assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{event}");
    }

    for event in ["noop", "NotARealEvent"] {
        let start = std::time::Instant::now();
        let mut child = agentdiet(&env)
            .args(["hook", event])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let _held_open = child.stdin.take().unwrap(); // never written, never closed
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "{event}"
        );
    }
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
    assert_eq!(lines[1]["result"]["tools"].as_array().unwrap().len(), 4);
}

#[test]
fn top_without_a_terminal_fails_with_a_clear_message() {
    let env = Env::empty();
    let out = run(&env, &["top"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "nothing drawn");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(agentdiet::top::NO_TTY), "{err}");
}

#[test]
fn show_counts_a_dereference_only_when_it_succeeds() {
    use agentdiet::cas::Cas;
    let env = Env::empty();
    let put = Cas::new(env.paths.cas_dir())
        .put(
            &agentdiet::scrubber::scrub(b"stored\n"),
            std::time::SystemTime::now(),
        )
        .unwrap();
    assert!(!run(&env, &["show", "#beef"]).status.success());
    assert!(
        !env.paths.state_file().exists(),
        "a failed show counts nothing"
    );
    let out = run(&env, &["show", &format!("#{}", put.short_id)]);
    assert_eq!(stdout(&out), "stored\n");
    assert_eq!(state(&env)["cas_dereferences"], 1);
}

#[test]
fn show_prints_the_entry_and_slices_lines() {
    use agentdiet::cas::Cas;
    use agentdiet::scrubber::scrub;
    use std::time::SystemTime;

    let env = Env::empty();
    let put = Cas::new(env.paths.cas_dir())
        .put(&scrub(b"one\ntwo\nthree\n"), SystemTime::now())
        .unwrap();
    let id = format!("#{}", put.short_id);

    let all = run(&env, &["show", &id]);
    assert!(all.status.success());
    assert_eq!(all.stdout, b"one\ntwo\nthree\n");

    let slice = run(&env, &["show", &id, "--lines", "2..3"]);
    assert_eq!(slice.stdout, b"two\nthree\n");

    let bare = run(&env, &["show", &put.short_id, "--lines", "1..1"]);
    assert_eq!(bare.stdout, b"one\n");
}

#[test]
fn show_reports_expired_and_not_found_differently() {
    use agentdiet::cas::Cas;
    use agentdiet::scrubber::scrub;
    use std::time::{Duration, SystemTime};

    let env = Env::empty();
    let old = SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60);
    let put = Cas::new(env.paths.cas_dir())
        .put(&scrub(b"stale\n"), old)
        .unwrap();

    let expired = run(&env, &["show", &format!("#{}", put.short_id)]);
    assert!(!expired.status.success());
    assert!(String::from_utf8_lossy(&expired.stderr).contains("expired"));

    let missing = run(&env, &["show", "#0000"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no vault entry"));

    let bad = run(&env, &["show", &put.short_id, "--lines", "5..2"]);
    assert!(String::from_utf8_lossy(&bad.stderr).contains("invalid line range"));
}

#[test]
fn init_without_flags_and_without_a_terminal_explains_itself() {
    let env = Env::empty();
    let out = run(&env, &["init"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--all") && err.contains("--dry-run"), "{err}");
    assert!(err.contains("detected here: shims"), "{err}");
    assert!(!env.paths.agentdiet_home.exists());
}

#[test]
fn init_all_dry_run_writes_nothing() {
    let env = Env::with_existing_configs();
    let before = env.snapshot();
    let out = run(&env, &["init", "--all", "--dry-run"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    assert!(
        row(&text, "Claude Code").contains("✓ would connect"),
        "{text}"
    );
    assert!(text.contains("Dry run: nothing was written."), "{text}");
    assert_eq!(env.snapshot(), before);
}

#[test]
fn init_dry_run_without_a_terminal_previews_the_detected_targets() {
    let env = Env::with_existing_configs();
    let before = env.snapshot();
    let out = run(&env, &["init", "--dry-run"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    // Copilot is not detected here: on macOS its config dir ignores XDG_CONFIG_HOME.
    for label in ["Claude Code", "Cursor", "Gemini CLI", "Codex", "Shims"] {
        assert!(row(&text, label).contains("✓ would connect"), "{text}");
    }
    assert!(
        !text.contains("Windsurf"),
        "not detected, so not previewed:\n{text}"
    );
    assert_eq!(env.snapshot(), before);
    assert!(!env.paths.agentdiet_home.exists());
}

#[test]
fn init_thrice_then_eject_via_the_binary() {
    let env = Env::with_existing_configs();
    let pristine = env.snapshot();

    let first = run(&env, &["init", "--all"]);
    assert!(first.status.success());
    let first = stdout(&first);
    assert_eq!(first.matches("✓ connected").count(), 7, "{first}");
    assert!(
        first.contains("11 commands linked in ~/.agentdiet/shims"),
        "{first}"
    );
    assert!(
        first.contains("Backed up the originals to ~/.agentdiet/backups/"),
        "{first}"
    );
    assert!(first.contains("so they load the new config."), "{first}");
    assert!(
        !first.contains(&env.paths.home.display().to_string()),
        "home shown as ~"
    );
    let after_first = env.snapshot();
    for _ in 0..2 {
        let out = run(&env, &["init", "--all"]);
        assert!(out.status.success());
        let again = stdout(&out);
        // Same targets, same counts: every one is already connected.
        assert_eq!(again.matches("✓ already connected").count(), 7, "{again}");
        assert!(again.contains("11 commands linked"), "{again}");
        assert!(!again.contains("Restart"), "nothing changed:\n{again}");
        assert!(!again.contains("Backed up"), "{again}");
        assert_eq!(env.snapshot(), after_first);
    }
    assert!(
        stdout(&run(&env, &["init", "--all"]))
            .contains("export PATH=\"$HOME/.agentdiet/shims:$PATH\"")
    );
    let shims_first = agentdiet(&env)
        .args(["init", "--all"])
        .env("PATH", shims_first_path(&env))
        .output()
        .unwrap();
    assert!(
        !stdout(&shims_first).contains("export PATH="),
        "already first on PATH"
    );

    // Registered commands point at the real binary and contain no shell-rc edits.
    let claude: Value =
        serde_json::from_str(&env.read(&env.paths.home.join(".claude.json"))).unwrap();
    assert_eq!(claude["mcpServers"]["agentdiet"]["args"][0], "mcp");

    let out = run(&env, &["eject"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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

    let path = std::env::join_paths([
        shims.as_path(),
        real_dir.as_path(),
        Path::new("/usr/bin"),
        Path::new("/bin"),
    ])
    .unwrap();
    // Forced: these tests may themselves run under an AI agent (an ancestor named `claude`).
    let out = Command::new(shims.join("make"))
        .args(["-j", "4"])
        .env("HOME", &env.paths.home)
        .env("PATH", path)
        .env("AGENTDIET_FORCE_HUMAN", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(stdout(&out).trim(), "real make: -j 4");
    assert_eq!(
        out.status.code(),
        Some(3),
        "exit status is the real command's"
    );
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
        .env("AGENTDIET_FORCE_AGENT", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(127));
}

// ---- status, doctor, mode, show last (SPEC §3b) --------------------------------------------------

fn doctor(env: &Env) -> Output {
    agentdiet(env)
        .arg("doctor")
        .env("PATH", shims_first_path(env))
        .output()
        .unwrap()
}

fn init_everything(env: &Env) {
    let out = run(env, &["init", "--all"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn status_and_doctor_never_write_anything() {
    let env = Env::with_existing_configs();
    init_everything(&env);
    let before = env.snapshot();
    for args in [&["status"][..], &["doctor"], &[]] {
        run(&env, args);
    }
    doctor(&env);
    assert_eq!(env.snapshot(), before);
    assert!(
        !env.paths.memory_db().exists(),
        "memory.db is never created"
    );
    assert!(
        !env.paths.state_file().exists(),
        "state.json is never created"
    );
}

/// The paths that could write are exercised for real: a live Vault entry (`Cas::stats` takes its lock)
/// and an existing memory.db with runs (opened to count them).
#[test]
fn status_and_doctor_never_write_to_an_existing_vault_or_memory_db() {
    use agentdiet::cas::Cas;
    use agentdiet::memory::{self, NewRun, Origin, RunKind, RunOutcome};
    use agentdiet::scrubber::scrub;

    let env = Env::with_existing_configs();
    init_everything(&env);
    let put = Cas::new(env.paths.cas_dir())
        .put(&scrub(b"stored\n"), std::time::SystemTime::now())
        .unwrap();
    let (family, title) = (scrub(b"cargo"), scrub(b"cargo test"));
    let run = NewRun {
        origin: Origin::Hook,
        kind: RunKind::Shell,
        family: &family,
        title: &title,
        outcome: RunOutcome::Ok,
        exit_code: Some(0),
        raw_tokens: 900,
        emitted_tokens: 90,
        cas_sha: Some(&put.sha),
        session: None,
    };
    memory::record_run(&env.paths.memory_db(), &run).unwrap();

    let before = env.snapshot();
    let report = doctor(&env);
    run_status(&env);
    assert_eq!(env.snapshot(), before, "no byte, WAL or lock file changed");
    let text = stdout(&report);
    assert!(row(&text, "Claude Code").contains("✓ working"), "{text}");
    assert!(
        row(&text, "Claude Code").contains("1 hook run recorded in this workspace, the last "),
        "{text}"
    );
    assert!(row(&text, "Vault").contains("1 entry"), "{text}");
    assert!(row(&text, "Memory").contains("1 run in"), "{text}");
}

fn run_status(env: &Env) -> Output {
    agentdiet(env)
        .arg("status")
        .env("PATH", shims_first_path(env))
        .output()
        .unwrap()
}

#[test]
fn doctor_on_a_fresh_home_says_nothing_is_connected() {
    let env = Env::empty();
    let out = doctor(&env);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(row(&text, "Setup").contains("✗ broken"), "{text}");
    assert!(text.contains("→ agentdiet init\n"), "{text}");
    assert!(row(&text, "Shims").contains("· not installed"), "{text}");

    let status = stdout(&run(&env, &["status"]));
    assert!(
        row(&status, "Assistants").contains("none connected · agentdiet init"),
        "{status}"
    );
    assert!(
        row(&status, "Health").contains("✗ 1 problem · agentdiet doctor"),
        "{status}"
    );
}

#[test]
fn doctor_passes_a_complete_setup_and_flags_each_kind_of_breakage() {
    let env = Env::with_existing_configs();
    init_everything(&env);
    let ok = doctor(&env);
    let text = stdout(&ok);
    assert_eq!(ok.status.code(), Some(0), "{text}");
    assert!(text.ends_with("All good.\n"), "{text}");
    assert!(row(&text, "Claude Code").contains("✓ configured"), "{text}");
    assert!(row(&text, "PATH").contains("✓ ok"), "{text}");
    let status = agentdiet(&env)
        .arg("status")
        .env("PATH", shims_first_path(&env))
        .output()
        .unwrap();
    assert!(row(&stdout(&status), "Health").contains("✓ no problems found"));

    // A corrupt state.json is a problem; removing it is the fix doctor names.
    env.write(env.paths.state_file(), "{\"status\": 7}");
    let out = doctor(&env);
    assert_eq!(out.status.code(), Some(1));
    assert!(row(&stdout(&out), "State").contains("✗ broken"));
    assert!(stdout(&out).contains("→ rm ~/.agentdiet/state.json"));
    fs::remove_file(env.paths.state_file()).unwrap();
    assert_eq!(doctor(&env).status.code(), Some(0));

    // A config pointing at a binary that no longer exists is a problem.
    let claude_json = env.paths.home.join(".claude.json");
    let original = env.read(&claude_json);
    let mut cfg: Value = serde_json::from_str(&original).unwrap();
    cfg["mcpServers"]["agentdiet"]["command"] = "/gone/bin/agentdiet".into();
    env.write(claude_json.clone(), &cfg.to_string());
    let out = doctor(&env);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(row(&text, "Claude Code").contains("✗ broken"), "{text}");
    assert!(
        text.contains("configured binary /gone/bin/agentdiet is missing"),
        "{text}"
    );

    // Another agentdiet that exists is only a warning, and doctor does not push `init` from here.
    let other = env.dir.path().join("other/agentdiet");
    env.write(other.clone(), "");
    cfg["mcpServers"]["agentdiet"]["command"] = other.display().to_string().into();
    env.write(claude_json.clone(), &cfg.to_string());
    let out = doctor(&env);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(
        row(&text, "Claude Code").contains("⚠ warning")
            && row(&text, "Claude Code").contains("uses a different agentdiet"),
        "{text}"
    );
    assert!(!text.contains("init --only claude"), "{text}");
    env.write(claude_json, &original);

    // A config that is not strict JSON can be neither checked nor updated.
    let cursor = env.paths.home.join(".cursor/mcp.json");
    env.write(cursor, "{ // agentdiet\n \"mcpServers\": {} }");
    let text = stdout(&doctor(&env));
    assert!(
        row(&text, "Cursor").contains("⚠ warning")
            && row(&text, "Cursor").contains("~/.cursor/mcp.json is not strict JSON"),
        "{text}"
    );

    // Shims that are installed but not on PATH are never reached.
    let out = agentdiet(&env)
        .arg("doctor")
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(row(&text, "PATH").contains("✗ broken"), "{text}");
    assert!(
        text.contains("export PATH=\"$HOME/.agentdiet/shims:$PATH\""),
        "{text}"
    );
}

#[test]
fn mode_shows_sets_and_rejects() {
    let env = Env::empty();
    let shown = run(&env, &["mode"]);
    assert!(shown.status.success());
    let text = stdout(&shown);
    assert!(text.starts_with("Diet Mode: Balanced\n"), "{text}");
    assert!(
        text.contains("  ● balanced    outputs over 600 est. tokens"),
        "{text}"
    );
    assert!(!env.paths.state_file().exists(), "showing writes nothing");

    assert_eq!(
        stdout(&run(&env, &["mode", "light"])),
        "Diet Mode set to Light.\n"
    );
    assert_eq!(state(&env)["mode"], "light");
    assert_eq!(
        stdout(&run(&env, &["mode", "light"])),
        "Diet Mode is already Light.\n"
    );
    run(&env, &["mode", "ultra"]);
    assert_eq!(state(&env)["mode"], "ultra-lean");

    let bad = run(&env, &["mode", "ligth"]);
    assert_eq!(bad.status.code(), Some(2));
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(
        err.contains("possible values: light, balanced, ultra-lean"),
        "{err}"
    );
}

#[test]
fn show_last_prints_the_last_trimmed_output_byte_for_byte() {
    use agentdiet::cas::Cas;
    use agentdiet::scrubber::scrub;

    let env = Env::empty();
    let out = run(&env, &["show", "last"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("nothing has been trimmed yet"), "{err}");

    let body = b"one\ntwo\n\xff raw\n";
    let put = Cas::new(env.paths.cas_dir())
        .put(&scrub(body), std::time::SystemTime::now())
        .unwrap();
    env.write(
        env.paths.state_file(),
        &format!("{{\"last_trimmed_id\": \"{}\"}}", put.short_id),
    );
    let by_id = run(&env, &["show", &format!("#{}", put.short_id)]).stdout;
    assert_eq!(by_id, body);
    for args in [&["show", "last"][..], &["show", "LAST"]] {
        let out = run(&env, args);
        assert!(out.status.success());
        assert_eq!(out.stdout, by_id, "{args:?}");
    }
    assert_eq!(
        run(&env, &["show", "last", "--lines", "2..2"]).stdout,
        b"two\n"
    );
    // A bare `show` (what an unquoted `show #id` becomes) is still a usage error, never `last`.
    let bare = run(&env, &["show"]);
    assert_eq!(bare.status.code(), Some(2));
    assert!(bare.stdout.is_empty());
    assert_eq!(
        state(&env)["cas_dereferences"],
        4,
        "every successful show counts"
    );
}

#[test]
fn people_facing_output_has_no_escape_codes_when_piped() {
    let env = Env::with_existing_configs();
    let mut outputs = vec![
        run(&env, &["init", "--all", "--dry-run"]),
        run(&env, &["init", "--all"]),
    ];
    for args in [
        &["status"][..],
        &["doctor"],
        &["mode"],
        &["mode", "light"],
        &["pause"],
    ] {
        outputs.push(run(&env, args));
    }
    for out in outputs {
        assert!(!out.stdout.contains(&0x1b), "{}", stdout(&out));
        assert!(!out.stderr.contains(&0x1b));
    }
}
