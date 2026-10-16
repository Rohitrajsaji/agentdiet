# CLAUDE.md — AgentDiet

AgentDiet (`agentdiet`) is an open-source, zero-runtime-dependency, single-binary Rust
context firewall for AI coding agents. `SPEC.md` is the source of truth for behavior;
this file is the source of truth for *how we build it*.

## Non-negotiable build rules

- **Single binary, zero runtime deps.** One `agentdiet` executable. No Node, Python,
  shared grammar files, or helper binaries at runtime. Rust crates are fine; they compile in.
  - Linux release builds: `x86_64-unknown-linux-musl` / `aarch64-unknown-linux-musl` (fully static).
  - macOS: as static as the platform allows (libSystem is always dynamic); no other dylibs.
- **Errors:** `thiserror` for typed errors in library modules; `anyhow` (with `.context()`)
  at binary/command boundaries. No `unwrap()` / `expect()` outside tests and proven invariants.
- **Storage:** `rusqlite` with features `["bundled", "bundled-full"]` — SQLite, FTS5 and
  BM25 ranking are compiled in. Never link a system SQLite. Databases open in WAL mode.
- **Code parsing:** `tree-sitter` with statically linked grammar crates only (no runtime
  grammar loading, no `.so`, no wasm).
- **CLI:** `clap` (derive API) for commands and flags; `inquire` for interactive prompts and
  multi-select checklists (`init`, Control Center).
- **TUI:** `ratatui` for `agentdiet top`.
- **Baseline crates:** `serde` + `serde_json` (state.json, hook payloads, MCP JSON-RPC, TOON)
  and `sha2` (CAS hashing). The MCP server is hand-rolled stdio JSON-RPC 2.0 — no async runtime.
- **Performance budgets are requirements, not goals:** shim passthrough < 0.2 ms, Diet Engine
  fast path < 10 ms (see `SPEC.md`). Keep the passthrough path free of heavy init: no DB open,
  no tree-sitter parse, no async runtime.
- Any dependency beyond the above needs a one-line justification in the commit message and
  must not introduce a runtime requirement.

## Verification loop (run after every edit)

```sh
cargo check --quiet
cargo nextest run
cargo clippy -- -D warnings
```

- All three must pass before a change is considered done. Fix warnings; do not `#[allow]`
  them without a comment explaining why.
- `cargo-nextest` is a separate install: `cargo install cargo-nextest --locked`.
- nextest does not run doctests — also run `cargo test --doc` when doc examples change.
- The loop applies once a `Cargo.toml` exists.

## Reference repos — clean-room lookup only

Reference repos live at `../agentdiet-refs/{rtk,context-mode,toon}`. Read them to understand
*patterns and behavior*, then write our own implementation from `SPEC.md`.

| Path | License | Rule |
|---|---|---|
| `rtk/engine/**`, `rtk/proxy/**` | BUSL-1.1 | **Never copy** code, tests, regexes or data. Behavior lookup only. |
| `context-mode/**` (whole repo) | Elastic-2.0 (ELv2) | **Never copy** anything. Behavior lookup only. |
| `rtk/src/**` and the rest of rtk | Apache-2.0 | Clean-room by default. If anything is ever adapted, keep the license notice and attribution (NOTICE). |
| `toon/**` | MIT | Clean-room by default. `toon/SPEC.md` is our TOON encoding reference. Adapted code keeps the MIT copyright line. |

- Never paste reference code into this repo, or into prompts that generate code here.
- When a design is informed by a reference, note it in a comment as
  `// pattern ref: <repo>/<path> (behavior only)`. Never reproduce BUSL/ELv2 code text.
- Secretlint (MIT, JavaScript) is the source for secret-scrubbing regexes; ported patterns
  need attribution — see `SPEC.md` Tier 3.

## Repo conventions

- Behavior changes start in `SPEC.md`, then code.
- Hook handlers and shims fail open: on any internal error, pass the raw output through or
  exec the real binary rather than break the user's agent or shell.
- Hooks never block on stdin or a TTY.
