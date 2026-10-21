# Security policy

## Reporting a vulnerability

Please report security problems **privately** through GitHub: open the repository's **Security** tab
and choose **Report a vulnerability** (GitHub private vulnerability reporting). Do not open a public
issue for a vulnerability. Include the AgentDiet version (`agentdiet --version`), your platform, and the
smallest steps that reproduce it. You can expect an acknowledgement within a week.

## Supported versions

Only the latest release receives security fixes while AgentDiet is in 0.x.

## What AgentDiet protects, and what it does not

AgentDiet runs locally. It makes no network connections, has no telemetry, and never calls an AI model.

- **Secret scrubbing is best effort.** Pattern-based detection (rules ported from Secretlint, with
  attribution) removes secrets from what the assistant sees and from everything AgentDiet stores. A
  clean result is not a guarantee. Not scrubbed: output on the shims' untouched path (a person running a
  command in a terminal) and binary output. Redacted secrets are never stored, so they cannot be
  recovered.
- **Stored data.** The Vault (`~/.agentdiet/cas/`), project memory (`.agentdiet/memory.db`) and state
  hold scrubbed text only. Directories are created `0700` and files `0600` from the first instant, never
  widened afterwards. Windows has no such modes: there the files live in the user profile, whose access
  control list is the protection (`SPEC.md` §10). Vault entries expire 7 days after their last use; `agentdiet eject --purge`
  deletes all of it.
- **Config backups are the exception.** Before changing an assistant's config, `init` keeps a
  byte-exact copy under `~/.agentdiet/backups/` so `eject` can restore it exactly. A backup is **not**
  scrubbed: it may contain secrets that were already in that config. It is protected by `0700`/`0600`.
- **Config edits** are idempotent, validated (a file that cannot be parsed is skipped and never
  rewritten), atomic, and keep the original file's permissions. `eject` removes only what it can prove
  AgentDiet wrote.
- **`agentdiet scan`** is a static, pattern-based check of skills, rules and MCP configs; a clean report
  is not proof of safety.
- **Hooks and shims fail open**: on an internal error the assistant gets the (scrubbed, if scrubbing
  finished) original output, or the real program runs. A bug in AgentDiet should cost tokens, never
  break your assistant or your shell.

## Supply chain

One static binary; dependencies come from crates.io only and are checked in CI with `cargo deny`
(known vulnerabilities, licences, sources). Release archives are built by CI from a tag with
`--locked`, are reproducible, and come with SHA-256 checksums (`SHA256SUMS`); `scripts/install.sh`
verifies the checksum before installing. The checksums come from the same release page as the archives,
so they prove the download is intact, not who built it: the archives are not signed yet.
