//! Command-line surface (clap derive).

use clap::{Parser, Subcommand};

use crate::init::Target;
use crate::state::Mode;

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
    /// Show whether AgentDiet is on, connected and saving anything
    Status,
    /// Pause trimming; output passes through untrimmed (still scrubbed) until resumed
    Pause,
    /// Resume trimming
    Resume,
    /// Show or set the Diet Mode: how aggressively output is trimmed
    Mode {
        /// The mode to switch to; omit it to see the current mode and what each one does
        #[arg(value_enum)]
        mode: Option<Mode>,
    },
    /// Print a trimmed output in full, by short ID or `last`
    Show {
        /// Short ID from a breadcrumb, e.g. '#4f9a' (quote it: an unquoted # starts a shell
        /// comment), or `last` for the last trimmed output
        id: String,
        /// Only these lines, e.g. 10..40 (1-based, inclusive)
        #[arg(long)]
        lines: Option<String>,
    },
    /// Check the setup and explain how to fix what is wrong; exits 1 on a problem
    Doctor,
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
        /// Print the planned changes; write nothing (works without a terminal)
        #[arg(long)]
        dry_run: bool,
    },
    /// Disconnect AgentDiet from some assistants, leaving the others as they are
    Disconnect {
        /// Assistants to disconnect (comma-separated or repeated): claude, cursor, windsurf,
        /// gemini, copilot, codex, shims. Removes them at user scope and in this project.
        #[arg(required = true, value_enum, value_delimiter = ',')]
        targets: Vec<Target>,
    },
    /// Restore original configs from backups and remove AgentDiet
    #[command(visible_alias = "uninstall")]
    Eject {
        /// Only these assistants (same as `disconnect`)
        #[arg(long, value_enum, value_delimiter = ',', conflicts_with = "purge")]
        only: Vec<Target>,
        /// Also delete the vault, project memory and state ledger
        #[arg(long)]
        purge: bool,
    },
    /// Open the live dashboard (refreshes every 2 s)
    Top,
    /// Print trimming counters and this workspace's run log totals
    Stats {
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Learn repo-specific recipes and pitfalls from the run log into EXPERIENCE.md (no LLM)
    Reflect {
        /// Print what would be written; write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Scan skills, rules and MCP configs for risks; exits 1 on findings
    Scan {
        /// Files or directories (default: .claude/, .cursor/, .mcp.json, SKILL.md, CLAUDE.md, AGENTS.md)
        paths: Vec<std::path::PathBuf>,
    },
    /// Start the MCP server on stdio (assistants run this; `init` registers it)
    #[command(hide = true)]
    Mcp,
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
        assert!(matches!(
            cli.command,
            Some(Command::Eject { purge: true, .. })
        ));
    }

    #[test]
    fn mode_takes_an_optional_name_with_an_ultra_alias() {
        let mode = |args: &[&str]| match Cli::try_parse_from(args).unwrap().command {
            Some(Command::Mode { mode }) => mode,
            other => panic!("expected mode, got {other:?}"),
        };
        assert_eq!(mode(&["agentdiet", "mode"]), None);
        assert_eq!(mode(&["agentdiet", "mode", "light"]), Some(Mode::Light));
        assert_eq!(mode(&["agentdiet", "mode", "ultra"]), Some(Mode::UltraLean));
        assert_eq!(
            mode(&["agentdiet", "mode", "ultra-lean"]),
            Some(Mode::UltraLean)
        );
        assert!(Cli::try_parse_from(["agentdiet", "mode", "ligth"]).is_err());
    }

    #[test]
    fn disconnect_takes_targets_and_eject_only_refuses_purge() {
        let cli =
            Cli::try_parse_from(["agentdiet", "disconnect", "cursor,codex", "gemini"]).unwrap();
        let Some(Command::Disconnect { targets }) = cli.command else {
            panic!("expected disconnect");
        };
        assert_eq!(targets, vec![Target::Cursor, Target::Codex, Target::Gemini]);
        assert!(Cli::try_parse_from(["agentdiet", "disconnect"]).is_err());
        let cli = Cli::try_parse_from(["agentdiet", "uninstall", "--only", "cursor"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::Eject { only, .. }) if only == [Target::Cursor])
        );
        assert!(
            Cli::try_parse_from(["agentdiet", "eject", "--only", "cursor", "--purge"]).is_err()
        );
    }

    #[test]
    fn mcp_and_hook_are_hidden_but_still_parse() {
        use clap::CommandFactory;
        let help = Cli::command().render_help().to_string();
        assert!(!help.contains("  mcp"), "{help}");
        assert!(!help.contains("  hook"), "{help}");
        assert!(help.contains("disconnect"));
        assert!(Cli::try_parse_from(["agentdiet", "mcp"]).is_ok());
    }

    #[test]
    fn show_needs_an_id_or_last() {
        // An unquoted `show #id` reaches us as a bare `show`: it must fail, not pick another entry.
        assert!(Cli::try_parse_from(["agentdiet", "show", "--lines", "1..3"]).is_err());
        let cli = Cli::try_parse_from(["agentdiet", "show", "last"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Show { id, .. }) if id == "last"));
    }

    #[test]
    fn hook_accepts_any_event_and_extra_args() {
        let cli = Cli::try_parse_from(["agentdiet", "hook", "Whatever", "--flag", "x"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Hook { .. })));
    }
}
