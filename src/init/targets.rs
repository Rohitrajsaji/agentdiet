//! The assistants agentdiet can connect to, and where each keeps its config.

use std::path::{Path, PathBuf};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use super::json_patch::{self, PatchError};
use super::{rule_pointer, toml_block};
use crate::Paths;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, ValueEnum, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    #[value(alias = "claude-code")]
    Claude,
    Cursor,
    Windsurf,
    #[value(alias = "gemini-cli")]
    Gemini,
    Copilot,
    Codex,
    Shims,
}

impl Target {
    pub const ALL: [Target; 7] = [
        Target::Claude,
        Target::Cursor,
        Target::Windsurf,
        Target::Gemini,
        Target::Copilot,
        Target::Codex,
        Target::Shims,
    ];

    /// Short slug, used as the backup subdirectory name.
    pub fn slug(self) -> &'static str {
        match self {
            Target::Claude => "claude",
            Target::Cursor => "cursor",
            Target::Windsurf => "windsurf",
            Target::Gemini => "gemini",
            Target::Copilot => "copilot",
            Target::Codex => "codex",
            Target::Shims => "shims",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Target::Claude => "Claude Code",
            Target::Cursor => "Cursor",
            Target::Windsurf => "Windsurf",
            Target::Gemini => "Gemini CLI",
            Target::Copilot => "Copilot (VS Code)",
            Target::Codex => "Codex",
            Target::Shims => "Shims (~/.agentdiet/shims)",
        }
    }
}

/// How a config file is patched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PatchKind {
    /// Claude Code `settings.json` hooks.
    ClaudeHooks,
    /// An MCP server entry under `key` of a JSON file.
    Mcp { key: String, typed: bool },
    /// Marker-delimited block in Codex's `config.toml`.
    CodexBlock,
    /// The one-line `EXPERIENCE.md` pointer in a Claude Code rules file.
    RulePointer,
}

impl PatchKind {
    fn mcp(key: &str, typed: bool) -> Self {
        PatchKind::Mcp {
            key: key.to_string(),
            typed,
        }
    }

    /// Ensure agentdiet is present. `Ok(None)` means already up to date.
    pub fn patch(&self, current: Option<&str>, exe: &Path) -> Result<Option<String>, PatchError> {
        match self {
            PatchKind::ClaudeHooks => json_patch::patch_claude_hooks(current, exe),
            PatchKind::Mcp { key, typed } => json_patch::patch_mcp(current, key, *typed, exe),
            PatchKind::CodexBlock => toml_block::patch(current, exe),
            PatchKind::RulePointer => rule_pointer::patch(current),
        }
    }

    /// Remove agentdiet's entries. `Ok(None)` means nothing to remove.
    pub fn unpatch(&self, text: &str) -> Result<Option<String>, PatchError> {
        match self {
            PatchKind::ClaudeHooks => json_patch::unpatch_claude_hooks(text),
            PatchKind::Mcp { key, .. } => json_patch::unpatch_mcp(text, key),
            PatchKind::CodexBlock => toml_block::unpatch(text),
            PatchKind::RulePointer => rule_pointer::unpatch(text),
        }
    }

    /// The agentdiet executable the entry in `text` runs. `None` for the rules pointer, which runs
    /// nothing, and for an entry without a readable command.
    pub fn configured_exe(&self, text: &str) -> Option<PathBuf> {
        match self {
            PatchKind::ClaudeHooks => json_patch::claude_hook_program(text),
            PatchKind::Mcp { key, .. } => json_patch::mcp_command(text, key),
            PatchKind::CodexBlock => toml_block::block_command(text),
            PatchKind::RulePointer => None,
        }
    }

    pub fn is_present(&self, text: &str) -> bool {
        match self {
            PatchKind::ClaudeHooks => json_patch::has_claude_hooks(text),
            PatchKind::Mcp { key, .. } => json_patch::has_mcp(text, key),
            PatchKind::CodexBlock => toml_block::has_block(text),
            PatchKind::RulePointer => rule_pointer::is_present(text),
        }
    }
}

/// One config file agentdiet manages for a target.
#[derive(Debug, Clone)]
pub struct FileSpec {
    pub path: PathBuf,
    pub kind: PatchKind,
}

fn spec(path: PathBuf, kind: PatchKind) -> FileSpec {
    FileSpec { path, kind }
}

