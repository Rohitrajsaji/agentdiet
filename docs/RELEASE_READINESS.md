# AgentDiet — P4 release readiness

Status: **release candidate prepared locally; not released** (2026-10-21). Section 4 records what was
verified, where, and what is still open. `SPEC.md` stays the source of truth for behaviour; this document records what was checked,
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

## 4. Results (2026-10-21, release candidate at `b3248a9`)

"Verified" below means a command was run and its exit status checked; where something was only built,
or only written, it says so. Nothing has been pushed, tagged or published.

### 4.1 Commits (P4)

| Commit | Slice |
|---|---|
| `ca3a7f2` | fix(privacy): titles, summaries and notes are scrubbed whole before they are cut |
| `a26350e` | fix(shim): a redirect under dash/ksh/mksh/posh/yash/busybox stays transparent (found on Linux) |
| `88e9380` | perf(spec): B1 judged against the exec floor measured in the same run (owner's decision) |
| `f413349` | test: every assistant end to end from its own config format; v0.1.0 upgrade data |
| `517cc4a` | fix(memory): SQLite's own handle is checked (`SQLITE_FCNTL_HAS_MOVED`), not just the path |
| `324e610` | feat(windows): everything but the shims |
| `ffda21a` | ci: CI and draft-release workflows, cargo-deny, reproducible packaging, installer |
| `29f97ef` | docs: CHANGELOG, SECURITY, VERSIONING, README install and platforms |
| `b3248a9` | fix(privacy): every diagnostic that can quote the caller's input is scrubbed |

### 4.2 Platforms

| Platform | Result | Evidence |
|---|---|---|
| macOS aarch64 (M1, Darwin 27) | **verified** | full suite at HEAD: 481 passed, 9 skipped (nextest; the skips are opt-in benchmarks and helper processes); clippy `-D warnings`, fmt, doctests; release binary links only `libSystem` (`check-binary-deps.sh`); RC smoke test (init, doctor, MCP trim, recovery, byte-exact disconnect) OK |
| macOS x86_64 | **built only** | release binary from HEAD, links only `libSystem`; not executed (no Rosetta on this Mac, and installing it is a system change) |
| Linux aarch64, glibc (Debian 12, Docker, non-root) | **verified** | at HEAD: fmt, check, clippy `-D warnings`, nextest **480 passed**, 9 skipped, doctests; with bash, zsh, dash, mksh, ksh, posh, yash and busybox installed. A first attempt at HEAD failed in nextest while the machine was saturated by x86_64 emulation; its log was lost with the container (see 4.10, flakes). Earlier (mid-P4 tree) the suite also passed on Rust **1.90** (472/472). x86_64 glibc: CI only |
| Linux musl aarch64 / x86_64 (release binaries) | **verified** (x86_64 under Docker's Rosetta emulation, not x86 hardware) | built from HEAD twice in different directories, byte-identical (aarch64 `38603dea…`, x86_64 `7f022b9e…`); no `DT_NEEDED`, no interpreter (`check-binary-deps.sh`); in a bare `alpine:3.20` container each passed the smoke test: version, `status --json`, init, doctor, MCP trim, recovery of all 4000 lines, byte-exact disconnect |
| Windows aarch64 (llvm-mingw), **Wine 10.0** on Debian 13 arm64 | **runs under Wine** | Wine is not Windows. At `324e610`: whole suite 351 passed, 0 failed, 3 ignored. At HEAD: clippy `-D warnings` clean; 352 passed, 2 failed, both wall-clock tests (a hook that must not wait on an open stdin; a 2 s launch bound) while the machine was saturated; both passed twice when rerun idle. The breadcrumb's literal recovery command works in `cmd.exe`. Wine 8.0 (Debian 12) could not run Rust ≥ 1.78 binaries (no `ProcessPrng`) and crashed in thread-local storage on arm64 |
| Windows x86_64 (mingw) | **built only**: release build, clippy `-D warnings`, every test binary compiles | Wine aborts under Docker Desktop's Rosetta x86_64 emulation (`xsave_header.xfeatures` assertion), so nothing x86_64-Windows was executed |
| Windows x86_64 MSVC (the release target) | **not built here** (no MSVC toolchain) | CI `windows` job and the release workflow, not yet run |

### 4.3 Defects found and fixed in P4

1. **Privacy: unredacted secret prefixes in memory.db** (present since v0.1.0). Titles were cut to 240
   bytes before scrubbing; a key crossing the cut was stored as a prefix too short to match. Fixed by
   scrubbing first; regression tests read the persisted rows and FTS index for command and read titles.
2. **Privacy: diagnostics repeated the caller's input** (`show <id>`, `scan <path>`, MCP tool and
   JSON-RPC errors, clap usage errors). Found by planting a key in every entry point's input and forcing
   error paths; all now scrubbed (SPEC P1). Hook payloads, `diet_exec` output and the shim agent path
   were already clean; panics in hook/shim/MCP print only a fixed note.
3. **Shim trimmed a redirected file under dash** (Debian/Ubuntu `/bin/sh`) and five other shells that
   redirect in the parent. Found by the first Linux run.
4. **memory.db open check was weaker than SPEC claimed**: comparing the path with the pre-opened handle
   misses a swap-and-restore around SQLite's `open`. Now SQLite's own handle is compared too. Audit: the
   bundled SQLite opens the database, `-wal` and `-shm` with `O_NOFOLLOW`, so planted symlinks (a cloned
   repository can carry them) are never written through (tested). Remaining, documented: a double swap
   between two `stat` calls (needs the user's own write access), pre-existing `.agentdiet/` used as is.
5. **Windows: `dirs` ignores `%USERPROFILE%`/`%APPDATA%`**, so AgentDiet and Node-based assistants could
   disagree, and `cargo test` on a Windows machine would have written to the developer's real assistant
   configs. Now the environment is read first; every test isolates both.
6. **Windows: the breadcrumb's recovery command failed in `cmd.exe`** (it keeps the `'` quotes). IDs in
   one pair of single quotes are now accepted; the breadcrumb text is unchanged.
7. Test-suite portability: Linux `openpty`, MSRV 1.90 inference, Unix-only tests gated with reasons.

### 4.4 Integrations

| Assistant | Automated (`tests/integrations.rs`: config parsed in the client's format, exact command started, MCP trim + recovery, doctor, byte-exact disconnect, reconnect) | Live (real client, scratch HOME) |
|---|---|---|
| Claude Code | yes | **2.1.305**: `claude mcp list` → Connected (2026-10-21); hooks replaced output live with 2.1.302 on 2026-10-17 |
| Codex | yes | **codex-cli 0.163.0**: `codex mcp get agentdiet` parses the server (stdio) (2026-10-21) |
| Gemini CLI | yes | not installed here: unverified live |
| Cursor | yes | needs the GUI: unverified live |
| Windsurf | yes | needs the GUI: unverified live |
| Copilot (VS Code) | yes | needs the GUI: unverified live |

Claude Code hooks on Windows (`"<exe>" hook <Event>`) have not been run by a real client.

### 4.5 Upgrade compatibility

- `tests/compat.rs` reads state, a Vault entry and memory.db **written by the v0.1.0 binary** (fixture
  committed): counters, `show`, `doctor`, MCP search/inspect, `resume`.
- Configs written by v0.1.0's `init` are updated in place by the new `init` and removed by `eject`,
  which restored the pre-v0.1.0 originals byte for byte (scratch HOME, v0.1.0 built from its tag).

### 4.6 Packaging and installation

- Archives: `package.py` output is byte-identical across runs (macOS, musl and a Windows zip checked).
- RC archives for macOS aarch64/x86_64 and musl aarch64/x86_64 plus completions, with `SHA256SUMS`, are
  in the session scratch directory (not in the repo, not published). They carry version **0.1.0**,
  because `Cargo.toml` still says so; they are a rehearsal, to be rebuilt by CI after the version bump.
- `install.sh` against those archives over `file://` (macOS): fresh install OK (checksum verified), upgrade over an older binary replaced it with the RC binary byte for byte, a tampered archive was refused with nothing installed. Not run on Linux (the Alpine image has no curl); the Linux branch is target detection only.
- musl binaries are reproducible (two build directories, `cmp` equal): **yes**, both targets. macOS release builds were not built twice.
- Homebrew: formula template only. On macOS a symlinked install records the stable symlink path in
  assistant configs. **On Linux it records the versioned target** (`/brew/Cellar/agentdiet/0.2.0/bin/agentdiet`, checked in Alpine with both musl binaries), because `current_exe` reads `/proc/self/exe`. A Linuxbrew upgrade would leave assistants pointing at a removed binary until `agentdiet init` is re-run (`doctor` reports it). `install.sh` is not affected (it replaces the file in place). Open: fix before publishing a Linux Homebrew formula (prefer the invoked path when it resolves to the same file; a SPEC change).

### 4.7 CI/CD

Written, linted (actionlint 1.7.7 with shellcheck: clean), **never run on GitHub**. Written CI is not
passing CI: the first push will be its first run, and jobs that have never run (Windows above all) should
be expected to need fixes.

### 4.8 Security

- `cargo deny` (advisories, licences, sources, bans) at HEAD: **ok**; warnings only for duplicate
  `foldhash`, `hashbrown`, `syn` versions. Advisory database in a scratch path.
- Secret probe at every entry point: see 4.3 (2). The transparent shim path and config backups are the
  documented exceptions (SECURITY.md).

### 4.9 Performance

- **B1: NOT MET**, recorded in SPEC §7 ("P4 re-measurement"): RC median 2.705 / 2.707 ms against a limit of F + 0.5 ≈ 2.0 ms (F = 1.49–1.50 ms measured in the same runs), about 0.7 ms over; the P3 binary measured 2.699 / 2.680 ms in the same alternating session, so P4 added nothing. The excess is the 10 MB process image; it was not won back, and only a dedicated small shim executable would remove it, which `CLAUDE.md` does not allow. Owner's decision.
- **B3: MET**, 0.027 ms against 0.2 ms.
- B4/B5: unchanged documented misses (not re-measured in P4).

### 4.10 Blockers and decisions left to the owner

**Blockers before publishing a release:**
1. **CI has never run.** Push to a branch and get every job green, Windows included.
2. **Version.** `Cargo.toml` is 0.1.0 and the `v0.1.0` tag already exists (before P0–P3), so the next
   release needs a new version: **0.2.0** is recommended (new commands, new MCP tool, behaviour changes,
   nothing removed). Bump it, move the CHANGELOG's Unreleased section under it, then tag.
3. **Real Windows** has not run anything; until the CI `windows` job passes, keep Windows marked
   "under test" in release notes.

4. **Linux symlinked installs** record the versioned path (4.6): fix before a Linux Homebrew formula,
   or ship install.sh/archives only.

**Owner decisions (not blockers):**
- B1 (see 4.9): accept the miss, or allow a dedicated shim executable.
- Whether a pre-existing `.agentdiet/` that is a symlink, or has a loose mode or foreign owner, should be
  refused or tightened (today it is used as is; SPEC §3 Tier 3 says so).
- Signing release archives (today: checksums only, which prove integrity, not origin).
- Gemini CLI, Cursor, Windsurf and Copilot live checks need those clients.

**Known limitations, documented:** B4/B5 misses (durability is not traded for speed); best-effort secret
detection; the transparent shim path is not scrubbed; Windows has no shims; `folding` tests depend on a
300 ms debug parse budget and failed once on this machine while it was saturated by x86_64 emulation
(passed 3/3 alone): a flake risk on slow CI runners. Wall-clock tests (hook stdin timeout, launch bounds)
behave the same way under heavy load.

### 4.11 Publishing steps (for the owner; none has been done)

1. `git push` the branch; wait for CI to pass on every job.
2. Bump `version` in `Cargo.toml` (and `Cargo.lock` via `cargo check`) to 0.2.0; date the CHANGELOG
   section; commit; push; CI green again.
3. `git tag -a v0.2.0 -m "AgentDiet 0.2.0"` and `git push origin v0.2.0`: `release.yml` builds every
   target, checks the tag against `Cargo.toml`, and creates a **draft** release with `SHA256SUMS`.
4. Download the draft's archives on a Mac and a Linux machine (and Windows), check them against
   `SHA256SUMS`, run `scripts/install.sh` with `AGENTDIET_VERSION=0.2.0` once the release is visible,
   then press Publish.
5. Optional: fill `packaging/homebrew/agentdiet.rb` from `SHA256SUMS` and add it to a tap.

### 4.12 Session housekeeping

- The real `~/.local/bin/agentdiet` is unchanged (9,939,344 bytes, 2026-10-19). Every AgentDiet run used a
  scratch HOME. `~/.codex/config.toml` shows a modification at 01:11 on 2026-10-21; this session's
  commands only read that file (and ran `codex --help`/`--version`), and Codex was active at the time
  (`~/.codex/agentdiet-orchestration` changed at 01:13), so it is Codex's own write, not this work's.
  `~/.claude.json` is rewritten by Claude Code itself on every session.
- Two writes under the real HOME happened and were undone: a `cargo deny` run on 2026-10-20 created
  `~/.cargo/advisory-dbs` (6.6 MB, removed; later runs used a scratch path), and on 2026-10-21
  `rustup target add x86_64-apple-darwin` installed that target's standard library into `~/.rustup` for
  the macOS x86_64 build (removed with `rustup target remove` right after the build).
