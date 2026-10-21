//! AgentDiet — a context firewall for AI coding agents.
//!
//! Slice 1: workspace scaffold, state ledger (pause/resume), Control Center,
//! and `init` / `eject`. Slice 2: secret scrubber, CAS vault and project
//! memory index. Slice 3B: the Diet Engine (`diet`, `compress`, `toon`,
//! `folding`). Slice 4: the hook router (`hook`), agent-path shims (`shim`) and
//! per-session sidecars (`session`). Slice 5: the MCP tools (`mcp`), the static scanner (`scan`)
//! and the `agentdiet top` dashboard (`top`).

use std::path::{Path, PathBuf};

pub mod cas;
pub mod center;
pub mod cli;
pub mod compress;
pub mod control;
pub mod diet;
pub mod folding;
pub mod fsutil;
pub mod hook;
pub mod init;
pub mod mcp;
pub mod memory;
pub mod reflect;
pub mod runlog;
pub mod scan;
pub mod scrubber;
pub mod session;
pub mod shim;
pub mod state;
pub mod stats;
pub mod status;
pub mod toon;
pub mod top;
pub mod tui;
pub mod ui;
pub mod words;

/// Every filesystem location the library touches, injected by the caller.
///
/// Only `main` builds this from the real environment ([`Paths::from_env`]);
/// everything else — and every test — receives it, so nothing in the library
/// reaches for the real `~/` on its own.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The user's home directory (`~`).
    pub home: PathBuf,
    /// `~/.agentdiet` — state, backups, shims, vault.
    pub agentdiet_home: PathBuf,
    /// Platform config dir (`~/Library/Application Support`, `~/.config`, ...).
    pub config_dir: PathBuf,
    /// Root of the current project, for `--project` scope.
    pub project_root: PathBuf,
    /// The running `agentdiet` executable, written into assistant configs.
    pub exe: PathBuf,
    /// [`WORKSPACE_ENV`]: the workspace that holds memory.db, overriding discovery.
    pub workspace_override: Option<PathBuf>,
}

/// Names the workspace whose `.agentdiet/memory.db` every entry point uses, overriding discovery.
pub const WORKSPACE_ENV: &str = "AGENTDIET_WORKSPACE";
/// Discovery walks at most this many directories up.
const MAX_WORKSPACE_HOPS: usize = 64;

/// The workspace for `start`: the nearest directory, `start` itself included, that already has a
/// `.agentdiet/memory.db` or is a git root (`.git`, a directory or a worktree's file), so a command
/// run in `app/src` records into the same memory.db as one run at the top. `home` is never a
/// workspace (its `.agentdiet` is the global directory) and the walk never goes above it. With no
/// marker, `start` itself.
pub fn discover_workspace(start: &Path, home: &Path) -> PathBuf {
    for dir in start.ancestors().take(MAX_WORKSPACE_HOPS) {
        if dir == home {
            break;
        }
        let has_db = std::fs::symlink_metadata(dir.join(".agentdiet").join("memory.db"))
            .is_ok_and(|m| m.file_type().is_file());
        if has_db || std::fs::symlink_metadata(dir.join(".git")).is_ok() {
            return dir.to_path_buf();
        }
    }
    start.to_path_buf()
}

/// The home directory: `$HOME` on Unix; on Windows `%USERPROFILE%`, which is what Node's
/// `os.homedir()` (Claude Code, Gemini CLI, VS Code) reads, then the profile folder Windows reports
/// (SPEC §10). `dirs` alone asks only Windows, ignoring the environment.
fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(dir) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    dirs::home_dir()
}

/// The platform config directory: `%APPDATA%` first on Windows (where VS Code keeps `Code\User`),
/// as for [`home_dir`]; the `dirs` lookup otherwise.
fn config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(dir) = std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    dirs::config_dir()
}

impl Paths {
    /// Build paths from explicit locations; `agentdiet_home` is `<home>/.agentdiet`.
    pub fn new(home: PathBuf, config_dir: PathBuf, project_root: PathBuf, exe: PathBuf) -> Self {
        let agentdiet_home = home.join(".agentdiet");
        Self {
            home,
            agentdiet_home,
            config_dir,
            project_root,
            exe,
            workspace_override: None,
        }
    }

    /// Resolve paths from the real environment. Called only from `main`.
    pub fn from_env() -> anyhow::Result<Self> {
        use anyhow::Context;
        let home = home_dir().context("could not determine the home directory")?;
        let config_dir = config_dir().unwrap_or_else(|| home.join(".config"));
        let project_root =
            std::env::current_dir().context("could not read the current directory")?;
        let exe = std::env::current_exe().context("could not locate the agentdiet executable")?;
        let mut paths = Self::new(home, config_dir, project_root, exe);
        paths.workspace_override = std::env::var_os(WORKSPACE_ENV)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        Ok(paths)
    }

