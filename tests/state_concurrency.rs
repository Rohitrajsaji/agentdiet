//! `state.json` under real process-level concurrency: telemetry writers and a user's pause racing,
//! in separate processes (re-executions of this test binary, as in `vault.rs`), so the advisory
//! file lock is what is being tested and not thread scheduling.

mod common;

use std::process::{Command, Stdio};
use std::time::Duration;

use agentdiet::state::{self, Status};
use common::Env;

const TELEMETRY_CHILDREN: usize = 4;
const INCREMENTS: u64 = 12;

fn paths_from_env() -> Option<agentdiet::Paths> {
    let home = std::env::var_os("AD_STATE_HOME")?;
    Some(agentdiet::Paths::new(
        home.clone().into(),
        std::path::PathBuf::from(&home).join("config"),
        std::path::PathBuf::from(&home).join("project"),
        "/x/agentdiet".into(),
    ))
}

/// Child: add to the paused-token counter, one locked read-modify-write at a time.
#[test]
#[ignore = "child-process helper; run by pause_survives_concurrent_telemetry_from_other_processes"]
fn state_child_telemetry() {
    let Some(paths) = paths_from_env() else {
        return;
    };
    for _ in 0..INCREMENTS {
        // A generous wait: this test is about lost updates, not about the 100 ms drop policy.
        state::update_within(&paths, Duration::from_secs(60), |s| {
            s.raw_tokens_passed_while_paused += 1;
            s.last_trimmed_id = Some("deadbeef".into());
        })
        .unwrap();
    }
}

/// Child: a user pressing pause (and later resume + pause again) in the middle of the storm.
#[test]
#[ignore = "child-process helper; run by pause_survives_concurrent_telemetry_from_other_processes"]
fn state_child_pauser() {
    let Some(paths) = paths_from_env() else {
        return;
    };
    for status in [Status::Paused, Status::Active, Status::Paused] {
        state::update_within(&paths, Duration::from_secs(60), |s| s.status = status).unwrap();
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn child(name: &str, home: &std::path::Path) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", name, "--nocapture"])
        .env("AD_STATE_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

#[test]
fn pause_survives_concurrent_telemetry_from_other_processes() {
    let env = Env::empty();
    let home = env.paths.home.clone();
    let mut kids: Vec<_> = (0..TELEMETRY_CHILDREN)
        .map(|_| child("state_child_telemetry", &home))
        .collect();
    kids.push(child("state_child_pauser", &home));
    for kid in kids {
        let out = kid.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let end = state::load(&env.paths);
    assert_eq!(
        end.status,
        Status::Paused,
        "the user's final pause was not overwritten"
    );
    assert_eq!(
        end.raw_tokens_passed_while_paused,
        TELEMETRY_CHILDREN as u64 * INCREMENTS,
        "no telemetry increment was lost"
    );
    assert_eq!(end.last_trimmed_id.as_deref(), Some("deadbeef"));
}

#[test]
fn an_explicit_change_waits_for_a_slow_telemetry_writer_and_telemetry_does_not_wait_for_it() {
    let env = Env::empty();
    // Hold the lock from another process-like owner (a second open file description).
    std::fs::create_dir_all(&env.paths.agentdiet_home).unwrap();
    let held =
        agentdiet::fsutil::lock_exclusive(&env.paths.state_lock_file(), Duration::from_secs(1))
            .unwrap();
    let started = std::time::Instant::now();
    assert!(
        state::record_telemetry(
            &env.paths,
            &state::Telemetry {
                paused_tokens: 1,
                ..Default::default()
            }
        )
        .is_err(),
        "telemetry is dropped"
    );
    assert!(started.elapsed() < Duration::from_secs(1));

    let paths = env.paths.clone();
    let pauser = std::thread::spawn(move || state::set_status(&paths, Status::Paused));
    std::thread::sleep(Duration::from_millis(300));
    drop(held); // within the explicit change's 2 s patience
    assert!(
        pauser.join().unwrap().is_ok(),
        "the explicit change waited and then succeeded"
    );
    assert_eq!(state::load(&env.paths).status, Status::Paused);
}
