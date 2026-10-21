//! Foundation hardening, end to end. Everything that depends on the process
//! umask runs in a **child process** (`sh -c 'umask …; exec …'`), never by
//! changing the umask or environment of this parallel test process.
//!
//! Unix only: umasks and file modes do not exist on Windows (SPEC §10: files there are protected by
//! the user profile's ACL).
#![cfg(unix)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::Env;

const BIN: &str = env!("CARGO_BIN_EXE_agentdiet");

/// Run `program args…` in a child with the given umask and an isolated HOME.
fn run_with_umask(env: &Env, umask: &str, program: &str, args: &[&str]) -> Output {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg("umask \"$1\" && shift && exec \"$@\"")
        .arg("sh")
        .arg(umask)
        .arg(program)
        .args(args)
        .env("HOME", &env.paths.home)
        .env("XDG_CONFIG_HOME", &env.paths.config_dir)
        .env("USERPROFILE", &env.paths.home)
        .current_dir(&env.paths.project_root)
        .stdin(Stdio::null());
    cmd.output().unwrap()
}

fn agentdiet(env: &Env, umask: &str, args: &[&str]) -> Output {
    run_with_umask(env, umask, BIN, args)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn assert_ok(out: &Output) {
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Every file and directory under `root` (not following symlinks), root excluded.
fn tree(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if fs::symlink_metadata(&path).unwrap().is_dir() {
                stack.push(path.clone());
            }
            out.push(path);
        }
    }
    out.sort();
    out
}

// ---- 1. config backups are private from creation, and still byte-exact -----

#[cfg(unix)]
#[test]
fn config_backups_are_private_and_byte_exact_under_any_umask() {
    for umask in ["022", "000"] {
        let env = Env::with_existing_configs();
        let settings = env.paths.home.join(".claude/settings.json");
        let claude_json = env.paths.home.join(".claude.json");
        set_mode(&settings, 0o600);
        set_mode(&claude_json, 0o600);
        let settings_before = fs::read(&settings).unwrap();
        let claude_json_before = fs::read(&claude_json).unwrap();

        assert_ok(&agentdiet(&env, umask, &["init", "--only", "claude"]));

        // The rewritten configs keep their private mode.
        assert_eq!(mode(&settings), 0o600, "umask {umask}");
        assert_eq!(mode(&claude_json), 0o600, "umask {umask}");

        // Everything under the backups directory is 0700 / 0600, temp files included.
        let backups = env.backups();
        assert_eq!(mode(&backups), 0o700, "umask {umask}");
        let entries = tree(&backups);
        assert!(
            entries.len() >= 5,
            "expected backups + manifest, got {entries:?}"
        );
        for path in &entries {
            let want = if path.is_dir() { 0o700 } else { 0o600 };
            assert_eq!(mode(path), want, "umask {umask}: {}", path.display());
            assert!(
                !path.to_string_lossy().contains("agentdiet-tmp"),
                "temp file left behind: {}",
                path.display()
            );
        }

        // The backup contract: byte-exact copies, so eject can restore them exactly.
        let backed_up = |name: &str| {
            entries
                .iter()
                .find(|p| p.file_name().is_some_and(|n| n == name))
                .unwrap_or_else(|| panic!("no backup named {name}"))
        };
        assert_eq!(
            fs::read(backed_up("settings.json")).unwrap(),
            settings_before
        );
        assert_eq!(
            fs::read(backed_up(".claude.json")).unwrap(),
            claude_json_before
        );

        assert_ok(&agentdiet(&env, umask, &["eject"]));
        assert_eq!(
            fs::read(&settings).unwrap(),
            settings_before,
            "umask {umask}"
        );
        assert_eq!(
            fs::read(&claude_json).unwrap(),
            claude_json_before,
            "umask {umask}"
        );
        assert_eq!(mode(&settings), 0o600, "umask {umask}");
    }
}

