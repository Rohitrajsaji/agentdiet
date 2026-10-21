# SPEC.md — AgentDiet

## 1. Overview

AgentDiet (`agentdiet`) is a context firewall for AI coding agents. It sits between an agent
and tool output — via agent hooks and PATH shims — and trims what the agent sees. It never
loses data: everything trimmed is stored in the Vault (after secret scrubbing, §3 Tier 3) and can be recovered by short ID. Redacted secrets are never recoverable.

Build rules live in `CLAUDE.md`. This file defines behavior.

**Status tags.** Text with no tag describes behavior that exists today. **[3B]** marks behavior that
this file specifies for Slice 3B (the Diet Engine library) and that is **not implemented yet**; code
and tests that contradict a **[3B]** rule are expected until that slice lands. **[4]** and **[5]**
mark later slices (§8). Nothing tagged here may be described as shipped.

## 2. UX Rules

### 2.1 Control Center

Running bare `agentdiet` in a terminal opens the Control Center, a full-screen ratatui view (no scrollback is
written). Without a terminal it prints the `agentdiet status` output (§3b) and exits 0.

**Panels.** A header (`● Active` or `■ Paused`, the Diet Mode and its one-line description from §3 Tier 2);
*Assistants* (one row per connected assistant: its `doctor` state, §3b); *Savings* (the §2.3 counters, "all
projects, estimated", the re-opened ratio and the last ID); *Needs attention* (the `doctor` problems and
warnings with their fixes; absent when there are none); *Recent activity* (this workspace's newest memory.db
rows); and a footer with the keys. Empty states are explicit: no assistants ("press a to connect one, or run
`agentdiet init`"), nothing trimmed yet, no activity in this workspace yet.

**Keys**, each with a CLI equivalent that the `?` overlay lists:

| Key | Does | CLI |
|---|---|---|
| `p` | pause / resume | `pause`, `resume` |
| `m` | mode picker with each mode's description (`1`/`2`/`3`, or ↑↓ and enter) | `mode <mode>` |
| `l` | scrollable view of the last trimmed output (↑↓, PgUp, PgDn) | `show last` |
| `a` | assistants list; enter connects or disconnects the selected one after a `y/N` confirmation | `init --only`, `disconnect` |
| `d` | every `doctor` check with its fix | `doctor` |
| `t` | the live dashboard (`top`, §3b) in the same terminal; `q` returns | `top` |
| `r` | reload everything, including the checks | |
| `?` | help overlay | |
| `q`, Ctrl-C | quit; Esc and `q` close an open view first | |

**Writes.** The Control Center changes something only through an explicit key: `p`, a picked mode, or a
confirmed connect or disconnect. A failure (a busy lock, an unreadable config) is shown as a message and the
view stays open. Its periodic reload never runs the Vault GC and never writes `state.json`. `l` reads the
entry through the Vault like `show` does (so it shares `show`'s own GC point, §3 Tier 3) but is not an
agent-facing read and is not counted as a re-open (§2.3). State is reloaded every 2 s; the `doctor` checks
run at start, on `r` and after an action. `t` runs the `top` dashboard on the same terminal, including its
Vault GC (§3b).

**Layout.** Below 70 columns the view is one column (the Recent panel is hidden and the footer shows bare
keys); below 40 columns or 12 rows it shows the status line and "Enlarge the terminal, or press q". A resize
redraws. The terminal is restored on every exit path, including a panic and Ctrl-C.

### 2.2 `agentdiet init`

- With no flags, opens an interactive `inquire` multi-select checklist of detected assistants.
- Flags:
  - `--only <targets>` — comma-separated list of targets.
  - `--all` — every supported target.
  - `--project` — write project-level configs instead of user-level ones.
  - `--dry-run` — print the planned diffs; write nothing.
- Targets: Claude Code, Cursor, Windsurf, Gemini CLI, Copilot, Codex, and `~/.agentdiet/shims`.
- Before modifying any file, back it up to `~/.agentdiet/backups/<timestamp>/<target>/…`.
- Patching is **idempotent**: a second run is a no-op; existing agentdiet entries are detected
  and updated, never duplicated. A file is only backed up and rewritten when its new bytes
  differ from its current bytes, so a no-op run creates no backup directory.
- A config that cannot be parsed (e.g. JSONC with comments or trailing commas) is **skipped with
  a warning and never rewritten**.
- **Config backups are a separate contract from the Vault.** A backup is a byte-exact copy of the
  user's original config, because `eject` must restore it byte-for-byte. It is therefore **not
  scrubbed** and may contain secrets the user already had in that file. It is protected by
  permissions instead: every directory under `~/.agentdiet/backups/` is created `0700` and every
  file in it (backups, `manifest.json`, and the temporary files used to write them) is created
  `0600`, regardless of the original file's mode or the process umask. "Created" means from the
  first instant: nothing is written with default permissions and tightened afterwards.
- Rewriting an existing config keeps that file's permissions and, where intentionally supported,
  writes through a symlink to its target (dotfile managers). The temporary file used for the
  rewrite is created `0600`, so a `0600` config is never exposed, even briefly, at a wider mode.
  A config that did not exist before `init` is created `0600`.
- **Codex `config.toml` patching is conservative and validated.** `init` appends or refreshes one
  marker-delimited block (`# >>> agentdiet >>>` … `# <<< agentdiet <<<`) defining
  `[mcp_servers.agentdiet]`. The file is parsed with a real TOML parser, not matched line by line:
  - A file that is not valid TOML is skipped with a warning and left byte-identical.
  - Any existing `mcp_servers.agentdiet` outside our block is a conflict, however it is spelled
    (`[mcp_servers.agentdiet] # note`, quoted keys, dotted keys, an inline `mcp_servers = {…}`).
    It is skipped with a warning and left byte-identical.
  - Marker lines count only when they are real comments. A marker inside a multi-line string, a
    duplicated marker, a missing or reversed marker, or a block whose body is not exactly an
    agentdiet entry is a conflict: the file is skipped and never partially edited or stripped.
  - The patched text is parsed again before it is written. It must be valid TOML and must equal the
    original document plus the one `mcp_servers.agentdiet` entry. Otherwise the file is skipped.
  - All edits are byte edits on the original text (append, or replace the block's own lines), so
    the user's comments and formatting are untouched.
  - `eject` applies the same marker validation and removes nothing it cannot prove is ours.
- Shell rc files are never edited; `init` prints the `PATH` line for the shims directory instead,
  unless the shims directory already comes first on this process's `PATH` (the `doctor` PATH check,
  §3b "Doctor").
- **The binary's path.** Every config `init` writes (hooks, MCP servers, shim links) names this binary
  by an absolute path: the path it was **invoked by** (`argv[0]`, made absolute, or found on `PATH` when
  it is a bare name) when that is named `agentdiet` and resolves to the same file as the running
  executable, so a package manager's stable link (Homebrew's `bin/agentdiet` → `Cellar/…/<version>/…`)
  is what assistants keep, and an upgrade does not break them. Otherwise, and always on Windows, the
  operating system's executable path. (Linux's `/proc/self/exe` resolves every link: until 0.2.0 Linux
  recorded the versioned target, so `doctor` reported a missing binary after an upgrade until `init`
  was re-run.)
- **Choosing targets without a terminal.** `--only` and `--all` pick the targets. Without either, a
  terminal gets the checklist. Without a terminal (stdin or stdout not a TTY), `--dry-run` previews
  the targets the checklist would pre-select (the detected assistants, plus the shims at user scope),
  and a real run exits 1 with a message that names `--only`, `--all` and the detected targets. Neither
  case writes or creates anything.
- **Output.** `init` prints one line per selected target, in checklist order: the target's label,
  a word for what happened, and a short detail. The words are `connected` (agentdiet was not present
  and its entries were written), `updated` (agentdiet was present and at least one file was
  rewritten), `already connected` (every file was up to date), `skipped` (nothing written, with the
  reason) and `partly connected` (some files written or up to date, some skipped, with the reasons).
  A dry run says `would connect` / `would update` instead and prints the planned diffs first. Paths
  under the home directory are shown as `~/…`. Shim symlinks are counted as commands (`11 commands
  linked`), never listed one per line. After the targets come the backup directory, when anything
  was backed up, and a numbered **Next** list: the `PATH` line (as above, only when the shims were
  selected), "restart <the assistants just connected or updated> so they load the new config" when
  anything was written, and `agentdiet doctor`. Between the targets and **Next** a **Verification** block
  gives each connected target the state `doctor` would (§3b): `verified` (runs of it were observed in
  this workspace), `configured, not yet observed` (Claude Code's hooks and the shims, which AgentDiet can
  observe, but has not yet), `configured` (an MCP-only assistant, which AgentDiet cannot attribute runs to),
  or its problem with the fix. Only the selected targets are checked, and a problem already named in
  **Next** (the `PATH` line) is not repeated. Configuration alone is never called working. A dry run
  skips verification and ends with
  `Dry run: nothing was written. Run the same command without --dry-run to apply.`
  This output is for people and is not a stable format; the library's `InitReport` is.
- Slice 1 writes, per target (user scope / `--project` scope):

  | Target | User scope | `--project` |
  |---|---|---|
  | Claude Code | hooks in `~/.claude/settings.json`; MCP in `~/.claude.json` (`mcpServers`); pointer in `~/.claude/rules/agentdiet.md` | `.claude/settings.json`; `.mcp.json`; `.claude/rules/agentdiet.md` |
  | Cursor | `~/.cursor/mcp.json` (`mcpServers`) | `.cursor/mcp.json` |
  | Windsurf | `~/.codeium/windsurf/mcp_config.json` (`mcpServers`) | not supported; skipped with a notice |
  | Gemini CLI | `~/.gemini/settings.json` (`mcpServers`) | `.gemini/settings.json` |
  | Copilot (VS Code) | `<config dir>/Code/User/mcp.json` (`servers`, `"type": "stdio"`) | `.vscode/mcp.json` |
  | Codex | `~/.codex/config.toml`, marker block `[mcp_servers.agentdiet]` | `.codex/config.toml` |
  | Shims | `~/.agentdiet/shims/<cmd>` symlinks to the binary | always global |

  Claude Code gets hooks for SessionStart, UserPromptSubmit, PreToolUse, PostToolUse,
  PostToolUseFailure and PreCompact, and a **one-line pointer** to `EXPERIENCE.md` (§3b "Reflect")
  in a rules file of its own. Claude Code loads every `.claude/rules/*.md` (and `~/.claude/rules/*.md`)
  without `paths` frontmatter at launch (code.claude.com/docs/en/memory, read 2026-10-19), so no
  CLAUDE.md is edited. The line ends with the marker `<!-- agentdiet:experience-pointer -->`; patching
  appends or refreshes that one line and `eject` removes only it (or the whole file, when `init`
  created it and nobody edited it). The other assistants get only the MCP entry until the hook slice.
- Shim commands: `git`, `cargo`, `pytest`, `npm`, `pnpm`, `yarn`, `go`, `make`, `ruff`, `docker`,
  `gh`. What a shim does with them is decided per call (§3 Tier 1, "Shim decision order").

### 2.3 Stateful Pause / Resume

Global state lives in `~/.agentdiet/state.json`:

```json
{
  "status": "active",
  "mode": "balanced",
  "raw_tokens_passed_while_paused": 0,
  "on_session_start": "prompt",
  "last_trimmed_id": null,
  "trimmed_outputs": 0,
  "tokens_saved": 0,
  "cas_dereferences": 0
}
```

| Field | Values |
|---|---|
| `status` | `"active"` \| `"paused"` |
| `mode` | `"light"` \| `"balanced"` \| `"ultra-lean"` |
| `raw_tokens_passed_while_paused` | non-negative integer |
| `on_session_start` | `"prompt"` \| `"auto_resume"` (default `"prompt"`) |
| `last_trimmed_id` | ID without the `#` (e.g. `"4f9a1b2c"`) of the most recently trimmed output, or `null` (default). Read by the Control Center's "Inspect last trimmed output". Any value of at least 4 hex is accepted on read. |
| `trimmed_outputs` | non-negative integer: replacements the engine emitted (`Replaced`, §3 Tier 2) |
| `tokens_saved` | non-negative integer: the sum, over those replacements, of the scrubbed original's estimated tokens minus the emitted text's (§3 Tier 2, "Estimated tokens") |
| `cas_dereferences` | non-negative integer: successful Vault reads by an agent-facing path, i.e. `agentdiet show '#id'` and the MCP tool `diet_inspect` (every breadcrumb and fold marker points at `show`). `agentdiet show last`, the Control Center's last-output view (`l`) and `agentdiet top` are not counted: they are a person reading. |

- A missing, unreadable or corrupt `state.json` loads as defaults (fail open); unknown fields are ignored,
  and a missing counter loads as 0.
- **The three counters are telemetry, best effort and approximate.** `trimmed_outputs` and `tokens_saved`
  ride the engine's existing `state.json` update for `last_trimmed_id` (one locked write, no extra
  `fsync`); `cas_dereferences` is one locked update after a successful read. Each waits at most 100 ms for
  the lock and is dropped when it cannot take it (§3 Tier 3, "`state.json` updates are serialised").
- **CAS dereference rate** = `cas_dereferences / trimmed_outputs`, shown as *re-opened* by `status`,
  `stats`, the Control Center and `top` (§3b) as "none yet" while `trimmed_outputs` is 0. It is flagged as
  a warning ("trimmed output is often read back in full; consider a lighter mode") only when it is
  **strictly above 20% and `trimmed_outputs` is at least 10** (`state::REOPEN_MIN_SAMPLE`). Below 10 a
  rate that would be high is shown with "too few trimmed outputs to judge yet (needs 10)" instead: one
  re-open of four outputs is 25% but says nothing yet. The counters, the rate and `state.json` are
  unchanged.
- **Attribution (audited for P3).** `cas_dereferences` counts every successful `show '#id'` and every
  `diet_inspect`. `diet_inspect` is always an agent (MCP), but `show '#id'` run by an agent through its
  shell cannot be told apart from the same command typed by a person: only Claude Code exports a session
  marker, so any heuristic would be wrong for the other assistants. People-facing text therefore says
  *re-opened* (an ID was read back in full), never "by agents". `show last` and the Control Center's `l`
  are still not counted: those are certainly a person.

- Writes are atomic (temp file + rename).
- Pausing: `agentdiet pause`, the Control Center toggle, or typing `@diet pause` in chat
  (detected in UserPromptSubmit).
- Once paused, it **stays paused** until manually resumed via `agentdiet resume`, the
  Control Center toggle, or typing `@diet resume` in chat (detected in UserPromptSubmit).
- While paused, the Diet Engine performs **no trimming, TOON encoding or folding**, and
  `raw_tokens_passed_while_paused` accumulates the estimated tokens (§3 Tier 2, "Estimated tokens")
  of the output passed. Secret scrubbing still applies (precedence rule P3): "raw" means
  *untransformed*, not *unscrubbed*. Only the two scrubbing exclusions listed under P3 (the human shim
  path and binary output) bypass scrubbing.
- **SessionStart while paused never blocks.** Hooks have no interactive TTY, so nothing ever
  reads stdin for confirmation.
  - `prompt` (default): emit exactly one line to hook stdout:
    `⏸ AgentDiet is paused. Run 'agentdiet resume' or type '@diet resume' to re-enable.`
  - `auto_resume`: set `status` to `"active"` and emit a one-line notice that it resumed.
- Binary output is not counted in `raw_tokens_passed_while_paused` (it has no text tokens); every
  other passed output is counted on its scrubbed bytes.
- **Paused whisper:** while paused, any output whose estimated token count exceeds **3,000** gets
  that same one-line paused notice appended. This is the one sanctioned exception to "never
  inflate" (§6): the notice is not counted against any saving. Binary output (P3) never gets a
  notice, because it is passed through byte-identical. Oversize output (P3) is scrubbed and gets the
  notice only if the counter says so: more than 8 MiB does not imply more than 3,000 estimated tokens
  (8 MiB of spaces counts 0), so the actual counter decides.

### 2.4 Escape Hatches

- **Full-output directive.** If a UserPromptSubmit contains one of the explicit directive tokens
  `@diet:full`, `/diet-full` or `--diet-full`, compression is bypassed for **exactly one tool
  execution**: the next tool call of that session. Natural language never triggers it: "full report
  link", "raw data", "unfiltered", "full-stack" and "full path" are prose, not directives.
  - Matching is case-insensitive and token-exact: the directive must start the prompt or follow a
    character that is not a letter, digit, `_`, `-`, `/`, `@`, `:` or `.`, and must end the prompt or
    be followed by a character that is not a letter, digit, `_` or `-`. So `` `@diet:full` `` and
    `(--diet-full)` trigger it, while `@diet:fullscreen`, `--diet-fully`, `x@diet:full` and the path
    `docs/diet-full` do not. `@diet:full` is the primary spelling: a prompt that *starts* with
    `/diet-full` may be taken by the assistant as a slash command before any hook sees it.
  - Lifecycle: UserPromptSubmit **arms** the flag (`raw_turn`) and every other prompt disarms it.
    The next PreToolUse **binds** it to that call: it stores the sha256 of the call's `tool_use_id`
    (`raw_tool_use`) and disarms `raw_turn`. That call's PostToolUse (or PostToolUseFailure, for a
    failing call) **consumes** it: the output is bypassed and the binding is cleared. A shim running
    inside the bound call (a shimmed `cargo` in that Bash command) bypasses too, and never clears the
    binding, so the same call's PostToolUse still sees it. An interrupted call that fires neither
    event leaves a binding that the next UserPromptSubmit or SessionStart clears.
