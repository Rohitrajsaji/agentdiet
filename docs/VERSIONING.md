# Versioning and compatibility

AgentDiet follows [Semantic Versioning](https://semver.org/). While the version is **0.x**, a minor
release (0.2 → 0.3) may change contracts listed below, and the CHANGELOG says how; a patch release
(0.2.0 → 0.2.1) never does. From 1.0, contracts change only in a major release.

## Contracts (kept stable within a release line)

| Contract | Where it is specified | Stability rule |
|---|---|---|
| `stats --json`, `status --json`, `doctor --json` | `SPEC.md` §3b | Keys are only added. Renaming or removing one bumps the object's `schema` number (`status`/`doctor`) and is a breaking change |
| Exit statuses: `doctor` 0/1, `scan` 0/1/2, `show` 1 on a missing entry, usage errors 2 | `SPEC.md` §3b, §5 | Breaking to change |
| MCP tools (`diet_exec`, `diet_search`, `diet_inspect`, `diet_reflect`): names, arguments, result shape | `SPEC.md` §3b | Arguments are only added, as optional |
| Breadcrumb and fold-marker text (`Run: agentdiet show '#id'`) and accepted ID forms | `SPEC.md` §3 Tier 3 | Old forms stay accepted forever |
| What `init` writes into assistant configs (entries, the Codex marker block, the rules pointer) | `SPEC.md` §2.2 | Older entries are always recognised, updated and removable by newer versions |
| `state.json`, Vault and memory.db formats | `SPEC.md` §2.3, §3 Tier 3 | A newer version reads what an older one wrote; unknown fields are ignored |
| Command names, flags and aliases | `SPEC.md` §5 | Removed only after a deprecation release; aliases are kept |

## Not contracts

Human-readable text (`status`, `doctor`, `stats` without `--json`, `init` summaries, help, the Control
Center, `top`) is written for people and may change in any release. Scripts should use the `--json`
outputs and exit statuses.

## Minimum supported Rust version

Building from source needs Rust **1.90** or newer (the newest `rust-version` among the dependencies);
CI checks it. Raising it is done in a minor release and noted in the CHANGELOG. Releases are built with
the toolchain pinned in CI (`RUST_TOOLCHAIN`).

## Upgrading

Assistant configs point at the binary's full path. Replacing the binary in place (same path) needs no
other step. If the path changes, run `agentdiet doctor` (it names the missing binary) and then
`agentdiet init --only <assistants>` from the new location. Re-generate shell completions after an
upgrade so new commands complete.