#[cfg(unix)]
#[test]
fn backups_of_world_readable_configs_are_still_private() {
    let env = Env::with_existing_configs();
    let cursor = env.paths.home.join(".cursor/mcp.json");
    set_mode(&cursor, 0o644);
    assert_ok(&agentdiet(&env, "000", &["init", "--only", "cursor"]));
    for path in tree(&env.backups()) {
        let want = if path.is_dir() { 0o700 } else { 0o600 };
        assert_eq!(mode(&path), want, "{}", path.display());
    }
    // The live config keeps its own (wider) mode: that is the user's choice.
    assert_eq!(mode(&cursor), 0o644);
}

#[cfg(unix)]
#[test]
fn a_new_state_dir_is_private() {
    let env = Env::empty();
    assert_ok(&agentdiet(&env, "000", &["pause"]));
    assert_eq!(mode(&env.paths.agentdiet_home), 0o700);
    assert_eq!(mode(&env.paths.state_file()), 0o600);
}

// ---- 1b. the vault, under a permissive umask, in a re-executed child -------

/// Child half of [`vault_is_private_under_a_permissive_umask`]: only runs when
/// the parent re-executes this binary with `AD_VAULT_CHILD_DIR` set.
#[test]
#[ignore = "child-process helper; run by vault_is_private_under_a_permissive_umask"]
fn vault_child_writes_entries() {
    use agentdiet::cas::Cas;
    use agentdiet::scrubber::scrub;
    use std::time::{Duration, SystemTime};

    let Some(dir) = std::env::var_os("AD_VAULT_CHILD_DIR") else {
        return;
    };
    let cas = Cas::new(PathBuf::from(dir));
    let now = SystemTime::now();
    cas.put(
        &scrub(b"expires soon\n"),
        now - Duration::from_secs(8 * 24 * 3600),
    )
    .unwrap();
    cas.put(&scrub(b"stays\n"), now).unwrap(); // GC tombstones the first entry
}