- Running the exact same command twice in a row auto-expands the output on the retry. "Same" means
  identical structured argv (and working directory) as the immediately preceding tool call in the
  same session. Tracking:
  - **Hooks** key a session sidecar by the payload's `session_id`. PreToolUse computes a signature
    for every tool call (Bash: working directory plus `parse_simple_command` argv, or the raw command
    string when it is not one simple command; any other tool: tool name plus its input) and, when it
    equals the previous call's signature, marks that `tool_use_id` as a retry. PostToolUse bypasses
    a marked call.
  - **Shims** compare working directory plus argv against the previous shim run in the same
    sidecar, kept in a field of their own (a hook signature and a shim signature never compare
    equal). Claude Code exports `CLAUDE_CODE_SESSION_ID` to the commands it runs, so a shim under
    Claude Code uses that session's sidecar and sees the raw-turn flag set by UserPromptSubmit.
    Elsewhere a shim keys its sidecar by the PID of its nearest AI-agent ancestor (`pid:<n>`), or by
    the shared key `agent:unknown` when the agent was recognised only by an environment marker;
    those agents have no hooks, so only retries apply to them.
  - Sidecars live at `~/.agentdiet/sessions/<first 32 hex of sha256(key)>.json` (directory `0700`,
    files `0600`) and hold only `raw_turn` and sha256 hashes (the previous hook signature, the
    previous shim signature, the retry's `tool_use_id`, the directive-bound `tool_use_id`): never
    command text (P1). Every
    read-modify-write holds `sessions/.lock` for at most 100 ms; a sidecar that cannot be read,
    written or locked counts as "no bypass" (fail open). Sidecars are scratch state, written
    atomically but without `fsync`: a crash can only lose a bypass. SessionStart deletes sidecars
    older than 7 days. `state.json` keeps exactly the §2.3 shape.
- Every bypass skips trimming, TOON and folding and writes nothing to the Vault. None of them skips
  secret scrubbing (precedence rule P3).

### 2.5 Terminal output

These rules cover output meant for people: `status`, `doctor`, `mode`, `init`, `eject`, `pause`,
`resume` and the Control Center. They never apply to agent-facing text, which keeps its own exact
contracts: the breadcrumb and fold markers, `show`'s raw bytes, `stats --json`, `scan`'s report lines
and exit codes, hook stdout and the MCP server.

- **Colour** is used only when the stream it is written to is a terminal, `NO_COLOR` is unset or
  empty, and `TERM` is not `dumb`. stdout and stderr are checked separately. Piped or redirected output
  never contains an escape sequence.
- **Glyphs.** `✓ ⚠ ✗ · ● ■ ○ ≈ → …` need a UTF-8 terminal. When the locale (`LC_ALL`, else `LC_CTYPE`, else
  `LANG`, the first one set) names a non-UTF-8 charset (`C`, `POSIX`, `ISO8859-1`) or `TERM` is `dumb`,
  people-facing output and the Control Center use `+ ! x - * # o ~ -> ...` instead (`■` is `#`). An unset locale counts as
  UTF-8. With `NO_COLOR`, the Control Center uses bold and reverse video, never colour.
- **Symbols are never the only signal.** Every `✓`, `⚠`, `✗` or `·` is followed by a word
  (`working`, `configured`, `warning`, `broken`, `not connected`, …), so the text reads correctly
  without colour, in a screen reader, or pasted into an issue.
- **Width.** Lines are laid out for an 80-column terminal: a label column as wide as the widest label
  shown (at least 12 characters), a status column, then free text. Fixed-text lines (`mode`) fit in 80
  columns; details that hold paths are allowed to wrap. Nothing depends on the terminal's width.
- **Paths** under the home directory are shown as `~/…`, diff headers included.
- **Pipes.** A reader that closes the pipe early (`agentdiet doctor | head`, `agentdiet show last | head`)
  ends the output quietly, for every command: never a panic, never a "Broken pipe" message. What the
  command does is not affected: a state change (`pause`, `resume`, `mode <m>`, `disconnect`, …) is made
  before anything is printed and stays made; `show` writes the same bytes up to the point the reader
  left and exits 0; `scan` keeps the exit status its findings decide (0, 1 or 2); `stats --json` exits
  0. A closed stderr is ignored the same way. Output that is data (`show`, `stats --json`, `scan`,
  `reflect --dry-run`'s file text) is never transliterated to ASCII; only people-facing text is.
- **Numbers** carry their scope and their nature: counters from `state.json` are "all projects",
  run log totals are "this workspace", and token counts are "estimated" (§3 Tier 2).
- **Full-screen views** (the Control Center and `top`) follow the same glyph and colour rules (`top`'s
  boxes become `+ - |` on an ASCII terminal). While one is open:
  - nothing is printed to stderr over it: a corrupt or unreadable `state.json` is shown inside the view
    (the Control Center's *Needs attention*, `top`'s footer), and the state warning that the CLI prints
    is suppressed for that process;
  - SIGTERM, SIGHUP and SIGINT restore the terminal (raw mode off, main screen, cursor shown) and the
    process then ends by that same signal, so a parent sees the real cause. The handler only records
    the signal (an atomic store); the restore runs in ordinary code, within 100 ms, from the event
    loop. Known limits: SIGKILL and SIGSTOP cannot be caught, so they leave the terminal as it was
    (`reset` fixes it); a signal that arrives while a confirmed connect or disconnect is being written
    takes effect once that action has finished (it is never cut short half-way; each file is written
    atomically); after SIGHUP the terminal
    may already be gone, and the restore's writes then fail silently; raw mode turns Ctrl-Z into a
    plain key, which is ignored; on platforms without Unix signals there is no handler.
- **Errors** a person sees say what failed, why, and what to do next, on at most three lines, with
  `~/…` paths, and never quote stored output. For `show`:

  | Case | Message (after `agentdiet: `) |
  |---|---|
  | unknown ID | `no trimmed output #<id> in the Vault.` · where IDs come from (the `🥗 Diet` line), `agentdiet show last`, and the 7-day lifetime |
  | expired ID | `#<id> has expired: Vault entries are kept 7 days after their last use.` · run the command again to see its full output |
  | invalid ID | `"<input>" is not a Vault ID: IDs are 4 or more hex characters, like '#4f9a1b2c'.` · copy it from the `🥗 Diet` line and quote it (an unquoted `#` starts a shell comment), or use `show last`. One pair of single quotes around an ID is accepted (`cmd.exe` passes them on, §10) |
  | ambiguous | the candidates, then "type more characters of the ID" |
  | damaged entry | `#<id> is damaged in the Vault (its bytes no longer match its ID), so nothing is shown.` · run the command again |
  | bad `--lines` | `"<input>" is not a line range: use START..END, 1-based and inclusive, like 10..40.` |
  | nothing trimmed | `nothing has been trimmed yet, so there is no last output to show.` · `agentdiet status` shows whether an assistant is connected |

  These are the CLI's words only: `CasError`'s own text, which the MCP tool `diet_inspect` returns to
  agents, does not change. The `state.json` warning shows `~/…` paths.
- **Words.** The user-facing names are *Vault* (never "CAS"), *trimmed output*, *re-opened* (a trimmed
  output was read back in full by its ID, by an agent or a person; `cas_dereferences` in JSON), *assistant*,
  *shims*, *Diet Mode*, *this workspace's activity* (never "run log" or "origin"), *untrimmed (still
  scrubbed)* (never "raw" for output that was scrubbed) and *estimated* tokens. A phrase that appears in
  more than one view (the status word, savings, re-opens, what passed while paused, the Vault, the mode
  line) is produced by one function, so `status`, `doctor`, `stats`, the Control Center and `top` cannot
  drift apart. People-facing output never says "CAS", "dereference", "run log", "origin", "TUI" or "LLM";
  paths such as `~/.agentdiet/state.json` are allowed. JSON keys, `state.json` fields and agent-facing
  text keep their names.

## 3. Architecture — 3 Tiers

### Tier 1 — Hook Router & PPID Shims

- `main.rs` inspects `argv[0]`. If invoked as `git`, `pytest`, `cargo` (and so on) through
  `~/.agentdiet/shims/`, it runs in shim mode.
- Shim mode decides between two paths:
  - **Transparent (human) path:** `execv` straight through to the real binary (resolved from `PATH`
    with the shims directory excluded). The requirement is **< 0.2 ms** of added end-to-end time; it
    is currently **NOT MET** (§7). Nothing is captured, scrubbed, stored or measured, and AgentDiet
    makes no privacy claim about it.
  - **Agent path:** run the command, capture its stdout and stderr, and route each through the Diet
    Engine (§3 Tier 2) as `Shell { argv, exit }`.
- **Shim decision order** (first match wins, cheapest checks first, so a human's terminal pays for
  no process walk):
  1. `AGENTDIET_SHIM_ACTIVE` is set → transparent. The agent path sets it on its child, so a shimmed
     command run by another shimmed command (a `git` inside a `cargo` build script) is never
     captured: its output is read by a program, not the model.
  2. `AGENTDIET_FORCE_HUMAN` set → transparent; `AGENTDIET_FORCE_AGENT` set → skip steps 7–8.
  3. stdin or stdout is a terminal → transparent (a human is at it, or the command is interactive).
  4. stdout is a character device (`> /dev/null`) → transparent.
  5. `compress::detect(argv)` finds no compressible shape → transparent. Only recognised shapes with
     finite output are captured, so dev servers (`npm run dev`, `cargo run`), `gh … --json` and
     interactive commands are never buffered. `npm run …` is never a recognised shape for that
     reason, which is why a typical web build (`npm run build`, `npm run lint`) is not captured by a
     shim: under Claude Code its successful output is still scrubbed and logged by PostToolUse, and a
     failure is logged by PostToolUseFailure. The transparent path `exec`s by design (B1, and a
     human's interactive command must own its terminal); it records nothing.
  6. stdout is not the **same open file as the parent's stdout** → transparent. Identity is device and
     inode (macOS `proc_pidfdinfo` vnode info, Linux `stat` of `/proc/<pid>/fd/1`) or, for a macOS
     pipe, the pipe's kernel handle; an identity that cannot be read counts as different. A command
     the agent's shell runs as-is writes where the shell writes, which is the agent; a redirection
     (`> out.txt`) or a pipe onward (`| tail`) gives the command its own stdout, so it is never
     dieted. Observed with Claude Code 2.1.302: a Bash command's stdout and stderr are a **regular
     file** (Claude Code's capture file) shared with the per-command `zsh` that is its parent, stdin is
     `/dev/null`, and nothing is a terminal; a file type alone could not tell that capture file from
     `> out.txt`.
     **Shells that redirect in the parent (found on Linux in P4).** The identity comparison assumes the
     shell applies `> out.txt` in the forked child. Measured on Debian 12 (2026-10-20): only **bash**
     and **zsh** do; **dash** (Debian/Ubuntu `/bin/sh`), **mksh**, **ksh**, **posh**, **yash** and
     **busybox sh** apply it in the parent shell before forking, so while the command runs the parent's
     stdout is that same file and the comparison wrongly says "the parent's". So when stdout is a
     **regular file** and the parent's process name is not `bash` or `zsh` (a leading `-` of a login
     shell ignored), the command is transparent. A pipe keeps the identity rule (a pipeline's ends are
     the child's own, in every shell measured). Cost of the miss: under such a shell an agent capture
     that is a regular file is not dieted (it is, as always, still a correct, untouched output).
  7. One of the environment markers `CLAUDECODE`, `CURSOR_TRACE_ID`, `GEMINI_CLI` is set → agent.
  8. The parent chain contains an AI agent → agent; otherwise transparent. The walk follows parent
     PIDs from the shim's parent, at most **16** hops, stops at PID 1 or a PID already seen, and
     reads process data through libc only (macOS `proc_pidinfo`/`proc_pidpath`, Linux `/proc`), never
     through a helper binary. A process is an agent when its process name (macOS `p_comm`, Linux
     `comm`) or its executable's base name, lowercased, is one of `claude`, `cursor`, `windsurf`,
     `codex`, `aider`, `gemini`, `copilot`, `opencode`, alone or followed by a space or `-`
     (`Cursor Helper (Plugin)`). Both are needed: Claude Code's executable is named after its
     version (`…/claude/versions/2.1.302`) while its process name is `claude`. Substrings never match
     (`claudette` is not `claude`).
- Agent path details: stdin is inherited; stdout and stderr are read on two threads until the child
  exits; each stream is one engine call (so each may carry its own breadcrumb and Vault entry) and is
  written back to its own descriptor. The shim then exits with the child's code, or re-raises the
  child's terminating signal (falling back to exit `128 + n`). A failure to spawn exits `127`/`126`
  as the transparent path does. Nothing re-runs the command (P2).
- **Known misses (shims):** an agent started as `node`/`python`/`bun` is found only through an
  environment marker. A command whose shell `exec`s it in the shell's own place (`sh -c 'cargo
  test'` with nothing after it) has the agent itself as its parent and is not dieted (step 6); Claude
  Code 2.1.302 runs commands under a per-command shell that does not do this. Inside a pipeline the
  last command shares the shell's stdout, so in `cat log | cargo test` only `cargo test` is dieted,
  correctly; but `(cargo test) | tail` is not, because the subshell's stdout is already the pipe.
  A live check under Claude Code 2.1.302 (2026-10-17): `cargo test` run as-is was dieted with a
  breadcrumb and kept exit 101; `cargo test | tail -3` and `cargo test > out.txt` received the raw
  output.
- Hook mode (`agentdiet hook <event>`, §3 Tier 1 "Hook router").
- **PreToolUse must protect interleaved thinking blocks:** it never modifies, reorders or
  strips them. It achieves this by emitting nothing at all: no `updatedInput`, no decision.

#### Hook router

Contract checked against the official Claude Code hooks reference (code.claude.com/docs/en/hooks,
read 2026-10-17) and payloads recorded by Claude Code **2.1.302**. A newer Claude Code may change
these shapes; re-checking them is a deliberate change to this section.

- **Input.** One JSON object on stdin with `session_id`, `cwd`, `hook_event_name` and per-event
  fields. Stdin is read only for the five handled events (unknown events, and `noop` used by the
  B2 benchmark, return at once without touching it), never when it is a terminal, for at most
  **5 s** and **32 MiB**. A timeout, an oversize payload, invalid JSON, a missing field or any
  internal error makes the hook exit `0` with nothing on stdout (fail open). Diagnostics go to stderr
  only. The payload is never echoed back.
- **Closed or broken stdio.** A hook whose stdout or stderr is closed, full of errors, or gone still exits
  `0` and never panics: a failed write is dropped, including the panic hook's own fixed note (a panic
  while handling a panic would abort). No write blocks longer than the write itself (no retries, no
  waiting). The same holds for the MCP server's notes (a failed note must not turn a successful
  `diet_exec` into an error) and for the shims' error messages (the shim's exit status stays the one it
  documents, e.g. 127 when the real program is missing).
- **Output channel.** SessionStart and UserPromptSubmit add plain stdout to the model's context;
  PostToolUse can only change a tool result through JSON
  `{"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedToolOutput":…}}`, whose value must
  match that tool's own result shape (Claude Code silently ignores a mismatched value); PreCompact's
  output is discarded by Claude Code.
- **SessionStart:** clears the session's sidecar and deletes stale sidecars. While paused: `prompt` prints the §2.3 notice line, `auto_resume` resumes and
  prints one line saying so. With `source: "compact"` (the SessionStart that follows a compaction)
  it also prints a **snapshot** of at most 2 KiB: the last trimmed ID and the most recent
  `memory.db` entries (kind, title, ID if the Vault entry is live), all of it already-scrubbed text.
  memory.db is never created just to be read.
- **UserPromptSubmit:** `@diet pause` / `@diet resume` (case-insensitive, whole words, anywhere in
  the prompt) set the status and print the one-line confirmation of `agentdiet pause`/`resume`; a
  full-output directive arms the raw-turn flag and any other prompt disarms it and clears a stale
  binding (§2.4).
- **PreToolUse:** records the retry signature and binds an armed raw-turn flag to this call (§2.4);
  prints nothing.
