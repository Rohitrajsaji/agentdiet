//! `scripts/check-binary-deps.sh` must fail closed: exit 0 only for a verified, supported artifact.
//!
//! The script is run with `sh`. Inspectors (`otool`, `readelf`) and `uname` are replaced by fake
//! executables placed first on `PATH`, so both platform branches and every inspector failure mode
//! are exercised on any host. Those tests prove the script's *logic*; they say nothing about what a
//! real `readelf` prints (that is only covered on a Linux host by CI). The one test with a real
//! inspector runs `otool -L` on this crate's own debug binary and therefore needs macOS.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/check-binary-deps.sh");
const LIBSYSTEM: &str =
    "\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1356.0.0)";

struct Rig {
    dir: TempDir,
}

struct Outcome {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Rig {
    fn new() -> Self {
        let rig = Self {
            dir: TempDir::new().unwrap(),
        };
        fs::create_dir(rig.fakes()).unwrap();
        rig
    }

    fn fakes(&self) -> PathBuf {
        self.dir.path().join("fakes")
    }

    fn tool(&self, name: &str, body: &str) {
        let path = self.fakes().join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn run(&self, args: &[&Path]) -> Outcome {
        let path = format!(
            "{}:{}",
            self.fakes().display(),
            std::env::var("PATH").unwrap()
        );
        let out = Command::new("sh")
            .arg(SCRIPT)
            .args(args)
            .env("PATH", path)
            .output()
            .unwrap();
        Outcome {
            code: out.status.code().expect("script killed by a signal"),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Pretend to be macOS with an `otool` that prints `body` (after the `<file>:` header) and
    /// exits `status`.
    fn macos(&self, body: &str, status: i32) {
        self.tool("uname", "echo Darwin");
        self.tool(
            "otool",
            &format!("printf '%s:\\n' \"$2\"\ncat <<'EOF'\n{body}\nEOF\nexit {status}"),
        );
    }

    fn linux(&self, headers: &str, dynamic: &str) {
        self.tool("uname", "echo Linux");
        self.tool(
            "readelf",
            &format!(
                "case \"$1\" in\n-h) cat <<'EOF'\n{HEADER}\nEOF\n;;\n-l) cat <<'EOF'\n{headers}\nEOF\n;;\n-d) cat <<'EOF'\n{dynamic}\nEOF\n;;\nesac",
                HEADER = "ELF Header:\n  Magic:   7f 45 4c 46"
            ),
        );
    }
}

const MACHO: [u8; 8] = [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0];
const ELF: [u8; 8] = [0x7f, b'E', b'L', b'F', 2, 1, 1, 0];

#[test]
fn text_file_is_rejected_with_the_real_inspector() {
    // The reproduction from the review: the real `otool` exits 0 on this file.
    let rig = Rig::new();
    let file = rig.file("not-exec", b"not an executable");
    let out = rig.run(&[&file]);
    assert_eq!(out.code, 2, "{}{}", out.stdout, out.stderr);
    assert!(!out.stdout.contains("OK:"));
}

#[test]
fn empty_and_truncated_files_are_rejected() {
    let rig = Rig::new();
    for (name, bytes) in [
        ("empty", &b""[..]),
        ("two", &[0xcf, 0xfa][..]),
        ("three", &[0x7f, b'E', b'L'][..]),
    ] {
        let file = rig.file(name, bytes);
        let out = rig.run(&[&file]);
        assert_eq!(out.code, 2, "{name}: {}{}", out.stdout, out.stderr);
        assert!(out.stderr.contains("truncated"), "{name}: {}", out.stderr);
    }
}

#[test]
fn missing_and_directory_inputs_are_rejected() {
    let rig = Rig::new();
    assert_eq!(rig.run(&[&rig.dir.path().join("nope")]).code, 2);
    assert_eq!(rig.run(&[rig.dir.path()]).code, 2);
    assert_eq!(rig.run(&[Path::new("--bogus")]).code, 2);
}

#[test]
fn macos_accepts_a_libsystem_only_binary() {
    let rig = Rig::new();
    rig.macos(LIBSYSTEM, 0);
    let file = rig.file("ok", &MACHO);
    let out = rig.run(&[&file]);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("OK:"));
}

#[test]
fn macos_forbidden_dylib_is_exit_1() {
    let rig = Rig::new();
    let iconv = "\t/usr/lib/libiconv.2.dylib (compatibility version 7.0.0, current version 7.0.0)";
    rig.macos(&format!("{LIBSYSTEM}\n{iconv}"), 0);
    let file = rig.file("bad", &MACHO);
    let out = rig.run(&[&file]);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(out.stderr.contains("libiconv"), "{}", out.stderr);
    assert!(!out.stdout.contains("OK:"));
}

#[test]
fn macos_otool_failure_is_exit_2_not_1() {
    let rig = Rig::new();
    rig.macos("otool: internal error", 1);
    let file = rig.file("f", &MACHO);
    let out = rig.run(&[&file]);
    assert_eq!(out.code, 2, "{}{}", out.stdout, out.stderr);
}

#[test]
fn macos_otool_exit_0_with_not_an_object_file_is_exit_2() {
    // What Apple's otool does for junk: success status, complaint on stdout.
    let rig = Rig::new();
    rig.tool("uname", "echo Darwin");
    rig.tool("otool", "echo \"$2: is not an object file\"");
    let file = rig.file("f", &MACHO);
    let out = rig.run(&[&file]);
    assert_eq!(out.code, 2, "{}{}", out.stdout, out.stderr);
}

#[test]
fn macos_missing_libsystem_or_unparseable_output_is_exit_2() {
    let rig = Rig::new();
    let file = rig.file("f", &MACHO);
    // A parseable list that lacks libSystem is not a normal executable.
    rig.macos(
        "\t/usr/lib/libiconv.2.dylib (compatibility version 7.0.0, current version 7.0.0)",
        0,
    );
    assert_eq!(rig.run(&[&file]).code, 2);
    // Extra, unrecognised lines (including a warning) are never ignored.
    rig.macos(&format!("{LIBSYSTEM}\nwarning: something odd"), 0);
    assert_eq!(rig.run(&[&file]).code, 2);
    // No output at all.
    rig.tool("otool", "exit 0");
    assert_eq!(rig.run(&[&file]).code, 2);
}

#[test]
fn macos_rejects_wrong_format_and_missing_inspector() {
    let rig = Rig::new();
    rig.macos(LIBSYSTEM, 0);
    let elf = rig.file("elf", &ELF);
    assert_eq!(rig.run(&[&elf]).code, 2, "ELF on macOS");

    // Without otool anywhere on PATH the script cannot check anything.
    let rig = Rig::new();
    rig.tool("uname", "echo Darwin");
    let file = rig.file("f", &MACHO);
    let path = rig.fakes();
    let out = Command::new("/bin/sh")
        .arg(SCRIPT)
        .arg(&file)
        .env("PATH", path)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn linux_static_binary_passes() {
    let rig = Rig::new();
    rig.linux(
        "Program Headers:\n  Type   Offset\n  LOAD   0x0",
        "There is no dynamic section in this file.",
    );
    let file = rig.file("ok", &ELF);
    let out = rig.run(&[&file]);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
}

#[test]
fn linux_dynamic_binary_is_exit_1() {
    let rig = Rig::new();
    rig.linux(
        "Program Headers:\n  INTERP 0x0\n      [Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]",
        "Dynamic section at offset 0x1 contains 1 entries:\n 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]",
    );
    let file = rig.file("dyn", &ELF);
    let out = rig.run(&[&file]);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(out.stderr.contains("libc.so.6"), "{}", out.stderr);
}

#[test]
fn linux_inspector_failures_and_wrong_format_are_exit_2() {
    let rig = Rig::new();
    let file = rig.file("f", &ELF);

    // readelf fails.
    rig.tool("uname", "echo Linux");
    rig.tool(
        "readelf",
        "echo 'readelf: Error: not an ELF file' >&2; exit 1",
    );
    assert_eq!(rig.run(&[&file]).code, 2);

    // readelf succeeds but describes nothing useful (no program headers).
    rig.linux(
        "There are no program headers in this file.",
        "There is no dynamic section in this file.",
    );
    assert_eq!(rig.run(&[&file]).code, 2);

    // readelf -d prints something we do not recognise.
    rig.linux("Program Headers:\n  LOAD 0x0", "garbage");
    assert_eq!(rig.run(&[&file]).code, 2);

    // Mach-O on Linux.
    rig.linux(
        "Program Headers:\n  LOAD 0x0",
        "There is no dynamic section in this file.",
    );
    let macho = rig.file("macho", &MACHO);
    assert_eq!(rig.run(&[&macho]).code, 2);
}

#[test]
fn unsupported_platform_is_exit_2() {
    let rig = Rig::new();
    rig.tool("uname", "echo FreeBSD");
    let file = rig.file("f", &ELF);
    assert_eq!(rig.run(&[&file]).code, 2);
}

#[test]
fn require_sqlite_distinguishes_absent_from_unreadable() {
    let rig = Rig::new();
    rig.macos(LIBSYSTEM, 0);
    let file = rig.file("f", &MACHO);
    let out = rig.run(&[Path::new("--require-sqlite"), &file]);
    assert_eq!(
        out.code, 1,
        "no fts5 string present: {}{}",
        out.stdout, out.stderr
    );

    let mut with_fts = MACHO.to_vec();
    with_fts.extend_from_slice(b"\nSQLITE FTS5 module\n");
    let file = rig.file("g", &with_fts);
    let out = rig.run(&[Path::new("--require-sqlite"), &file]);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);

    // grep itself failing is a failed check, not "no match".
    rig.tool(
        "grep",
        "case \"$*\" in *-a*) exit 2 ;; esac\nexec /usr/bin/grep \"$@\"",
    );
    let out = rig.run(&[&file]);
    assert_eq!(out.code, 2, "{}{}", out.stdout, out.stderr);
}

/// Real `otool` on a real artifact: the debug binary this test run was built from.
#[cfg(target_os = "macos")]
#[test]
fn macos_real_binary_with_the_real_inspector_passes() {
    let rig = Rig::new();
    let out = rig.run(&[Path::new(env!("CARGO_BIN_EXE_agentdiet"))]);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("/usr/lib/libSystem.B.dylib"));
    assert!(!out.stdout.contains("libiconv"));
}
