# Changelog

All notable changes to AgentDiet. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/) as described in `docs/VERSIONING.md`.

## [0.2.0] - 2026-10-21

Everything since the `v0.1.0` tag. Windows is **under test, not yet supported** (see Known issues).

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
- A Windows build without the shims (`SPEC.md` §10), **under test, not yet supported**: every command,
  the Control Center, hooks and the MCP server build, and the test suite passes on GitHub's Windows Server 2025 runner (x86_64, MSVC); interactive
  console use and real assistant clients on Windows have not been validated.
- Release tooling: CI for macOS, Linux and Windows, static Linux (musl) builds, reproducible archives
  with SHA-256 checksums, `scripts/install.sh`, `scripts/live-check.sh`.
- Every release archive carries a Sigstore-signed build provenance attestation
  (`gh attestation verify`; `SECURITY.md`).

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

- `~/.agentdiet` and a project's `.agentdiet/` are checked before AgentDiet writes under them
  (`SPEC.md` §3 Tier 3 "Private roots"): a project `.agentdiet` that is a symlink is refused (project
  memory is then off there; `doctor` explains), a symlinked `~/.agentdiet` must point at a directory
  you own, a directory owned by another user is refused, and one of yours with loose permissions is
  tightened to `0700`. Installations made by v0.1.0 are unaffected.

### Fixed
- Linux: installing through a link (Homebrew's `bin/agentdiet`) recorded the versioned target in
  every assistant's config, which an upgrade then removed; the link is recorded now, as on macOS.
  Run `agentdiet init` once after upgrading to replace an old versioned path.
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
- **Privacy:** error messages (stderr, MCP tool errors, usage errors) that quote an argument are
  scrubbed: `agentdiet show <a key>` or a bad `diet_inspect` ID no longer repeats a secret.

### Known issues
- The shims' overhead (B1) is about 2.7 ms per command on an Apple M1, about 0.7 ms over the budget
  (the measured exec floor plus 0.5 ms, `SPEC.md` §7). The single-binary design is kept; see README.
- Windows is under test, not yet supported: the test suite passes on GitHub's Windows Server 2025 runner (x86_64, MSVC), but Ctrl-C in the
  Control Center and `top`, Claude Code hooks and other clients, and recovery commands in PowerShell
  and Git Bash have not been validated on Windows. Windows has no shims.
- B4/B5 engine timings exceed their budgets on large or adversarial inputs (durability is not traded
  for speed).

### Compatibility with v0.1.0
- Unchanged: breadcrumb and fold-marker text; every ID form v0.1.0 accepted (a quoted form was added); `scan`'s report format and exit
  statuses; the three original MCP tools' names and arguments.
- Configs written by v0.1.0's `init` are recognised, updated in place by a new `init` and removed by
  `eject`; state, Vault and memory.db written by v0.1.0 are read as they are.

## [0.1.0] - 2026-10-17

First tagged version (`e9daa72`): hook router and PPID-aware shims, the Diet Engine (shell compression,
TOON, signature folding), the secret scrubber, the Vault and project memory, the MCP server
(`diet_exec`, `diet_search`, `diet_inspect`), `scan`, `top`, `stats`, `init` and `eject`. Its GitHub release (published 2026-10-17) carries a single
asset: a bare macOS arm64 `agentdiet` binary, with no archive, checksums or attestation; other platforms
built from source.
