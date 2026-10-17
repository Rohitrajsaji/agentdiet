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

Running bare `agentdiet` opens an interactive Control Center menu (`inquire`), in this order:

1. **Pause / Resume** — a toggle; the label reflects the current status.
2. **Inspect last trimmed output** — shows the most recent ID (e.g. `#4f9a1b2c`) and its content.
3. **Switch Diet Mode** — Light / Balanced / Ultra-Lean.
4. **Manage Connected Assistants** — add or remove the integrations that `init` manages.
5. **Open `agentdiet top`** — the ratatui TUI.

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
- Shell rc files are never edited; `init` prints the `PATH` line for the shims directory instead.
- Slice 1 writes, per target (user scope / `--project` scope):

  | Target | User scope | `--project` |
  |---|---|---|
  | Claude Code | hooks in `~/.claude/settings.json`; MCP in `~/.claude.json` (`mcpServers`) | `.claude/settings.json`; `.mcp.json` |
  | Cursor | `~/.cursor/mcp.json` (`mcpServers`) | `.cursor/mcp.json` |
  | Windsurf | `~/.codeium/windsurf/mcp_config.json` (`mcpServers`) | not supported; skipped with a notice |
  | Gemini CLI | `~/.gemini/settings.json` (`mcpServers`) | `.gemini/settings.json` |
  | Copilot (VS Code) | `<config dir>/Code/User/mcp.json` (`servers`, `"type": "stdio"`) | `.vscode/mcp.json` |
  | Codex | `~/.codex/config.toml`, marker block `[mcp_servers.agentdiet]` | `.codex/config.toml` |
  | Shims | `~/.agentdiet/shims/<cmd>` symlinks to the binary | always global |

  Claude Code gets hooks for SessionStart, UserPromptSubmit, PreToolUse, PostToolUse and
  PreCompact; the other assistants get only the MCP entry until the hook slice.
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
| `cas_dereferences` | non-negative integer: successful Vault reads by an agent-facing path, i.e. `agentdiet show` and the MCP tool `diet_inspect` (every breadcrumb and fold marker points at `show`). The Control Center's "Inspect last trimmed output" and `agentdiet top` are not counted. |

- A missing, unreadable or corrupt `state.json` loads as defaults (fail open); unknown fields are ignored,
  and a missing counter loads as 0.
- **The three counters are telemetry, best effort and approximate.** `trimmed_outputs` and `tokens_saved`
  ride the engine's existing `state.json` update for `last_trimmed_id` (one locked write, no extra
  `fsync`); `cas_dereferences` is one locked update after a successful read. Each waits at most 100 ms for
  the lock and is dropped when it cannot take it (§3 Tier 3, "`state.json` updates are serialised").
- **CAS dereference rate** = `cas_dereferences / trimmed_outputs`, shown by `agentdiet top` (§3b) as
  "n/a" while `trimmed_outputs` is 0 and flagged as a warning when it is **strictly above 20%**: agents
  are going back to the full output often, so the trimming is probably too aggressive for them.

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

- If a UserPromptSubmit contains `raw`, `full` or `unfiltered`, compression is bypassed for the
  **next turn only**: every tool output until the next UserPromptSubmit, which sets or clears the
  flag again (SessionStart clears it too). Matching is whole-word and case-insensitive, where a word
  is a maximal run of ASCII letters, digits, `_` and `-`, so "fully" or "full-stack" do not trigger
  it, while "raw." or "(full)" do.
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
    previous shim signature, the retry's `tool_use_id`): never command text (P1). Every
    read-modify-write holds `sessions/.lock` for at most 100 ms; a sidecar that cannot be read,
    written or locked counts as "no bypass" (fail open). Sidecars are scratch state, written
    atomically but without `fsync`: a crash can only lose a bypass. SessionStart deletes sidecars
    older than 7 days. `state.json` keeps exactly the §2.3 shape.
- Every bypass skips trimming, TOON and folding and writes nothing to the Vault. None of them skips
  secret scrubbing (precedence rule P3).

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
     interactive commands are never buffered.
  6. stdout is not the **same open file as the parent's stdout** → transparent. Identity is device and
     inode (macOS `proc_pidfdinfo` vnode info, Linux `stat` of `/proc/<pid>/fd/1`) or, for a macOS
     pipe, the pipe's kernel handle; an identity that cannot be read counts as different. A command
     the agent's shell runs as-is writes where the shell writes, which is the agent; a redirection
     (`> out.txt`) or a pipe onward (`| tail`) gives the command its own stdout, so it is never
     dieted. Observed with Claude Code 2.1.302: a Bash command's stdout and stderr are a **regular
     file** (Claude Code's capture file) shared with the per-command `zsh` that is its parent, stdin is
     `/dev/null`, and nothing is a terminal; a file type alone could not tell that capture file from
     `> out.txt`.
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
  the prompt) set the status and print the one-line confirmation of `agentdiet pause`/`resume`; the
  escape words set the raw-turn flag (§2.4).
