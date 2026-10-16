# CLAUDE.md — AgentDiet

AgentDiet (`agentdiet`) is an open-source, zero-runtime-dependency, single-binary Rust
context firewall for AI coding agents. `SPEC.md` is the source of truth for behavior;
this file is the source of truth for *how we build it*.

## Non-negotiable build rules

- **Single binary, zero runtime deps.** One `agentdiet` executable. No Node, Python,
  shared grammar files, or helper binaries at runtime. Rust crates are fine; they compile in.
  - Linux release builds: `x86_64-unknown-linux-musl` / `aarch64-unknown-linux-musl` (fully static).
  - macOS: the release binary may load only `/usr/lib/libSystem.B.dylib`. `build.rs` passes
    `-Wl,-dead_strip_dylibs` on Apple targets, which drops dylibs nothing references, notably
    `libiconv` (the `libc` crate links it unconditionally on Apple). If a future dependency
    really calls iconv, the linker keeps it and the check fails: stop and decide explicitly, do not
    loosen this rule silently. Run `scripts/check-binary-deps.sh --build` after any dependency
    change; it inspects a freshly built artifact (macOS and Linux only; it exits 2, not 0, on
    anything it cannot check).
- **Errors:** `thiserror` for typed errors in library modules; `anyhow` (with `.context()`)
  at binary/command boundaries. No `unwrap()` / `expect()` outside tests and proven invariants.
- **Storage:** `rusqlite` with `default-features = false, features = ["bundled"]`. `bundled`
  compiles SQLite from source with FTS5 (including its built-in `bm25()`) enabled; nothing more is
  needed. Do not enable `bundled-full`/`modern-full` or the type-integration features (`chrono`,
  `time`, `jiff`, `url`, `uuid`, `serde_json`, `csvtab`, `load_extension`): they add crates, and
  `chrono` -> `iana-time-zone` links CoreFoundation on macOS. Never link a system SQLite.
  Databases open in WAL mode. FTS5 is proven by tests that query it (`src/memory.rs`), not by
  `strings` on a binary: link-time optimisation drops SQLite from any binary in which nothing
  reachable uses `memory.rs`.
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
  Each budget is measured at the boundary `SPEC.md` §7 defines for it (shim benchmark:
  `cargo test --release --test perf -- --ignored --nocapture`). Report measured numbers and record
  misses in `SPEC.md`; never claim an unmeasured pass or move a budget to a boundary it passes.
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
| `toon/**` | MIT | Clean-room by default. `toon/SPEC.md` is only a pointer; the normative TOON text is the revision pinned in `SPEC.md` §9 (toon-format/spec). Adapted code keeps the MIT copyright line. |

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
