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
pub mod scan;
pub mod scrubber;
pub mod session;
pub mod shim;
pub mod state;
pub mod toon;
pub mod top;

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
        }
    }

    /// Resolve paths from the real environment. Called only from `main`.
    pub fn from_env() -> anyhow::Result<Self> {
        use anyhow::Context;
        let home = dirs::home_dir().context("could not determine the home directory")?;
        let config_dir = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
        let project_root =
            std::env::current_dir().context("could not read the current directory")?;
        let exe = std::env::current_exe().context("could not locate the agentdiet executable")?;
        Ok(Self::new(home, config_dir, project_root, exe))
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

    /// Project-local `.agentdiet/` directory.
    pub fn project_data_dir(&self) -> PathBuf {
        self.project_root.join(".agentdiet")
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
