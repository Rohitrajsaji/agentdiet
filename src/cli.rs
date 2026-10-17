//! Command-line surface (clap derive).

use clap::{Parser, Subcommand};

use crate::init::Target;

#[derive(Debug, Parser)]
#[command(
    name = "agentdiet",
    version,
    about = "Context firewall for AI coding agents",
    long_about = "AgentDiet trims what AI coding agents see from tool output, and keeps the raw \
                  output recoverable. Run with no arguments to open the Control Center."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Connect AgentDiet to your AI assistants
    Init {
        /// Comma-separated targets: claude, cursor, windsurf, gemini, copilot, codex, shims
        #[arg(long, value_enum, value_delimiter = ',', conflicts_with = "all")]
        only: Vec<Target>,
        /// Every supported target
        #[arg(long)]
        all: bool,
        /// Write project-level configs instead of user-level ones
        #[arg(long)]
        project: bool,
        /// Print the planned diffs; write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Pause trimming; output passes through raw until resumed
    Pause,
    /// Resume trimming
    Resume,
    /// Print a trimmed output by short ID
    Show {
        /// Short ID, e.g. #4f9a
        id: String,
        /// Only these lines, e.g. 10..40 (1-based, inclusive)
        #[arg(long)]
        lines: Option<String>,
    },
    /// Open the live TUI dashboard
    Top,
    /// Start the MCP server on stdio
    Mcp,
    /// Scan skills, rules and MCP configs for risks; exits 1 on findings
    Scan {
        /// Files or directories (default: .claude/, .cursor/, .mcp.json, SKILL.md, CLAUDE.md, AGENTS.md)
        paths: Vec<std::path::PathBuf>,
    },
    /// Restore original configs from backups and remove AgentDiet
    #[command(visible_alias = "uninstall")]
    Eject {
        /// Also delete the vault, project memory and state ledger
        #[arg(long)]
        purge: bool,
    },
    /// Internal: invoked by assistant hooks
    #[command(hide = true)]
    Hook {
        event: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn bare_invocation_has_no_subcommand() {
        assert!(
            Cli::try_parse_from(["agentdiet"])
                .unwrap()
                .command
                .is_none()
        );
    }

    #[test]
    fn only_parses_comma_separated_targets() {
        let cli = Cli::try_parse_from(["agentdiet", "init", "--only", "claude,codex,claude-code"])
            .unwrap();
        let Some(Command::Init { only, .. }) = cli.command else {
            panic!("expected init");
        };
        assert_eq!(only, vec![Target::Claude, Target::Codex, Target::Claude]);
    }

    #[test]
    fn all_conflicts_with_only() {
        assert!(Cli::try_parse_from(["agentdiet", "init", "--all", "--only", "claude"]).is_err());
    }

    #[test]
    fn uninstall_is_an_alias_for_eject() {
        let cli = Cli::try_parse_from(["agentdiet", "uninstall", "--purge"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Eject { purge: true })));
    }

    #[test]
    fn hook_accepts_any_event_and_extra_args() {
        let cli = Cli::try_parse_from(["agentdiet", "hook", "Whatever", "--flag", "x"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Hook { .. })));
    }
}