#[cfg(unix)]
#[test]
fn vault_is_private_under_a_permissive_umask() {
    let env = Env::empty();
    let cas_dir = env.paths.cas_dir();
    let exe = std::env::current_exe().unwrap();
    let out = Command::new("sh")
        .arg("-c")
        .arg("umask 000 && exec \"$@\"")
        .arg("sh")
        .arg(&exe)
        .args([
            "--ignored",
            "--exact",
            "vault_child_writes_entries",
            "--nocapture",
        ])
        .env("AD_VAULT_CHILD_DIR", &cas_dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_ok(&out);

    let entries = tree(&cas_dir);
    let names: Vec<String> = entries
        .iter()
        .filter_map(|p| p.extension())
        .map(|e| e.to_string_lossy().into_owned())
        .collect();
    assert!(
        names.contains(&"log".to_string()) && names.contains(&"expired".to_string()),
        "{names:?}"
    );
    // The vault's lock file is private too (it has no extension, hence not in `names`).
    assert!(cas_dir.join(".lock").is_file());
    assert_eq!(mode(&cas_dir), 0o700);
    assert_eq!(mode(&env.paths.agentdiet_home), 0o700);
    for path in entries {
        assert_eq!(mode(&path), 0o600, "{}", path.display());
    }
}

// ---- 2. Codex config.toml: conflicts stay byte-identical, valid stays valid -

fn codex_path(env: &Env) -> PathBuf {
    env.paths.home.join(".codex/config.toml")
}

fn valid_toml(text: &str) -> toml::Table {
    toml::from_str(text).unwrap_or_else(|e| panic!("not valid TOML: {e}\n{text}"))
}

#[test]
fn codex_conflicts_are_skipped_with_a_warning_and_left_byte_identical() {
    let multiline_markers = "note = '''\n# >>> agentdiet >>>\nx\n# <<< agentdiet <<<\n'''\n";
    let duplicate_blocks = format!(
        "{b}\n{b}",
        b = "# >>> agentdiet >>>\n[mcp_servers.agentdiet]\ncommand = \"a\"\nargs = [\"mcp\"]\n# <<< agentdiet <<<\n"
    );
    let conflicts: Vec<(&str, String)> = vec![
        (
            // The reproduced bug: trailing comment after the table header.
            "trailing comment",
            "[mcp_servers.agentdiet] # existing user config\ncommand = \"existing-binary\"\n"
                .into(),
        ),
        (
            "quoted key",
            "[mcp_servers.\"agentdiet\"]\ncommand = \"x\"\n".into(),
        ),
        (
            "dotted key",
            "mcp_servers.agentdiet.command = \"x\"\n".into(),
        ),
        (
            "inline table",
            "mcp_servers = { agentdiet = { command = \"x\" } }\n".into(),
        ),
        ("markers in a string", multiline_markers.into()),
        ("duplicate blocks", duplicate_blocks),
        ("not toml", "this = = broken\n".into()),
    ];

    for (label, original) in conflicts {
        let env = Env::empty();
        env.write(codex_path(&env), &original);
        let before = env.snapshot_without_backups();

        let out = agentdiet(&env, "022", &["init", "--only", "codex"]);
        assert_ok(&out);
        let combined = format!("{}{}", text(&out.stdout), text(&out.stderr));
        assert!(
            combined.contains("left untouched"),
            "{label}: no warning in {combined}"
        );

        assert_eq!(
            env.read(&codex_path(&env)),
            original,
            "{label}: file changed"
        );
        assert_eq!(
            env.snapshot_without_backups(),
            before,
            "{label}: something else changed"
        );
        assert!(
            !env.backups().exists(),
            "{label}: a no-op must leave no backup"
        );

        // And eject must not strip what it cannot prove is its own.
        assert_ok(&agentdiet(&env, "022", &["eject"]));
        assert_eq!(
            env.read(&codex_path(&env)),
            original,
            "{label}: eject changed the file"
        );
    }
}

#[test]
fn valid_codex_configs_stay_valid_and_round_trip_through_eject() {
    let fixtures = [
        "model = \"o3\" # keep me\n\n[profiles.fast]\nx = 1\n",
        "# only a comment",
        "[mcp_servers.context7]\ncommand = \"npx\"\nargs = [\"-y\", \"ctx\"]\n",
        "s = \"\"\"\nline # not a comment\n\"\"\"\n[t]\nk = 1",
    ];
    for original in fixtures {
        let env = Env::empty();
        env.write(codex_path(&env), original);
        let before = valid_toml(original);

        assert_ok(&agentdiet(&env, "022", &["init", "--only", "codex"]));
        let patched = env.read(&codex_path(&env));
        assert!(patched.starts_with(original), "{original:?}");
        let after = valid_toml(&patched);
        assert_eq!(
            after["mcp_servers"]["agentdiet"]["args"][0].as_str(),
            Some("mcp")
        );
        for (key, value) in &before {
            if key != "mcp_servers" {
                assert_eq!(after.get(key), Some(value), "{key} changed");
            }
        }

        // Second run is a byte-identical no-op.
        assert_ok(&agentdiet(&env, "022", &["init", "--only", "codex"]));
        assert_eq!(env.read(&codex_path(&env)), patched);

        assert_ok(&agentdiet(&env, "022", &["eject"]));
        assert_eq!(env.read(&codex_path(&env)), original);
    }
}

// ---- private roots (SPEC §3 Tier 3 "Private roots") --------------------------------------------

/// A trimmed `diet_exec` through the MCP server, the way an assistant runs one; its text.
fn trimmed_mcp_call(env: &Env) -> String {
    use std::io::Write as _;
    let mut child = Command::new(BIN)
        .arg("mcp")
        .env("HOME", &env.paths.home)
        .env("USERPROFILE", &env.paths.home)
        .env_remove("AGENTDIET_WORKSPACE")
        .current_dir(&env.paths.project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let session = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"diet_exec","arguments":{"command":["seq","1","4000"]}}}"#,
        "\n"
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(session.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let last: serde_json::Value =
        serde_json::from_str(text(&out.stdout).lines().last().unwrap()).unwrap();
    let shown = last["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(shown.contains("🥗 Diet: "), "still trimmed: {shown}");
    shown
}

fn doctor_check(env: &Env, name: &str) -> serde_json::Value {
    let out = agentdiet(env, "022", &["doctor", "--json"]);
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["check"] == name)
        .unwrap()
        .clone()
}

#[test]
fn a_symlinked_project_agentdiet_is_refused_and_nothing_is_written_through_it() {
    let env = Env::empty();
    fs::create_dir_all(env.paths.project_root.join(".git")).unwrap();
    // What a cloned repository can ship: `.agentdiet` pointing at another of the user's directories.
    let elsewhere = env.dir.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, env.paths.project_root.join(".agentdiet")).unwrap();

    trimmed_mcp_call(&env); // fails open: the output is trimmed and in the Vault
    assert_eq!(
        fs::read_dir(&elsewhere).unwrap().count(),
        0,
        "nothing written through the link"
    );
    let memory = doctor_check(&env, "memory");
    assert_eq!(memory["level"], "broken", "{memory}");
    assert!(
        memory["detail"].as_str().unwrap().contains("is a symlink"),
        "{memory}"
    );
    assert!(
        memory["fix"]
            .as_str()
            .unwrap()
            .contains("AGENTDIET_WORKSPACE"),
        "{memory}"
    );
}

