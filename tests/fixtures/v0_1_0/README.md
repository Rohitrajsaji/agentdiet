Data written by the `v0.1.0` release binary (tag `v0.1.0`, `e9daa72`), unmodified: `diet_exec` of a
`cargo test` printing 400 passing tests, then `agentdiet pause`. `tests/compat.rs` checks that the
current build reads all of it (SPEC: "a newer version reads what an older one wrote",
docs/VERSIONING.md). Regenerate only from that tag, never with a newer build.
