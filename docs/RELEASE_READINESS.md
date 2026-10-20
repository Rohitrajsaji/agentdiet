# AgentDiet — P4 release readiness

Status: **audit and plan** (this document). Results are added in the final section as each phase is
verified. `SPEC.md` stays the source of truth for behaviour; this document records what was checked,
where, and what is still open.

## 1. Audit (2026-10-20, at `0d310ed`)

Everything below was inspected or measured on this date; "verified" means a command was actually run,
and where.

### 1.1 Platforms

| Platform | State before P4 | Evidence |
|---|---|---|
| macOS aarch64 (Apple M1, Darwin 27) | **Verified**: the whole suite (471 tests), pseudo-terminal tests, benchmarks | every phase so far |
| macOS x86_64 | Not run | no Intel machine; `cfg(target_os = "macos")` code is architecture-neutral |
| Linux (glibc) | **Never executed.** Linux-only code exists (`/proc` agent detection, fd identity in `shim.rs`) | no Linux run in the history |
| Linux musl (x86_64, aarch64), the intended static release targets | Not built | README says "not verified for v0.1.0" |
| Windows | **Does not build** as far as can be told without a compile: shims are symlinks plus `exec`, `fsutil` uses `O_NOFOLLOW`/`flock`/Unix modes, some Unix-only calls are unguarded (`shim.rs`, `mcp.rs` process groups are guarded) | code reading; to be confirmed by a real compile |

### 1.2 Integrations

| Assistant | What AgentDiet writes | Automated tests | Live (real client) |
|---|---|---|---|
| Claude Code | hooks, MCP server, rules pointer | config patch/eject/restore; every hook event with recorded payloads; MCP JSON-RPC | **yes**, Claude Code 2.1.302 on 2026-10-17 (hooks replaced Bash/Read output; `diet_inspect` called) |
| Codex | MCP server (`config.toml` block) | patch/eject, TOML validation and conflicts | no |
| Gemini CLI | MCP server | patch/eject | no (not installed here) |
| Cursor | MCP server | patch/eject | no (needs the GUI) |
| Windsurf | MCP server | patch/eject | no (not installed here) |
| Copilot (VS Code) | MCP server (`servers`, `type: stdio`) | patch/eject | no (needs the GUI) |

Gap: nothing checks that a real client other than Claude Code parses what `init` writes and starts the
server.

### 1.3 Security

- `cargo deny` (advisory database fetched 2026-10-20 into a scratch path): **advisories 0, licenses all
  permissive (MIT/Apache-2.0/BSD/ISC/Unicode/Zlib/MPL), sources crates.io only**; 3 duplicate crate
  versions (`foldhash`, `hashbrown`, `syn`), warnings only.
- Config backups are byte-exact (may contain the user's own secrets) and protected by `0700`/`0600`
  from creation (SPEC §2.2); tested on macOS. On Windows the Unix modes do not exist: the port must say
  what protects them.
- Vault and memory.db are scrubbed before writing, private modes from creation; tested on macOS.
- No `SECURITY.md`; no CI running `cargo deny`.

### 1.4 Performance budgets (SPEC §7; none was relaxed)

| Budget | Requirement | Measured | Assessment |
|---|---|---|---|
| B1 shim overhead | < 0.2 ms | 2.7–3.9 ms (macOS) | **Not attainable as specified** on macOS with any separate-executable shim: a 33 KB C program that only `execv`s the real binary adds **1.72 ms** (n = 1000, this machine), the same as AgentDiet's own launch (+1.71 ms). Decision (owner, 2026-10-20): amend B1 relative to the measured exec floor, with this evidence, and try to win back the ~1 ms above it. |
| B4 engine fast path | < 10 ms at 256 KiB | realistic shipped paths 5.4–9.8 ms p50; misses on JSON→TOON p99, adversarial "secrets everywhere" (13.7 ms) and the fold path (28.7 ms; folding has no entry point yet) | documented miss, not a release blocker |
| B5 + B2 end-to-end | < 10 ms | 22–26 ms, dominated by durable `fsync` (macOS `F_FULLFSYNC`) | documented miss; durability is not traded for speed |

### 1.5 Packaging and installation

- Only `cargo install --path .` is documented; no prebuilt binaries, checksums, install script or Homebrew.
- `Cargo.toml` lacks `repository`, `homepage`, `rust-version`, `keywords`, `categories`.
- A `v0.1.0` tag exists at `e9daa72` (before P0–P3); `main` is 5 commits ahead of `origin/main`. The
  next release cannot reuse 0.1.0.
- Assistant configs pin the binary's path. On macOS a symlinked install (Homebrew's `bin/agentdiet` →
  `Cellar/…/0.1.0/bin/agentdiet`) records the **stable symlink** (checked with a simulated Cellar).
  On Linux `current_exe` reads `/proc/self/exe`, which resolves symlinks, so a Linuxbrew upgrade could
  leave configs pointing at a removed version: to be checked on Linux.

