# Vendored TOON conformance fixtures

`encode/*.json` are copied unmodified from https://github.com/toon-format/spec at commit
`d09c3d33e37389345a74bd96981eb96dd9be6c5e` (release tag `v4.4.0`), path `tests/fixtures/encode`
(git tree `f8ef3e83407327e43e6dc9ec9e0cbb961ee3cb47`). They are MIT licensed, Copyright (c) 2025-PRESENT
Johann Schopplich; the licence text is in `LICENSE` next to this file.

Only the `encode` fixtures are vendored: AgentDiet writes TOON and never reads it, so the `decode`
fixtures have nothing to test. `supported.txt` is AgentDiet's own record of which fixture cases the
encoder must reproduce byte for byte (see `tests/toon_fixtures.rs`).