    pub fn state_file(&self) -> PathBuf {
        self.agentdiet_home.join("state.json")
    }

    /// Sibling lock file serialising every read-modify-write of `state.json`.
    pub fn state_lock_file(&self) -> PathBuf {
        self.agentdiet_home.join("state.json.lock")
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.agentdiet_home.join("backups")
    }

    pub fn shims_dir(&self) -> PathBuf {
        self.agentdiet_home.join("shims")
    }

    pub fn cas_dir(&self) -> PathBuf {
        self.agentdiet_home.join("cas")
    }

    /// Per-session sidecars: the raw-turn flag and retry signatures (SPEC §2.4).
    pub fn sessions_dir(&self) -> PathBuf {
        self.agentdiet_home.join("sessions")
    }

    /// Project-local `memory.db` (SQLite, WAL, FTS5).
    pub fn memory_db(&self) -> PathBuf {
        self.project_data_dir().join("memory.db")
    }

    /// Project-local `.agentdiet/` directory, in the [`Paths::workspace_root`].
    pub fn project_data_dir(&self) -> PathBuf {
        self.workspace_root().join(".agentdiet")
    }

    /// The workspace that holds memory.db and EXPERIENCE.md: [`WORKSPACE_ENV`] when it names a
    /// directory (an absolute path, or one relative to `project_root`), else
    /// [`discover_workspace`] from `project_root`. `init --project` keeps writing assistant
    /// configs to `project_root` itself.
    pub fn workspace_root(&self) -> PathBuf {
        if let Some(dir) = &self.workspace_override {
            let dir = self.project_root.join(dir);
            if dir.is_dir() {
                return dir;
            }
        }
        discover_workspace(&self.project_root, &self.home)
    }

    /// `EXPERIENCE.md`, the `agentdiet reflect` output, at the workspace root.
    pub fn experience_file(&self) -> PathBuf {
        self.workspace_root().join("EXPERIENCE.md")
    }
}

/// Final path component as a `&str`, treating both `/` and `\` as separators
/// (a Windows path can show up in a config read on another platform).
pub fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// `true` if `path` names an `agentdiet` executable.
pub fn is_agentdiet_exe(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|p| matches!(basename(p), "agentdiet" | "agentdiet.exe"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn a_nested_directory_finds_the_git_root_or_an_existing_memory_db() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        let repo = home.join("code/app");
        let nested = repo.join("packages/web/src");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            discover_workspace(&nested, &home),
            nested,
            "no marker: the start"
        );

        fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(discover_workspace(&nested, &home), repo);
        assert_eq!(discover_workspace(&repo, &home), repo);

        // A memory.db already in a package wins over the git root above it (nearest marker).
        let web = repo.join("packages/web");
        fs::create_dir_all(web.join(".agentdiet")).unwrap();
        assert_eq!(
            discover_workspace(&nested, &home),
            repo,
            "an empty .agentdiet is no marker"
        );
        fs::write(web.join(".agentdiet/memory.db"), b"").unwrap();
        assert_eq!(discover_workspace(&nested, &home), web);
    }

    #[test]
    fn home_and_its_global_agentdiet_dir_are_never_a_workspace() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(home.join(".agentdiet")).unwrap();
        fs::write(home.join(".agentdiet/memory.db"), b"").unwrap();
        fs::create_dir_all(home.join(".git")).unwrap();
        let project = home.join("scratch/x");
        fs::create_dir_all(&project).unwrap();
        assert_eq!(discover_workspace(&project, &home), project);
        assert_eq!(
            discover_workspace(&home, &home),
            home,
            "home itself: unchanged start"
        );
    }

    #[test]
    fn the_workspace_override_wins_when_it_is_a_directory() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project/sub");
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        let mut paths = Paths::new(
            dir.path().join("home"),
            dir.path().join("config"),
            project.clone(),
            "/x/agentdiet".into(),
        );
        assert_eq!(paths.workspace_root(), project);
        paths.workspace_override = Some(elsewhere.clone());
        assert_eq!(paths.memory_db(), elsewhere.join(".agentdiet/memory.db"));
        paths.workspace_override = Some(dir.path().join("missing"));
        assert_eq!(paths.workspace_root(), project, "not a directory: ignored");
    }
}
