//! The Diet Engine end to end through its library API (the hook and shim entry points are covered
//! by `tests/hooks.rs` and `tests/shim_agent.rs`).
//! Everything runs in a `TempDir` home; the recovery tests execute the literal command from the
//! breadcrumb with the built `agentdiet` binary.
//!
//! Unix only: recovery runs the breadcrumb through `sh -c` (and the rest shares its helpers); the
//! engine's platform-independent rules are covered by its unit tests on every platform.
#![cfg(unix)]

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
            .env("USERPROFILE", &env.paths.home)
            .env("XDG_CONFIG_HOME", &env.paths.config_dir)
            .env("APPDATA", &env.paths.config_dir)
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

fn issues_json(n: usize) -> String {
    let rows: Vec<serde_json::Value> = (0..n)
        .map(|i| {
            serde_json::json!({
                "number": 1000 + i,
                "title": format!("Fix flaky test number {i}"),
                "state": if i % 3 == 0 { "open" } else { "closed" },
                "author": "octocat",
                "comments": i % 7,
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({ "total": n, "items": rows })).unwrap()
}

#[test]
fn model_facing_json_becomes_toon_with_the_original_recoverable() {
    let env = Env::empty();
    let raw = issues_json(120);
    let src = Source::ModelJson;
    let out = Engine::new(&env.paths).run(
        &Input {
            bytes: raw.as_bytes(),
            source: &src,
        },
        &Settings::new(Mode::Balanced),
    );
    assert_eq!(out.disposition, Disposition::Replaced(Transform::Toon));
    let text = String::from_utf8(out.output.clone()).unwrap();
    // TOON document, then a separate breadcrumb line outside it; the document has no trailing newline.
    let (doc, crumb) = text.trim_end_matches('\n').rsplit_once('\n').unwrap();
    assert!(doc.starts_with("total: 120\nitems[120]{number,title,state,author,comments}:\n  1000,Fix flaky test number 0,open,octocat,0\n"), "{doc}");
    assert!(
        crumb.starts_with("🥗 Diet: JSON converted to TOON"),
        "{crumb}"
    );
    assert!(text.ends_with('\n') && !doc.ends_with('\n'));
    assert!(
        out.output.len() * 100 <= raw.len() * 80,
        "{} vs {}",
        out.output.len(),
        raw.len()
    );

    // The stored original is the JSON, byte for byte, and comes back through the real command.
    let id = out.id.clone().unwrap();
    let shown = in_shell(&env, "sh", &format!("agentdiet show '#{id}'")).unwrap();
    assert_eq!(shown.stdout, raw.as_bytes());
}

#[test]
fn shell_json_is_never_converted_not_even_for_gh() {
    let env = Env::empty();
    let raw = issues_json(120);
    for argv in [
        vec!["gh", "issue", "list", "--json", "number,title"],
        vec!["gh", "api", "repos/o/r/issues"],
        vec!["curl", "-s", "https://api.example.com/issues"],
        vec!["cat", "issues.json"],
    ] {
        let src = Source::Shell {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            exit: ExitStatus::Code(0),
        };
        let out = Engine::new(&env.paths).run(
            &Input {
                bytes: raw.as_bytes(),
                source: &src,
            },
            &Settings::new(Mode::UltraLean),
        );
        assert_eq!(out.output, raw.as_bytes(), "{argv:?}");
        assert_eq!(out.disposition, Disposition::Unchanged, "{argv:?}");
    }
    assert!(!env.paths.cas_dir().exists());
}

#[test]
fn json_that_is_not_encodable_or_does_not_shrink_enough_is_left_alone() {
    let env = Env::empty();
    let src = Source::ModelJson;
    let engine = Engine::new(&env.paths);
    // Non-canonical number: the whole document declines.
    let with_float = issues_json(120).replace("\"comments\": 1\n", "\"comments\": 1.50\n");
    // Compact JSON of a tiny document: below the token minimum.
    let tiny = r#"{"a":1}"#.to_string();
    // Needs a list form.
    let list_form = serde_json::to_string_pretty(&serde_json::json!({
        "rows": (0..150).map(|i| serde_json::json!([i, i + 1])).collect::<Vec<_>>()
    }))
    .unwrap();
    assert!(with_float.contains("1.50"), "the fixture edit must apply");
    for raw in [with_float, tiny, list_form, "{not json".repeat(200)] {
        let out = engine.run(
            &Input {
                bytes: raw.as_bytes(),
                source: &src,
            },
            &Settings::new(Mode::Balanced),
        );
        assert_eq!(out.output, raw.as_bytes());
        assert_eq!(out.disposition, Disposition::Unchanged);
    }
}

#[test]
fn secrets_inside_json_are_redacted_before_conversion_and_before_storage() {
    let env = Env::empty();
    let raw = issues_json(100).replace(
        "\"author\": \"octocat\"",
        &format!("\"author\": \"octocat\", \"note\": \"id={}\"", aws_key()),
    );
    let src = Source::ModelJson;
    let out = Engine::new(&env.paths).run(
        &Input {
            bytes: raw.as_bytes(),
            source: &src,
        },
        &Settings::new(Mode::Balanced),
    );
    assert!(out.redactions > 0);
    let text = String::from_utf8(out.output.clone()).unwrap();
    assert!(!text.contains(&aws_key()));
    assert!(text.contains("[REDACTED:aws-access-key-id]"));
    let id = out.id.clone().unwrap();
    let shown = in_shell(&env, "sh", &format!("agentdiet show '#{id}'")).unwrap();
    assert!(!String::from_utf8_lossy(&shown.stdout).contains(&aws_key()));
}
