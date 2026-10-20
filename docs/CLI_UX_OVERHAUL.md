# AgentDiet CLI/UX overhaul

Status: **P0 implemented**; P1–P3 designed, not started. `SPEC.md` stays the source of truth for
behaviour (§2.2 `init` output, §2.5 terminal rules, §3b Status / Doctor / Mode / `show last`); this
document holds the design reasoning, the mockups and the task list.

## 1. Goal

A first-time user installs the binary, runs one command, and then knows four things without reading
the README:

1. Is AgentDiet on?
2. Is it connected to my assistant, and is that connection actually working?
3. What has it done for me?
4. What do I type next?

## 2. Principles

1. **Simple by default, advanced when needed.** `status`, `pause`/`resume`, `mode`, `show last`,
   `doctor` and `init` cover daily use; `stats`, `scan`, `reflect`, `top` and flags are there when
   wanted.
2. **Every action has an obvious one-word command**, and the interactive menu teaches it (P1).
3. **No implementation jargon.** People see *Vault*, *trimmed output*, *re-opened*, *assistant*,
   *shims*, *Diet Mode*; never "CAS", "dereference", "PatchKind" or "manifest".
4. **Configured is not working.** A config entry that exists is `configured`; `working` needs
   evidence (runs in the run log). Missing evidence is never shown as a failure, and never as success.
5. **No misleading numbers.** Each number says its scope (all projects / this workspace) and that
   tokens are estimates. Rates show their raw counts.
6. **One vocabulary** across CLI, TUI, errors and docs (§9).
7. **Agent-facing text is a contract, not a surface to polish.** Breadcrumbs, fold markers, `show`
   bytes, `stats --json`, `scan` lines and exit codes, hook stdout and MCP results do not change.
8. **No new dependencies; no cost on the hot path.** Colour is ~40 lines of hand-written ANSI; shim and
   hook dispatch in `main.rs` stay before clap and touch none of the new code.

## 3. Current problems and root causes

Observed by running every command in a scratch `HOME` (2026-10-20, commit `5092910`).

| # | Problem | Root cause |
|---|---|---|
| 1 | No way to change the Diet Mode from the shell | Mode was only reachable through the Control Center and `top` |
| 2 | No `status` command; the status line shows only for a bare non-TTY run | Status was a fallback of the menu, not a command |
| 3 | `show` needs an ID; `show last` fails with `invalid ID "last"` | `show` was built for agents following a breadcrumb, not for people |
| 4 | `init` prints 14 absolute paths, all "patched", even for new symlinks | `InitReport` is a flat file list; the printer echoes it |
| 5 | Counts disagree between runs: "changed 14" then "4 already up to date" | Changed shims are counted one per link, unchanged shims as one directory |
| 6 | The `PATH` line is printed on every run | No check of the current `PATH` |
| 7 | `init --dry-run` fails without a terminal | Target choice falls through to the checklist before the dry-run flag is considered |
| 8 | "CAS dereferences" in `stats` | Internal names leak into user text |
| 9 | Nothing detects shims missing from `PATH`, a moved binary, or a broken config | No diagnostics command; `init` only reports what it changed |
| 10 | "Connected" is shown from config presence alone (`top`) | No distinction between configured and working |
| 11 | `--help` lists `mcp` beside everyday commands; no examples | Help is clap's default rendering of declaration order |
| 12 | `eject` is all-or-nothing on the CLI | `eject::eject(only)` exists but the CLI never passes `only` |
| 13 | Control Center shows a one-line prompt; it does not teach the commands | Menu built as a thin action list |

P0 fixes 1–7, 9 and 10; 8 is fixed in the new commands and left in `stats` until P2. 11–13 are later phases.

## 4. Command hierarchy

Everyday (first in help):

| Command | Aliases | Does |
|---|---|---|
| `agentdiet` | | Control Center (terminal); without a terminal, the one-line status and a pointer to `status` |
| `agentdiet status` | | One-screen overview, read-only |
| `agentdiet pause` / `resume` | | Stop / restart trimming |
| `agentdiet mode [light\|balanced\|ultra-lean]` | `ultra` = `ultra-lean` | Print or set the Diet Mode |
| `agentdiet show <#id\|last> [--lines a..b]` | | Print a trimmed output in full |
| `agentdiet doctor` | | Diagnose setup; exit 1 on a problem |

Setup:

| Command | Aliases | Does |
|---|---|---|
| `agentdiet init [--only …] [--all] [--project] [--dry-run]` | | Connect assistants |
| `agentdiet eject [--purge]` (P1: `[--only …]`) | `uninstall` | Disconnect and restore configs |

Insight and tools: `top`, `stats [--json]`, `scan [paths]`, `reflect [--dry-run]`.

For assistants (hidden in P2 help): `mcp`, `hook <event>` (already hidden).

Backwards compatibility: every existing command, flag and alias keeps working unchanged. `show '#id'`
behaves exactly as before; only the `last` form is new. A bare `show` stays a usage error on purpose:
an unquoted `show #4f9a` reaches agentdiet as a bare `show`, and defaulting it to `last` would hand
an agent a different entry without any error (caught by `tests/engine.rs` during P0).

## 5. Workflows

### First run

```
$ cargo install --path . --locked
$ agentdiet init            # checklist, detected assistants pre-selected
  … per-target summary, then Next: PATH line, restart assistants, agentdiet doctor
$ agentdiet doctor          # after restarting the assistant
$ agentdiet status          # from now on, the everyday check
```

P1 adds: a bare `agentdiet` with nothing connected opens on "Connect your assistants" instead of
the menu, and `init` runs the doctor checks itself and prints the verdict line.

### Returning user

```
$ agentdiet status          # on? mode? saving anything? healthy?
$ agentdiet show last       # see what was just trimmed
$ agentdiet mode light      # trimming too much for this task
$ agentdiet pause           # or "@diet pause" in Claude Code
```

### Something looks wrong

```
$ agentdiet status          # Health: ✗ 1 problem — run agentdiet doctor
$ agentdiet doctor          # names the check, the file and the exact fix command
```

## 6. Mockups

The P0 mockups below are captured from real runs of the P0 build in a scratch `HOME` (only the
binary path is shortened to `~/.cargo/bin/agentdiet`). P1–P3 mockups are designs.

### 6.1 `agentdiet status`

```
AgentDiet is active · Balanced mode

  Assistants    Claude Code, Codex and Shims
  Saved         ≈134,830 tokens in 4 trimmed outputs (all projects, estimated)
  Re-opened     1 of 4 (25%) ⚠ high: agents re-read trimmed output; consider a lighter mode
  Last trimmed  #683db530 · agentdiet show last
  Health        ✗ 1 problem · agentdiet doctor
```

Fresh home (nothing connected, nothing trimmed):

```
AgentDiet is active · Balanced mode

  Assistants    none connected · agentdiet init
  Saved         nothing trimmed yet
  Health        ✗ 1 problem · agentdiet doctor
```

While paused, a `Paused` row shows the estimated tokens passed untrimmed (still scrubbed) and
`agentdiet resume`.

### 6.2 `agentdiet doctor`

```
AgentDiet doctor

  Binary        ✓ ok              ~/.cargo/bin/agentdiet (0.1.0)
  State         ✓ ok              active, Balanced mode
  Claude Code   ✓ configured      no activity recorded in this workspace yet; restart Claude Code if it was running
  Cursor        ⚠ warning         ~/.cursor/mcp.json is not strict JSON (comments or trailing commas?); AgentDiet cannot check or update it
                  → fix the file, then run: agentdiet init --only cursor
  Codex         ⚠ warning         uses a different agentdiet: ~/old/agentdiet (this is ~/.cargo/bin/agentdiet)
  Others        · not installed   Windsurf, Gemini CLI, Copilot (VS Code)
  Shims         ✓ configured      11 commands linked; no shim runs recorded in this workspace yet
  PATH          ✗ broken          ~/.agentdiet/shims is not on PATH in this shell, so the shims are never reached
                  → add to your shell rc: export PATH="$HOME/.agentdiet/shims:$PATH"
  Vault         ✓ ok              empty
  Memory        ✓ ok              nothing recorded in this workspace yet

1 problem, 2 warnings.
```

With hook runs in this workspace's run log, Claude Code reads `✓ working  34 hook runs recorded in
this workspace, the last 2 h ago`; a config whose binary is gone reads `✗ broken  configured binary /gone/agentdiet is
missing` with `→ run 'agentdiet init --only claude' from the agentdiet binary it should use`; a
corrupt `state.json` reads `✗ broken` with `→ rm ~/.agentdiet/state.json`. Project-level configs get
their own row, e.g. `Cursor (project)`.

### 6.3 `agentdiet init`

First run:

```
  Claude Code   ✓ connected           hooks, MCP server, EXPERIENCE.md pointer
  Cursor        ✓ connected           MCP server
  Codex         ✓ connected           MCP server
  Shims         ✓ connected           11 commands linked in ~/.agentdiet/shims