#[test]
fn a_loose_project_agentdiet_is_reported_then_tightened_by_the_first_write() {
    let env = Env::empty();
    fs::create_dir_all(env.paths.project_root.join(".git")).unwrap();
    let root = env.paths.project_root.join(".agentdiet");
    fs::create_dir_all(&root).unwrap();
    set_mode(&root, 0o755); // as a checkout or a manual mkdir leaves it

    let memory = doctor_check(&env, "memory");
    assert_eq!(memory["level"], "warning", "{memory}");
    assert!(
        memory["detail"].as_str().unwrap().contains("0755"),
        "{memory}"
    );
    assert_eq!(mode(&root), 0o755, "doctor never writes");

    trimmed_mcp_call(&env);
    assert_eq!(mode(&root), 0o700);
    assert!(root.join("memory.db").is_file());
    assert_eq!(doctor_check(&env, "memory")["level"], "ok");
}

#[test]
fn a_home_agentdiet_linked_to_the_users_own_directory_is_followed() {
    let env = Env::empty();
    let target = env.dir.path().join("dotfiles/agentdiet");
    fs::create_dir_all(&target).unwrap();
    set_mode(&target, 0o700);
    fs::create_dir_all(&env.paths.home).unwrap();
    std::os::unix::fs::symlink(&target, &env.paths.agentdiet_home).unwrap();

    assert_ok(&agentdiet(&env, "022", &["pause"]));
    assert!(
        target.join("state.json").is_file(),
        "state lives in the link's target"
    );
    assert_ok(&agentdiet(&env, "022", &["resume"]));
    trimmed_mcp_call(&env);
    assert!(target.join("cas").is_dir(), "and the Vault");
    assert_eq!(doctor_check(&env, "vault")["level"], "ok");

    // A link to nothing is refused, and doctor says how to fix it.
    fs::remove_file(&env.paths.agentdiet_home).unwrap();
    std::os::unix::fs::symlink(env.dir.path().join("missing"), &env.paths.agentdiet_home).unwrap();
    assert!(!agentdiet(&env, "022", &["pause"]).status.success());
    let vault = doctor_check(&env, "vault");
    assert_eq!(vault["level"], "broken", "{vault}");
}
