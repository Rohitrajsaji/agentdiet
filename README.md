# AgentDiet

**A context firewall for AI coding agents.** AgentDiet sits between your agent and the output of
the tools it runs. It trims the noise (passing tests, build progress, download bars) and turns
model-facing JSON into compact TOON. It also redacts secrets before anything reaches the model or
the disk.

Nothing is lost. Each trimmed output is stored in a local Vault, and a one-line breadcrumb tells the
agent how to get it back:

```
test result: ok. 400 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
🥗 Diet: kept 14/412 lines; cargo test: 398 passing test lines collapsed; exit 0 Run: agentdiet show '#f5f26875'
```

The one exception is redacted secrets: they are never stored, so they can never be recovered.

AgentDiet is one static Rust binary with zero runtime dependencies: no Node, no Python, no grammar
files. `SPEC.md` is the authoritative description of its behaviour.

## How it works: three tiers

```
 agent ──tool call──▶ tool ──raw output──▶ ┌──────────────────────────────┐ ──dieted──▶ agent
                                           │ 1. Hook router / PPID shims  │
                                           │ 2. Diet Engine               │
                                           │ 3. Vault + memory.db         │
                                           └──────────────────────────────┘
```

1. **Hook router and PPID shims.**
   - *Hooks:* Claude Code's `PostToolUse` hook hands each successful tool result to AgentDiet, which
     returns a replacement of exactly the same shape (`updatedToolOutput`). AgentDiet's own MCP tools
     are never re-processed.
   - *Shims:* `~/.agentdiet/shims/{git,cargo,pytest,npm,…}` catch commands that an agent runs,
     including failing ones. They walk the parent process chain to tell an agent from a human, and
     they only capture output that goes straight back to the agent. A human at a terminal, a pipe or
     a redirect gets the real binary via `execv`.
2. **Diet Engine.** It scrubs secrets, then builds a candidate:
   - command-aware compression for cargo, pytest, go test, npm/pnpm/yarn, git, make, docker and ruff;
   - TOON for JSON that MCP tools and WebFetch return to the model.

   A candidate is used only if it is strictly smaller in bytes and in estimated tokens (breadcrumb
   included) and meets the Diet Mode's required saving; otherwise the scrubbed original passes. Error
   and warning lines are always kept verbatim, with their context.
3. **Vault and memory.**
   - *Vault:* the full scrubbed original is written durably to a content-addressed store at
     `~/.agentdiet/cas/` before any replacement is emitted. Entries expire 7 days after their last
     write.
   - *memory.db:* a summary is indexed in the project's `.agentdiet/memory.db` (SQLite FTS5 with BM25
     ranking), so agents can search past outputs.

## Before / After

These figures come from real runs on an Apple M1 (macOS), in Balanced mode, on 2026-10-17, at commit
`1e72a5d`, through `diet_exec`, which uses the same engine as the hooks and shims:

- *Bytes* compare the stored scrubbed original with what the agent received, stdout and stderr together.
- *Tokens* are AgentDiet's own deterministic estimate (`SPEC.md` §3, "Estimated tokens"), not any
  provider's tokenizer. Each row's saving equals the engine's own `tokens_saved` counter.

