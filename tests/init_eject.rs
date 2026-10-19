mod common;

use std::fs;
use std::path::PathBuf;

use agentdiet::init::{self, InitOptions, Target, eject, json_patch};
#[cfg(unix)]
use agentdiet::shim;
use common::*;
use serde_json::Value;

fn opts(targets: Vec<Target>, project: bool, dry_run: bool) -> InitOptions {
    InitOptions {
        targets,
        project,
        dry_run,
    }
}

fn all() -> InitOptions {
    opts(Target::ALL.to_vec(), false, false)
}

fn json(env: &Env, path: PathBuf) -> Value {
    serde_json::from_str(&env.read(&path)).unwrap()
}

/// Number of agentdiet hook commands registered for `event`.
fn hook_count(settings: &Value, event: &str) -> usize {
    settings["hooks"][event]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .flat_map(|g| g["hooks"].as_array().unwrap())
                .filter(|h| json_patch::is_agentdiet_hook_command(h["command"].as_str().unwrap()))
                .count()
        })
        .unwrap_or(0)
}

#[cfg(unix)]
fn shim_links(env: &Env) -> usize {
    shim::SHIM_COMMANDS
        .iter()
        .filter(|c| fs::symlink_metadata(env.paths.shims_dir().join(c)).is_ok())
        .count()
}

#[test]
fn init_three_times_is_fully_idempotent() {
    let env = Env::with_existing_configs();
    let h = env.paths.home.clone();

    let first = init::run(&env.paths, &all()).unwrap();
    assert!(first.skipped.is_empty(), "{:?}", first.skipped);
    assert!(!first.changed.is_empty());
    let after_first = env.snapshot();

    for run in 2..=3 {
        let report = init::run(&env.paths, &all()).unwrap();
        assert!(
            report.changed.is_empty(),
            "run {run} changed {:?}",
            report.changed
        );
        assert!(report.backup_dir.is_none(), "run {run} made a backup");
        // Byte-identical tree: no rewritten files and no new backup directories.
        assert_eq!(env.snapshot(), after_first, "run {run} altered the tree");
    }

    // Exactly one backup directory, from the first run.
    assert_eq!(fs::read_dir(env.backups()).unwrap().count(), 1);

    // One hook per Claude event, foreign hook preserved.
    let settings = json(&env, h.join(".claude/settings.json"));
    for event in json_patch::CLAUDE_EVENTS {
        assert_eq!(hook_count(&settings, event), 1, "{event}");
    }
    assert_eq!(settings["theme"], "dark");
    assert_eq!(
        settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "echo mine"
    );

    // One MCP entry per JSON config, other servers preserved.
    let mcp_files = [
        (h.join(".claude.json"), "mcpServers"),
        (h.join(".cursor/mcp.json"), "mcpServers"),
        (h.join(".codeium/windsurf/mcp_config.json"), "mcpServers"),
        (h.join(".gemini/settings.json"), "mcpServers"),
        (env.paths.config_dir.join("Code/User/mcp.json"), "servers"),
    ];
    for (path, key) in mcp_files {
        let v = json(&env, path.clone());
        let servers = v[key].as_object().unwrap();
        assert_eq!(
            servers.keys().filter(|k| *k == "agentdiet").count(),
            1,
            "{}",
            path.display()
        );
    }
    assert!(json(&env, h.join(".claude.json"))["mcpServers"]["context7"].is_object());
    assert_eq!(json(&env, h.join(".claude.json"))["numStartups"], 3);

    // One Codex marker block, user content intact.
    let codex = env.read(&h.join(".codex/config.toml"));
    assert_eq!(codex.matches("# >>> agentdiet >>>").count(), 1);
    assert_eq!(codex.matches("[mcp_servers.agentdiet]").count(), 1);
    assert!(codex.starts_with(CODEX_CONFIG));

    #[cfg(unix)]
    assert_eq!(shim_links(&env), shim::SHIM_COMMANDS.len());
}

#[test]
fn eject_restores_exact_originals_and_removes_everything_else() {
    let env = Env::with_existing_configs();
    let pristine = env.snapshot();

    init::run(&env.paths, &all()).unwrap();
    init::run(&env.paths, &all()).unwrap();
    assert_ne!(
        env.snapshot_without_backups(),
        pristine,
        "init changed nothing?"
    );

    let report = eject::eject(&env.paths, None, false).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(!report.restored.is_empty());

    // Exactly the pre-init files, byte for byte — including the file init
    // created (Windsurf) being gone and shims removed.
    assert_eq!(
        env.snapshot_without_backups(),
        strip_agentdiet_dir(pristine)
    );
    assert!(
        !env.paths.home.join(".codeium").exists(),
        "created dirs removed"
    );
    assert!(!env.paths.shims_dir().exists());

    // Backups survive an eject, and a second eject is a no-op.
    assert!(env.backups().exists());
    let before = env.snapshot();
    let second = eject::eject(&env.paths, None, false).unwrap();
    assert!(second.is_empty(), "{second:?}");
    assert_eq!(env.snapshot(), before);
}

