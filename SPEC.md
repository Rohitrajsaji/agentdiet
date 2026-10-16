# SPEC.md — AgentDiet

## 1. Overview

AgentDiet (`agentdiet`) is a context firewall for AI coding agents. It sits between an agent
and tool output — via agent hooks and PATH shims — and trims what the agent sees. It never
loses data: everything trimmed is stored raw in the Vault and can be recovered by short ID.

Build rules live in `CLAUDE.md`. This file defines behavior.

## 2. UX Rules

### 2.1 Control Center

Running bare `agentdiet` opens an interactive Control Center menu (`inquire`), in this order:

1. **Pause / Resume** — a toggle; the label reflects the current status.
2. **Inspect last trimmed output** — shows the most recent short ID (e.g. `#4f9a`) and its content.
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
  and updated, never duplicated.

### 2.3 Stateful Pause / Resume

Global state lives in `~/.agentdiet/state.json`:

```json
{
  "status": "active",
  "mode": "balanced",
  "raw_tokens_passed_while_paused": 0,
  "on_session_start": "prompt"
}
```

| Field | Values |
|---|---|
| `status` | `"active"` \| `"paused"` |
| `mode` | `"light"` \| `"balanced"` \| `"ultra-lean"` |
| `raw_tokens_passed_while_paused` | non-negative integer |
| `on_session_start` | `"prompt"` \| `"auto_resume"` (default `"prompt"`) |

- Writes are atomic (temp file + rename).
- Pausing: `agentdiet pause`, the Control Center toggle, or typing `@diet pause` in chat
  (detected in UserPromptSubmit).
- Once paused, it **stays paused** until manually resumed via `agentdiet resume`, the
  Control Center toggle, or typing `@diet resume` in chat (detected in UserPromptSubmit).
- While paused, all output passes through raw and `raw_tokens_passed_while_paused` accumulates.
- **SessionStart while paused never blocks.** Hooks have no interactive TTY, so nothing ever
  reads stdin for confirmation.
  - `prompt` (default): emit exactly one line to hook stdout:
    `⏸ AgentDiet is paused. Run 'agentdiet resume' or type '@diet resume' to re-enable.`
  - `auto_resume`: set `status` to `"active"` and emit a one-line notice that it resumed.
- **Paused whisper:** while paused, any raw tool output exceeding **3,000 tokens** gets that
  same one-line paused notice appended.

### 2.4 Escape Hatches

- If a UserPromptSubmit contains `raw`, `full` or `unfiltered`, compression is bypassed for the
  **next turn only**. Matching is whole-word and case-insensitive, so "fully" or "full-stack"
  do not trigger it.
- Running the exact same command twice in a row auto-expands the output on the retry.

## 3. Architecture — 3 Tiers

### Tier 1 — Hook Router & PPID Shims

- `main.rs` inspects `argv[0]`. If invoked as `git`, `pytest`, `cargo` (and so on) through
  `~/.agentdiet/shims/`, it runs in shim mode.
- Shim mode walks the parent process tree (PPID chain):
  - **No AI-agent ancestor (human shell):** `execv` straight through to the real binary
    (resolved from `PATH` with the shims directory excluded) in **< 0.2 ms**.
  - **AI-agent ancestor:** run the command and route its output through the Diet Engine.
- Hook mode (`agentdiet hook <event>`) handles SessionStart, UserPromptSubmit, PreToolUse,
  PostToolUse and PreCompact.
- **PreToolUse must protect interleaved thinking blocks:** it never modifies, reorders or
  strips them.

### Tier 2 — Fast-Path Diet Engine (< 10 ms)

- **Shell output compression.** If the compressed token count is **≥ the raw token count**,
  refuse to replace the output and emit the raw output unchanged. This fixes the `ls`
  token-inflation bug.
- **TOON encoding.** Uniform / tabular JSON is encoded to TOON losslessly.
- **Signature folding.** For `Read` on files over **300 lines**, tree-sitter folds function
  bodies down to their signatures.
- **Diet Mode** (Light / Balanced / Ultra-Lean) sets how aggressive the thresholds are. Exact
  thresholds are decided during implementation and recorded here when fixed.

### Tier 3 — Persistent Vault

1. **Scrub secrets** before storing or emitting anything, using regexes ported from Secretlint's
   rule set. Secretlint is MIT-licensed; ported patterns keep attribution in a
   `THIRD_PARTY_NOTICES` file.
2. **Store raw output losslessly** at `~/.agentdiet/cas/<full_sha256>.log`. The 64-hex sha256 is
   computed over the scrubbed output. Storage is content-addressed, so identical output dedupes.
3. **7-day auto-GC.** CAS files older than 7 days are deleted opportunistically (on writes, or by
   `agentdiet top`). `show` on an expired ID reports "expired" rather than "not found".
4. **Short IDs** are shown as `#4f9a`: the shortest unique prefix of the sha256, minimum 4 hex
   chars. Input IDs are resolved by unique prefix match against the CAS filenames; an ambiguous
   prefix is an error that lists the candidates.
5. **Slicing:** `agentdiet show #4f9a --lines <start>..<end>` returns only those lines
   (1-based, inclusive).
6. **Breadcrumb.** Append one line to the trimmed output:
   `🥗 Diet: <summary of what was trimmed> Run: agentdiet show #4f9a`
7. **Index summaries** into `.agentdiet/memory.db` (project-local SQLite, WAL, FTS5 + BM25).
   PreCompact recovery uses this index to re-surface relevant context.

### 3b. Interfaces & Tools (outside the hot path)

- **`agentdiet mcp`** — stdio JSON-RPC 2.0 MCP server exposing three tools:
  - `diet_inspect` — fetch a CAS entry by `#id`, with an optional line range.
  - `diet_search` — FTS5/BM25 search over `memory.db`.
  - `diet_exec` — run a command through the Diet Engine; returns trimmed output plus a breadcrumb.
- **`agentdiet scan`** — static AST/regex security scanner for `SKILL.md` files and MCP configs.
  Flags secrets, dangerous shell commands and prompt-injection patterns; exits non-zero on findings.
- **`agentdiet eject` / `agentdiet uninstall`** — restore original configs from
  `~/.agentdiet/backups/`, remove agentdiet hook entries and shims. Idempotent. `cas/` and
  `memory.db` are kept unless `--purge` is passed.

## 4. Filesystem Layout

- **Global — `~/.agentdiet/`:** `state.json`, `backups/`, `shims/`, `cas/`
- **Project-local — `.agentdiet/`:** `memory.db`

## 5. Commands

| Command | What it does |
|---|---|
| `agentdiet` | Opens the Control Center |
| `agentdiet init [--only <targets>] [--all] [--project] [--dry-run]` | Connects assistants |
| `agentdiet pause` | Pauses trimming |
| `agentdiet resume` | Resumes trimming |
| `agentdiet show #id [--lines a..b]` | Prints the full raw output, or a line slice |
| `agentdiet top` | Opens the TUI |
| `agentdiet mcp` | Starts the MCP server |
| `agentdiet scan [paths]` | Runs the security scanner |
| `agentdiet eject` / `agentdiet uninstall [--purge]` | Restores backups and removes agentdiet |
| `agentdiet hook <event>` | Internal; invoked by assistant hooks |

## 6. Invariants

- Never lose data: anything trimmed is recoverable through `show`.
- Never inflate: emitted output is never larger than the raw output.
- Fail open: internal errors pass raw output / exec the real binary.
- Thinking blocks are never touched.
- Secrets are never written to disk unscrubbed.
- Hooks never block on stdin or a TTY.
