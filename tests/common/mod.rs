//! Shared fixtures: an isolated fake `~/`, config dir and project, all inside a `TempDir`.
#![allow(dead_code)] // each test binary uses a different subset

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use agentdiet::Paths;
use tempfile::TempDir;

pub struct Env {
    pub dir: TempDir,
    pub paths: Paths,
}

/// Deliberately awkward formatting and key order, so any rewrite of an
/// untouched file would show up as a byte difference.
pub const CLAUDE_SETTINGS: &str = "{\n    \"theme\":   \"dark\",\n  \"hooks\": {\"PreToolUse\": [ {\"matcher\":\"Bash\",\"hooks\":[{\"type\":\"command\",\"command\":\"echo mine\"}]} ]},\n\"model\": \"x\" }";
pub const CLAUDE_JSON: &str = "{ \"numStartups\":   3,\n  \"mcpServers\": {\"context7\": {\"command\": \"npx\"}} }\n";
pub const CURSOR_MCP: &str = "{\"mcpServers\":{\"other\":{\"command\":\"o\"}}}";
pub const GEMINI_SETTINGS: &str = "{\n\t\"theme\": \"GitHub\"\n}\n";
pub const COPILOT_MCP: &str = "{ \"servers\": {}, \"inputs\": [] }";
pub const CODEX_CONFIG: &str = "model = \"o3\"\n\n[mcp_servers.context7]\ncommand = \"npx\"";

impl Env {
    /// Empty home: nothing pre-installed.
    pub fn empty() -> Self {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        let config = dir.path().join("config");
        let project = dir.path().join("project");
        for d in [&home, &config, &project] {
            fs::create_dir_all(d).unwrap();
        }
        let exe = home.join("bin/agentdiet");
        let paths = Paths::new(home, config, project, exe);
        Env { dir, paths }
    }

    /// A home where the user already has configs for every assistant.
    pub fn with_existing_configs() -> Self {
        let env = Self::empty();
        let h = &env.paths.home;
        env.write(h.join(".claude/settings.json"), CLAUDE_SETTINGS);
        env.write(h.join(".claude.json"), CLAUDE_JSON);
        env.write(h.join(".cursor/mcp.json"), CURSOR_MCP);
        env.write(h.join(".gemini/settings.json"), GEMINI_SETTINGS);
        env.write(env.paths.config_dir.join("Code/User/mcp.json"), COPILOT_MCP);
        env.write(h.join(".codex/config.toml"), CODEX_CONFIG);
        // Windsurf is deliberately absent: init must create it, eject must remove it.
        env
    }

    pub fn write(&self, path: PathBuf, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    pub fn read(&self, path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    pub fn backups(&self) -> PathBuf {
        self.paths.backups_dir()
    }

    /// Every file and symlink under the temp dir, keyed by path relative to it.
    pub fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        walk(self.dir.path(), self.dir.path(), &mut out);
        out
    }

    /// Like [`snapshot`], without anything under `~/.agentdiet/backups`.
    pub fn snapshot_without_backups(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let skip = self.backups().strip_prefix(self.dir.path()).unwrap().to_path_buf();
        self.snapshot().into_iter().filter(|(p, _)| !p.starts_with(&skip)).collect()
    }
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap().to_path_buf();
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&path).unwrap();
            out.insert(rel, format!("-> {}", target.display()).into_bytes());
        } else if meta.is_dir() {
            walk(root, &path, out);
        } else {
            out.insert(rel, fs::read(&path).unwrap());
        }
    }
}