### 1.6 Documentation

README (P2/P3) covers install, quick start, troubleshooting, completions. Missing: `CHANGELOG.md`,
`SECURITY.md`, versioning and compatibility policy, upgrade and uninstall steps for binary installs,
supported-platform statement.

### 1.7 CI/CD

None: no `.github/`. Nothing runs fmt, clippy, tests, `cargo deny` or the binary-dependency check on
push; no release workflow.

### 1.8 Blockers versus documented limitations

**Release blockers** (must be closed or explicitly waived by the owner):
1. No CI.
2. Linux never executed; musl release targets never built.
3. No release artifacts, checksums, or install path other than building from source.
4. No `CHANGELOG.md`, `SECURITY.md`, versioning/compatibility policy.
5. B1 as specified is unattainable; the amendment (approved) must be written, measured and met or
   recorded as a miss.
6. Windows: the owner chose a full port for P4; until it builds and its tests run, Windows is unsupported.

**Acceptable, documented limitations** (stay in SPEC §7/§8 and the README):
- B4 and B5 misses (durability and adversarial inputs).
- Live verification limited to the clients that can be run here; others stay "not live-verified".
- Secret detection is best effort; the transparent shim path and binary output are not scrubbed.
- File reads are scrubbed, not folded; TOON covers a subset.

## 2. Decisions taken (owner, 2026-10-20)

| Question | Decision |
|---|---|
| B1 | Amend with evidence: keep an in-process budget, set the end-to-end budget relative to the measured exec floor, then reduce the excess |
| Linux | Run the suite and musl builds in Docker Linux containers on this Mac |
| Windows | Full native port (P4) |
| Live checks | Run the installed Codex and Claude Code CLIs against AgentDiet's configs in a scratch HOME, read-only commands |

## 3. Plan

Each step is independently testable and committed on its own; nothing is pushed, tagged or published.

1. **Linux (Docker).** Full suite as a non-root user on Linux arm64 (native here) with the README's
   minimum Rust (1.90); musl builds for aarch64 and x86_64 (x86_64 emulated), each run in a minimal
   container; `ldd`/`file` show static linking; the Linuxbrew symlink question.
2. **CI/CD.** `.github/workflows/ci.yml` (macOS + Linux: fmt, clippy, nextest, doc tests, `cargo deny`,
   binary-dependency check, musl builds) and `release.yml` (tag-triggered build matrix, SHA-256
   checksums, draft release). Written and linted locally; **not run** until pushed with the owner's
   approval.
3. **Integration validation.** Live, scratch HOME: `codex mcp list` (and a server start) with
   `CODEX_HOME` pointing at AgentDiet's `config.toml`; `claude mcp list` with `HOME` pointing at its
   `.claude.json`. Automated: a protocol-level smoke harness (`scripts/smoke-mcp.sh`) that any user can
   run against any client's config. Each assistant is labelled *automated*, *live (version, date)* or
   *unverified*.
4. **Security.** `deny.toml` in the repo and in CI; review of `init`/`eject` restoration and permission
   tests on Linux; Windows protection model documented with the port.
5. **Performance.** SPEC §7 B1 amendment with the floor measurement; a floor arm in `tests/perf.rs`;
   profile the ~1 ms above the floor; re-measure.
6. **Distribution and docs.** `Cargo.toml` metadata; `scripts/install.sh` (download, verify checksum,
   place binary, no rc edits); a Homebrew formula **template** (not published); `CHANGELOG.md`,
   `SECURITY.md`, `docs/VERSIONING.md`; README install/upgrade/uninstall and platform table.
7. **Windows port.** Design in SPEC first (shims as `.exe` hard links or copies with spawn-and-wait, a
   Toolhelp-based process walk, file protection via the user profile's ACLs, `LockFileEx` locks);
   built for `x86_64-pc-windows-gnu` in a Linux container and its tests run under Wine there, clearly
   labelled as **not** real Windows; CI adds a `windows-latest` job for the real check.
8. **Final verification and report** (section 4).

## 4. Results

(Filled in as each phase is verified.)