Backed up the originals to ~/.agentdiet/backups/1792506135277

Next:
  1. Add the shims to your PATH (AgentDiet never edits shell rc files):
       export PATH="$HOME/.agentdiet/shims:$PATH"
  2. Restart Claude Code, Cursor and Codex so they load the new config.
  3. Check the setup: agentdiet doctor
```

Second run (same counts, nothing to restart):

```
  Claude Code   ✓ already connected   hooks, MCP server, EXPERIENCE.md pointer
  Cursor        ✓ already connected   MCP server
  Codex         ✓ already connected   MCP server
  Shims         ✓ already connected   11 commands linked in ~/.agentdiet/shims

Next:
  1. Add the shims to your PATH (AgentDiet never edits shell rc files):
       export PATH="$HOME/.agentdiet/shims:$PATH"
  2. Check the setup: agentdiet doctor
```

A skipped file is explained under its target:

```
  Cursor        ⚠ skipped
                  ~/.cursor/mcp.json: not valid JSON (key must be a string at line 1 column 3); it may use comments or trailing commas; left untouched
```

`--dry-run` prints the diffs (the shims as one line, `+ ~/.agentdiet/shims/{git,cargo,…} -> <exe>`),
the same summary with `would connect` / `would update`, then
`Dry run: nothing was written. Run the same command without --dry-run to apply.`

Without a terminal and without `--only`/`--all` (exit 1):

```
agentdiet: init needs a terminal for the checklist. Pass --only <targets> or --all, or add --dry-run to preview (detected here: claude,cursor,codex,shims; all targets: claude,cursor,windsurf,gemini,copilot,codex,shims)
```

### 6.4 `agentdiet mode`

```
Diet Mode: Balanced

  ○ light       outputs over 1,500 est. tokens · must save 40% · 5 context lines
  ● balanced    outputs over 600 est. tokens · must save 20% · 3 context lines
  ○ ultra-lean  outputs over 250 est. tokens · must save 10% · 1 context line

Change it: agentdiet mode <light|balanced|ultra-lean>
```

`agentdiet mode light` → `Diet Mode set to Light.` (again: `Diet Mode is already Light.`);
`agentdiet mode ligth` → clap's `invalid value 'ligth'`, the possible values and
`tip: a similar value exists: 'light'` (exit 2).

### 6.5 Control Center (P1 design)

`inquire::Select` stays (no new dependency). The status block is printed above the prompt, and each
item shows the command that does the same thing, so the menu teaches the CLI.

```
AgentDiet is active · Balanced mode
  Assistants    Claude Code, Shims        Health  ✓ no problems found
  Saved         ≈134,830 tokens in 4 trimmed outputs (all projects, estimated)

? What do you want to do?
> Pause AgentDiet                 agentdiet pause
  Show the last trimmed output    agentdiet show last
  Change Diet Mode (Balanced)     agentdiet mode
  Connect or disconnect assistants  agentdiet init / eject --only
  Check the setup                 agentdiet doctor
  Open the live dashboard         agentdiet top
  Quit
[↑↓ / j k move · type to filter · enter select · esc quit]
```

Navigation rules:

- `↑`/`↓` and `j`/`k` (inquire `vim_mode`) move; typing filters (`pau` + enter pauses); `enter`
  selects; `esc` or `ctrl-c` quits, from the menu and from every sub-prompt (a cancelled sub-prompt
  returns to the menu).
- After an action the menu redraws with the refreshed status block; the result line ("⏸ AgentDiet
  paused.") stays above it.
- Item order: the SPEC §2.1 five keep their order; *Check the setup* and *Quit* are appended.
  Changing §2.1 is part of P1.

`agentdiet top` keys stay `q`/`Esc`, `p`, `m`, `r`; P2 renames its "CAS dereference rate" to
"Re-opened" and the footer to the same vocabulary.

### 6.6 `--help` (P2 design)

clap 4 has one heading for all subcommands (`subcommand_help_heading`) and no per-group headings, so
grouping uses a `help_template` with a hand-written command section (kept in sync by a test that
compares it with the `Command` variants).

```
AgentDiet trims what AI coding agents see from tool output; the full output stays recoverable.

