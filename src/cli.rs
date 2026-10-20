//! Command-line surface (clap derive). The help text is part of the UX (SPEC §5): grouped commands,
//! plain words, and examples that are tested to parse.

use clap::{Parser, Subcommand};

use crate::init::Target;
use crate::state::Mode;

/// The command map under `agentdiet --help` (clap has one subcommand heading, so the groups are
/// written here; a test keeps this list and the real commands in step).
pub const COMMAND_MAP: &str = "\
Everyday:
  status          Is AgentDiet on, connected and saving anything?
  pause, resume   Stop or restart trimming (output is still scrubbed of secrets)
  mode [MODE]     Show or set the Diet Mode: light, balanced or ultra-lean
  show <ID|last>  Print a trimmed output in full
  doctor          Find setup problems and how to fix them

Setup:
  init            Connect AgentDiet to your AI assistants
  disconnect      Remove AgentDiet from some assistants only
  eject           Remove AgentDiet everywhere and restore your configs (alias: uninstall)

Advanced:
  top             Live dashboard of savings and recent activity
  stats           Savings and this workspace's activity (--json for scripts)
  scan            Check skills, rules and MCP configs for risky content
  reflect         Turn this workspace's past runs into tips in EXPERIENCE.md

Examples:
  agentdiet init --only claude,shims
  agentdiet show last --lines 1..40
  agentdiet mode light

Run `agentdiet` with no arguments for the Control Center, or `agentdiet help <command>` for details.";

const TEMPLATE: &str = "\
{about-with-newline}
{usage-heading} {usage}{after-help}

Options:
{options}";

#[derive(Debug, Parser)]
#[command(
    name = "agentdiet",
    version,
    about = "AgentDiet trims the tool output your AI coding assistant reads, so it stays focused \
             and spends fewer tokens. Everything trimmed stays recoverable.",
    help_template = TEMPLATE,
    after_help = COMMAND_MAP
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show whether AgentDiet is on, connected and saving anything
    #[command(after_help = "\
Examples:
  agentdiet status
  agentdiet doctor    # the details, with a fix for each problem

Exit status: always 0 (`agentdiet doctor` exits 1 when something is broken).")]
    Status,
    /// Pause trimming: output passes through untrimmed (still scrubbed) until you resume
    #[command(after_help = "\
In Claude Code you can also type `@diet pause` and `@diet resume` in the chat.")]
    Pause,
    /// Resume trimming
    Resume,
    /// Show or set the Diet Mode: how much output is trimmed
    #[command(after_help = "\
Examples:
  agentdiet mode          # the current mode and what each one does
  agentdiet mode light    # trim only very large outputs
  agentdiet mode ultra    # same as ultra-lean")]
    Mode {
        /// The mode to switch to; leave it out to see the current mode and what each one does
        #[arg(value_enum)]
        mode: Option<Mode>,
    },
    /// Print a trimmed output in full, by its ID or `last`
    #[command(after_help = "\
Examples:
  agentdiet show last
  agentdiet show last --lines 1..40
  agentdiet show '#4f9a1b2c'    # the ID from a 🥗 Diet line; keep the quotes

Trimmed outputs are kept 7 days after their last use. Secrets were removed before storing.")]
    Show {
        /// The ID from a 🥗 Diet line, e.g. '#4f9a1b2c' (quote it: an unquoted # starts a shell
        /// comment), or `last` for the newest trimmed output
        id: String,
        /// Only these lines, e.g. 10..40 (1-based, inclusive)
        #[arg(long)]
        lines: Option<String>,
    },
    /// Find setup problems and how to fix them; exits 1 when something is broken
    #[command(after_help = "\
Examples:
  agentdiet doctor

Exit status: 0 when nothing is broken (warnings allowed), 1 otherwise.")]
    Doctor,
    /// Connect AgentDiet to your AI assistants
    #[command(after_help = "\
Examples:
  agentdiet init                           # choose from a checklist (needs a terminal)
  agentdiet init --only claude,shims       # connect these
  agentdiet init --all --dry-run           # preview everything, change nothing
  agentdiet init --project --only claude   # this repository's configs, not your own

Every file is backed up first (~/.agentdiet/backups); running it again changes nothing.")]
    Init {
        /// Assistants, comma-separated: claude (Claude Code), cursor, windsurf, gemini (Gemini CLI),
        /// copilot (VS Code), codex, shims (wrappers for git, cargo, npm and others)
        #[arg(
            long,
            value_enum,
            value_delimiter = ',',
            conflicts_with = "all",
            hide_possible_values = true,
            value_name = "ASSISTANTS"
        )]
        only: Vec<Target>,
        /// Every supported assistant
        #[arg(long)]
        all: bool,
        /// Write this repository's configs instead of your user-level ones
        #[arg(long)]
        project: bool,
        /// Show what would change; write nothing (works without a terminal)
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove AgentDiet from some assistants, leaving the others as they are
    #[command(after_help = "\
Examples:
  agentdiet disconnect cursor
  agentdiet disconnect codex,gemini

