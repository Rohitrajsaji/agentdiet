# AgentDiet

**AgentDiet keeps your AI coding assistant's context clean.** When the assistant runs a tool (a test
suite, a build, `npm install`, an MCP call), most of what comes back is noise: hundreds of passing
tests, progress bars, repeated warnings. AgentDiet trims that noise before the assistant reads it,
keeps every error and warning, and removes secrets before anything reaches the model or the disk.

Nothing useful is lost. The full output is stored in a local **Vault**, and the assistant gets one
extra line saying how to get it back:

```
test result: ok. 400 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
🥗 Diet: kept 14/412 lines; cargo test: 398 passing test lines collapsed; exit 0 Run: agentdiet show '#f5f26875'
```

The one exception is secrets: they are removed before storing, so they can never be recovered.

AgentDiet is a single binary with no runtime dependencies (no Node, no Python). It works with Claude
Code, Cursor, Windsurf, Gemini CLI, GitHub Copilot in VS Code and Codex.

## Quick start

**1. Install.** On macOS or Linux, the install script downloads the release for your machine, checks
its SHA-256 checksum, and puts `agentdiet` in `~/.local/bin` (it never edits your shell files):

```sh
curl -fsSL https://raw.githubusercontent.com/Rohitrajsaji/agentdiet/main/scripts/install.sh | sh
```

Or build it yourself (Rust 1.90 or newer and a C compiler):

```sh
cargo install --path . --locked        # from a checkout of this repository
```