| Output | Bytes raw → dieted | Est. tokens raw → dieted | Saved |
|---|---|---|---|
| `cargo test`, 400 passing tests | 11,872 → 843 | 5,047 → 309 | 94% |
| `cargo build` from clean (AgentDiet's own dependency tree) | 4,058 → 508 | 1,579 → 194 | 88% |
| `go test -v ./...`, 300 passing tests | 15,420 → 390 | 6,615 → 154 | 98% |
| MCP tool result: JSON array of 200 uniform objects → TOON | 17,582 → 4,478 | 8,202 → 2,038 | 75% |

Small or already-terse outputs are not trimmed (in Balanced mode, nothing under 600 estimated tokens),
but they are still scrubbed.

## Quickstart

AgentDiet's configs point at the binary by absolute path, so put it somewhere stable first:

```sh
cargo install --path . --locked        # or copy target/release/agentdiet onto your PATH
```

Connect your assistants:

```sh
agentdiet init                         # interactive checklist of detected assistants
agentdiet init --only claude,shims     # or pick targets: claude, cursor, windsurf, gemini, copilot, codex, shims
agentdiet init --project --dry-run     # project-level configs; print the diff, write nothing
```

- `init` backs up every file before changing it (`~/.agentdiet/backups/`).
- It is idempotent: running it again changes nothing.
- It never edits your shell rc files. For the shims, add the line it prints:

  ```sh
  export PATH="$HOME/.agentdiet/shims:$PATH"
  ```

Day to day:

```sh
agentdiet                              # Control Center: pause/resume, inspect last output, Diet Mode, assistants, top
agentdiet status                       # on or paused, Diet Mode, connected assistants, tokens saved, health
agentdiet doctor                       # checks configs, binary paths, shims and PATH; prints the fix for each problem
agentdiet pause                        # stays paused until resumed (Claude Code: or type "@diet pause" in chat)
agentdiet resume                       # (Claude Code: or type "@diet resume" in chat)
agentdiet mode                         # show the Diet Mode; `agentdiet mode light|balanced|ultra-lean` sets it
agentdiet show last                    # the last trimmed output in full
agentdiet show '#f5f26875'             # full stored output for a breadcrumb ID
agentdiet show '#f5f26875' --lines 10..40
agentdiet top                          # live dashboard: tokens saved, Vault, CAS dereference rate
agentdiet scan                         # static scan of .claude/, .cursor/, .mcp.json, SKILL.md, CLAUDE.md, AGENTS.md
agentdiet stats                        # counters plus this workspace's run log (shim / MCP / hook runs); --json
agentdiet reflect                      # write EXPERIENCE.md: recipes and pitfalls learned from the run log (no LLM)
agentdiet eject                        # restore original configs, remove hooks and shims (--purge also deletes the Vault)
```

- Always quote an ID: an unquoted `#…` starts a shell comment.
- `scan` flags hidden Unicode, secrets, prompt-injection phrases and dangerous shell commands. It
  exits 0 when clean, 1 on findings, and 2 when a path can't be read.
- Need one output in full? In Claude Code, put `@diet:full` (or `--diet-full`) in your prompt, and
  the next tool call's output is passed through (still scrubbed); the call after it is trimmed again.
  Plain words never do this: "the full report" or "raw data" is prose. Chat commands and the directive
  come from Claude Code's prompt hook; they don't work in other assistants.
- Memory lives in the workspace root: the nearest directory with `.agentdiet/memory.db` or `.git`
  (or `AGENTDIET_WORKSPACE`), so commands run in a subdirectory land in the same place.
- `agentdiet reflect` keeps `EXPERIENCE.md` at most 2 KB, cites a Vault ID or time for every lesson,
  and keeps anything you write outside its marker block. `init` adds a one-line pointer to it in
  `.claude/rules/agentdiet.md`.
- Running the exact same command twice in a row expands it on the retry.

### MCP tools

`agentdiet mcp` is a stdio JSON-RPC 2.0 server, which `init` registers for every assistant. It
offers four tools:

| Tool | Arguments | Does |
|---|---|---|
| `diet_exec` | `command` (argv array or one simple command), `cwd?` | Runs the command without a shell, through the Diet Engine; returns the trimmed output (long output of any command is cut to head, tail and problem lines), a breadcrumb and the exit status |
| `diet_search` | `query`, `limit?` | BM25 search over the project's memory; each hit carries its `#id` |
| `diet_inspect` | `id`, `start_line?`, `end_line?` | Returns the stored (scrubbed) output, or a slice of it |
| `diet_reflect` | `dry_run?` | Runs `agentdiet reflect` and returns `EXPERIENCE.md` |

### Diet Modes

| | Light | Balanced (default) | Ultra-Lean |
|---|---|---|---|
| Minimum size before trimming (est. tokens) | 1500 | 600 | 250 |
| Required saving | ≥ 40% | ≥ 20% | ≥ 10% |
| Context lines kept around each error/warning | 5 | 3 | 1 |
| Last lines always kept | 20 | 10 | 5 |

## Build from source

You need rustc 1.90 or newer and a C compiler.
- *rustc:* 1.90 is the newest `rust-version` declared by a dependency (tree-sitter). Only rustc
  1.99.1 has actually been tested.
- *C compiler:* needed for the bundled SQLite and the statically linked tree-sitter grammars.

```sh
cargo build --release --locked         # → target/release/agentdiet
scripts/check-binary-deps.sh --build   # verify the artifact links nothing beyond libSystem (macOS) / nothing (Linux musl)
```

The intended Linux release targets are `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`,
which give fully static binaries. These builds have not been verified for v0.1.0. They need
`rustup target add <target>` and a musl-capable C toolchain:

```sh
cargo build --release --locked --target x86_64-unknown-linux-musl
```

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
  - Shim passthrough adds about 3.7 ms against a < 0.2 ms budget; this needs a shim redesign.
  - The engine's pure transform is up to about 29 ms on 256 KiB inputs, against < 10 ms.
  - A full engine run on 64 KiB, including the durable Vault write, takes about 22–26 ms. Most of
    that is macOS `F_FULLFSYNC`, and durability is never traded for speed.
- **Live checks so far: Claude Code 2.1.302 only.** That covers the hooks, the MCP tools, and a
  shimmed `cargo test`. The other assistants (Cursor, Windsurf, Gemini CLI, Copilot, Codex) get the
  MCP server but no hooks.
  - The shims diet only commands that an agent runs without a terminal.
  - A command run in an integrated terminal takes the transparent path.
  - None of this has been checked live outside Claude Code.
- **Failing commands** are trimmed only through the shims. Claude Code's hook can't replace the
  result of a failed Bash call.
- **File reads are scrubbed, never folded.** Signature folding exists in the engine but no entry
  point uses it, until it is settled how an agent edits a folded file.
- **TOON** applies only to JSON from MCP tools and WebFetch, never to shell stdout, and only for a
  subset of the format. Unsupported shapes are left unchanged.
- **Secret detection is best effort.** It is pattern based, and a clean result is not a guarantee.
  Output that takes the shim's transparent (human) path, and binary output, are not scrubbed.

## License

MIT; see [`LICENSE`](LICENSE). Secret-scrubbing patterns ported from
[Secretlint](https://github.com/secretlint/secretlint) and the vendored TOON conformance fixtures are
MIT-licensed by their authors; see [`THIRD_PARTY_NOTICES`](THIRD_PARTY_NOTICES).