- **PostToolUse:** builds engine settings from `state.json` (mode, paused) and the sidecar (bypass:
  a marked retry is `RepeatedCommand`, else the raw-turn flag is `RawNextTurn`) and runs the engine
  per text field; it prints the replacement JSON only if some field's bytes changed. A copy of
  `tool_response` is made with only those string fields replaced, every other field kept as it was.
  - `Bash`: the result is `{stdout, stderr, interrupted, isImage, noOutputExpected, …}`. `stdout` and
    `stderr` are each `Shell { argv, exit: 0 }` (PostToolUse fires only for a successful command),
    argv from `parse_simple_command` or empty when the command is not one simple command (then the
    text is only scrubbed). A field that already carries an AgentDiet breadcrumb line was dieted by a
    shim and is left alone.
  - `Read` — result `{type: "text", file: {content, filePath, numLines, startLine, totalLines}}`,
    `content` being the file's own text. **Not folded** in Slice 4 (how an agent edits a folded file
    is still open, §3 Signature folding): passed as `Read { extent: Ranged }`, so it is scrubbed
    only.
  - MCP tools (`mcp__*`, result a string or an array of `{type: "text", text}` blocks) and
    `WebFetch` (`result`): each text is `ModelJson`. JSON may become TOON; anything else is only
    scrubbed.
  - AgentDiet's own MCP tools (`mcp__agentdiet__*`) are left untouched: their results were produced by
    the Diet Engine or the Vault already, and re-processing a `diet_inspect` result would trim (or TOON)
    the very output the agent asked to see in full.
  - Every other tool is left untouched.
- **Failing commands.** A Bash command that exits non-zero fires PostToolUseFailure, which can only
  *add* context, never replace the result. The hook therefore cannot shrink a failing `cargo test`;
  the shim (agent path) is what diets failing commands. PostToolUseFailure is registered
  **record-only**: it prints nothing, spends a full-output directive bound to the call (§2.4) and
  records the failure in the run log (§3 Tier 3, "Run log"). Its payload (code.claude.com/docs/en/hooks,
  read 2026-10-19) carries `tool_name`, `tool_input`, `tool_use_id` and an `error` string whose first
  line is `Exit code N` for a Bash command that ran; the rest of `error` is display text and is never
  stored.
- **PreCompact:** exits `0` with no output and never blocks compaction; the snapshot is delivered by
  the SessionStart that follows (above).
- **Panics.** The hook and the agent shim install a panic hook that prints only `PANIC_NOTE` to
  stderr and never the payload; the hook then exits `0` with nothing on stdout.
- **Live check.** Unit and integration tests cannot prove that a given Claude Code accepts the
  `updatedToolOutput` shape. It is verified by hand: `agentdiet init --only claude --project` in a
  scratch project, then a large successful `cargo test` through Claude Code, and the model must see
  the breadcrumb. Status: see §8.
  **Run 2026-10-17, Claude Code 2.1.302, headless (`claude -p`, `--mcp-config .mcp.json`), release binary
  of `1e72a5d`, `cargo` not shimmed:** passed. The model quoted the Bash breadcrumb
  (`kept 14/412 lines; cargo test: 398 passing test lines collapsed`), quoted a `Read` of a file holding an
  AWS key as `[REDACTED:aws-access-key-id]` (never the key), and quoted the exact `diet_inspect` slice of
  that ID; `state.json` then held `trimmed_outputs` 1 and `cas_dereferences` 1. That slice was 5 lines of
  plain text, which no `mcp__*` rule would change anyway, so a second run checked the `mcp__agentdiet__*`
  exclusion: `diet_inspect` (no range) of a Vault entry holding a 17,582-byte pretty-printed JSON array
  of 200 uniform objects, which as any other MCP tool's result becomes TOON. The model received all
  17,582 bytes as JSON, with no TOON and no breadcrumb.
  Payloads captured by a second PostToolUse hook in that run: Bash `tool_response` had exactly the keys
  `stdout, stderr, interrupted, isImage, noOutputExpected`, with cargo's stderr already merged into
  `stdout` (`stderr` empty); Read `{type, file}`; an MCP tool result an array of `{type, text}` blocks.

### Tier 2 — Fast-Path Diet Engine

Slice 3B delivered the engine as a **library API**; Slice 4 calls it from the hook router
(PostToolUse) and the shims' agent path (§3 Tier 1). End-to-end acceptance by Claude Code is
subject to the live check recorded there.

#### Output pipeline and precedence **[3B]**

The engine takes the captured bytes of one tool output (plus structured metadata: argv, exit
status, tool kind) and returns the bytes to emit. When rules conflict, the **lower-numbered rule
wins**.

- **P1 — Nothing unscrubbed reaches disk.** The Vault, `memory.db`, `state.json`, breadcrumbs,
  summaries and diagnostics hold only scrubbed text. This covers recorded command lines and
  arguments too (token-bearing git URLs, `curl -H "Authorization: Bearer …"`). The one exception is
  the byte-exact config backup made by `init` (§2.2). The same holds for what AgentDiet itself prints:
  every stderr line, MCP tool error and JSON-RPC error message, and a usage error that quotes an
  argument, is scrubbed, because each may quote the caller's own input (an ID, a path, a value).
  Checked at every entry point with a key planted in the input and error paths forced (P4). Not
  covered by design: the transparent shim path, which passes the real program's bytes untouched.
  The hook, shim and MCP entry points print only a fixed note on a panic; other commands keep
  Rust's default panic message.
- **P2 — Never break the caller (fail open).** Any error or panic inside the engine yields the best
  safe output: the scrubbed output if scrubbing finished, otherwise the raw output (a scrubber
  failure is the one case where privacy yields to delivering the command's output). The engine only
  ever sees output of a command that has already run; **no processing error may cause a command to
  run again.**
- **P3 — Redact on emit, in every mode.** Detected secrets are replaced (§3 Tier 3) even when
  paused, in raw mode, bypassed or in identity mode. A redaction can make text longer
  (`[REDACTED:aws-access-key-id]` replaces a 20-byte key with 28 bytes), so "never inflate" (P5) is
  measured against the *scrubbed* original. Redacted spans are not stored anywhere and are
  **never recoverable**. Detection is pattern based and best effort: AgentDiet does not claim to find
  every secret. Exactly **two exclusions from scrubbing** are explicit:
  1. the transparent shim path (Tier 1). This includes a shim run *by an agent* whenever the decision
     order picks the transparent path (a terminal, a device or another program's pipe on stdout, a
     nested shim, or an argv shape the engine does not compress): what reaches the agent then is
     unscrubbed. Under
     Claude Code, a successful Bash call's output is still scrubbed by PostToolUse;
  2. **binary output** (a NUL byte in the first 8 KiB): passed through byte-identical, neither
     scrubbed, transformed nor stored, because rewriting inside binary data corrupts it. Text in
     UTF-16 contains NULs and is therefore treated as binary.

  Separately, **oversize output** (more than 8 MiB; an initial value, to be re-evaluated with
  measurements) *is* scrubbed, but is never transformed, stored or indexed: it is emitted as the
  scrubbed original with no breadcrumb. This is a limit on transformation, not an exclusion from
  scrubbing.
- **P4 — Identity.** If nothing was redacted and no transform was applied, the output is the exact
  input bytes, including non-UTF-8 bytes, CRLF line endings and a missing final newline.
- **P5 — Never inflate (final check).** A replacement (trimmed text, TOON or folded text) is emitted
  only if the **complete final text**, including the actual breadcrumb, folding markers and any
  notices, is strictly smaller than the scrubbed original in **bytes** and in **estimated tokens**,
  and also meets the mode's required saving (below). Otherwise the scrubbed original is emitted with
  no breadcrumb. The check runs again after persisting (P6), when the real ID length is known; a
  cheaper pre-check with an 8-hex ID decides whether to persist at all, so most hopeless candidates
  never touch the Vault. An entry persisted for a candidate that then fails the final check is
  harmless: nothing references it and GC removes it.
- **P6 — Recoverability, persist first.** Before a replacement is emitted, the full scrubbed
  original is written to the Vault durably (temp file, `fsync` of the file, rename, `fsync` of the
  directory). Durability is never traded for latency: skipping `fsync` is not an acceptable
  optimisation.
  - Vault write fails: emit the scrubbed original, no breadcrumb.
  - Vault write succeeded but the `state.json` or `memory.db` update fails: still emit the
    replacement, and write one bounded (≤ 200 bytes), scrubbed note to **stderr**. Diagnostics never
    go to stdout of `agentdiet mcp`, nor to a hook's output channel.
  - Reading an entry whose bytes no longer hash to its name reports a distinct **corrupt** outcome
    that is neither "not found" nor "expired", and returns no content. **[3B]**
    Today `show` returns the bytes without re-hashing, and `put` fsyncs the file but not its
    directory; both gaps are closed in 3B.
- **P7 — Bypass.** Paused status, a full-output directive (`@diet:full`, next tool call only), and the same
  command run twice in a row (§2.4) skip trimming, TOON and folding, and write nothing to the
  Vault. Scrubbing still applies (P3).

Evaluation order: classify (binary / oversize / bypass) → scrub → build candidate → pre-check →
persist → final check → emit → best-effort index and state update.

**Library contract.** `diet.rs` exposes a pure step (`transform`: classify, scrub, candidate,
pre-check; no I/O, no clock, no globals) and a persisting step (`finish`: durable Vault write, actual-ID
rebuild, final check, then best-effort state and index updates). Callers pass the bytes plus:

- the **source**: `Shell { argv, exit }` (output of a command that already ran, `exit` being a code,
  a signal or unknown), `Read { path, extent }` (`extent` is `Complete` for the whole file, or `Ranged`
  for a slice, which is never folded and never described as a complete file), or `ModelJson` (a
  JSON document the caller marks as model-facing, such as an MCP tool result: the only source that
  may become TOON);
- the **settings**: mode, paused flag and bypass reason (`None`, `RawNextTurn`, `RepeatedCommand`).

The result is the bytes to emit, what happened (identity, scrubbed only, binary, oversize, paused,
bypassed, replaced, storage failed) and a bounded list of scrubbed diagnostics. **The library never
prints**: the caller writes a diagnostic to stderr. Nothing in the engine executes or re-executes a
command; it is handed output that already exists.

- **Breadcrumb framing.** A replacement body that does not end in a line break gets one (`\r\n` if the
  body's last line break is CRLF, else `\n`) before the breadcrumb line, and the breadcrumb ends with
  the same break. Those bytes count in P5. Folded output carries no trailing breadcrumb: its markers
  are the breadcrumbs.
- **Panics.** The engine runs inside `catch_unwind`. A panic after scrubbing emits the scrubbed
  original; a panic in the scrubber emits the raw output (the documented P2 exception). The panic
  payload is never read or printed by the engine; the diagnostic is the fixed text
  `agentdiet: internal error; output passed through`. The default panic hook still prints the
  message and location to stderr, so a binary that runs the engine in a hook (Slice 4) must install a
  hook that does not echo payloads; a library must not replace a process-global hook. Out-of-memory
  and process aborts cannot be recovered from in process and are not claimed to be.
- **Timing.** The pure step and the persisting step are measured separately (§7).

#### Estimated tokens **[3B]**

`estimate_tokens(bytes)` is a deterministic *estimate*, defined only on the bytes:

- a maximal run of ASCII letters, digits or `_` counts `ceil(len / 4)`;
- a maximal run of `\n` and `\r` bytes that contains at least one `\n` counts 1;
- any other space, tab, or `\r` run without a `\n` counts 0;
- every other UTF-8 scalar value, and every invalid byte, counts 1.

It is used for the mode thresholds, `raw_tokens_passed_while_paused` and the 3,000-token notice. It
says nothing about any provider's tokenizer: a shorter string can have more provider tokens, and no
tokenizer is bundled. The only cross-provider guarantee AgentDiet makes is in bytes (P5), and the
estimate is an additional gate, never a replacement for the byte comparison.

#### Diet Modes **[3B]**

| Setting | Light | Balanced | Ultra-Lean |
|---|---|---|---|
| Minimum estimated tokens before any trimming or TOON is attempted | 1500 | 600 | 250 |
| Required saving, in bytes and in estimated tokens, breadcrumb included | ≥ 40% | ≥ 20% | ≥ 10% |
| Context lines kept around each error/warning line | 5 | 3 | 1 |
| Last lines always kept | 20 | 10 | 5 |
| Minimum body lines for folding (a file must also exceed 300 lines, in every mode) | 20 | 8 | 3 |

Diagnostics are not mode-dependent. Every mode keeps **every meaningful warning and error line
verbatim**. A warning may be collapsed only if it is an exact duplicate of another line (kept once,
with a count) or matches an explicit, per-tool noise rule below; the count is reported in the
breadcrumb. A large number of warnings is never a reason to drop them.

#### Shell output compression **[3B]**

- **Dispatch uses structured data, never substrings of shell text.** The engine is given the
  executable and arguments as an argv vector. A hook's shell string qualifies only if it parses as
  exactly one simple command (no pipe, `;`, `&`, `&&`, `||`, redirection, subshell, command
  substitution or newline outside quotes). Anything else is left unchanged. Known miss: compound
  commands such as `cd app && cargo test` are not compressed by the hook (a shimmed `cargo` inside
  them still is, on the agent path).
- **Machine-readable output is never altered:** `--porcelain`, `-z`, `--format`/`--pretty`/
  `--template`, `--json`, `--message-format` (any value), `--output-format`, `-json` and similar
  flags; `git diff`, `git log`, `git show`, `git blame` and `git status --short`; and any invocation
  whose stdout is redirected to a file or piped onward. The engine is only ever handed output that
  goes to the model, and a caller must not hand it output that goes to a file or another program;
  shims check that a command's stdout is its shell's own (§3 Tier 1, decision step 6).
- **Dispatch is by executable and subcommand.** The executable is the basename of `argv[0]`
  (`python -m pytest` counts as pytest); global options that take a value are skipped by a per-tool
  table (`git -C dir`, `cargo +nightly`); an unknown option before the subcommand means the shape is
  not understood and the output is unchanged. A string merely containing `cargo` or `test` somewhere
  never selects a tool.
- **Shell strings.** `parse_simple_command` turns a hook's shell string into an argv only when it is
  exactly one simple command (optionally preceded by `NAME=value` assignments); any metacharacter,
  expansion or comment outside single quotes makes it `None`.
- **Protection.** A noise line is removed only if it is further than the mode's context distance from
  every error/warning/failure line and outside the mode's last lines. Everything within reach of a
  diagnostic stays, whether or not it looks like noise.
- A line is removed only if it matches an explicit noise rule for the detected tool. Everything
  else stays. Always kept: error and failure lines with their context lines, the last N lines, the
  exit status, and every line not covered by a noise rule. Unknown tools, unparseable output and
  binary output are left unchanged (scrubbed only). Processing is byte-based, so non-UTF-8 output
  is safe; line endings are preserved; carriage-return progress bars collapse to their final state;
  colour codes are stripped only for supported tools.

  | Tool | Noise that may be removed or counted |
  |---|---|
  | cargo | `Compiling`/`Checking`/`Downloading`/`Downloaded`/`Fresh` lines; passing `test … ok` lines collapsed to a count. Failure sections are kept. |
  | pytest | progress dots and `PASSED` lines. FAILURES/ERRORS sections and the short summary are kept. |
  | go test | `=== RUN`, `--- PASS`, `ok pkg` lines. `--- FAIL` blocks and panics are kept. |
  | npm, pnpm, yarn | progress lines; repeated `warn deprecated` lines grouped with a count. `ERR!` lines are kept. |
| npm/pnpm `ls`, `list`, `ll`, `la`; `yarn list` | tree lines ending in ` deduped` (a package already shown elsewhere), counted. Every other line is kept. Only the tree's own problem markers (head/tail fallback, below) protect their neighbours: `es-errors` and `UNMET OPTIONAL DEPENDENCY` are not diagnostics here. `--json` and `--parseable` are machine-readable and never altered. |
  | git | transfer progress only. |
  | make | `make[N]: Entering/Leaving directory` lines. |
  | docker | layer pull/download progress. Step headers and errors are kept. |
  | ruff | in Ultra-Lean only, the source-snippet lines under a diagnostic; every diagnostic line is kept. |
  | gh | none from a shell invocation: `gh … --json` output is machine-readable and is never altered. Its JSON is eligible for TOON only as a separately marked `ModelJson` input (below). |

- Rules informed by other tools are recorded in code as `// pattern ref: <repo>/<path> (behavior
  only)` and follow the clean-room rules in `CLAUDE.md`.

#### Head/tail fallback (`diet_exec` only)

`diet_exec` is a request by the agent for a dieted run, so its output gets one more rule that hooks
and shims never apply (they see output such as a `cat` or a `git diff` that must not be cut, and
"unknown tools are left unchanged" still holds for them). When the output of a `diet_exec` command has
more than `4 × (head + tail)` lines, where *tail* is the mode's "last lines always kept" and *head* is
twice that (Light 40 + 20, Balanced 20 + 10, Ultra-Lean 10 + 5, so more than 240 / 120 / 60 lines), and
no tool rule above brings it down to that many kept lines, it is cut to:

- its first *head* and last *tail* lines;
- every problem line with the mode's context lines around it. A problem line is a dependency-tree
  marker (`UNMET ` except `UNMET OPTIONAL`, ` invalid`, ` extraneous`, ` missing`, `ERR!`,
  `npm error`) or, except in an `npm`/`pnpm`/`yarn` tree listing, one of the protection rule's
  generic diagnostics. In a tree, package names such as `es-errors` are not diagnostics and an
  `UNMET OPTIONAL DEPENDENCY` is another platform's optional binary (a live Next.js tree had 78 of
  them);
- one marker line per elided run, `[… N lines elided: A..B …]`, where `A..B` is the exact 1-based
  line range in the Vault copy (`agentdiet show '#<id>' --lines A..B`, or `diet_inspect` with
  `start_line`/`end_line`), with the run's own line ending.

Machine-readable invocations are never cut (`--json` and the other flags above, `git diff`/`log`/
`show`/`blame` and `git status --short`). The mode's token minimum, the saving gate (P5), persist-first
(P6) and the breadcrumb all apply as to any replacement; the breadcrumb's summary reads
`<tool>: N lines elided (head H, tail T and problem lines kept)`. A tool rule that already shrinks the
output to at most the limit wins (a `cargo test` keeps its failure sections whole). Known miss: output
that is one enormous line is not cut.

#### TOON encoding

Output that parses, as a whole, as one JSON document is a TOON candidate. AgentDiet targets
**`toon-spec: 4.4`** (§9 pins the exact source). TOON output is not byte-identical to the input, so
it always goes through the Vault and carries a breadcrumb; the breadcrumb is a separate line
*outside* the TOON document, which itself has no trailing newline (spec §12).

The encoder implements a **subset**, and where the normative spec would require a form AgentDiet
does not implement, the encoder **declines and the output is left unchanged** (it never emits a
lesser form):

- Supported: a root object (§8; the empty one is the empty document, which the engine never
  replaces anything with), a root primitive (a root string starting with U+FEFF is quoted), a root
  primitive array or a root tabular array (§9.1, §9.3) and the empty root array `[]`; nested and
  empty objects (`key:`); primitive values; empty arrays as `key: []`; inline primitive arrays
  `key[N]: a,b`; and flat tabular arrays `key[N]{f1,f2}:` whose elements are all non-empty objects
  with **identical key sequences** and only primitive values. Comma delimiter, 2-space indent, LF.
- Declined (output unchanged): keyed tabular objects, including at the root (§9.5, decided by the
  spec's exact test: at least two entries, every value a non-empty object, one key set, every
  column uniform-primitive or nested-uniform; an object that fails the test nests as §8 says and is
  encoded); nested field groups (§9.3, an object-valued column); every list form (§9.2, §9.4 and
  §10: arrays of arrays, mixed arrays, arrays with empty or non-uniform objects, array-valued
  columns); arrays of objects with the same key *set* in a different order (the spec requires
  tabular form there, which would reorder keys).
- Strings are quoted per §7.2 and escaped per §7.1 (only those escapes, `\uXXXX` in lowercase hex
  for other control characters); keys per §7.3 (non-ASCII keys are quoted).
- Numbers: the JSON number's **original text** is emitted only if it is already canonical (§2):
  plain decimal, no exponent, no leading zeros, no unnecessary trailing *fractional* zeros (`10` and
  `100` are fine, `1.50` and `1.0` are not), not `-0`, at most 15 significant digits, and zero or
  within [1e-6, 1e21). Any other number (`1.10`, `1e5`, `-0`, a 17-digit id) makes the whole document
  decline. Known miss: 64-bit ids above 15 digits keep such documents out of TOON. The original text
  is retained with `serde_json`'s additive `raw_value` feature (`RawValue`): unlike
  `arbitrary_precision` it does not change how `Number`/`Value` behave for `state.json` and the
  assistants' configs, which are parsed and re-serialised elsewhere.
- **Source.** A shell invocation's stdout is never converted, `gh … --json` included (machine-readable
  output is never altered, above). A document becomes a TOON candidate only as `ModelJson`.