Assistant configs point at the binary's full path, so install it somewhere it will stay. See
[Installing, upgrading and removing](#installing-upgrading-and-removing) for the details.

**2. Connect your assistants.** In a terminal, `agentdiet init` shows a checklist of the assistants it
finds; or name them:

```sh
agentdiet init --only claude,shims
```

```
  Claude Code   ✓ connected           hooks, MCP server, EXPERIENCE.md pointer
  Shims         ✓ connected           11 commands linked in ~/.agentdiet/shims

Backed up the originals to ~/.agentdiet/backups/1792513109815

Verification
  Claude Code   ✓ configured, not yet observed  restart it, then run any tool
  Shims         ✓ configured, not yet observed  recorded once an agent runs git, cargo, …

Next:
  1. Add the shims to your PATH (AgentDiet never edits shell rc files):
       export PATH="$HOME/.agentdiet/shims:$PATH"
  2. Restart Claude Code so it loads the new config.
  3. Check the setup: agentdiet doctor
```

Every file is backed up before it is changed, and running `init` again changes nothing. Use
`agentdiet init --all --dry-run` to preview everything without writing anything.

**3. Follow the "Next" steps**: add the `PATH` line to your shell's rc file (for the shims), restart the
assistant, then check:

```sh
agentdiet doctor
```

`doctor` checks every connection and prints the exact fix for anything wrong. "Configured, not yet
observed" is normal until the assistant has run a tool; after that it says "verified".

**4. Use your assistant as usual.** Open the Control Center any time:

```sh
agentdiet
```

Here the `PATH` line from step 3 has not been added yet, so it shows under *Needs attention*:

```
 AgentDiet  ● Active · Balanced mode                                     ? help
 Trims outputs over 600 est. tokens · must save 20% · 3 context lines
 Assistants                              Savings (all projects, estimated)
 ✓ Claude Code       not yet observed    ≈134,830 tokens saved
 ✓ Cursor            configured          4 trimmed outputs
 ✓ Shims             not yet observed    1 re-open for 4 trimmed outputs (25%)
                                         ⚠ high: try a lighter mode
                                         last #683db530 · l to view
 Needs attention
 ✗ PATH ~/.agentdiet/shims is not on PATH in this shell, so the shims are
   never reached
   → add to your shell rc: export PATH="$HOME/.agentdiet/shims:$PATH"
 Recent activity (this workspace)
 #683db530  cargo test  kept 14/412 lines; 398 passing lines collapsed

 p pause  m mode  l last output  a assistants  d doctor  t live  ? help  q quit
```

Every key has a command-line equivalent, which `?` lists.

## Everyday use

| You want to… | Run | Or in Claude Code's chat |
|---|---|---|
| See whether it is on and saving anything | `agentdiet status` | |
| Stop trimming for a while | `agentdiet pause` | `@diet pause` |
| Start again | `agentdiet resume` | `@diet resume` |
| Trim more or less | `agentdiet mode light` (or `balanced`, `ultra-lean`) | |
| Read the last trimmed output in full | `agentdiet show last` | |
| Read a specific one | `agentdiet show '#f5f26875'` (keep the quotes) | |
| See one tool call untrimmed | | put `@diet:full` in your prompt |
| Check the setup | `agentdiet doctor` | |
| Stop using it with one assistant | `agentdiet disconnect cursor` | |

```sh
agentdiet status
agentdiet show last --lines 1..40      # just a slice
agentdiet mode                         # the current mode and what each one does
```

- **Pausing** keeps secret removal on: output passes through untrimmed but still scrubbed. It stays
  paused until you resume, across restarts too.
- **`@diet:full`** affects exactly the next tool call; the one after is trimmed again. Plain words such
  as "the full report" never trigger it. Chat directives work in Claude Code only.
- **Retrying** the exact same command twice in a row gives the full output on the retry.
- **IDs** come from the `🥗 Diet` line. Always quote them in a shell: an unquoted `#` starts a comment.
  Trimmed outputs are kept 7 days after their last use.

## Diet Modes

| | Light | Balanced (default) | Ultra-Lean |
|---|---|---|---|
| Trims outputs over (estimated tokens) | 1,500 | 600 | 250 |
| Only if it saves at least | 40% | 20% | 10% |
| Lines kept around each error or warning | 5 | 3 | 1 |
| Last lines always kept | 20 | 10 | 5 |
| Good for | the agent seeing the most | everyday work | the tightest context |

If `status` warns that agents re-open trimmed output often, switch to a lighter mode.

## Troubleshooting

Start with `agentdiet doctor`: it names the problem and the fix. The common ones:

| `doctor` says | What it means | Fix |
|---|---|---|
| `PATH ✗ broken … is not on PATH in this shell` | The shims are never reached | Add `export PATH="$HOME/.agentdiet/shims:$PATH"` to your shell rc and open a new terminal |
| `PATH ⚠ … resolves to … before the shims` | Another directory comes first | Put the shims directory first on `PATH` |
| `configured, not yet observed` | Connected, but no tool runs recorded in this workspace yet | Restart the assistant and run something; not a problem |
| `configured binary … is missing` | AgentDiet was moved or deleted after `init` | Put it back, or run `agentdiet init --only <assistant>` from where it is now |
| `Binary ⚠ … run …, a different build` | Your assistants use another copy of agentdiet than the one you ran | Nothing needed; run `init` from the copy you want them to use |
| `… is not strict JSON (comments or trailing commas?)` | AgentDiet cannot safely edit that file | Fix the file, then `agentdiet init --only <assistant>` |
| `skipped by init: … edited by hand` | Someone changed AgentDiet's block in that file | Fix or delete the block by hand |
| `State ✗ broken … not valid state JSON` | `~/.agentdiet/state.json` is damaged | `rm ~/.agentdiet/state.json` (the counters start again) |

Other hints:

- If `show` says "no trimmed output … in the Vault", the ID is mistyped or older than 7 days.
- Editors started from the macOS Dock may not see your shell's `PATH`; `doctor` checks the shell you run
  it in.
- If a terminal is ever left in a strange state (only possible after `kill -9`), run `reset`.

## Assistants and what `init` changes

| Assistant | What it gets | Files (user level; `--project` writes the repository's own) |
|---|---|---|
| Claude Code | hooks, the MCP server, an `EXPERIENCE.md` pointer | `~/.claude/settings.json`, `~/.claude.json`, `~/.claude/rules/agentdiet.md` |
| Cursor | the MCP server | `~/.cursor/mcp.json` |
| Windsurf | the MCP server | `~/.codeium/windsurf/mcp_config.json` (user level only) |
| Gemini CLI | the MCP server | `~/.gemini/settings.json` |
| Copilot (VS Code) | the MCP server | VS Code's user `mcp.json` |
| Codex | the MCP server | `~/.codex/config.toml` |
| Shims | wrappers for `git`, `cargo`, `pytest`, `npm`, `pnpm`, `yarn`, `go`, `make`, `ruff`, `docker`, `gh` | `~/.agentdiet/shims/` |

- Only Claude Code has hooks today, so it gets the most trimming; the others use the MCP tools and the
  shims.
- The shims only trim commands an agent runs. Your own commands in a terminal, a pipe or a redirect run
  the real program untouched.
- Originals are backed up under `~/.agentdiet/backups/`, and shell rc files are never edited.

```sh
agentdiet disconnect cursor            # one assistant; its original config is restored
agentdiet init --only cursor           # and back again
agentdiet eject                        # everything; your original configs come back
agentdiet eject --purge                # …and delete stored outputs, activity and counters
```

## Shell completion

Tab-completion for commands, flags, Diet Modes and assistant names:

```sh
# bash (needs the bash-completion package)
mkdir -p ~/.local/share/bash-completion/completions
agentdiet completions bash > ~/.local/share/bash-completion/completions/agentdiet

# zsh: also add `fpath=(~/.zfunc $fpath)` to ~/.zshrc, before `compinit`
mkdir -p ~/.zfunc
agentdiet completions zsh > ~/.zfunc/_agentdiet

# fish
mkdir -p ~/.config/fish/completions
agentdiet completions fish > ~/.config/fish/completions/agentdiet.fish
```

Open a new shell afterwards. Re-run the command after upgrading AgentDiet so new commands complete too.

## Advanced

```sh
agentdiet top                          # live dashboard (refreshes every 2 s)
agentdiet stats                        # savings and this workspace's activity
agentdiet stats --json                 # the same numbers, stable keys, for scripts
agentdiet doctor --json                # every check with its fix, for scripts and CI
agentdiet scan                         # check .claude/, .cursor/, .mcp.json, SKILL.md, CLAUDE.md, AGENTS.md
agentdiet reflect --dry-run            # tips learned from this workspace's past runs, for EXPERIENCE.md
```

- **`scan`** looks for hidden Unicode, secrets, prompt-injection phrases and dangerous shell commands. It
  exits 0 when clean, 1 on findings and 2 when a path cannot be read. A clean result is not a proof of
  safety.
- **`reflect`** writes `EXPERIENCE.md` (at most 2 KB) from past runs, offline and without any AI model.
  Every tip cites where it came from, and anything you write outside its marked block is kept.
- **Workspace:** activity is recorded in the nearest directory with `.agentdiet/memory.db` or `.git`
  (or `AGENTDIET_WORKSPACE`), so commands run in a subdirectory count for the whole project.
- **Scripts and CI:** `agentdiet status --json` and `agentdiet doctor --json` print one JSON object with
  stable keys (documented in `SPEC.md` §3b); `doctor --json` keeps `doctor`'s exit status (1 when
  something is broken), so `agentdiet doctor --json > health.json || echo "needs attention"` works.
- **Internal commands**, which assistants run and `--help` does not list: `agentdiet mcp` (the MCP
  server `init` registers) and `agentdiet hook <event>` (Claude Code's hooks).

### MCP tools

`agentdiet mcp` is a stdio JSON-RPC 2.0 server with four tools:

| Tool | Arguments | Does |
|---|---|---|
| `diet_exec` | `command` (argv array or one simple command), `cwd?` | Runs the command without a shell, through the same trimming; returns the trimmed output (long output of any command is cut to head, tail and problem lines), the `🥗 Diet` line and the exit status |
| `diet_search` | `query`, `limit?` | Searches past outputs of this project; each hit carries its `#id` |
| `diet_inspect` | `id`, `start_line?`, `end_line?` | Returns a stored output (secrets removed), or a slice of it |
| `diet_reflect` | `dry_run?` | Runs `agentdiet reflect` and returns `EXPERIENCE.md` |

## Before / After

Real runs on an Apple M1 (macOS), Balanced mode, 2026-10-17, commit `1e72a5d`, through `diet_exec`
(the same engine as the hooks and shims). *Bytes* compare the stored original (secrets removed) with
what the agent received. *Tokens* are AgentDiet's own deterministic estimate (`SPEC.md` §3), not any
provider's tokenizer.

| Output | Bytes before → after | Est. tokens before → after | Saved |
|---|---|---|---|
| `cargo test`, 400 passing tests | 11,872 → 843 | 5,047 → 309 | 94% |
| `cargo build` from clean (AgentDiet's own dependency tree) | 4,058 → 508 | 1,579 → 194 | 88% |
| `go test -v ./...`, 300 passing tests | 15,420 → 390 | 6,615 → 154 | 98% |
| MCP tool result: JSON array of 200 uniform objects → TOON | 17,582 → 4,478 | 8,202 → 2,038 | 75% |

Small or already-terse outputs are left as they are (in Balanced mode, nothing under 600 estimated
tokens), but secrets are still removed.

## How it works

```
 agent ──tool call──▶ tool ──output──▶ ┌──────────────────────────────┐ ──trimmed──▶ agent
                                       │ 1. Hooks and shims           │
                                       │ 2. Diet Engine               │
                                       │ 3. Vault + project memory    │
                                       └──────────────────────────────┘
```

1. **Hooks and shims** catch tool output.
   - *Hooks:* Claude Code's `PostToolUse` hook hands each successful tool result to AgentDiet, which
     returns a replacement of exactly the same shape. AgentDiet's own MCP tools are never re-processed.
   - *Shims:* `~/.agentdiet/shims/{git,cargo,pytest,npm,…}` catch commands an agent runs, including
     failing ones. They walk the parent process chain to tell an agent from a person, and only capture
     output that goes straight back to the agent; a person gets the real program.
2. **Diet Engine.** It removes secrets, then builds a shorter version:
   - command-aware compression for cargo, pytest, go test, npm/pnpm/yarn, git, make, docker and ruff;
   - TOON, a compact form of JSON, for what MCP tools and WebFetch return.

   The shorter version is used only if it is strictly smaller in bytes and in estimated tokens (the
   `🥗 Diet` line included) and saves what the Diet Mode requires; otherwise the original (secrets
   removed) passes. Error and warning lines are always kept verbatim, with their context.
3. **Vault and project memory.**
   - *Vault:* the full original (secrets removed) is written durably to `~/.agentdiet/cas/` before any
     shorter version is sent. Entries expire 7 days after their last use.
   - *Project memory:* a summary goes into the project's `.agentdiet/memory.db` (SQLite full-text
     search), so agents can search past outputs.

`SPEC.md` is the authoritative description of the behaviour.

## Installing, upgrading and removing

| Platform | Status |
|---|---|
| macOS (Apple silicon, Intel) | supported |
| Linux x86_64 and arm64 (any distribution: the release binaries are fully static) | supported |
| Windows x86_64 | supported **without the shims** (hooks, the MCP server and every command work); not yet run on real Windows, only under Wine; see `SPEC.md` §10 |

**Release archives** (`agentdiet-<version>-<target>.tar.gz`, `.zip` for Windows) contain the binary,
the licence and these documents; `SHA256SUMS` lists every archive's checksum, and shell completions come
as `agentdiet-<version>-completions.tar.gz`. To install by hand: download the archive for your platform
and `SHA256SUMS`, check it (`shasum -a 256 -c SHA256SUMS --ignore-missing`, or `sha256sum -c`), unpack
it, and put `agentdiet` somewhere on your `PATH`. On Windows (the install script is for macOS and
Linux): check the `.zip` with `Get-FileHash agentdiet-<version>-x86_64-pc-windows-msvc.zip` against
`SHA256SUMS`, unpack it, put `agentdiet.exe` in a directory on your `PATH`, then `agentdiet init`
(it skips the shims). Recovery commands work as printed in PowerShell, Git Bash and `cmd.exe`.

**Install script settings:** `AGENTDIET_VERSION=0.2.0` for a specific version, `AGENTDIET_INSTALL_DIR`
for another directory.

**Upgrading:** run the install script again (or replace the binary at the same path). Your assistants
already point at that path, so nothing else is needed; `agentdiet doctor` confirms it. If you move the
binary instead, `doctor` names the missing path and `agentdiet init --only <assistants>` fixes it. Then
re-generate shell completions. Stored outputs, settings and project memory from older versions keep
working.

**Removing:** first `agentdiet eject` (restores every assistant's original config and removes the
shims; add `--purge` to delete stored outputs and counters too), then delete the binary.

**Compatibility:** what scripts may rely on (the `--json` outputs, exit statuses, MCP tools, the
breadcrumb format) is listed in [`docs/VERSIONING.md`](docs/VERSIONING.md); changes are in
[`CHANGELOG.md`](CHANGELOG.md). Security reports: [`SECURITY.md`](SECURITY.md).

## Build from source

You need rustc 1.90 or newer and a C compiler.
- *rustc:* 1.90 is the newest `rust-version` declared by a dependency (tree-sitter). The whole suite
  has run on 1.99.1 (macOS, Linux) and on 1.90 (Linux); CI checks that 1.90 still compiles it.
- *C compiler:* needed for the bundled SQLite and the statically linked tree-sitter grammars.

```sh
cargo build --release --locked         # → target/release/agentdiet
scripts/check-binary-deps.sh --build   # verify the artifact links nothing beyond libSystem (macOS) / nothing (Linux musl)
```

The Linux release targets are `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, which give
fully static binaries. They need `rustup target add <target>` and a musl-capable C toolchain (on
Debian/Ubuntu: `musl-tools`, then `CC=musl-gcc`):

```sh
CC=musl-gcc cargo build --release --locked --target x86_64-unknown-linux-musl
```

CI (`.github/workflows/ci.yml`) runs the checks below on macOS, Linux and Windows, the minimum Rust
version, the musl builds and `cargo deny`; `release.yml` builds the release archives from a version tag
as a draft. Neither has run on GitHub yet.

Development loop (all three must pass):

```sh
cargo install cargo-nextest --locked   # once
cargo check --quiet
cargo nextest run
cargo clippy --all-targets -- -D warnings
```

The benchmarks are opt-in: `cargo test --release --test perf -- --ignored --nocapture`.

## Status and known limits

This is v0.1.0, and some things are not done yet. `SPEC.md` §7–§8 has the details and the measurements.

- **Performance budgets are not met yet.** All three were measured on an Apple M1:
  - The shims add about 2.7–3.9 ms per command (it varies by session) against a < 0.2 ms budget; this
    needs a shim redesign.
  - The engine's pure transform is up to about 29 ms on 256 KiB inputs, against < 10 ms.
  - A full engine run on 64 KiB, including the durable Vault write, takes about 22–26 ms. Most of
    that is macOS `F_FULLFSYNC`, and durability is never traded for speed.
- **Live checks so far: Claude Code only.** That covers the hooks, the MCP tools, and a shimmed
  `cargo test`. The other assistants get the MCP server but no hooks, and have not been checked live.
  - The shims trim only commands that an agent runs without a terminal; a command run in an
    integrated terminal runs untouched.
- **Failing commands** are trimmed only through the shims: Claude Code's hook cannot replace the result
  of a failed Bash call.
- **File reads are scrubbed, never folded.** Signature folding exists in the engine but no entry
  point uses it yet.
- **TOON** applies only to JSON from MCP tools and WebFetch, never to shell output, and only for a
  subset of the format. Unsupported shapes are left unchanged.
- **Secret detection is best effort.** It is pattern based, and a clean result is not a guarantee.
  Output that takes the shims' untouched (person) path, and binary output, are not scrubbed.

## License

MIT; see [`LICENSE`](LICENSE). Secret-scrubbing patterns ported from
[Secretlint](https://github.com/secretlint/secretlint) and the vendored TOON conformance fixtures are
MIT-licensed by their authors; see [`THIRD_PARTY_NOTICES`](THIRD_PARTY_NOTICES).