/// The config files for `target`, or `None` if it has no config at that scope
/// (shims are not a config file; Windsurf has no project-level MCP config).
pub fn files(paths: &Paths, target: Target, project: bool) -> Option<Vec<FileSpec>> {
    let (home, root) = (&paths.home, &paths.project_root);
    Some(match (target, project) {
        (Target::Shims, _) | (Target::Windsurf, true) => return None,
        (Target::Claude, false) => vec![
            spec(home.join(".claude/settings.json"), PatchKind::ClaudeHooks),
            spec(
                home.join(".claude.json"),
                PatchKind::mcp("mcpServers", true),
            ),
            spec(
                home.join(".claude/rules/agentdiet.md"),
                PatchKind::RulePointer,
            ),
        ],
        (Target::Claude, true) => vec![
            spec(root.join(".claude/settings.json"), PatchKind::ClaudeHooks),
            spec(root.join(".mcp.json"), PatchKind::mcp("mcpServers", true)),
            spec(
                root.join(".claude/rules/agentdiet.md"),
                PatchKind::RulePointer,
            ),
        ],
        (Target::Cursor, false) => vec![spec(
            home.join(".cursor/mcp.json"),
            PatchKind::mcp("mcpServers", false),
        )],
        (Target::Cursor, true) => vec![spec(
            root.join(".cursor/mcp.json"),
            PatchKind::mcp("mcpServers", false),
        )],
        (Target::Windsurf, false) => vec![spec(
            home.join(".codeium/windsurf/mcp_config.json"),
            PatchKind::mcp("mcpServers", false),
        )],
        (Target::Gemini, false) => vec![spec(
            home.join(".gemini/settings.json"),
            PatchKind::mcp("mcpServers", false),
        )],
        (Target::Gemini, true) => vec![spec(
            root.join(".gemini/settings.json"),
            PatchKind::mcp("mcpServers", false),
        )],
        (Target::Copilot, false) => vec![spec(
            paths.config_dir.join("Code/User/mcp.json"),
            PatchKind::mcp("servers", true),
        )],
        (Target::Copilot, true) => vec![spec(
            root.join(".vscode/mcp.json"),
            PatchKind::mcp("servers", true),
        )],
        (Target::Codex, false) => {
            vec![spec(home.join(".codex/config.toml"), PatchKind::CodexBlock)]
        }
        (Target::Codex, true) => vec![spec(root.join(".codex/config.toml"), PatchKind::CodexBlock)],
    })
}

/// Directory whose existence suggests `target` is installed (for the
/// interactive checklist).
fn marker_dir(paths: &Paths, target: Target, project: bool) -> Option<PathBuf> {
    let base = if project {
        &paths.project_root
    } else {
        &paths.home
    };
    Some(match target {
        Target::Claude => base.join(".claude"),
        Target::Cursor => base.join(".cursor"),
        Target::Windsurf if !project => base.join(".codeium/windsurf"),
        Target::Windsurf => return None,
        Target::Gemini => base.join(".gemini"),
        Target::Copilot if project => base.join(".vscode"),
        Target::Copilot => paths.config_dir.join("Code/User"),
        Target::Codex => base.join(".codex"),
        Target::Shims => return None,
    })
}

/// Targets whose assistant looks installed. Shims are always offered.
pub fn detect(paths: &Paths, project: bool) -> Vec<Target> {
    Target::ALL
        .into_iter()
        .filter(|&t| match t {
            Target::Shims => !project,
            Target::Windsurf if project => false,
            _ => marker_dir(paths, t, project).is_some_and(|d| d.is_dir()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Paths {
        Paths::new(
            "/h".into(),
            "/h/cfg".into(),
            "/p".into(),
            "/h/bin/agentdiet".into(),
        )
    }

    #[test]
    fn copilot_uses_servers_key_and_vscode_user_dir() {
        let f = files(&paths(), Target::Copilot, false).unwrap();
        assert_eq!(f[0].path, PathBuf::from("/h/cfg/Code/User/mcp.json"));
        assert_eq!(f[0].kind, PatchKind::mcp("servers", true));
    }

    #[test]
    fn windsurf_has_no_project_scope_and_shims_have_no_files() {
        assert!(files(&paths(), Target::Windsurf, true).is_none());
        assert!(files(&paths(), Target::Shims, false).is_none());
    }

    #[test]
    fn project_scope_stays_inside_the_project() {
        for t in Target::ALL {
            for f in files(&paths(), t, true).into_iter().flatten() {
                assert!(f.path.starts_with("/p"), "{t:?}: {}", f.path.display());
            }
        }
    }

    #[test]
    fn claude_gets_hooks_and_mcp() {
        let f = files(&paths(), Target::Claude, false).unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].kind, PatchKind::ClaudeHooks);
        assert_eq!(f[2].kind, PatchKind::RulePointer);
        assert!(f[2].path.ends_with(".claude/rules/agentdiet.md"));
    }
}