- The JSON reader is strict RFC 8259 (`serde_json`, with a visitor that records each object's keys in
  order and rejects a repeated key). Duplicate object keys, a lone surrogate escape, a byte order
  mark, trailing content, or nesting deeper than 64 make the document decline. Re-parsing nested
  values costs at most 64 passes over the input.
- **Conformance evidence** is the official MIT-licensed fixtures at the pinned revision (§9),
  vendored with their copyright notice in `THIRD_PARTY_NOTICES`: for every `encode` fixture the
  encoder must either reproduce the expected output byte for byte or decline, and every fixture
  inside the supported subset must be reproduced, and the test fails if the supported set shrinks
  below a recorded count (declining everything must not pass). No decoder is written: the evidence is
  the official expected outputs plus a differential test against the official reference encoder.
  Measured: of the 160 official `encode` fixtures, 94 are reproduced byte for byte, 45 are declined
  (each one checked to be a documented unsupported form, option or number) and 21 use non-default
  options (tab/pipe delimiters, other indents). The reference encoder (`@toon-format/toon` 4.4.0, not
  vendored or linked; `tests/toon_reference.rs`, opt-in) agreed with this encoder on 305,000 generated
  documents with no difference, every decline being explained by the document.

#### Signature folding

- Applies to a `Read` whose `extent` is `Complete`, whose scrubbed text has more than 300 lines, with
  the language taken from the file extension: Rust, TypeScript/TSX, JavaScript and Python, via
  statically linked tree-sitter grammars. Input that is not valid UTF-8, has an unknown extension, or
  parses with any error is left unchanged. A `Ranged` read is never folded: its line numbers would not
  be the file's, and it is not described as a complete file. The input is the file's own bytes; a
  tool's decorated rendering (such as `cat -n` line prefixes) must be converted to file bytes by the
  hook layer or not folded at all. Slice 4's hook does not fold Reads at all (§3 Hook router).
- **Bounded parse.** The parser does not report a syntax error until it has read the whole input, so
  text that is not source code would otherwise run it to the end of the input. Two guards bound
  that cost:
  - *Prefix probe.* A file longer than 2 KiB first has its first 2 KiB, cut at a line boundary,
    parsed with a work limit of **40 progress-callback steps** (the parser calls back once per fixed
    batch of its internal operations, so the limit measures work, not time, and behaves the same on
    every machine and build). Over the test fixtures in all five languages, cut at every line
    boundary and shifted through the file, valid code needed at most 20 steps and number-and-bracket
    text at least 71. Prose and diagnostics need *fewer* steps than valid code, so only the
    structural rules below catch them.
    The file is left unchanged if the probe exceeds the limit; or if the prefix has a syntax error
    (an `ERROR` or `MISSING` node) that starts **before the start of the prefix's last top-level
    node**; or if `ERROR` nodes cover at least 90% of the prefix and hold no well-formed node of more
    than one line (prose or diagnostics, which Rust's grammar swallows into a single `ERROR` node
    with few steps). A construct left open by the cut (an unclosed function, `impl`, block or
    triple-quoted string) is none of these: its errors lie inside the last top-level node, and its
    `ERROR` node holds the complete functions before the cut. A prefix with no line break is not
    probed.
  - *Full parse.* The whole parse has a **30 ms** deadline in a release build (300 ms in an
    unoptimized one, which parses about ten times slower); overrun leaves the file unchanged. A valid
    source whose parse takes longer than that on the machine, for example under heavy load, is
    therefore not folded.
  Text that looks like source to the prefix probe is bounded only by the full-parse deadline. Tests
  cover a valid function, `impl` and class spanning the probe's cut (still folded) and non-source text
  in each language (unchanged without a full parse).
- The parse runs on the **scrubbed** text, so every line number is already a line of the Vault copy.
  The mode's token minimum does not apply to folding; the 300-line rule, the body-length rule and the
  saving requirement do.
- Only function and method **bodies** are replaced. Signatures, attributes, doc comments,
  docstrings, classes, impls, traits, types, imports and constants stay, and the result keeps valid
  indentation.
- A body is folded only if it has at least the mode's minimum number of lines and its marker is
  shorter than the body. A marker looks like
  `{ /* folded L120-188: agentdiet show '#4f9a1b2c' --lines 120..188 */ }`. Line numbers refer to
  the scrubbed Vault copy; a multi-line redaction (a private key) shifts them relative to the
  file on disk, which a test must cover.
- In Python the folded body becomes `...` followed by a `#` comment carrying the same marker text,
  so the result is still valid at the original indentation; in the brace languages the body becomes
  the `{ /* … */ }` form above.
- A folded view is not the file. How an agent edits a file after a folded `Read` is still an open
  question; until it is settled, no entry point folds a `Read`.

### Tier 3 — Persistent Vault

1. **Scrub secrets** before storing or emitting anything the engine handles, using regexes ported
   from Secretlint's rule set. Secretlint is MIT-licensed; ported patterns keep attribution in a
   `THIRD_PARTY_NOTICES` file. The two exclusions (human shim path and binary
   output) are listed under precedence rule P3 in Tier 2; oversize output is scrubbed but not
   transformed or stored. Detection is best effort, not a guarantee.
2. **Store the scrubbed output losslessly** (byte-exact apart from redactions) at `~/.agentdiet/cas/<full_sha256>.log`. The 64-hex sha256 is
   computed over the scrubbed output. Storage is content-addressed, so identical output dedupes.
3. **7-day auto-GC.** CAS files older than 7 days are deleted opportunistically (on writes, or by
   `agentdiet top`). `show` on an expired ID reports "expired" rather than "not found".
4. **IDs.** An ID that AgentDiet **emits** (breadcrumbs, fold markers, `state.json`, ambiguity
   candidates) is the shortest prefix of the sha256 that is unique among every live entry **and
   every tombstone**, but never shorter than **8** hex characters (`cas::MIN_EMIT_ID_LEN`); the full
   64 hex if nothing shorter is unique. It is written `#4f9a1b2c`. Input IDs are resolved by unique prefix match against the CAS filenames and
   may be as short as 4 hex; an ambiguous prefix is an error that lists the candidates. Uniqueness
   is a property of the vault at emission time: an older ID can later become ambiguous, and then
   reports the ambiguity rather than returning a different entry.
5. **Slicing:** `agentdiet show '#4f9a' --lines <start>..<end>` returns only those lines
   (1-based, inclusive).
6. **Breadcrumb.** Append one line to the trimmed output **[3B]**:
   `🥗 Diet: <summary of what was trimmed> Run: agentdiet show '#4f9a1b2c'`
   The ID is always single-quoted, because an unquoted `#…` starts a shell comment in `sh`, `bash`
   and `zsh`.

   **Bare IDs were assessed for P3 and deferred** (2026-10-20). Measured with the engine's estimator on
   real lines: dropping `'#…'` saves 3 estimated tokens and 3 bytes per breadcrumb or fold marker, 2.5%
   of the 120 tokens emitted for `seq 1 4000` through `diet_exec` and 1.0% of the 309 emitted for the
   README's `cargo test` row (less for larger outputs). In `sh`, `bash` and `zsh` the quoted form,
   `"#id"` and a bare ID all print the entry; an unquoted `#id` is already a safe usage error (`show`
   requires an ID, §3b). `diet_inspect` and `show` accept both forms already (`normalize_id`). Against
   that: the hook recognises existing breadcrumbs by `" Run: agentdiet show '#"` (`hook::has_breadcrumb`),
   so both forms would have to be recognised indefinitely; 18 assertions in 6 test files (plus unit tests
   in `diet.rs` and `hook.rs`) pin the format; a bare 8-hex
   string reads like a git short hash next to `git` output; and that agents still follow the new line
   could be checked live for Claude Code only. A 1–2.5% saving does not outweigh a model-facing change
   that cannot be verified across the assistants, so the format stays. Revisit with live checks on more
   assistants. The summary is scrubbed and at most 160 bytes, e.g. `kept 42/1830 lines; cargo test:
   1780 passing lines collapsed; exit 101; 2 secrets redacted (not recoverable)`. The whole line
   counts against the no-inflation check (P5).
7. **Index summaries** into `.agentdiet/memory.db` (project-local SQLite, WAL, FTS5 + BM25) in the
   **workspace root** (below). PreCompact recovery uses this index to re-surface relevant context.
   Index rows hold scrubbed text only (P1). Every bounded text AgentDiet writes (an entry's title —
   the command line or the read path —, a run title, the summary, a stderr note) is scrubbed whole
   first and only then cut to its bound, so a secret straddling the cut is redacted rather than kept
   as a prefix too short to match; the cut text is scrubbed again. Entry titles are cut to 240 bytes.

#### Workspace root

memory.db and `EXPERIENCE.md` live in one directory per project, the *workspace root*, so that a
command run in `app/src` records into the same memory.db as one run at the top:

1. `AGENTDIET_WORKSPACE`, when it names a directory (absolute, or relative to the working directory).
2. Otherwise the nearest directory, starting at the working directory (a hook's payload `cwd`, the
   MCP server's directory, a shim's current directory) and walking up at most 64 levels, that already
   holds a regular file `.agentdiet/memory.db` or has a `.git` entry (a directory, or a worktree's
   file). The nearest marker wins, so a package that deliberately has its own memory.db keeps it.
3. Otherwise the working directory itself.

The home directory is never a workspace root and the walk never goes above it: `~/.agentdiet` is the
global directory, not a project's. `init --project` still writes assistant configs to the working
directory itself. Discovery runs only where memory.db is used: never on the transparent shim path.

#### Run log