Their original configs are restored (or only AgentDiet's entries removed, if you edited them since).
To reconnect: agentdiet init --only <assistant>")]
    Disconnect {
        /// Assistants (comma-separated or repeated): claude, cursor, windsurf, gemini, copilot,
        /// codex, shims. Removes them from your user configs and from this repository's.
        #[arg(
            required = true,
            value_enum,
            value_delimiter = ',',
            hide_possible_values = true,
            value_name = "ASSISTANT"
        )]
        targets: Vec<Target>,
    },
    /// Remove AgentDiet everywhere and restore your original configs
    #[command(
        visible_alias = "uninstall",
        after_help = "\
Examples:
  agentdiet eject                  # every assistant, the shims, and your original configs back
  agentdiet eject --only cursor    # just Cursor (same as: agentdiet disconnect cursor)
  agentdiet eject --purge          # also delete stored outputs, activity and counters"
    )]
    Eject {
        /// Only these assistants (same as `agentdiet disconnect`)
        #[arg(
            long,
            value_enum,
            value_delimiter = ',',
            conflicts_with = "purge",
            hide_possible_values = true,
            value_name = "ASSISTANTS"
        )]
        only: Vec<Target>,
        /// Also delete the Vault (stored outputs), this repository's activity (.agentdiet/memory.db)
        /// and the counters (~/.agentdiet/state.json)
        #[arg(long)]
        purge: bool,
    },
    /// Open the live dashboard of savings and recent activity (refreshes every 2 s)
    Top,
    /// Show savings and this workspace's activity
    #[command(after_help = "\
Examples:
  agentdiet stats
  agentdiet stats --json    # stable keys, for scripts")]
    Stats {
        /// Print JSON instead of text (stable keys; meant for scripts)
        #[arg(long)]
        json: bool,
    },
    /// Turn this workspace's past runs into tips in EXPERIENCE.md (offline; no AI model involved)
    #[command(after_help = "\
Examples:
  agentdiet reflect --dry-run    # print what would be written
  agentdiet reflect")]
    Reflect {
        /// Print what would be written; write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Check skills, rules and MCP configs for risky content; exits 1 on findings
    #[command(after_help = "\
Examples:
  agentdiet scan                     # .claude/, .cursor/, .mcp.json, SKILL.md, CLAUDE.md, AGENTS.md
  agentdiet scan .claude/ AGENTS.md

Exit status: 0 clean, 1 findings, 2 a path could not be read. A clean result is not proof of safety.")]
    Scan {
        /// Files or directories (default: .claude/, .cursor/, .mcp.json, SKILL.md, CLAUDE.md, AGENTS.md)
        paths: Vec<std::path::PathBuf>,
    },
    /// Internal: the MCP server that `init` registers (assistants start it)
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

    /// `agentdiet …` lines in a help text, as argv: a trailing `  # comment` dropped, single quotes
    /// removed (the shell would).
    fn example_lines(text: &str) -> Vec<Vec<String>> {
        text.lines()
            .map(str::trim_start)
            .filter(|l| l.starts_with("agentdiet"))
            .map(|l| {
                let l = l.split("  #").next().unwrap_or(l);
                l.split_whitespace()
                    .map(|w| w.trim_matches('\'').to_string())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_command_map_lists_exactly_the_visible_commands() {
        use clap::CommandFactory;
        let mut listed = Vec::new();
        for line in COMMAND_MAP.split("Examples:").next().unwrap().lines() {
            let Some(entry) = line.strip_prefix("  ") else {
                continue;
            };
            let names = entry.split("  ").next().unwrap();
            for name in names.split(", ") {
                listed.push(name.split_whitespace().next().unwrap().to_string());
            }
        }
        let mut visible: Vec<String> = Cli::command()
            .get_subcommands()
            .filter(|c| !c.is_hide_set())
            .map(|c| c.get_name().to_string())
            .collect();
        let mut sorted = listed.clone();
        sorted.sort();
        visible.sort();
        assert_eq!(sorted, visible, "every visible command, once: {listed:?}");
        sorted.dedup();
        assert_eq!(sorted.len(), listed.len(), "listed twice: {listed:?}");
    }

    #[test]
    fn every_help_example_parses() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let mut texts = vec![COMMAND_MAP.to_string()];
        for sub in cmd.get_subcommands() {
            if let Some(after) = sub.get_after_help() {
                texts.push(after.to_string());
            }
        }
        let mut seen = 0;
        for text in &texts {
            for argv in example_lines(text) {
                seen += 1;
                assert!(
                    Cli::try_parse_from(&argv).is_ok(),
                    "help example does not parse: {argv:?}"
                );
            }
        }
        assert!(seen >= 20, "only {seen} examples found");
    }

    #[test]
    fn old_spellings_and_aliases_keep_working() {
        for argv in [
            &["agentdiet", "uninstall"][..],
            &["agentdiet", "uninstall", "--purge"],
            &["agentdiet", "eject", "--only", "claude-code,gemini-cli"],
            &["agentdiet", "mode", "ultra"],
            &["agentdiet", "mode", "ultra-lean"],
            &[
                "agentdiet",
                "init",
                "--only",
                "claude,cursor,windsurf,gemini,copilot,codex,shims",
            ],
            &["agentdiet", "show", "#4f9a", "--lines", "1..2"],
            &["agentdiet", "stats", "--json"],
            &["agentdiet", "scan", "a", "b"],
            &["agentdiet", "reflect", "--dry-run"],
            &["agentdiet", "top"],
            &["agentdiet", "mcp"],
            &["agentdiet", "hook", "PostToolUse"],
            &["agentdiet", "help", "init"],
        ] {
            let parsed = Cli::try_parse_from(argv);
            let help_is_ok =
                matches!(&parsed, Err(e) if e.kind() == clap::error::ErrorKind::DisplayHelp);
            assert!(parsed.is_ok() || help_is_ok, "{argv:?}");
        }
    }

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
