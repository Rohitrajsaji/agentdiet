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
  `gh`. Until the Diet Engine ships, shim mode always execs the real binary.

### 2.3 Stateful Pause / Resume

Global state lives in `~/.agentdiet/state.json`:

```json
{
  "status": "active",
  "mode": "balanced",
  "raw_tokens_passed_while_paused": 0,
  "on_session_start": "prompt",
  "last_trimmed_id": null
}
```

| Field | Values |
|---|---|
| `status` | `"active"` \| `"paused"` |
| `mode` | `"light"` \| `"balanced"` \| `"ultra-lean"` |
| `raw_tokens_passed_while_paused` | non-negative integer |
| `on_session_start` | `"prompt"` \| `"auto_resume"` (default `"prompt"`) |
| `last_trimmed_id` | short ID (e.g. `"4f9a"`) of the most recently trimmed output, or `null` (default). Read by the Control Center's "Inspect last trimmed output". |

- A missing, unreadable or corrupt `state.json` loads as defaults (fail open); unknown fields are ignored.

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

#### Tier 3 details (Slice 2)

- **Redaction:** a secret span is replaced by `[REDACTED:<rule-id>]`; surrounding text (e.g. the
  variable name) is kept. PEM private-key blocks are replaced whole.
- **CAS files** are `<64hex>.log` (dir mode 0700, files 0600). Writing identical content again
  refreshes the file's mtime instead of rewriting it; the 7-day TTL counts from the last write.
- **GC** runs on `put` and on `show`. Deleting an expired file leaves a zero-byte
  `<64hex>.expired` tombstone, which is how `show` tells "expired" from "not found". A new `put`
  of the same content removes the tombstone. Tombstones are kept until the vault is purged: they
  are the vault's memory of which IDs were once issued.
- **An old ID never resolves to an unrelated entry.** The short ID shown for an entry is the
  shortest prefix (at least 4 hex) that no *other* entry shares, counting both live entries and
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
- **ID input** is accepted as `#4f9a` or bare `4f9a` (an unquoted `#...` is a shell comment).
  Case-insensitive, hex only, at least 4 characters.
- **Slicing** splits on `\n` and keeps line endings (CRLF and a missing trailing newline survive).
  A start of 0 or start > end is an error; an end past EOF clamps; a start past EOF yields nothing.
- **memory.db** has one `entries` table (`kind` is `command` | `read` | `decision`) mirrored into
  an FTS5 table over `title` and `body`. Results are ordered by `bm25()` ascending (lower is
  better) with title weighted above body. Query tokens are quoted as FTS5 phrases and ANDed, so
  user text can never produce an FTS syntax error.

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
  - Every `init` write that changes a file records a `manifest.json` in its backup directory
    (original absolute path, whether the file existed, sha256 of what was written).
  - Eject restores the **earliest** backup of each path byte-for-byte (or deletes a file init
    created) when the file still matches what init last wrote. If the user edited it since, eject
    removes only the agentdiet entries and keeps the user's edits.
  - Processed manifests are renamed `manifest.ejected.json`; backups are kept.

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
- Secrets are never written to disk unscrubbed. The one exception is the byte-exact backup of a
  user's own config file made by `init` (see §2.2), which is kept `0700`/`0600`.
- Hooks never block on stdin or a TTY.