fn strip_agentdiet_dir(
    mut snap: std::collections::BTreeMap<PathBuf, Vec<u8>>,
) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    snap.retain(|p, _| !p.components().any(|c| c.as_os_str() == ".agentdiet"));
    snap
}

#[test]
fn eject_after_exe_moved_restores_the_pristine_original() {
    let env = Env::with_existing_configs();
    let pristine = env.snapshot();

    init::run(&env.paths, &all()).unwrap();

    // Reinstall from a new location: entries are updated in place, not duplicated.
    let mut moved = env.paths.clone();
    moved.exe = env.paths.home.join("new/agentdiet");
    let report = init::run(&moved, &all()).unwrap();
    assert!(!report.changed.is_empty());
    let settings = json(&env, env.paths.home.join(".claude/settings.json"));
    for event in json_patch::CLAUDE_EVENTS {
        assert_eq!(hook_count(&settings, event), 1);
    }
    assert!(
        env.read(&env.paths.home.join(".codex/config.toml"))
            .contains("new/agentdiet")
    );
    assert!(init::run(&moved, &all()).unwrap().changed.is_empty());

    // Two generations of backups exist; eject goes back to the earliest.
    assert_eq!(fs::read_dir(env.backups()).unwrap().count(), 2);
    eject::eject(&moved, None, false).unwrap();
    assert_eq!(
        env.snapshot_without_backups(),
        strip_agentdiet_dir(pristine)
    );
}

#[test]
fn eject_keeps_user_edits_made_after_init() {
    let env = Env::with_existing_configs();
    init::run(&env.paths, &all()).unwrap();

    let settings_path = env.paths.home.join(".claude/settings.json");
    let mut settings = json(&env, settings_path.clone());
    settings["added_later"] = Value::Bool(true);
    fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();

    let report = eject::eject(&env.paths, None, false).unwrap();
    assert!(report.cleaned.contains(&settings_path));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("edited after init"))
    );

    let after = json(&env, settings_path);
    assert_eq!(after["added_later"], true, "user edit survives");
    assert_eq!(after["theme"], "dark");
    for event in json_patch::CLAUDE_EVENTS {
        assert_eq!(hook_count(&after, event), 0, "{event}");
    }
    assert_eq!(
        after["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "echo mine"
    );
    // Untouched-by-user files still get the exact original back.
    assert_eq!(
        env.read(&env.paths.home.join(".cursor/mcp.json")),
        CURSOR_MCP
    );
}

#[test]
fn dry_run_changes_nothing_and_prints_diffs() {
    let env = Env::with_existing_configs();
    let before = env.snapshot();
    let report = init::run(&env.paths, &opts(Target::ALL.to_vec(), false, true)).unwrap();

    assert!(!report.diffs.is_empty());
    assert!(!report.changed.is_empty());
    assert!(report.backup_dir.is_none());
    assert_eq!(env.snapshot(), before);
    assert!(!env.paths.agentdiet_home.exists());
    assert!(
        report
            .diffs
            .iter()
            .any(|d| d.contains("+ ") && d.contains("agentdiet"))
    );
}

#[test]
fn project_scope_writes_only_inside_the_project() {
    let env = Env::with_existing_configs();
    let home_before: Vec<_> = env
        .snapshot()
        .into_iter()
        .filter(|(p, _)| p.starts_with("home"))
        .collect();

    let targets: Vec<Target> = Target::ALL
        .into_iter()
        .filter(|t| *t != Target::Shims)
        .collect();
    let report = init::run(&env.paths, &opts(targets, true, false)).unwrap();
    assert!(
        report.skipped.iter().any(|s| s.contains("Windsurf")),
        "windsurf has no project scope: {:?}",
        report.skipped
    );

    let root = &env.paths.project_root;
    for rel in [
        ".claude/settings.json",
        ".mcp.json",
        ".claude/rules/agentdiet.md",
        ".cursor/mcp.json",
        ".gemini/settings.json",
        ".vscode/mcp.json",
        ".codex/config.toml",
    ] {
        assert!(root.join(rel).exists(), "{rel}");
    }
    let settings = json(&env, root.join(".claude/settings.json"));
    assert_eq!(hook_count(&settings, "PreToolUse"), 1);
    assert_eq!(hook_count(&settings, "PostToolUseFailure"), 1);
    let pointer = env.read(&root.join(".claude/rules/agentdiet.md"));
    assert_eq!(pointer.lines().count(), 1, "a one-line pointer: {pointer}");
    assert!(pointer.contains("EXPERIENCE.md"));
    assert!(json(&env, root.join(".vscode/mcp.json"))["servers"]["agentdiet"].is_object());

    // User-level files are untouched.
    let home_after: Vec<_> = env
        .snapshot()
        .into_iter()
        .filter(|(p, _)| p.starts_with("home") && !p.starts_with("home/.agentdiet"))
        .collect();
    let home_before: Vec<_> = home_before
        .into_iter()
        .filter(|(p, _)| !p.starts_with("home/.agentdiet"))
        .collect();
    assert_eq!(home_after, home_before);

    // Project init is idempotent and ejectable too.
    let again = init::run(
        &env.paths,
        &opts(
            Target::ALL
                .into_iter()
                .filter(|t| *t != Target::Shims)
                .collect(),
            true,
            false,
        ),
    )
    .unwrap();
    assert!(again.changed.is_empty());
    eject::eject(&env.paths, None, false).unwrap();
    assert!(!root.join(".mcp.json").exists());
    assert!(!root.join(".claude").exists());
}