Everyday:
  status            Is AgentDiet on, connected and saving anything?
  pause | resume    Stop or restart trimming
  mode [MODE]       Show or set the Diet Mode (light, balanced, ultra-lean)
  show <ID|last>    Print a trimmed output in full
  doctor            Find and explain setup problems

Setup:
  init              Connect AgentDiet to your assistants
  eject             Disconnect and restore your original configs (alias: uninstall)

Insight & tools:
  top  stats  scan  reflect

Examples:
  agentdiet init --only claude,shims
  agentdiet show last --lines 1..40
  agentdiet mode light

Run `agentdiet` with no arguments for the interactive Control Center.
```

## 7. Exact P0 behaviour

The normative text is SPEC §2.2 (init), §2.5 (terminal output) and §3b (Status, Doctor, Mode,
`show last`). Summary:

- **`status`**: read-only (no GC, no `state.json` write, memory.db opened only if it exists); exit 0.
- **`doctor`**: the same read-only rule; checks Binary, State, each assistant (user scope, plus
  project scope where an entry exists), Shims + PATH, Vault, Memory; exit 1 iff something is `broken`.
  Per-target precedence: missing binary (`broken`) > invalid or unreadable file > skipped (conflict) >
  different binary > incomplete (all `warning`) > configured/working.
- **`mode`**: no argument prints; an argument sets (2 s lock, like `pause`); unknown is clap exit 2.
- **`show last`**: identical bytes to `show '#<id>'`; nothing to show → stderr + exit 1. The ID stays
  required (no default), so an unquoted `show #id` still fails.
- **`init`**: per-target summary; PATH line only when the shims are not first on `PATH`; non-TTY
  `--dry-run` previews the detected targets; non-TTY real run exits 1 naming the detected targets.

## 8. Errors and recovery

| Situation | What the user sees | Recovery |
|---|---|---|
| `state.json` corrupt | doctor: `State broken … not valid state JSON` | `rm ~/.agentdiet/state.json` (counters lost); `resume` is not enough, it writes nothing when already active |
| Config not valid JSON/TOML | doctor `warning` (the assistant may accept JSONC); init `skipped … left untouched` | Fix the file; `agentdiet init --only <t>` |
| Binary moved or deleted | doctor `broken … configured binary … is missing` | Run `init --only <t>` from the binary to keep |
| Two binaries (dev build) | doctor `warning … uses a different agentdiet` | Nothing required; run `init` from the one you want |
| Shims not on PATH | doctor `broken`; init prints the export line | Add the line to the shell rc |
| A command shadows a shim | doctor `warning … git resolves to /opt/homebrew/bin/git first` | Put the shims directory first |
| `show last` with nothing trimmed | stderr `nothing has been trimmed yet`, exit 1 | — |
| `show` on an expired ID | `#id has expired (vault entries are kept 7 days)`, exit 1 | Re-run the command |
| `mode` typo | clap: `invalid value 'ligth'` + `possible values: light, balanced, ultra-lean` + tip | — |
| State lock busy | `state is busy … try again` (existing) | Retry |

## 9. Vocabulary

| Say | Not |
|---|---|
| Vault | CAS, content-addressed store |
| trimmed output | replacement, candidate |
| re-opened (from the Vault) | dereference |
| assistant | target (except in `--only` help, where the slugs are listed) |
| connected / configured / working | patched, installed |
| Diet Mode | profile, level |
| estimated tokens | tokens (bare) |

## 10. Phases

### P0 — everyday commands and honest setup output (done)

Files: `src/cli.rs`, `src/main.rs`, `src/ui.rs` (new: colour, symbols, `~` paths), `src/status.rs`
(new: `status` + `doctor`), `src/control.rs` (`mode` text), `src/init.rs` (per-target report and
printer, non-TTY dry run, PATH check), `src/init/shims.rs` (unchanged shims counted per link),
`src/init/targets.rs` + `json_patch.rs` + `toml_block.rs` (configured-binary extraction),
`src/state.rs` (`Mode` parsing), `src/memory.rs` (`last_run_ms`), `src/lib.rs`, `SPEC.md`,
`README.md` (day-to-day commands), `tests/cli.rs` (integration tests for all of the above).

Acceptance criteria:

- `status` and `doctor` leave `~/.agentdiet` and the project byte-identical (snapshot tests on an
  empty home and on one with a live Vault entry and a memory.db with runs), and never create memory.db
  or `state.json`.
