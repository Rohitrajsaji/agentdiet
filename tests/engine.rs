//! The Diet Engine end to end (library API only: nothing in the CLI, shims or hooks calls it yet).
//! Everything runs in a `TempDir` home; the recovery tests execute the literal command from the
//! breadcrumb with the built `agentdiet` binary.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use agentdiet::diet::{Disposition, Engine, ExitStatus, Input, Settings, Source, Transform};
use agentdiet::scrubber::scrub;
use agentdiet::state::Mode;
use common::Env;

const AGENTDIET: &str = env!("CARGO_BIN_EXE_agentdiet");

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/compress/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn aws_key() -> String {
    ["AK", "IA"].concat() + &"Q7".repeat(8)
}

fn cargo_test() -> Source {
    Source::Shell {
        argv: vec!["cargo".into(), "test".into()],
        exit: ExitStatus::Code(101),
    }
}

/// A directory holding an `agentdiet` symlink, to put on `PATH`.
fn bin_dir(env: &Env) -> PathBuf {
    let dir = env.dir.path().join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    let link = dir.join("agentdiet");
    if !link.exists() {
        std::os::unix::fs::symlink(AGENTDIET, &link).unwrap();
    }
    dir
}

/// Run `command` in `shell -c`, as the model would paste it, with HOME in the scratch dir.
fn in_shell(env: &Env, shell: &str, command: &str) -> Option<std::process::Output> {
    which(shell)?;
    let path = format!("{}:/usr/bin:/bin", bin_dir(env).display());
    Some(
        Command::new(shell)
            .arg("-c")
            .arg(command)
            .env_clear()
            .env("HOME", &env.paths.home)
            .env("XDG_CONFIG_HOME", &env.paths.config_dir)
            .env("PATH", path)
            .current_dir(&env.paths.project_root)
            .stdin(Stdio::null())
            .output()
            .unwrap(),
    )
}

fn which(name: &str) -> Option<PathBuf> {
    ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"]
        .iter()
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
}

#[test]
fn a_replacement_can_be_recovered_exactly_by_running_its_literal_run_command() {
    let env = Env::empty();
    let mut raw = fixture("cargo_test_failures.txt");
    raw.extend_from_slice(format!("AWS_ACCESS_KEY_ID in use: {}\n", aws_key()).as_bytes());
    let src = cargo_test();
    let out = Engine::new(&env.paths).run(
        &Input {
            bytes: &raw,
            source: &src,
        },
        &Settings::new(Mode::Balanced),
    );
    assert_eq!(
        out.disposition,
        Disposition::Replaced(Transform::Compressed)
    );
    assert!(out.note.is_none(), "{:?}", out.note);
    let text = String::from_utf8(out.output.clone()).unwrap();
    assert!(!text.contains(&aws_key()));
    assert!(out.output.len() < raw.len());

    // The breadcrumb is the last line, with the quoted ID.
    let crumb = text.lines().last().unwrap();
    let id = out.id.clone().unwrap();
    assert!(crumb.starts_with("🥗 Diet: "), "{crumb}");
    assert!(crumb.contains("exit 101"), "{crumb}");
    assert!(
        crumb.contains("1 secret redacted (not recoverable)"),
        "{crumb}"
    );
    assert!(crumb.len() <= "🥗 Diet:  Run: agentdiet show '#'".len() + 160 + id.len());
    let run = crumb.split_once(" Run: ").unwrap().1;
    assert_eq!(run, format!("agentdiet show '#{id}'"));

    // Executing exactly that text, in two shells, returns the scrubbed original byte for byte.
    let expected = scrub(&raw).into_bytes();
    let mut ran = 0;
    for shell in ["sh", "bash", "zsh"] {
        let Some(shown) = in_shell(&env, shell, run) else {
            continue;
        };
        ran += 1;
        assert!(
            shown.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&shown.stderr)
        );
        assert_eq!(shown.stdout, expected, "{shell}");
        assert!(!String::from_utf8_lossy(&shown.stdout).contains(&aws_key()));
    }
    assert!(ran >= 2, "need at least sh and one more shell");

    // The quotes matter: unquoted, the shell treats `#id` as a comment and `show` gets no ID.
    let unquoted = in_shell(&env, "sh", &format!("agentdiet show #{id}")).unwrap();
    assert!(!unquoted.status.success());

    // Slices of the stored copy work too.
    let slice = in_shell(&env, "sh", &format!("agentdiet show '#{id}' --lines 1..3")).unwrap();
    let first_three: Vec<u8> = expected
        .split_inclusive(|b| *b == b'\n')
        .take(3)
        .flatten()
        .copied()
        .collect();
    assert_eq!(slice.stdout, first_three);

    // Bookkeeping landed where `Inspect last trimmed output` looks.
    let state = agentdiet::state::load(&env.paths);
    assert_eq!(state.last_trimmed_id.as_deref(), Some(id.as_str()));
}