#[test]
fn unparseable_configs_are_skipped_and_never_rewritten() {
    let env = Env::with_existing_configs();
    let jsonc = "{\n  // my cursor servers\n  \"mcpServers\": {},\n}\n";
    env.write(env.paths.home.join(".cursor/mcp.json"), jsonc);
    let hand_written = "[mcp_servers.agentdiet]\ncommand = \"mine\"\n";
    env.write(env.paths.home.join(".codex/config.toml"), hand_written);

    let report = init::run(&env.paths, &all()).unwrap();
    assert_eq!(report.skipped.len(), 2, "{:?}", report.skipped);
    assert_eq!(env.read(&env.paths.home.join(".cursor/mcp.json")), jsonc);
    assert_eq!(
        env.read(&env.paths.home.join(".codex/config.toml")),
        hand_written
    );
    // The rest still got connected.
    assert!(
        json(&env, env.paths.home.join(".gemini/settings.json"))["mcpServers"]["agentdiet"]
            .is_object()
    );

    // And eject leaves the skipped files alone.
    eject::eject(&env.paths, None, false).unwrap();
    assert_eq!(env.read(&env.paths.home.join(".cursor/mcp.json")), jsonc);
    assert_eq!(
        env.read(&env.paths.home.join(".codex/config.toml")),
        hand_written
    );
}

#[test]
fn partial_eject_only_touches_the_chosen_targets() {
    let env = Env::with_existing_configs();
    init::run(&env.paths, &all()).unwrap();

    eject::eject(&env.paths, Some(&[Target::Cursor]), false).unwrap();
    assert_eq!(
        env.read(&env.paths.home.join(".cursor/mcp.json")),
        CURSOR_MCP
    );
    assert!(
        json(&env, env.paths.home.join(".gemini/settings.json"))["mcpServers"]["agentdiet"]
            .is_object()
    );
    assert!(init::installed(&env.paths).contains(&Target::Gemini));
    assert!(!init::installed(&env.paths).contains(&Target::Cursor));

    // Full eject afterwards finishes the job.
    eject::eject(&env.paths, None, false).unwrap();
    assert!(init::installed(&env.paths).is_empty());
}

#[test]
fn eject_without_manifest_still_strips_agentdiet_entries() {
    let env = Env::with_existing_configs();
    init::run(&env.paths, &all()).unwrap();
    fs::remove_dir_all(env.backups()).unwrap();

    let report = eject::eject(&env.paths, None, false).unwrap();
    assert!(!report.cleaned.is_empty());
    assert!(init::installed(&env.paths).is_empty());
    let cursor = json(&env, env.paths.home.join(".cursor/mcp.json"));
    assert_eq!(cursor["mcpServers"]["other"]["command"], "o");
    assert!(cursor["mcpServers"].get("agentdiet").is_none());
}

#[test]
fn purge_removes_vault_state_and_project_memory() {
    let env = Env::empty();
    init::run(&env.paths, &opts(vec![Target::Cursor], false, false)).unwrap();
    fs::create_dir_all(env.paths.cas_dir()).unwrap();
    fs::write(env.paths.cas_dir().join("abc.log"), "x").unwrap();
    fs::write(env.paths.state_file(), "{}").unwrap();
    fs::create_dir_all(env.paths.project_data_dir()).unwrap();
    fs::write(env.paths.project_data_dir().join("memory.db"), "x").unwrap();

    // Without --purge the vault is kept.
    eject::eject(&env.paths, None, false).unwrap();
    assert!(env.paths.cas_dir().exists());
    assert!(env.paths.state_file().exists());

    let report = eject::eject(&env.paths, None, true).unwrap();
    assert_eq!(report.purged.len(), 3, "{:?}", report.purged);
    assert!(!env.paths.cas_dir().exists());
    assert!(!env.paths.state_file().exists());
    assert!(!env.paths.project_data_dir().join("memory.db").exists());
    assert!(env.backups().exists(), "backups survive a purge");
}

#[test]
fn rapid_back_to_back_backups_never_collide() {
    // Two real changes within the same millisecond must land in distinct dirs.
    let env = Env::empty();
    let mut moved = env.paths.clone();
    for i in 0..5 {
        moved.exe = env.paths.home.join(format!("v{i}/agentdiet"));
        init::run(&moved, &opts(vec![Target::Cursor], false, false)).unwrap();
    }
    assert_eq!(fs::read_dir(env.backups()).unwrap().count(), 5);
    eject::eject(&moved, None, false).unwrap();
    assert!(
        !env.paths.home.join(".cursor/mcp.json").exists(),
        "back to 'did not exist'"
    );
}