Schema v3 of memory.db adds a `runs` table: one row per command or tool call an entry point saw,
trimmed or not, holding the time, the origin (`shim`, `mcp`, `hook`), the kind (`shell`, `read`,
`search`), the family (the executable's base name, or the tool name), the title (the command line,
path or pattern: scrubbed, then cut to 240 bytes, so a secret straddling the cut is still redacted),
the outcome (`ok`, `fail`, `miss`), the exit code (`128 + n` for a signal; none when unknown), the
estimated tokens before and after AgentDiet, the Vault entry when one was stored and the session
label (the agent's session id, `pid:<n>` or `agent:unknown`). It never holds output.

- **Writers.** A shim's agent path and `diet_exec`: every run, with its exit status. PostToolUse:
  every successful Bash call except one a shim already dieted (it carries a breadcrumb and the shim
  logged it), Reads of at least 1,000 estimated tokens, and Grep/Glob calls whose result carries a
  count field (`numFiles`, `filenames`, `numLines`, `numMatches`, `content`): zero is a `miss`. The
  result shapes are not documented by Claude Code. Glob's was checked live (below); Grep's was not. A
  result with none of those fields is not logged. PostToolUseFailure: Bash (`fail`, exit
  code from the `Exit code N` line), Read, Grep and Glob (`miss`); a user interrupt is not logged.
- **Bounds.** Telemetry, best effort: memory.db is opened with a 100 ms busy timeout and a failure is
  at most a bounded stderr note; it never changes what the agent is given. The newest 10,000 rows are
  kept. Logging adds a SQLite open and insert to every hooked Bash call and every agent-path shim run;
  that cost is measured in §7.
- A binary from before schema v3 refuses a v3 memory.db ("schema version 3 is newer"), and fails open
  as for any memory.db error.
- **Live check (2026-10-19, Claude Code 2.1.303, headless `claude -p` in a Next.js project after
  `agentdiet init --only claude --project`, release binary of this change).** The prompt contained the
  words "full report link" and armed nothing. A Glob with no matches was logged `miss` and one with
  matches `ok`; `git status --shortt` came back through PostToolUseFailure and was logged `fail` with
  exit code 129, and the corrected `git status --short` `ok` with exit 0; all four rows carried the
  run's session id. Asked whether any startup instruction mentioned `EXPERIENCE.md`, the model quoted
  the pointer line from `.claude/rules/agentdiet.md`.

#### Tier 3 details (Slice 2)

- **Redaction:** a secret span is replaced by `[REDACTED:<rule-id>]`; surrounding text (e.g. the
  variable name) is kept. PEM private-key blocks are replaced whole. Credentials embedded in a URL
  (`scheme://user:PASSWORD@host`, or a 20+ character token used as the user name) are redacted
  (`url-password`, `url-token`; original rules), which is what makes recorded `git clone` and
  `git remote -v` lines safe to store (P1). A short user name such as `git@` is not a secret.
- **CAS files** are `<64hex>.log` (dir mode 0700, files 0600). Writing identical content again
  refreshes the file's mtime instead of rewriting it; the 7-day TTL counts from the last write.
- **GC** runs on `put` and on `show`. Deleting an expired file leaves a zero-byte
  `<64hex>.expired` tombstone, which is how `show` tells "expired" from "not found". A new `put`
  of the same content removes the tombstone. Tombstones are kept until the vault is purged: they
  are the vault's memory of which IDs were once issued.
- **An old ID never resolves to an unrelated entry.** The short ID shown for an entry is the
  shortest prefix (at least 8 hex, item 4) that no *other* entry shares, counting both live entries and
  tombstones. Resolving an input prefix looks at live entries **and** tombstones:
  - one match, live: that entry;
  - one match, tombstone: `expired`;
  - several matches, all tombstones: `expired` (every candidate is gone; nothing is returned);
  - several matches, at least one live: an ambiguity error listing every candidate, in sorted
    order, as the shortest prefix that distinguishes it, with expired ones marked `(expired)`.
    Content is never returned for an ambiguous prefix.
  So an old breadcrumb whose entry has expired reports `expired` or an ambiguity, even if a
  different entry has since landed on the same first characters. Re-putting identical content
  revives its entry (its tombstone is removed); an old shorter ID that is now shared with another
  entry reports an ambiguity, and the new, longer ID is the one to use.
- **Vault entries are plain files, never links.** Only regular files named `<64hex>.log` /
  `<64hex>.expired` count as entries. A symlink (or other non-regular file) in the vault directory
  is never read, written or followed: it is ignored by lookups and replaced by a regular file when
  its name is needed. A dedupe hit re-checks the stored bytes against the sha256 before reusing
  them; a mismatch is rewritten.
- **ID input** is accepted as `#4f9a` or bare `4f9a` (quote it: an unquoted `#...` is a shell
  comment). Case-insensitive, hex only, at least 4 characters.
- **Slicing** splits on `\n` and keeps line endings (CRLF and a missing trailing newline survive).
  A start of 0 or start > end is an error; an end past EOF clamps; a start past EOF yields nothing.
- **memory.db** has an `entries` table (`kind` is `command` | `read` | `decision`) mirrored into
  an FTS5 table over `title` and `body`. Results are ordered by `bm25()` ascending (lower is
  better) with title weighted above body. Query tokens are quoted as FTS5 phrases and ANDed, so
  user text can never produce an FTS syntax error.
- **memory.db rows are events, not content.** The same output produced by different commands or
  sessions is separate rows, so retrieval keeps its provenance. A row is a duplicate only if its
  kind, scrubbed title, scrubbed body, `cas_sha` and session all match (a repeated identical run in
  one session is one). The body is part of the key so that a different summary (for example another
  exit status over the same output) is not discarded. The key is a `NOT NULL` column holding the
  sha256 of those fields, each length-prefixed, an absent `cas_sha`/session counting as the empty
  string, so a `NULL` can never defeat uniqueness. It is enforced by a unique index plus
  `INSERT OR IGNORE` in one statement, not by a read-then-insert check, so concurrent inserts cannot
  duplicate (schema v2). The migration runs in an `IMMEDIATE` transaction that re-reads
  `user_version` first (concurrent first opens are safe), deletes existing duplicates keeping the
  lowest `id` and fills the key.
- **memory.db metadata.** `cas_sha`, when given, must be 64 lowercase hex; `session` is free text and
  therefore only accepted as scrubbed text of at most 256 bytes. The file is created `0600` before
  SQLite opens it (SQLite gives `-wal` and `-shm` the main file's mode) and the project
  `.agentdiet/` directory `0700`. The final path component is opened with `O_NOFOLLOW|O_NONBLOCK`
  first, and once SQLite has opened the file, before anything is read or written, two checks must
  pass: SQLite's own handle still matches the path (`SQLITE_FCNTL_HAS_MOVED`: the inode SQLite opened
  against a fresh `stat`), and the path is still the inode opened first. Comparing the path alone is
  not enough: a file swapped in for SQLite's `open` and swapped back before the check would pass it.
  (`SQLITE_OPEN_NOFOLLOW` is not used: it rejects a symlink anywhere in the path, such as macOS's
  `/var` or a project below a linked directory.) The bundled SQLite opens the database, `-wal` and
  `-shm` with `O_NOFOLLOW` itself, so a symlink planted at any of them (a cloned repository can carry
  one) is never written through; indexing fails instead and the engine's note says so.
  **What remains** (documented, not prevented): a double swap between those two `stat` calls, which
  needs write access to the `.agentdiet/` directory, that is, the user's own privileges; on Windows
  the VFS has no `HAS_MOVED` and `require_same_file` checks only that the path is a regular file
  (§10). Error values never contain row text.
- **Private roots** (P4.5, owner's decisions of 2026-10-21). Everything AgentDiet writes lives under
  one of two roots: `~/.agentdiet` (state, Vault, sessions, backups) and the workspace's
  `.agentdiet/` (memory.db). Before anything is created under a root, the root itself is checked:
  - missing: created `0700`, as before;
  - **the project root is a symlink: refused.** A cloned repository can carry `.agentdiet` as a link
    to anywhere in the user's files; project memory and the run log are then off in that workspace
    (the engine's note says the memory index failed), and `doctor` says why and how to fix it
    (replace the link with a directory, or set `AGENTDIET_WORKSPACE`);
  - **the home root is a symlink:** followed when its target is a directory owned by the user
    (dotfile managers, another disk), refused otherwise;
  - not a directory, or **owned by another user: refused**;
  - owned by the user with group or other permission bits (`0755` from a checkout or a manual
    `mkdir`): **tightened to `0700`** by the first write. `status` and `doctor` never write: they
    report it ("tightened to 0700 on the next write") and, for the project root, do not open
    memory.db until then.
  Directories and files *inside* a root keep the rules above (created private, never widened).
  Installations made by earlier versions are unaffected: every root they created is a `0700`
  directory owned by the user. On Windows only the symlink rule applies (the profile's ACL is the
  protection, §10).
- **`state.json` updates are serialised.** Every read-modify-write holds an exclusive advisory lock
  (`flock` on the private sibling file `state.json.lock`, created `0600`, never deleted) from the read
  to the rename. An explicit `pause`/`resume`/mode change waits at most **2 s** and then fails with a
  "state is busy" error; engine telemetry (`last_trimmed_id`, the paused-token counter) waits at most
  **100 ms** and is dropped when it cannot take the lock, so a hook is never held up for long. The
  lock is released on every path, including errors and panics (it is the open file's lifetime), and
  a crashed holder releases it. Atomic rename alone is not enough: it makes a write whole, not the
  read-modify-write exclusive. `load` is lock-free (it only ever sees a whole file) and opens the file
  without blocking, so a FIFO planted at `state.json` is reported as unreadable, not waited on.
- **The Vault is serialised too.** `put`, `gc`, `resolve` and `read` each hold an exclusive lock on
  `cas/.lock` (waiting at most 2 s, then `Busy`; the engine treats `Busy` as a failed Vault write and
  emits the scrubbed original). `put` holds the lock from the write through GC and ID selection, so a
  concurrent GC can never delete content that was just re-put, and the breadcrumb for it is valid the
  moment `put` returns. Two locks are never held at once (the CAS lock is released before `state.json`
  or `memory.db` is touched), so there is no lock ordering to get wrong, and no function takes its own
  lock twice. **Bound of the guarantee:** an entry is kept for 7 days after its last `put`, measured
  by each process's wall clock; after that GC may remove it, and a process whose clock runs more than
  a TTL ahead can remove it early. A breadcrumb is therefore valid for 7 days from the last time that
  exact content was stored. (When identical content is stored again, the refreshed timestamp is not
  `fsync`ed: a crash right then can only leave the older, earlier expiry, never lose the content.)
- **Persistence order and durability.** A write goes to a temp file, is `fsync`ed, renamed into
  place, and the containing directory is `fsync`ed. A directory created for the occasion is also
  made durable (its parent is `fsync`ed). GC creates tombstones, `fsync`s the directory, removes the
  logs and `fsync`s again, so an issued ID is never forgotten across a crash. On macOS the standard
  library's `sync_all` is `F_FULLFSYNC`; that cost is reported, not avoided (§7). Reads verify the
  sha256 before returning any byte, and a mismatch is the distinct `Corrupt` error, which carries no
  content (P6).
- **Opening an entry.** Vault entries, `memory.db` and lock files are opened with `O_NOFOLLOW` and
  `O_NONBLOCK` and the opened descriptor must be a regular file, so a symlink or a FIFO swapped in
  after any earlier check cannot be followed or waited on. `O_NOFOLLOW` covers the final path
  component only: a symlinked *parent* directory (for example `~/.agentdiet` itself) is followed, and
  anyone who can replace that directory already owns the user's account. Config files that `init`
  edits keep their separate, intentional rule of writing through a symlink to its target.

### 3b. Interfaces & Tools (outside the hot path)

- **`agentdiet mcp`** — stdio JSON-RPC 2.0 MCP server exposing four tools:
  - `diet_inspect` — fetch a CAS entry by `#id`, with an optional line range.
  - `diet_search` — FTS5/BM25 search over `memory.db`.
  - `diet_exec` — run a command through the Diet Engine; returns trimmed output plus a breadcrumb.
  - `diet_reflect` — `agentdiet reflect` (below) as a tool.
- **`agentdiet status`**, **`agentdiet doctor`**, **`agentdiet mode [<mode>]`** and
  **`agentdiet show last`** — the everyday commands for people (below).
- **`agentdiet stats [--json]`** — the §2.3 counters plus the workspace's run log totals (below).
- **`agentdiet reflect [--dry-run]`** — learn `EXPERIENCE.md` from the run log, with no LLM (below).
- **`agentdiet scan`** — static regex security scanner for skill, rule and MCP config files.
  Flags hidden Unicode, secrets, prompt-injection phrases and dangerous shell commands; exits
  non-zero on findings.
- **`agentdiet top`** — the live ratatui dashboard (below).

#### MCP server

- **Transport.** Newline-delimited JSON-RPC 2.0 on stdin/stdout, synchronous (one request at a time,
  no async runtime). Methods: `initialize` (echoes the client's `protocolVersion`), `ping`,
  `tools/list`, `tools/call`; notifications get no reply; anything else is `-32601`. **Stdout carries
  JSON-RPC lines only**: engine notes and every other diagnostic go to stderr. The process installs the
  quiet panic hook (§3 Tier 1, "Panics") and each `tools/call` runs inside `catch_unwind`, so a panic is
  an `isError` result with the fixed `PANIC_NOTE` and the server keeps serving.
- **Results.** Every tool result is `{"content": [{"type": "text", "text": …}, …], "isError": bool}`.
  `isError` is `true` for invalid arguments, an ID that is invalid, not found, expired, ambiguous or
  corrupt (the text says which, with the candidates of an ambiguous prefix), a command that cannot be
  spawned, and a command that timed out. A command that ran and exited non-zero is **not** an error:
  its output and exit status are the result. `tools/call` with an unknown tool name, or without a
  `name`, is the JSON-RPC error `-32602`.
- **Project.** The workspace root (§3 Tier 3) of the server's working directory is the project whose
  `.agentdiet/memory.db` is searched and indexed and whose `EXPERIENCE.md` `diet_reflect` writes
  (assistants start MCP servers in the project directory).
- **`diet_inspect`** `{id: string, start_line?: integer, end_line?: integer}`. `id` is `#4f9a` or `4f9a`
  (§3 Tier 3). The lines are 1-based and inclusive, as `show --lines`: only `start_line` reads to the
  end, only `end_line` reads from line 1, `0` or start > end is an error. The text is the stored
  (scrubbed) bytes, decoded as UTF-8 with invalid sequences replaced. At most **256 KiB** is returned,
  cut at a line boundary; when cut, a second text block says how many lines were left out and to ask
  for them with `start_line`/`end_line`. A successful read counts one `cas_dereferences` (§2.3).
- **`diet_search`** `{query: string, limit?: integer}` (default 10, clamped to 1..50). BM25 order as
  §3 Tier 3 "memory.db". One line per hit: `#<id> <kind>: <title> — <snippet>`, where `#<id>` is the
  live entry's emitted ID, `(expired)` when its Vault entry is gone, and absent when the row has no
  entry. No database yet: "No indexed entries yet." (memory.db is never created just to be read). No
  hits: "No matches.".
- **`diet_exec`** `{command: string | string[], cwd?: string}`.
  - `command` as an array is the argv. As a string it must parse as one simple command
    (`parse_simple_command`, §3 Tier 2); anything else is an `isError` result asking for an argv
    array. **No shell is ever started**: there is no `sh -c` fallback.
  - `cwd` (default: the server's directory; a relative path resolves against it) must be an existing
    directory. The run is indexed in the server's project memory.db (above), whatever `cwd` is, so
    `diet_search` finds it.
  - The child gets stdin `/dev/null` (it must never read the server's JSON-RPC stream), piped stdout and
    stderr, `AGENTDIET_SHIM_ACTIVE=1` (shims on its `PATH` take the transparent path, so nothing is
    dieted twice) and **its own process group**. After **120 s** the whole group is killed (`SIGKILL`),
    so grandchildren holding the pipes cannot keep the call alive. Each stream is captured up to
    **16 MiB**; the rest is read and discarded, and the result says the stream was truncated (more
    than 8 MiB is oversize for the engine anyway, P3). `AGENTDIET_MCP_EXEC_TIMEOUT_MS` overrides the
    timeout (for tests).
  - Settings come from `state.json` (mode, paused). The same argv and `cwd` as the previous `diet_exec`
    in the same server process is a retry (`RepeatedCommand`, §2.4); otherwise no bypass. Each stream is
    one engine call with `Shell { argv, exit }` (exit `Unknown` after a timeout), exactly as the shims'
    agent path.
  - The text is the dieted stdout, then, when stderr is not empty, a line `--- stderr ---` and the
    dieted stderr, then one status line: `[exit N]`, `[signal N]`, or `[timed out after Ns; killed]`.
    Nothing re-runs the command (P2).
  - Engine calls from `diet_exec` enable the head/tail fallback (§3 Tier 2), so long output of any
    command that is not machine-readable comes back as a compact summary with a breadcrumb, and each
    run is logged (§3 Tier 3, "Run log", origin `mcp`).
- **`diet_reflect`** `{dry_run?: boolean}` (default `false`). Runs `reflect` (below) in the server's
  project; the result is two text blocks, the file's text and the one-line summary. A failure is an
  `isError` result.

#### Stats

`agentdiet stats` prints the §2.3 counters (all projects, from `state.json`) and, for the workspace, the
run log totals per entry point: runs, failed runs, runs whose output was replaced, and estimated tokens
before and after. It never creates memory.db. The text uses the §2.5 words:

```
AgentDiet is active · Balanced mode

All projects (estimated)
  trimmed outputs      4
  tokens saved         ≈134,830
  re-opened            1 re-open for 4 trimmed outputs (25%) · too few trimmed outputs to judge yet (needs 10)
  while paused         ≈0 tokens passed untrimmed (still scrubbed)
  last trimmed         #683db530 · agentdiet show last

This workspace (~/code/app)
  via                 runs  failed  trimmed  est. tokens before → after
  shims                  4       0        0  0 → 0
  MCP (diet_exec)        1       0        1  8,000 → 120
  Claude Code hooks     34       0        0  32,080 → 32,080
```

Without memory.db the workspace part says "Nothing recorded in this workspace yet."; with nothing
trimmed, "re-opened" and "last trimmed" say "none yet". `--json` is a **contract** and does not
change: one JSON object with the keys `status`, `mode`, `trimmed_outputs`, `tokens_saved`,
`cas_dereferences`, `raw_tokens_passed_while_paused`, `last_trimmed_id`, `workspace`, `memory_db` and
`runs` (`shim`, `mcp`, `hook`, each `runs`, `failed`, `trimmed`, `raw_tokens`, `emitted_tokens`), with an
absolute `workspace` path, whatever the locale.

#### Status

`agentdiet status` is the one-screen answer to "is AgentDiet on, and is it doing anything?". It is
**read-only**: it never writes `state.json`, never runs the Vault GC, and opens memory.db only when it
already exists. It always exits 0. Lines, in order (§2.5 rules apply):

1. `AgentDiet is active · <Mode> mode`, or `AgentDiet is paused · <Mode> mode`.
2. *Assistants*: the labels of the targets agentdiet is connected to (any scope), or
   `none connected` with `agentdiet init` as the next step.
3. *Savings*: `≈<tokens_saved> tokens saved in <trimmed_outputs> trimmed outputs (all projects, estimated)`,
   or `nothing trimmed yet` while `trimmed_outputs` is 0.
4. *Re-opened*, only while `trimmed_outputs` is above 0: `<n> re-open(s) for <trimmed_outputs> trimmed output(s)
   (<rate>)`, with the same warning as `agentdiet top` when the rate is strictly above 20% over at least
   10 trimmed outputs, or the "too few … to judge yet" note below 10 (§2.3).
   Raw counts are shown so a rate from a small sample is visibly one.
5. *Paused*, only while paused: the estimated tokens passed untrimmed (still scrubbed) and
   `agentdiet resume`.
6. *Last trimmed*: `#<id>` and `agentdiet show last`, when `last_trimmed_id` is set.
7. *Health*: the `doctor` verdict in one line: `✓ no problems found`, or the number of problems and
   warnings and `agentdiet doctor`.

**`status --json`** prints one JSON object and nothing else on stdout (any warning goes to stderr), exits
0, and is never transliterated. It is a **contract**: keys are added only with their meaning kept, and a
rename or removal bumps `schema`. Keys, in this order:

| Key | Value |
|---|---|
| `schema` | `1` |
| `status` | `"active"` \| `"paused"` |
| `mode` | `"light"` \| `"balanced"` \| `"ultra-lean"` |
| `assistants` | the connected targets (any scope) as `init` slugs, in `Target::ALL` order, `"shims"` included |
| `trimmed_outputs`, `tokens_saved`, `cas_dereferences`, `raw_tokens_passed_while_paused` | the §2.3 counters (same names as `stats --json`) |
| `last_trimmed_id` | ID without `#`, or `null` |
| `reopen` | `{ "rate": <number or null>, "verdict": "none_yet" \| "fine" \| "too_few_to_judge" \| "high" }` (§2.3) |
| `health` | `{ "ok": <no broken check>, "problems": <n>, "warnings": <n> }` (the `doctor` counts) |

#### Doctor

`agentdiet doctor` checks that AgentDiet is wired up and working. It shares `status`'s read-only rule
(it opens memory.db only when the file exists, and reads, never repairs, every file). Each check prints one line,
`<label>  <symbol> <word>  <detail>`, and, when there is something to do, an indented `→ <fix>`
line with an exact command. The words are `verified`, `ok`, `configured`, `warning`, `broken`,
`not connected` and `not installed`; a configured target AgentDiet can observe but has not yet reads
`configured, not yet observed`. `verified` means runs were recorded, never that a config exists. Checks, in order:

1. **This binary**: the running executable's path and version (information only).
2. **State**: `~/.agentdiet/state.json`. Missing is `ok` (defaults). A file that cannot be read or is
   not valid state JSON is `broken`: hooks and shims silently use the defaults (§2.3). The fix is to
   remove the file (`rm ~/.agentdiet/state.json`); AgentDiet starts again from defaults and its
   counters are lost. (`resume` is not a fix: on defaults it is already active and writes nothing.)
3. **Each assistant** (every `init` target except the shims) at user scope, and at project scope
   when the current project's file for it holds an agentdiet entry. Each of the target's files is
   classified, and the target gets the worst of them:
   - *not connected*: no file holds agentdiet's entry. Shown only when the assistant is detected
     (as `init` detects it), with `agentdiet init --only <target>`; undetected assistants are listed on
     one `not installed` line.
   - `warning`, invalid file: a file that is not strict JSON / valid TOML or has an unexpected shape,
     and that mentions `agentdiet` (or is one of several files of a connected target). AgentDiet can
     neither check nor update it, but the assistant may well read it (Cursor and VS Code accept JSONC),
     so it is not reported as broken. Fix: correct the file, then `agentdiet init --only <target>`. An
     invalid file that does not mention agentdiet leaves the target `not connected`, with a note that
     `init` would skip it.
   - `broken`, missing binary: the agentdiet executable a file points at does not exist. Fix: run
     `init --only <target>` from the agentdiet binary the assistant should use.
   - `warning`, different binary: the file points at another agentdiet executable that exists. Both
     paths are shown. This is not a failure (a development build is a common cause), and doctor
     never suggests re-running `init` from a binary the configs do not use. When every such target
     (and the shims) points at the same other binary, doctor reports it once, on the **Binary** row
     (`assistants run ~/.local/bin/agentdiet; this is a different build`), and those targets show
     their own state instead of repeating the warning.
   - `warning`, skipped: `init` deliberately leaves the file alone (a conflict, e.g. a hand-written
     `mcp_servers.agentdiet` in Codex's `config.toml`).
   - `warning`, incomplete: some of the target's files hold agentdiet's entry and others lack it,
     or an entry differs from what `init` would write for the binary it points at (a missing hook
     event, a missing `EXPERIENCE.md` pointer). Fix: `agentdiet init --only <target>`.
   - Otherwise the target is `configured`. A configured Claude Code target is `verified` when this
     workspace's run log has hook runs; the detail gives their count and how long ago the newest was
     recorded, so old evidence reads as old. Otherwise it says `configured, not yet observed` ("no
     activity recorded in this workspace yet; restart Claude Code if it was running"). MCP-only assistants stay `configured`: an MCP run cannot be
     attributed to one assistant. Missing activity is never a failure.
4. **Shims**: not installed is `not installed` (optional). Otherwise each shim symlink must point at
   an agentdiet executable that exists (`broken` if it dangles; `warning` if it is another
   agentdiet, or if a shim is missing). Then **PATH, in this shell**: the shims directory missing from
   `PATH` is `broken` (the shims are never reached), with the `export PATH=…` line as the fix; a
   `PATH` directory before it that holds one of the shimmed commands is a `warning` naming the command
   it shadows. The check reads doctor's own `PATH`, which can differ from an assistant's (an editor
   started from the macOS Dock does not inherit the shell's `PATH`). The shims are `verified` when
   this workspace's run log has shim runs (count and age of the newest, as above), and otherwise
   `configured, not yet observed`.
5. **Vault**: `~/.agentdiet/cas/`. Missing is `ok` (empty); otherwise its live entries and size, or
   `broken` when it cannot be listed.
6. **Workspace memory**: the workspace's memory.db. Missing is `ok` ("nothing recorded here yet");
   otherwise its run count, or `broken` when it cannot be opened.

When no assistant and no shims are connected, doctor reports `broken: nothing is connected` with
`agentdiet init`.

**`doctor --json`** prints one JSON object and nothing else on stdout, with the same checks in the same
order and the **same exit status** (0, or 1 when something is broken). Same contract rules as
`status --json`. Keys:

| Key | Value |
|---|---|
| `schema` | `1` |
| `ok` | `true` when nothing is `broken` (warnings allowed) |
| `problems`, `warnings` | counts |
| `checks` | array, one object per line of the text report: `check` (`"binary"`, `"state"`, `"assistant"`, `"others"`, `"shims"`, `"path"`, `"vault"`, `"memory"`, `"setup"`), `target` (an `init` slug, or `null`), `scope` (`"user"` \| `"project"` for an assistant, else `null`), `label`, `level` (`"ok"` \| `"info"` \| `"warning"` \| `"broken"`), `status` (the text report's word: `"verified"`, `"configured"`, `"not connected"`, …), `observed` (`"verified"` \| `"not_yet"` \| `"not_observable"` \| `null`), `detail` and `fix` (the text report's sentences, `null` when there is no fix) |

Neither JSON output carries file contents: details are the doctor's own sentences with `~/…` paths, never
text read from an assistant's config (a parse error is named, not quoted). The last line is `All good.` or the counts that are not zero (`1 problem, 2 warnings.`, `3 warnings.`). Exit status: 0
when nothing is `broken`, 1 otherwise.

#### Mode

`agentdiet mode` prints the current Diet Mode and the three modes, each described from its
§3 Tier 2 parameters (size threshold, required saving, context lines), with the current one marked.
`agentdiet mode <light|balanced|ultra-lean>` (also `ultra`) sets it, waiting up to 2 s for the state
lock like `pause`; it prints `Diet Mode set to <Mode>.` or `Diet Mode is already <Mode>.`. An unknown
mode is a usage error (exit 2) that lists the valid ones. The change applies to the next tool output.

#### `show last`

`agentdiet show last` (`last` in any case) shows the entry named by `last_trimmed_id`, exactly as
`show '#<id>'` would: raw bytes on stdout, `--lines` allowed. It is **not** counted as a dereference
(§2.3): breadcrumbs always give agents the `#id` form, and `last` is how a person looks at their own
output, which must not raise the "agents re-read trimmed output" warning. When
nothing has been trimmed yet, or that entry has expired or is missing, it prints the reason on stderr
and exits 1; stdout stays empty. The ID stays **required**: `last` is never a default, because an
unquoted `agentdiet show #4f9a` reaches agentdiet as a bare `show` (the shell drops the comment), and
that must fail as a usage error rather than print a different entry.

#### Reflect

`agentdiet reflect` turns the workspace's run log into `EXPERIENCE.md` at the workspace root, with
**no model calls**: the same run log and Vault give the same bytes. The structure follows Harness²'s
separation of an immutable base harness (the assistant's own instructions, which AgentDiet never
edits) from an evidence-backed experience file (behaviour reference only; Apache-2.0, nothing copied).

1. **Propose.** Over the newest 2,000 runs, grouped by session (runs without one, by origin), in log
   order, with a shim run and the PostToolUse of the same Bash call within 120 s counted once:
   - *Recipe:* a `shell` run that failed, followed within the next 5 runs and 30 minutes by a
     successful run of the same family whose words differ from it only by flags (a flag only one side
     has, with the value right after it) or by at most as many plain words, each within an edit
     distance of 40% of its counterpart (`biuld` → `build`), and by at most 4 words in all. The same
     words again is a flaky retry, not a recipe. Tokens: the failed output.
   - *Bloat:* a successful run of at least 1,000 estimated tokens. Tokens: its output.
   - *Miss:* a `search` or `read` that found nothing, with the first later successful `search` or
     `read` of a different title (the same window) as its correction, if any.
   One lesson per cue (the failing command, the bloated title, the missed path or pattern): repeats add
   occurrences and tokens and the newest witness supplies the correction and the evidence, so entries
   never duplicate.
2. **Probe.** A recipe or miss is kept when it wasted at least 500 estimated tokens or was seen at
   least twice; bloat must be seen at least twice. Kept lessons are ranked by impact (tokens + 250 per
   occurrence), then by recency, then by kind and cue.
3. **Compose.** The lessons are rendered in rank order, each as one line citing the live Vault ID of
   its newest witness (`#4f9a1b2c`) or that run's UTC time, into a block between
   `<!-- agentdiet:experience:begin -->` and `<!-- agentdiet:experience:end -->`, under the headings
   "Command recipes", "Context bloat" and "Search and path misses". A lesson that does not fit is
   skipped, so **the whole file is at most 2,048 bytes** (unless its hand-written part alone is
   larger); text outside the markers is kept byte for byte, and a file without markers gets the
   block appended. Titles are shown at most 72 characters; a cut recipe also names the words it
   added and dropped. A file whose bytes would not change is not rewritten.

Everything rendered comes from the run log, which holds scrubbed text only (P1). Known limits: a
recipe means "this worked next", not "this is why it failed" (a test that fails and then passes after
a fix with a different filter reads as a recipe); search misses depend on the unverified Grep/Glob
result fields (§3 Tier 3, "Run log"). Nothing runs `reflect` automatically: it is run by hand, by an
agent through `diet_reflect`, or by a user's own hook.

#### Static scanner

`agentdiet scan [paths…]` scans the given files and directories or, with none, these entries of the
current project when they exist: `.claude/`, `.cursor/`, `.mcp.json`, `SKILL.md`, `CLAUDE.md`,
`AGENTS.md`. Directories are walked recursively; symlinks are never followed; `.git`, `node_modules`,
`target` and `.agentdiet` are skipped; files over 4 MiB and binary files (a NUL in the first 8 KiB) are
skipped. Detection is regex and character based (no AST), pattern-matched and **best effort**: a clean
report is not a proof of safety. The rules are original (clean-room; SkillSpector's static mode was
only behaviour inspiration).

Every file gets families **a**, **b** and **d**; *instruction files* (`*.md`, `*.mdc`, `*.txt`,
`.cursorrules`, `.windsurfrules`) also get **c**. Text rules match per line, case-insensitively.

| Rule | Severity | Matches |
|---|---|---|
| `hidden-unicode/bidi` | high | bidirectional controls U+202A–U+202E, U+2066–U+2069 |
| `hidden-unicode/tag` | high | Unicode tag characters U+E0000–U+E007F (invisible "ASCII smuggling") |
| `hidden-unicode/zero-width` | medium | U+200B, U+200C, U+2060, U+180E; U+FEFF anywhere but offset 0; U+200D unless both neighbours are non-ASCII (emoji ZWJ sequences pass) |
| `secret/<scrubber rule id>` | high | any scrubber detector (§3 Tier 3), e.g. `secret/aws-access-key-id`; for `secret/dotenv-secret` only when the value is a credential (below) |
| `injection/ignore-instructions` | high | ignore / disregard / forget / override … previous / prior / above / earlier / system … instructions / rules / prompt / guidelines |
| `injection/conceal-from-user` | high | do not / don't / never tell / inform / show / mention / reveal / notify … the user; without telling / informing / notifying / asking the user; hide … from the user |
| `injection/exfiltrate-secrets` | high | send / post / upload / exfiltrate / transmit / forward / leak … environment variables / env vars / `.env` / API keys / credentials / secrets / tokens / SSH keys |
| `injection/bypass-guardrails` | medium | bypass / disable / turn off / skip / ignore … safety / security checks / guardrails / permission checks / confirmation prompts / sandbox |
| `shell/pipe-to-shell` | high | `curl` or `wget` piped into `sh`, `bash`, `zsh`, `dash`, `ksh`, `python`, `perl`, `ruby` or `node` (optionally via `sudo`) |
| `shell/base64-exec` | high | `base64 -d`/`--decode`/`-D` piped into a shell; `eval` of a base64 decode; `exec(base64.b64decode(`; `eval(atob(` |
| `shell/reverse-shell` | high | `/dev/tcp/` or `/dev/udp/`; `nc`/`ncat`/`netcat` with `-e` or `-c` |
| `shell/netcat` | medium | any other `nc`/`ncat`/`netcat` invocation with a host and a port |
| `shell/credential-path` | high | `~/.ssh`, `$HOME/.ssh`, `id_rsa`/`id_ed25519`, `~/.aws`, `.aws/credentials`, `~/.gnupg`, `~/.kube/config`, `~/.netrc`, `~/.docker/config.json` |
| `shell/env-exfil` | high | `env`/`printenv` piped into `curl`/`wget`/`nc`; `curl`/`wget` with `$(env)`/`$(printenv)` or posting `@.env` |

- **Secret-named assignments.** The scrubber's `dotenv-secret` rule fires on any assignment to a name
  containing `SECRET`, `TOKEN`, `API_KEY` and the like, which is right for redaction (a false
  redaction is cheap) but wrong for a scanner (a false finding fails a build). The scanner reports such
  an assignment only when its value is **one literal** (a quoted string, optionally `r`/`b`/`u`/`f`
  prefixed and followed only by `;`, `,` or a comment, or a bare `.env` token) that is not a pattern
  (no `\ ^ $ | ( ) [ ] { } * ?`, not a raw `r"…"` string), has no whitespace, and either starts with
  a known credential prefix (`sk-ant-`, `sk-proj-`, `ghp_`, `github_pat_`, `AKIA`, `xoxb-`; at least
  16 characters) or is a generated-looking secret: at least 20 characters with Shannon entropy of at
  least 4.2 bits per character, or, for an all-hex value (which tops out at 4 bits), at least 32
  characters and 3.0 bits. Calls (`re.compile(…)`), collections (`{"a", "b"}`, `[…]`), attribute
  lookups (`settings.SECRET_KEY`), numbers and short or low-entropy placeholders are not reported.
  Known miss: a short hand-written password (`PASSWORD=hunter2`) is no longer reported by the scanner;
  the runtime scrubber still redacts it.
- **JSON files** also get a structural pass: every string value (after JSON unescaping, so `\u202e`
  written as an escape is caught) goes through family **a**; each `mcpServers.*`/`servers.*` entry's
  `command` and `args` joined with spaces, and every `command` string under `hooks`, go through family
  **d**. Such a finding is reported at the first line containing that value's text (line 1 when none
  does). Findings are deduplicated by file, line and rule.
- **Report** (stdout), one line per finding, sorted by path and line:
  `<path>:<line>:<col>  <SEVERITY>  <rule>  <message>  | <excerpt>`. The excerpt is the line **after
  scrubbing**, at most 80 characters, with every control, bidi, zero-width and tag character shown as
  `U+XXXX`, so the report can neither leak a secret nor reorder the terminal. Secret findings carry no
  excerpt. Last line: `N finding(s) (h high, m medium) in F file(s); scanned S file(s).` or
  `No findings; scanned S file(s).`, followed on the same line by
  `Not checked: E path(s) could not be read.` whenever an explicit path failed (exit status 2).
- **Exit status:** 0 when there are no findings, **1** when there is at least one, **2** when an explicit
  path does not exist or cannot be read (usage error; nothing else is reported as clean).

#### `agentdiet top`

- Needs a terminal: with stdin or stdout not a TTY it exits 1 with
  "`agentdiet top` needs an interactive terminal (stdin and stdout must be a TTY)." and draws nothing.
- Shows, in the §2.5 words and with the same phrases as `status` and the Control Center: a header with
  `● Active` / `■ Paused`, the Diet Mode and the connected assistants (`init` targets at user scope);
  *tokens saved* in *trimmed outputs* (all projects, estimated), *re-opened* (§2.3, with the
  warning above 20%), what *passed while paused* (untrimmed, still scrubbed), the last trimmed ID and the
  Vault (entries, size, expired); *Recent activity (this workspace)*: the most recent memory.db rows
  (`#id` when its Vault entry is live, kind, command or path, summary); and a footer with the keys.
- Keys: `p` pause/resume, `m` next mode (Light → Balanced → Ultra-Lean), `r` reload, `q`/Esc quit (or
  "back" when it was opened from the Control Center, which it then returns to), Ctrl-C quit. It also
  reloads every 2 s. A failed change (e.g. "state is busy") is shown in the dashboard; it does not exit.
- Loading runs the Vault GC (§3 Tier 3, item 3) and never creates `memory.db` or the Vault. The terminal is
  restored on every exit path, including a panic. Control Center item 5 opens it.
- **`agentdiet eject` / `agentdiet uninstall`** — restore original configs from
  `~/.agentdiet/backups/`, remove agentdiet hook entries and shims. Idempotent. `cas/` and
  `memory.db` are kept unless `--purge` is passed.
  - Every `init` write that changes a file records a `manifest.json` in its backup directory
    (original absolute path, whether the file existed, sha256 of what was written).
  - Eject restores the **earliest** backup of each path byte-for-byte (or deletes a file init
    created) when the file still matches what init last wrote. If the user edited it since, eject
    removes only the agentdiet entries and keeps the user's edits.
  - Processed manifests are renamed `manifest.ejected.json`; backups are kept. A manifest is renamed only
    once all of its entries are ejected, so a partial eject leaves the rest restorable.
  - **`--only <targets>`, and its everyday name `agentdiet disconnect <targets>`**, eject just those targets
    (comma-separated or repeated), at user scope and in the current project, and leave every other target's
    files byte-identical. The shims target removes the shim links. `--purge` deletes shared data and is
    refused with `--only` (usage error, exit 2). The output has one line per target: `disconnected`
    (with what was restored, deleted or cleaned) or `nothing to remove`, then "Restart <the assistants> so
    they stop loading AgentDiet." A full eject prints the same lines for every target it touched.

## 4. Filesystem Layout

- **Global — `~/.agentdiet/`:** `state.json`, `backups/`, `shims/`, `cas/`, `sessions/` (retry and
  raw-turn sidecars, §2.4)
- **Project-local, in the workspace root (§3 Tier 3):** `.agentdiet/memory.db`; `EXPERIENCE.md`
  (`agentdiet reflect`)

## 5. Commands

`agentdiet --help` lists the commands in three groups, **Everyday** (`status`, `pause`/`resume`, `mode`,
`show`, `doctor`), **Setup** (`init`, `disconnect`, `eject`) and **Advanced** (`top`, `stats`, `scan`,
`reflect`), each with a one-line plain-English purpose, then three examples and a pointer to the Control
Center and to `agentdiet help <command>`. Every command's own `--help` ends with short examples (and its
exit statuses where they mean something). Internal commands (`mcp`, `hook`) are not listed but work as
before; so do all aliases (`uninstall`, `ultra`, `claude-code`, `gemini-cli`). Every example in the help
parses as a real command line (tested).

| Command | What it does |
|---|---|
| `agentdiet` | Opens the Control Center |
| `agentdiet status` | One-screen overview: on/paused, mode, assistants, savings, health (read-only) |
| `agentdiet doctor` | Checks configs, binary paths, shims and `PATH`, state, Vault and memory; exits 1 on a problem |
| `agentdiet init [--only <targets>] [--all] [--project] [--dry-run]` | Connects assistants |
| `agentdiet pause` | Pauses trimming |
| `agentdiet resume` | Resumes trimming |
| `agentdiet mode [light\|balanced\|ultra-lean]` | Prints or sets the Diet Mode |
| `agentdiet show <'#id'\|last> [--lines a..b]` | Prints the full stored (scrubbed) output, or a line slice; `last` is the last trimmed output |
| `agentdiet top` | Opens the TUI |
| `agentdiet scan [paths]` | Runs the security scanner |
| `agentdiet completions <bash\|zsh\|fish>` | Prints a completion script for that shell on stdout (commands, aliases, flags, Diet Modes, assistant names; never the internal `mcp`/`hook`). Generated from the same clap definition as `--help` by `clap_complete`, so it cannot drift |
| `agentdiet stats [--json]` | Prints trimming counters and the workspace's run log totals |
| `agentdiet reflect [--dry-run]` | Writes `EXPERIENCE.md` from the run log (no LLM) |
| `agentdiet disconnect <targets>` | Removes agentdiet from the chosen assistants only (same targets as `init --only`) |
| `agentdiet eject [--only <targets>] [--purge]` / `agentdiet uninstall` | Restores backups and removes agentdiet; `--only` is `disconnect`; `--purge` needs a full eject |
| `agentdiet mcp` | Internal; the MCP server that `init` registers. Hidden from `--help`, fully supported |
| `agentdiet hook <event>` | Internal; invoked by assistant hooks. Hidden from `--help` |

## 6. Invariants

Precedence between them is fixed by rules P1–P7 (§3 Tier 2).

- Never lose data: anything the engine trims is recoverable through `show` from the Vault, **except
  secrets, which are redacted before storage and are never recoverable**. A replacement is emitted
  only after its original is durably stored (P6).
- Never inflate: emitted output is never larger than the **scrubbed** original, in bytes and in
  estimated tokens, counting the breadcrumb, fold markers and notices (P5). Redaction can make text
  longer, which is why the baseline is the scrubbed original. The one sanctioned exception is the
  3,000-token paused notice (§2.3).
- Fail open: an internal error emits the best safe output (scrubbed if scrubbing finished) or, in
  shim mode, execs the real binary. Processing errors never re-run a command that has already run
  (P2).
- Thinking blocks are never touched.
- Secrets are never written to disk unscrubbed (P1). The one exception is the byte-exact backup of
  a user's own config file made by `init` (see §2.2), which is kept `0700`/`0600`. Secret
  detection is best effort and does not cover the transparent shim path or binary output (P3).
- Hooks never block on stdin or a TTY.

## 7. Performance

A budget is only reported against the boundary it was measured at, and a miss is recorded as a miss.
Requirements are not redefined to match a measurement.

| Boundary | What it covers | Requirement | Status |
|---|---|---|---|
| **B1 — shim end-to-end overhead** | Wall time of spawning a shimmed command until it exits, minus the same command spawned directly (`E2 − E0`), human path, warm | **≤ F + 0.5 ms**, where **F** is the exec floor measured in the same run (`EF − E0`: a minimal C program that only `execv`s the same real binary). Until 2026-10-20 the requirement was < 0.2 ms; see "B1 amendment" below | **NOT MET** (P4, 2026-10-21, release candidate `b3248a9`): median **2.705 / 2.707 ms** in two runs against F = **1.49 / 1.50 ms**, limit ≈ 2.0 ms, so about 0.7 ms over; the P3 binary in the same alternating session: 2.699 / 2.680 ms (no regression). History: Slice 4: median **3.74–3.84 ms** against 3.22–3.33 ms for the Slice 3B binary in the same session (see "Slice 4 re-measurement" below). Slice 3A: median 2.24 ms (p95 2.34, p99 2.59; n = 1000 iteration-matched pairs). |
| B2 — process launch | Spawning `agentdiet hook noop` minus spawning the direct command (`E1 − E0`) | none; context for B1 | median 1.12 ms (p95 1.20, p99 1.24; n = 1000) |
| B3 — in-process routing | `shim_name` + `resolve_real` on a synthetic `PATH`, up to the `exec` call | **< 0.2 ms** (the original B1 number, now applied to what AgentDiet's code itself costs) | median 0.027 ms (p95 0.029, p99 0.030; n = 10000) |
| **B4 — Diet Engine fast path** | Pure transform (classify, scrub, candidate, checks) of an input of up to 256 KiB, in process, warm, no I/O | < 10 ms | **NOT MET** at 256 KiB. Worst p50 **28.7 ms**: realistic Rust source through the fold path (p99 29.6 ms; the tree-sitter parse alone is ~22 ms, so a large valid file cannot meet the budget while it is folded). Also over 10 ms at p50: adversarial "secrets everywhere" `shell` 13.7 ms and `read` 12.9 ms (the scrubber takes 6.2 ms on clean text of this size), "deep (60) JSON, large leaf" `json` 12.7 ms (TOON's 10-pass bound) and "diagnostic on every line" `read` 10.0 ms. Realistic 256 KiB JSON → TOON: p50 9.8 ms, p99 over 10 ms in every run (10.2–36.2 ms). Other realistic 256 KiB rows: cargo 7.7 ms, pytest 6.7 ms, npm 5.4 ms. The hostile "numbers and punctuation" `read` was p50 **247 ms** until the bounded parse (§3, Signature folding) and is now 8.4 ms (p99 8.8 ms); "multibyte text" `read` is 8.5 ms. Several rows show isolated p99 spikes of 26–37 ms (n = 60) that follow no one input. Larger inputs (not judged): 1 MiB of Rust is now left unchanged after ~57 ms where it used to fold in ~117 ms, because it exceeds the 30 ms parse deadline. Measured at `10d1ad6` plus the bounded-parse change, release, n = 60 per row. |
| **B5 — persistence** | Durable Vault write (`fsync` file and directory), GC, ID selection, index and state update; measured with 0, 100 and 1000 entries in the Vault | reported with B4; the engine's end-to-end budget is B4 + B5 + process launch (B2) < 10 ms | **MISS** on the end-to-end budget. `Engine::run` on 64 KiB: p50 **22–26 ms** at 0, 100 and 1000 entries (flat in entry count; three runs). Floors: `fsync` of a 32 KiB file plus its directory p50 8.1–9.3 ms; `state::record_telemetry` p50 8.6–10.4 ms; `Cas::put` p50 10.5–12.6 ms. Cold first `Engine::run` (30 fresh processes) p50 38–43 ms. |

Measured on Darwin 27.0.0 (macOS), Apple M1, aarch64, rustc 1.99.1, release build with
`lto = true` (binary 1,432,480 bytes, ad-hoc code signature, sha256 `689e4da7…`, the artifact of
`cargo build --release --locked` copied out of `target/` and timed through `AGENTDIET_PERF_BIN`),
at commit `a7a84e6` with a clean tree, 1000 samples per arm after 50 warm-up runs. Three earlier
runs, one of them on the relinked `cargo test --release` binary, gave B1 medians of 2.27-2.28 ms.
Absolute p50: direct `true` 1.11 ms, `agentdiet hook noop` 2.23 ms, shim 3.35 ms. An independent
earlier measurement (n = 100, different harness) found a median difference of 3.70 ms; the
harnesses differ, but both are far above 0.2 ms.

**Slice 4 re-measurement** (2026-10-17, same machine and harness, n = 1000 per arm). Two release
binaries built with `cargo build --release --locked` were timed through `AGENTDIET_PERF_BIN`,
alternating head, new, head, new:

| Binary | Size | B1 p50 | B2 p50 | B3 p50 |
|---|---|---|---|---|
| Slice 3B (`4ae86e0`) | 1,449,168 bytes | 3.22, 3.33 ms | 1.97, 2.00 ms | 0.026 ms |
| Slice 4 (this change) | 9,492,064 bytes | 3.84, 3.74 ms | 2.36, 2.43 ms | 0.026–0.032 ms |

- The machine was slower in this session than at Slice 3A: the *unchanged* Slice 3B binary measured
  B1 3.22–3.33 ms here against 2.24 ms then. Only the paired comparison above is meaningful.
- Slice 4 adds about **0.4–0.6 ms** to B1, and the same to B2, whose code path (`hook noop` returns
  before reading stdin) is unchanged. The cost tracks the binary, not the routing: once the hook and
  the shim's agent path call the Diet Engine, link-time optimisation can no longer drop it, so the
  scrubber's regexes, the tree-sitter grammars (most of the 5.1 MB `__const` section), TOON and
  SQLite are now in the one binary. A larger image costs more to load on every launch.
- B3 still times only `shim_name` + `resolve_real`. The new routing checks before the transparent
  `exec` (environment reads, `isatty` on stdin and stdout, one `fstat` of stdout) are **not
  measured** separately. The harness sends stdout to `/dev/null`, so it exits at decision step 4. A
  human at a terminal exits at step 3 (in `git log | less`, git's stdin is still the terminal). The
  callers that reach steps 6–8 on the transparent path are non-interactive ones with neither stdin
  nor stdout a terminal (scripts, cron, CI) running a recognised shape. They pay for two
  descriptor lookups and possibly the parent walk, and that cost is **unmeasured**.
- Hook latency (B2 + the engine's B4 + B5 per PostToolUse) was not re-measured in Slice 4.

**Slice 5 re-measurement** (2026-10-17, same machine and harness, n = 1000 per arm, 50 warm-up). Both
binaries built with `cargo build --release --locked` and timed through `AGENTDIET_PERF_BIN`, alternating
head, new, head, new:

| Binary | Size | B1 p50 | B2 p50 | B3 p50 |
|---|---|---|---|---|
| Slice 4 (`5b259a7`, sha256 `2dea7f89…`) | 9,492,144 bytes | 3.73, 3.72 ms | 2.49, 2.49 ms | 0.025 ms |
| Slice 5 (this change, sha256 `dd4328db…`) | 9,823,456 bytes | 3.75, 3.73 ms | 2.50, 2.51 ms | 0.025 ms |

- ratatui, crossterm (one copy, 0.29, shared with inquire 0.9) and the MCP, scanner and TUI code add
  331,312 bytes and about **0.01–0.02 ms** to B1 and B2, within run-to-run noise. B1 remains **NOT MET**;
  nothing in Slice 5 touches the shim routing or the engine's hot path. The engine's telemetry counters
  ride its existing `state.json` write (§2.3), so B5 gains no `fsync`; B4 and B5 were not re-measured.

**Dogfooding-fix re-measurement** (2026-10-19, same machine, release binaries built with
`cargo build --release --locked` from `e9daa72` (head, 9,823,456 bytes, sha256 `dfce2790…`) and this
change (9,939,344 bytes, sha256 `8c1f9822…`), timed through `AGENTDIET_PERF_BIN`, alternating head,
new, head, new, n = 1000 per arm):

| Binary | B1 p50 | B2 p50 |
|---|---|---|
| head (`e9daa72`) | 3.48, 3.33 ms | 2.32, 2.29 ms |
| this change | 3.40, 3.50 ms | 2.31, 2.35 ms |

- The transparent shim path is unchanged (it records nothing and runs no workspace discovery), and B1
  and B2 are within run-to-run noise. **B1 remains NOT MET.** p95 was 7.0–7.2 ms in every run: the
  machine was busier than in earlier sessions.
- The run log (§3 Tier 3) adds a SQLite open and insert where it records. Paired wall time per spawn
  from a Python driver (it includes the driver's own spawn cost, identical for both binaries), n = 300
  per arm, two runs:

  | Call | head p50 | new p50 | added |
  |---|---|---|---|
  | `hook PostToolUse`, small successful Bash output | 6.08–6.10 ms | 7.08–7.12 ms | ~1.0 ms |
  | `hook PostToolUseFailure` (head: unregistered event, returns before reading stdin) | 5.50–5.51 ms | 6.85–6.88 ms | ~1.4 ms |
  | shim agent path, `cargo test` printing one line | 17.96–17.99 ms | 19.19–19.28 ms | ~1.2–1.3 ms |

  These costs sit on the agent paths, which have no budget of their own; the hook's share of the engine
  budget (B4 + B5 + B2) was already a miss and is not re-measured here.

**UX-P0 re-measurement** (2026-10-20, same machine and harness, n = 1000 per arm). Release binaries
built with `cargo build --release --locked` from `5092910` (head, 9,939,344 bytes, sha256 `460fc8b3…`)
and this change (10,022,080 bytes, sha256 `3510c5cb…`), copied out and timed through
`AGENTDIET_PERF_BIN`, alternating head, new, head, new:

| Binary | B1 p50 | B2 p50 | B3 p50 |
|---|---|---|---|
| head (`5092910`) | 3.88, 3.49 ms | 2.59, 2.39 ms | 0.025 ms |
| this change | 3.45, 3.58 ms | 2.41, 2.41 ms | 0.027 ms |

- `status`, `doctor`, `mode` and the `init` summary add 82,736 bytes. Shim and hook dispatch still run
  before clap and reach none of that code; B1 and B2 are within run-to-run noise. **B1 remains NOT MET.**

**UX-P1 re-measurement** (2026-10-20, same machine and harness, n = 1000 per arm). Release binaries built
with `cargo build --release --locked` from `a827255` (UX-P0, 10,022,096 bytes, sha256 `5bfc51f2…`) and
this change (10,071,616 bytes, sha256 `fd107e8e…`), timed through `AGENTDIET_PERF_BIN`, alternating head,
new, head, new:

| Binary | B1 p50 | B2 p50 | B3 p50 |
|---|---|---|---|
| UX-P0 (`a827255`) | 3.63, 3.60 ms | 2.46, 2.43 ms | 0.027 ms |
| this change | 3.58, 3.58 ms | 2.44, 2.40 ms | 0.027 ms |

- The Control Center, `disconnect` and the Verification block add 49,520 bytes. Shim and hook dispatch
  still run before clap and reach none of that code; B1 and B2 are within run-to-run noise. **B1 remains
  NOT MET.**
- Re-run on the final P1 binary (with the signal handling, which is installed only when a full-screen
  view opens; 10,104,672 bytes, sha256 `4ecd4e10…`), same method, on a quieter machine: head B1 2.66,
  2.69 ms and B2 1.59, 1.62 ms; this change B1 2.69, 2.68 ms and B2 1.61, 1.61 ms. Within noise; only
  the paired comparison is meaningful across sessions. **B1 remains NOT MET.**

**UX-P2 re-measurement** (2026-10-20, same machine and harness, n = 1000 per arm). Release binaries built
with `cargo build --release --locked` from `0b58e02` (UX-P1, 10,104,672 bytes, sha256 `67484e53…`) and
this change (10,104,704 bytes, sha256 `c7d8aacd…`), timed through `AGENTDIET_PERF_BIN`, alternating:

| Binary | B1 p50 | B2 p50 | B3 p50 |
|---|---|---|---|
| UX-P1 (`0b58e02`) | 2.76, 2.78 ms | 1.63, 1.64 ms | 0.027 ms |
| this change | 2.75, 2.79 ms | 1.62, 1.65 ms | 0.027, 0.026 ms |

- P2 changes people-facing text and output handling only; shim and hook dispatch are untouched. Within
  noise. The same scenario run by both binaries (an MCP session with `diet_exec` and `diet_inspect`, a
  `PostToolUse` hook, `show '#id'`, `stats --json`, `scan`) produced byte-identical output. **B1 remains
  NOT MET.**

**UX-P3 re-measurement** (2026-10-20, same machine and harness). Release binaries built with
`cargo build --release --locked` from `b2b3c40` (UX-P2, 10,104,704 bytes) and this change (10,253,520
bytes, sha256 `63b60a73…`; `scripts/check-binary-deps.sh --build`: only `libSystem`), alternating:

| Binary | B1 p50 | B2 p50 | B3 p50 | `hook PostToolUse` p50, small / trimmed (n = 280) |
|---|---|---|---|---|
| UX-P2 (`b2b3c40`) | 2.76, 2.79 ms | 1.62, 1.65 ms | 0.027 ms | 5.46 / 17.32 ms |
| this change | 2.75, 2.79 ms | 1.62, 1.65 ms | 0.027 ms | 5.47 / 17.50 ms |

- `clap_complete` and the JSON outputs add 148,816 bytes; nothing new runs on the shim or hook path
  (the hook's notes now go through a non-panicking write). Within noise. The hook figures are wall time
  per spawn from a Python driver (it includes the driver's own spawn cost, the same for both binaries).
  The P2 regression scenario (MCP session, `PostToolUse` hook, `show '#id'`, `stats --json`, `scan`) gives
  byte-identical output. **B1 remains NOT MET.**

**B1 amendment** (owner's decision, 2026-10-20; recorded here so the change is never silent). The original
< 0.2 ms cannot be met by any shim that is a separate executable on `PATH`, on macOS: measured on this
machine (Apple M1, Darwin 27, n = 1000 interleaved), a 33 KB C program that does nothing but `execv` the
real binary adds **1.72 ms** over running that binary directly, the same as launching AgentDiet alone
(`hook noop`, +1.71 ms). Launch costs against a 17 KB C program that only exits: a minimal Rust binary
(std, LTO) **+0.38 ms**, AgentDiet (10 MB) **+1.21 ms**; Apple's own `/usr/bin/true` is 0.47 ms *faster*
than either (platform binaries are validated differently). So B1 is now judged against the exec floor
**F** measured in the same run, with 0.5 ms allowed for AgentDiet's own process image and decision, and
the 0.2 ms number moves to B3, which is what AgentDiet's code alone controls. A miss stays a miss: what
is above F + 0.5 ms is reported as NOT MET, and closing it is a design question (a smaller image, or a
dedicated shim executable, which `CLAUDE.md`'s single-binary rule does not allow today).

**P4 re-measurement** (2026-10-21, Apple M1, Darwin 27, no containers running, n = 1000 interleaved per
run, `AGENTDIET_PERF_BIN` alternating between the release candidate `b3248a9` and the P3 binary):

| Run | Binary | B1 median | F | F + 0.5 | Verdict | B3 median |
|---|---|---|---|---|---|---|
| 1 | RC | 2.705 ms | 1.493 ms | 1.993 ms | NOT MET | 0.0271 ms (MET) |
| 1 | P3 | 2.699 ms | 1.505 ms | 2.005 ms | NOT MET | 0.0271 ms |
| 2 | RC | 2.707 ms | 1.502 ms | 2.002 ms | NOT MET | 0.0270 ms (MET) |
| 2 | P3 | 2.680 ms | 1.495 ms | 1.995 ms | NOT MET | 0.0271 ms |

**v0.2.0 (P4.5, 2026-10-21, `5e4edc3`)**, same method, alternating with the P4 release candidate:
0.2.0 median **2.772 / 2.788 ms** (F = 1.570 / 1.573 ms, limit 2.070 / 2.073 ms: **NOT MET**); the P4
candidate 2.776 / 2.789 ms in the same session. B3 0.027 ms (MET). The private-root and invoked-path
checks are not on the shim path, and the numbers show no change. **B1 ships in v0.2.0 as a known
miss** (CHANGELOG "Known issues"); the single-binary architecture is kept (owner, 2026-10-21).

What is above the floor (~1.2 ms) is AgentDiet's own process image: launching the 10 MB binary costs
+1.21 ms over a 17 KB C program that only exits, of which a minimal Rust binary accounts for +0.38 ms.
P4 did not win it back: routing is already 0.03 ms, and the remaining cost scales with the image,
which only a separate small shim executable would avoid (not allowed by `CLAUDE.md`'s single-binary
rule). B1 stays a recorded miss; the decision (accept, or allow a dedicated shim binary) is the owner's.

Notes:

- Routing (B3, 0.03 ms) is about 1% of B1. B1 is consistent with paying for two process images in
  one pid: starting `agentdiet` (B2, 1.12 ms) and then `exec`ing the real binary, whose own start-up
  (about 1.1 ms for `true`) the direct arm pays too, so B1 is roughly twice B2. This is an
  observation from the arms, not a profile. Closing the gap needs a different shim design (for
  example a tiny dedicated shim, or fewer exec hops). That redesign is still open after Slice 4,
  and the engine now linked into the single binary makes the launch cost worse (above). It is not
  closed by a faster `resolve_real` and not by redefining B1.
- Durable writes will not be removed to hit B5. Candidates that keep the write-ahead guarantee are
  amortised GC (today `put` scans the vault on every call), no redundant directory creation, and
  doing the transform and the write in one pass. If B5 still misses, it is recorded as a miss.
- Benchmarks are opt-in (`cargo test --release --test perf -- --ignored --nocapture`), use only
  `TempDir` homes and a synthetic `PATH` (never the user's real assistant configs), alternate
  baseline and shim runs, discard a warm-up, and report sample count with p50, p95 and p99. Debug
  builds are refused: their numbers would be meaningless.
- Known misses to keep honest: B1 above; macOS `fsync` (`F_FULLFSYNC`) may by itself cost more than
  B5 allows; the first call in a cold process pays regex compilation in the scrubber.

## 8. Roadmap and status

| Slice | Content | Status |
|---|---|---|
| 1 | scaffold, state ledger, Control Center, `init`/`eject` | done |
| 2 | scrubber, Vault, FTS5 memory | done |
| 3A | dependency cleanup, this specification, shim benchmark | done (B1 requirement still NOT MET) |
| 3B | Diet Engine library: pipeline, compression, TOON, folding, `show`/ID/memory/state changes marked **[3B]** | done (B4 and B5 requirements still NOT MET, §7): storage prerequisites, the `diet.rs` pipeline, shell compression (`compress.rs`), the TOON subset (`toon.rs`) and signature folding (`folding.rs`, with a bounded parse) are implemented as a library, and the §7 numbers are measured. Nothing is wired to a real entry point until Slice 4 |
| 4 | hooks **and** PPID shims, wiring the engine to real entry points, shim redesign question (B1) | in progress: hook router (`hook.rs`), agent-path shims and the per-session sidecars are implemented and tested; the shim agent path was checked live under Claude Code 2.1.302 (§3 Tier 1); the Control Center shows the last trimmed entry. The live Claude Code check passed under Claude Code 2.1.302 on 2026-10-17 (§3 Hook router, "Live check"): `updatedToolOutput` replacements of `Bash` and `Read` results reached the model, and a JSON `mcp__agentdiet__diet_inspect` result reached it unchanged. Open: B1 still NOT MET and worse than in Slice 3B (§7; the shim redesign is not done), Reads are not folded |
| 5 | MCP tools (`diet_exec` etc.), scanner, `agentdiet top` | done: `diet_inspect`/`diet_search`/`diet_exec` in the stdio MCP server (§3b), the static scanner with the §3b rule table, the `agentdiet top` dashboard (also Control Center item 5) and the §2.3 telemetry counters, with end-to-end tests; B1 re-measured (§7, still NOT MET); `top` was driven in an 80x24 pseudo-terminal, directly and through Control Center item 5 (renders, keys work, the terminal is restored and the menu returns). Live checks (2026-10-17): a scripted JSON-RPC client ran `diet_exec` (`cargo test`, 400 tests: breadcrumb, Vault entry, memory.db row), `diet_search` (hit `#<id> command: cargo test`) and `diet_inspect` (lines 3..9 byte-identical to `agentdiet show --lines 3..9`, `cas_dereferences` +1); `@modelcontextprotocol/inspector --cli` listed the three tools and called `diet_inspect`; and Claude Code 2.1.302 connected the server and called `diet_inspect` (§3 Hook router, "Live check") |
| 5.1 | dogfooding fixes (`@diet:full` directive, `diet_exec` head/tail fallback and `npm ls` rule, workspace root and run log, scanner credential filter) and `stats` / `reflect` / `diet_reflect` / `EXPERIENCE.md` pointer | done, with tests; the run log's latency is measured above (§7); live check under Claude Code 2.1.303 (§3 Tier 3, "Run log"). Open: Grep's result fields were not checked against a live payload; B1 still NOT MET |

| UX-P0 | CLI/UX overhaul, phase P0 (`docs/CLI_UX_OVERHAUL.md`): `status`, `doctor`, `mode`, `show last`, the §2.2 `init` output and non-terminal `--dry-run`, the §2.5 terminal rules | done, with tests. Later phases (Control Center redesign, `eject --only`, help layout, completions) are in that document and not started |
| UX-P1 | CLI/UX overhaul, phase P1 (`docs/CLI_UX_OVERHAUL.md`): the ratatui Control Center (§2.1), `disconnect` and `eject --only`, post-`init` verification, `verified` / `not yet observed` wording, one-line binary warning, ASCII glyph fallback, `mcp` hidden from help | done, with tests. Help layout, wording of `stats`/`top`, completions, `--json` and bare-ID breadcrumbs are P2/P3 |
| UX-P2 | CLI/UX overhaul, phase P2: broken pipes end quietly for every command (§2.5), one vocabulary from `src/words.rs` across `status`, `doctor`, `stats`, the Control Center and `top`, actionable `show` errors (`CasError` unchanged), grouped `--help` with tested examples (§5), README overhaul | done, with tests. P3 (completions, `--json` for `status`/`doctor`, bare-ID breadcrumbs, a re-open minimum sample) not started |
| UX-P3 | CLI/UX overhaul, phase P3: re-open warning only from 10 trimmed outputs and neutral "re-opened" wording (§2.3 audit), `status --json` / `doctor --json` (§3b schemas), hooks, MCP notes and shims never panic on closed stdio (§3 Tier 1), `agentdiet completions bash\|zsh\|fish` (`clap_complete`), bare-ID breadcrumbs assessed and deferred (§3 Tier 3 item 6) | done, with tests |

License: MIT (`LICENSE`); third-party notices in `THIRD_PARTY_NOTICES`.

## 9. Reference pins

**TOON** — canonical specification, pinned:

- Repository: https://github.com/toon-format/spec (MIT, Copyright (c) 2025-PRESENT Johann Schopplich)
- Release tag `v4.4.0` (annotated tag `4bf71f491ba8474b405710e8926ea8268700d36f`), resolving to commit
  `d09c3d33e37389345a74bd96981eb96dd9be6c5e` (2026-10-08).
- `SPEC.md` at that commit: git blob `d2d14b3134cd3e0b49eb627bafd010c5efc58707`, SHA-256
  `b445972d3ffb6752d2012d6599ac6903a45d84125f8f07e3d3eec5ee9a6cd1da`. The document states
  *Version 4.4, Working Draft, 2026-10-08*; AgentDiet declares `toon-spec: 4.4`.
- Conformance fixtures: `tests/fixtures` at that commit (tree `394f2dff934d7c79a5daa54fdeae7bcc7988254f`;
  `encode/` tree `f8ef3e83407327e43e6dc9ec9e0cbb961ee3cb47`).
- This file cites the spec by section number (for example "spec §9.3") rather than copying it. The
  reference checkout `toon/SPEC.md` is only a pointer to the repository above and is not the
  normative text. A newer revision may change the subset's requirements; moving the pin is a
  deliberate, reviewed change to this section.

**Secretlint** (MIT) is the source of the scrubber's ported patterns; see `THIRD_PARTY_NOTICES`.

## 10. Platforms

| Platform | Support | What has actually been run (see `docs/RELEASE_READINESS.md` §4) |
|---|---|---|
| macOS aarch64 | supported | full suite, pseudo-terminal tests, benchmarks, live client checks |
| macOS x86_64 | supported | built locally from the release candidate; links only `libSystem`; not executed (no Intel hardware or Rosetta here). The release workflow has not run |
| Linux aarch64 / x86_64, glibc | supported | full suite on aarch64 (Docker, Debian 12); x86_64 not run (the CI job is written, not yet run) |
| Linux aarch64 / x86_64, static musl (the release binaries) | supported | built reproducibly and smoke-tested in bare Alpine containers (Docker); x86_64 under Docker's Rosetta emulation, not x86 hardware |
| Windows x86_64 / aarch64 | **under test, not yet supported** for v0.2.0; no shims (P4) | x86_64 MSVC: whole suite on GitHub's `windows-2025` runner (375 passed, 0 failed, 2026-10-21), the static release binary runs and imports no C-runtime DLL. aarch64 (llvm-mingw): whole suite under Wine 10 only. Not validated: interactive console (Ctrl-C), real assistant clients, PowerShell and Git Bash recovery commands |

**Windows (P4; under test).** Built for everything but the shims: the CLI, the Control Center and `top`,
`init`/`disconnect`/`eject` for every assistant, the MCP server, Claude Code hooks, the Vault, project
memory, `doctor`, `status`, `stats`, `scan`, `reflect` and completions. The suite passes on GitHub's
`windows-2025` runner, but Windows stays **not supported** for v0.2.0 (owner, 2026-10-21) until
interactive console behaviour and real assistant clients have been validated on Windows.

- **Shims are not supported on Windows.** The shim's safety check (decision step 6, §3 Tier 1) compares
  open files of the shim and its parent, which Windows does not expose to another process; without it a
  shim could not tell an agent's capture from a redirect. `init --only shims` (or `--all`) skips the
  shims on Windows with "shims are not supported on Windows yet", and `doctor` shows them as not
  installed with the same reason. The binary never enters shim mode on Windows, whatever its name.
- **Locations.** The home directory is `%USERPROFILE%` and the platform config directory `%APPDATA%`
  (VS Code's `Code\User\mcp.json` lives there), read from the environment as Node's `os.homedir()`
  reads them (Claude Code, Gemini CLI and VS Code are Node programs); only when one is unset or empty
  does AgentDiet ask Windows for the profile or roaming-AppData folder. (Asking Windows first, as the
  `dirs` crate does, would ignore the environment: AgentDiet and the assistants could disagree, and
  no test could keep a run away from the real profile.)
- **Recovery commands in `cmd.exe`.** `cmd.exe` does not treat `'` as quoting, so the breadcrumb's
  `agentdiet show '#<id>'` reaches AgentDiet with its quotes; IDs are accepted inside one pair of single
  quotes everywhere an ID is read (checked in `cmd.exe` under Wine). PowerShell and Git Bash treat `'…'` as quoting, so they should need nothing special; neither has been run. The breadcrumb text
  itself is unchanged on every platform.
- **Claude Code hooks** are written as `"<exe>" hook <Event>`, which `cmd.exe` and Git Bash run as is;
  PowerShell would need a leading `&`. Which shell Claude Code uses for hooks on Windows has not been
  checked live.
- **File protection.** Windows has no Unix modes; AgentDiet creates its files under `%USERPROFILE%`,
  whose default access control list grants only the user, SYSTEM and Administrators, and does not change
  ACLs. The `0700`/`0600` guarantees of §2.2 and §3 Tier 3 hold on Unix only; on Windows the profile's
  ACL is the protection, config backups included.
- **No following of links.** Private files are opened with `FILE_FLAG_OPEN_REPARSE_POINT`, so a symlink
  or junction planted at a private path is opened as itself and refused as "not a regular file", the
  equivalent of `O_NOFOLLOW`. There is no identity check after SQLite opens memory.db on Windows: its
  Windows VFS has no `SQLITE_FCNTL_HAS_MOVED`, and the path check confirms only a regular file
  (§3 Tier 3 "memory.db metadata").
- **Durability.** File contents are flushed (`FlushFileBuffers`, through `sync_all`) before the atomic
  rename; a directory cannot be synced on Windows, so that step is skipped (NTFS journals its metadata).
- **Locks** are `LockFileEx` locks (Rust's `File::try_lock`), with the same timeouts.
- **One file, no runtime.** The Windows release binary (`x86_64-pc-windows-msvc`) links the C runtime
  statically (`+crt-static`), so it loads only DLLs that ship with Windows, the equivalent of the
  single-binary rule on macOS and Linux. (Not yet checked with a DLL listing on a real build.)
- **`diet_exec`** kills only the child it started when a call is cancelled or times out: Windows has no
  process groups, so a grandchild can outlive it.
- **Full-screen views.** There are no SIGTERM/SIGHUP handlers on Windows; Ctrl-C should reach the view as
  a key in raw mode, as on Unix (crossterm's behaviour; no Windows console has been driven yet).
- **Not yet observed on Windows** (Wine has no real ACLs, junction semantics or console): the profile ACL
  protection, refusal of a planted junction, and `LockFileEx` contention between processes. Each
  follows from the code and the platform's documented behaviour; the CI `windows` job is the first
  real check.
