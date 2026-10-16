//! Link-time policy for the release binary (see CLAUDE.md, "Single binary, zero runtime deps").
//!
//! On Apple targets the `libc` crate links `libiconv` unconditionally, even though AgentDiet never
//! calls an iconv function. `-dead_strip_dylibs` makes the linker drop any dylib none of whose
//! symbols are referenced, so only `libSystem` stays. It lives here rather than in
//! `.cargo/config.toml` because that file does not travel with `cargo install` and `RUSTFLAGS`
//! overrides it. `scripts/check-binary-deps.sh` verifies the result on a built artifact.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // `cfg!(target_os)` here would describe the host running this script, not the target.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-Wl,-dead_strip_dylibs");
    }
}