- `doctor` exits 1 for: corrupt `state.json`, a config pointing at a missing binary, shims off `PATH`,
  nothing connected; exits 0 for a fully configured scratch home with the shims first on `PATH`;
  reports `warning` (exit 0) for a different existing binary and for a config that is not strict JSON.
- `mode` with no argument lists the three modes from `ModeParams`; `mode light` / `mode ultra` set
  `state.json`; `mode bogus` exits 2.
- `show last` and `show LAST` print the same bytes as `show '#<id>'`; with nothing trimmed they exit
  1 with empty stdout; a bare `show` is still a usage error (exit 2).
- `init` prints one line per target; a second run says `already connected` for every target and its
  counts match the first; the PATH line is absent when the shims are first on `PATH`.
- `init --dry-run` without a terminal succeeds, writes nothing and creates no `~/.agentdiet`.
- Piped output of `status`, `doctor`, `mode`, `init` contains no `\x1b`; with a TTY, `NO_COLOR=1`
  removes it (unit test on the colour decision).
- Regression: the whole existing suite passes; `stats --json`, `scan`, `show '#id'`, hooks, MCP and
  shim tests are unchanged.

### P1 — Control Center and selective disconnect

- Control Center per §6.5 (status block, command hints, *Check the setup*, *Quit*, `vim_mode`);
  SPEC §2.1 updated. Files: `src/control.rs`.
- `agentdiet eject --only <targets>` (reuses `eject::eject(only)`); `--purge` stays full-eject only and
  errors when combined with `--only`. Eject output in the init format (one line per target:
  `disconnected`, `restored`, `nothing to remove`). Files: `src/cli.rs`, `src/init.rs`.
- First run: bare `agentdiet` with nothing connected offers `init`; `init` ends with the doctor verdict.
- Doctor: collapse repeated "uses a different agentdiet" warnings into one line on the Binary row
  ("assistants run ~/.local/bin/agentdiet; this is a different build"). Seen live on a developer
  machine running the debug build: five identical warnings for one cause.
- Acceptance: menu-order test updated to the new §2.1; `eject --only cursor` leaves Claude Code
  byte-identical; `eject --only x --purge` is a usage error; eject of nothing prints one line.

### P2 — help, vocabulary, errors

- Help per §6.6; `mcp` hidden (decision below); `after_help` examples on `init`, `show`, `mode`, `scan`.
- `stats` text: "CAS dereferences" → "re-opened"; `top`: same; JSON unchanged.
- Error hints: `no vault entry matches #x` adds "IDs come from breadcrumbs; `agentdiet status` shows
  the last one"; the state-busy error names the retry.
- README rewritten around §5 workflows.
- Broken pipes: `stats`, `reflect`, `scan`, `pause`/`resume` and the eject report still use
  `print!`/`println!`, which panic when the reader closes early (`agentdiet stats | head -1`); route
  them through `ui::emit` like the P0 commands.
- Acceptance: a test that every visible `Command` variant appears in the help template; `stats --json`
  snapshot unchanged; `top` render tests updated to the new words.

### P3 — optional

- `agentdiet completions <shell>` (needs the `clap_complete` crate: pure Rust, compile-time only;
  requires the CLAUDE.md dependency justification).
- `status --json` and `doctor --json` for scripts (new stable contracts; SPEC first).
- Re-open warning only once at least 10 outputs were trimmed (avoids "1 of 2 = 50% ⚠").
- Bare-hex breadcrumbs (`Run: agentdiet show 4f9a1b2c`): removes the quoting pitfall and saves bytes,
  but changes model-facing text: needs SPEC §3 Tier 3 item 6, the P5 never-inflate tests and a live
  check that agents still follow it.

## 11. Open decisions (not blocking P0)

1. Hide `mcp` from `--help` (P2)? Recommended: yes; it stays callable.
2. Primary name `eject` or `uninstall`? Recommended: keep `eject` primary, `uninstall` alias (no churn).
3. Add `clap_complete` (P3)?
4. Bare-hex breadcrumbs (P3)?

## 12. Constraints kept

- Shim and hook dispatch in `main.rs` run before clap and before any P0 code. The shim benchmark was
  re-measured against head (SPEC §7, "UX-P0 re-measurement"): B1 3.45/3.58 ms vs head 3.88/3.49 ms,
  B2 2.41/2.41 ms vs 2.59/2.39 ms, within noise. B1 remains NOT MET, as before.
- No new crates. `Cargo.lock` unchanged.
- `status` and `doctor` are read-only; `mode` uses the existing locked `state::set_mode`.
