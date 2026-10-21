# Changelog

All notable changes to AgentDiet. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/) as described in `docs/VERSIONING.md`.

## [Unreleased]

Everything since the `v0.1.0` tag. Not yet published.

### Added
- `agentdiet status`, `doctor`, `mode`, `show last` and `disconnect` (`eject --only`).
- The Control Center: bare `agentdiet` in a terminal is a one-screen view with one key per action.
- `stats` (and `stats --json`), `status --json` and `doctor --json` (documented in `SPEC.md` §3b).
- `agentdiet reflect`, `EXPERIENCE.md` and the `diet_reflect` MCP tool: repo-specific tips learned
  offline from past runs; a run log of shim, MCP and hook runs per workspace.
- `agentdiet completions bash|zsh|fish`.
- Claude Code: a `PostToolUseFailure` hook (failed tool calls), and a one-line `EXPERIENCE.md` pointer in
  `~/.claude/rules/agentdiet.md`. Re-run `agentdiet init` to add them to an existing setup.
- `init` verifies what it connected ("configured", "not yet observed", "verified").
- Windows support without the shims (`SPEC.md` §10): every command, the Control Center, hooks and the
  MCP server. Tested under Wine only so far.
- Release tooling: CI for macOS, Linux and Windows, static Linux (musl) builds, reproducible archives
  with SHA-256 checksums, `scripts/install.sh`, `scripts/live-check.sh`.

### Changed
- Grouped `--help` with examples; one vocabulary across the CLI, the Control Center and `top`; errors
  that say what to do next; a beginner-oriented README.
- The re-open warning needs at least 10 trimmed outputs and no longer claims that agents did the
  re-opening (a person's `show '#id'` counts too). `show last` is not counted.
- `mcp` and `hook` are hidden from `--help` (they work as before).
- The full-output directive is only `@diet:full`, `--diet-full` or `/diet-full`, for exactly one tool
  call; plain words such as "full" no longer trigger it.
- `diet_exec` cuts any long output to its head, tail and problem lines (stored in the Vault).
- Project memory is found at the workspace root (the nearest `.git` or `.agentdiet/memory.db`, or
  `AGENTDIET_WORKSPACE`), so subdirectories share it.

### Fixed
- **Privacy:** a secret in a command line or read path that crossed the 240-byte title cut could leave
  an unredacted prefix in `memory.db` (present since v0.1.0). Titles, summaries and stderr notes are
  now scrubbed whole before they are cut. Rows written by earlier builds are not rewritten: delete
  `.agentdiet/memory.db` in a project to drop them (the Vault is not affected).
- No command panics or prints "Broken pipe" when its output is piped into `head` or closed.
- A hook with a closed stderr no longer dies by SIGABRT; a shim without its real program exits 127.
- Corrupt `state.json` warnings no longer print over full-screen views; SIGTERM/SIGHUP/SIGINT restore
  the terminal.
- `scan` no longer reports `TOKEN_*`-named variables without a credential-like literal.
- Shims: under a shell that applies `> file` in the parent (dash — Debian's and Ubuntu's `/bin/sh` —,
  ksh, mksh, yash, busybox sh) a redirected command's output was trimmed in the file; it is now left
  untouched.
- memory.db: the open now checks that SQLite itself opened the checked file (a file swapped in and
  back around SQLite's open was not detected).
- An ID inside single quotes (`'#4f9a1b2c'`, as `cmd.exe` passes it) is accepted.

### Compatibility with v0.1.0
- Unchanged: breadcrumb and fold-marker text; every ID form v0.1.0 accepted (a quoted form was added); `scan`'s report format and exit
  statuses; the three original MCP tools' names and arguments.
- Configs written by v0.1.0's `init` are recognised, updated in place by a new `init` and removed by
  `eject`; state, Vault and memory.db written by v0.1.0 are read as they are.

## [0.1.0] - 2026-10-17

First tagged version (`e9daa72`): hook router and PPID-aware shims, the Diet Engine (shell compression,
TOON, signature folding), the secret scrubber, the Vault and project memory, the MCP server
(`diet_exec`, `diet_search`, `diet_inspect`), `scan`, `top`, `stats`, `init` and `eject`. Built from
source only; no release binaries were published.