- **PreToolUse:** records the retry signature (§2.4); prints nothing.
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
  the shim (agent path) is what diets failing commands. PostToolUseFailure is not registered.
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
  the byte-exact config backup made by `init` (§2.2).
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
- **P7 — Bypass.** Paused status, a `raw`/`full`/`unfiltered` prompt (next turn only), and the same
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
  | git | transfer progress only. |
  | make | `make[N]: Entering/Leaving directory` lines. |
  | docker | layer pull/download progress. Step headers and errors are kept. |
  | ruff | in Ultra-Lean only, the source-snippet lines under a diagnostic; every diagnostic line is kept. |
  | gh | none from a shell invocation: `gh … --json` output is machine-readable and is never altered. Its JSON is eligible for TOON only as a separately marked `ModelJson` input (below). |

- Rules informed by other tools are recorded in code as `// pattern ref: <repo>/<path> (behavior
  only)` and follow the clean-room rules in `CLAUDE.md`.

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
   and `zsh`. The summary is scrubbed and at most 160 bytes, e.g. `kept 42/1830 lines; cargo test:
   1780 passing lines collapsed; exit 101; 2 secrets redacted (not recoverable)`. The whole line
   counts against the no-inflation check (P5).
7. **Index summaries** into `.agentdiet/memory.db` (project-local SQLite, WAL, FTS5 + BM25).
   PreCompact recovery uses this index to re-surface relevant context. Index rows hold scrubbed
   text only (P1).

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
- **memory.db** has one `entries` table (`kind` is `command` | `read` | `decision`) mirrored into
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
  first, and once SQLite has opened the file, before anything is read or written, the path must still
  be that same inode. (`SQLITE_OPEN_NOFOLLOW` is not used: it rejects a symlink anywhere in the path,
  such as macOS's `/var` or a project below a linked directory.) A swap in the instant between
  SQLite's `open` and the check is detected, not prevented. Error values never contain row text.
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

- **`agentdiet mcp`** — stdio JSON-RPC 2.0 MCP server exposing three tools:
  - `diet_inspect` — fetch a CAS entry by `#id`, with an optional line range.
  - `diet_search` — FTS5/BM25 search over `memory.db`.
  - `diet_exec` — run a command through the Diet Engine; returns trimmed output plus a breadcrumb.
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
- **Project.** The server's working directory is the project whose `.agentdiet/memory.db` is searched
  and indexed (assistants start MCP servers in the project directory).
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
| `secret/<scrubber rule id>` | high | any scrubber detector (§3 Tier 3), e.g. `secret/aws-access-key-id` |
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
- Shows: a header with Active/Paused, the Diet Mode and the connected assistants (`init` targets at user
  scope); the metrics `tokens_saved`, `trimmed_outputs`, `raw_tokens_passed_while_paused`, the last
  trimmed ID, the Vault (live entries, their size, expired IDs) and the CAS dereference rate (§2.3),
  highlighted with a warning above 20%; a table of the most recent memory.db rows (`#id` when its Vault
  entry is live, kind, command or path, summary); and a footer with the keys.
- Keys: `q`/`Esc` quit, `p` pause/resume, `m` cycles Light → Balanced → Ultra-Lean, `r` refreshes. It also
  refreshes every 2 s. A failed change (e.g. "state is busy") is shown in the dashboard; it does not exit.
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
  - Processed manifests are renamed `manifest.ejected.json`; backups are kept.

## 4. Filesystem Layout

- **Global — `~/.agentdiet/`:** `state.json`, `backups/`, `shims/`, `cas/`, `sessions/` (retry and
  raw-turn sidecars, §2.4)
- **Project-local — `.agentdiet/`:** `memory.db`

## 5. Commands

| Command | What it does |
|---|---|
| `agentdiet` | Opens the Control Center |
| `agentdiet init [--only <targets>] [--all] [--project] [--dry-run]` | Connects assistants |
| `agentdiet pause` | Pauses trimming |
| `agentdiet resume` | Resumes trimming |
| `agentdiet show '#id' [--lines a..b]` | Prints the full stored (scrubbed) output, or a line slice |
| `agentdiet top` | Opens the TUI |
| `agentdiet mcp` | Starts the MCP server |
| `agentdiet scan [paths]` | Runs the security scanner |
| `agentdiet eject` / `agentdiet uninstall [--purge]` | Restores backups and removes agentdiet |
| `agentdiet hook <event>` | Internal; invoked by assistant hooks |

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
| **B1 — shim end-to-end overhead** | Wall time of spawning a shimmed command until it exits, minus the same command spawned directly (`E2 − E0`), human path, warm | < 0.2 ms | **NOT MET.** Slice 4: median **3.74–3.84 ms** against 3.22–3.33 ms for the Slice 3B binary in the same session (see "Slice 4 re-measurement" below). Slice 3A: median 2.24 ms (p95 2.34, p99 2.59; n = 1000 iteration-matched pairs). |
| B2 — process launch | Spawning `agentdiet hook noop` minus spawning the direct command (`E1 − E0`) | none; context for B1 | median 1.12 ms (p95 1.20, p99 1.24; n = 1000) |
| B3 — in-process routing | `shim_name` + `resolve_real` on a synthetic `PATH`, up to the `exec` call | none; context for B1 | median 0.027 ms (p95 0.029, p99 0.030; n = 10000) |
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
