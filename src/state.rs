//! The state ledger: `~/.agentdiet/state.json`.
//!
//! Hooks and shims read this on every call and must fail open, so [`load`]
//! never errors — a missing or corrupt file simply yields the defaults.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::Paths;
use crate::fsutil::{self, LockError};

/// How long an explicit user change (`pause`, `resume`, a mode switch) waits for the state lock.
pub const EXPLICIT_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
/// How long engine telemetry waits before giving up and dropping its update, so a hook is never
/// held up by a slow writer.
pub const TELEMETRY_LOCK_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("could not write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("could not serialize state: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("state is busy: another agentdiet process held the lock for {0:?}; try again")]
    Busy(Duration),
    #[error("could not lock the state file: {0}")]
    Lock(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Active,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Light,
    #[default]
    Balanced,
    #[value(alias = "ultra")]
    UltraLean,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Light, Mode::Balanced, Mode::UltraLean];

    /// The name `agentdiet mode` takes and `state.json` stores.
    pub fn slug(self) -> &'static str {
        match self {
            Mode::Light => "light",
            Mode::Balanced => "balanced",
            Mode::UltraLean => "ultra-lean",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Light => "Light",
            Mode::Balanced => "Balanced",
            Mode::UltraLean => "Ultra-Lean",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnSessionStart {
    #[default]
    Prompt,
    AutoResume,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub status: Status,
    pub mode: Mode,
    pub raw_tokens_passed_while_paused: u64,
    pub on_session_start: OnSessionStart,
    /// Short ID (e.g. `4f9a`) of the most recently trimmed output.
    pub last_trimmed_id: Option<String>,
    /// Replacements the engine emitted (telemetry, best effort).
    pub trimmed_outputs: u64,
    /// Estimated tokens those replacements saved (telemetry, best effort).
    pub tokens_saved: u64,
    /// Successful agent-facing Vault reads: `show` and `diet_inspect` (telemetry, best effort).
    pub cas_dereferences: u64,
}

impl State {
    /// `cas_dereferences / trimmed_outputs`, or `None` while nothing has been trimmed.
    pub fn dereference_rate(&self) -> Option<f64> {
        (self.trimmed_outputs > 0)
            .then(|| self.cas_dereferences as f64 / self.trimmed_outputs as f64)
    }
}

/// A dereference rate above this is flagged by `agentdiet top` (SPEC §2.3).
pub const DEREFERENCE_WARN_RATE: f64 = 0.20;

/// One engine telemetry update; see [`record_telemetry`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Telemetry<'a> {
    /// The ID of an output that was just replaced.
    pub last_trimmed_id: Option<&'a str>,
    /// Estimated tokens passed while paused.
    pub paused_tokens: u64,
    /// A replacement was emitted (counts one `trimmed_outputs`).
    pub trimmed: bool,
    /// Estimated tokens that replacement saved.
    pub tokens_saved: u64,
}

/// Whether a state mutation actually changed anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Changed,
    Unchanged,
}

/// While set, [`load`] keeps its warning to itself: a full-screen view (the Control Center, `top`)
/// sets it, because stderr would land on top of the screen. Those views show the problem themselves
/// (the doctor `State` check, [`read`]).
static QUIET: AtomicBool = AtomicBool::new(false);

/// Stop [`load`] (and so every state update) from writing warnings to stderr, for this process.
pub fn quiet_warnings() {
    QUIET.store(true, Ordering::Relaxed);
}

/// The state, falling back to defaults, and the reason when the file exists but is unreadable or
/// corrupt. Never prints.
pub fn read(paths: &Paths) -> (State, Option<String>) {
    let path = paths.state_file();
    match fsutil::read_optional_regular(&path) {
        Ok(None) => (State::default(), None),
        Ok(Some(bytes)) => match serde_json::from_slice(&bytes) {
            Ok(state) => (state, None),
            Err(e) => (
                State::default(),
                Some(format!(
                    "ignoring corrupt {} ({e}); using defaults",
                    crate::ui::tilde(&paths.home, &path)
                )),
            ),
        },
        Err(e) => (
            State::default(),
            Some(format!(
                "cannot read {} ({e}); using defaults",
                crate::ui::tilde(&paths.home, &path)
            )),
        ),
    }
}

/// Load the state, falling back to defaults (with a stderr warning when the file exists but is
/// unreadable or corrupt, unless [`quiet_warnings`] was called).
pub fn load(paths: &Paths) -> State {
    let (state, warning) = read(paths);
    if let Some(warning) = warning
        && !QUIET.load(Ordering::Relaxed)
    {
        crate::ui::warn(&format!("agentdiet: {warning}"));
    }
    state
}

/// Atomically write the state.
pub fn save(paths: &Paths, state: &State) -> Result<(), StateError> {
    let mut json = serde_json::to_string_pretty(state)?;
    json.push('\n');
    let path = paths.state_file();
    let write = || {
        if let Some(parent) = path.parent() {
            fsutil::create_private_dir_all(parent)?;
        }
        fsutil::atomic_write(&path, json.as_bytes())
    };
    write().map_err(|source| StateError::Write {
        path: path.display().to_string(),
        source,
    })
}

/// Load, apply `f`, and save only if `f` changed the state, as one exclusive
/// read-modify-write: the lock is held from the read to the rename, so two
/// processes can never both start from the same old state. Waits at most
/// `timeout`, then fails with [`StateError::Busy`]. The lock is released on
/// every path (it is a guard).
pub fn update_within(
    paths: &Paths,
    timeout: Duration,
    f: impl FnOnce(&mut State),
) -> Result<(State, Change), StateError> {
    let lock_path = paths.state_lock_file();
    let lock_err = |source: io::Error| StateError::Write {
        path: lock_path.display().to_string(),
        source,
    };
    fsutil::create_private_dir_all(&paths.agentdiet_home).map_err(lock_err)?;
    let _guard = fsutil::lock_exclusive(&lock_path, timeout).map_err(|e| match e {
        LockError::Timeout { waited, .. } => StateError::Busy(waited),
        other => StateError::Lock(other.to_string()),
    })?;

    let before = load(paths);
    let mut after = before.clone();
    f(&mut after);
    if after == before && paths.state_file().exists() {
        return Ok((after, Change::Unchanged));
    }
    save(paths, &after)?;
    let change = if after == before {
        Change::Unchanged
    } else {
        Change::Changed
    };
    Ok((after, change))
}

/// [`update_within`] for an explicit user change (waits up to [`EXPLICIT_LOCK_TIMEOUT`]).
pub fn update(paths: &Paths, f: impl FnOnce(&mut State)) -> Result<(State, Change), StateError> {
    update_within(paths, EXPLICIT_LOCK_TIMEOUT, f)
}

/// Engine telemetry: the last trimmed ID, the paused-token counter and the trimming counters, in
/// one locked update. Waits at most [`TELEMETRY_LOCK_TIMEOUT`]; the caller drops the update on any
/// error rather than delaying or failing a hook.
pub fn record_telemetry(paths: &Paths, telemetry: &Telemetry<'_>) -> Result<(), StateError> {
    update_within(paths, TELEMETRY_LOCK_TIMEOUT, |state| {
        if let Some(id) = telemetry.last_trimmed_id {
            state.last_trimmed_id = Some(id.to_string());
        }
        state.raw_tokens_passed_while_paused = state
            .raw_tokens_passed_while_paused
            .saturating_add(telemetry.paused_tokens);
        if telemetry.trimmed {
            state.trimmed_outputs = state.trimmed_outputs.saturating_add(1);
            state.tokens_saved = state.tokens_saved.saturating_add(telemetry.tokens_saved);
        }
    })
    .map(drop)
}

/// Count one successful agent-facing Vault read (`show`, `diet_inspect`). Same lock policy as
/// [`record_telemetry`]; the caller ignores a failure.
pub fn record_dereference(paths: &Paths) -> Result<(), StateError> {
    update_within(paths, TELEMETRY_LOCK_TIMEOUT, |state| {
        state.cas_dereferences = state.cas_dereferences.saturating_add(1);
    })
    .map(drop)
}

pub fn set_status(paths: &Paths, status: Status) -> Result<Change, StateError> {
    update(paths, |s| s.status = status).map(|(_, c)| c)
}

pub fn set_mode(paths: &Paths, mode: Mode) -> Result<Change, StateError> {
    update(paths, |s| s.mode = mode).map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    fn paths(dir: &TempDir) -> Paths {
        Paths::new(
            dir.path().join("home"),
            dir.path().join("config"),
            dir.path().join("project"),
            PathBuf::from("/x/agentdiet"),
        )
    }

    #[test]
    fn spec_json_round_trips() {
        let json = r#"{
  "status": "active",
  "mode": "balanced",
  "raw_tokens_passed_while_paused": 0,
  "on_session_start": "prompt",
  "last_trimmed_id": null,
  "trimmed_outputs": 0,
  "tokens_saved": 0,
  "cas_dereferences": 0
}"#;
        let state: State = serde_json::from_str(json).unwrap();
        assert_eq!(state, State::default());
        let again: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        let expected: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(again, expected);
    }

    #[test]
    fn enum_spellings_match_the_spec() {
        let state = State {
            status: Status::Paused,
            mode: Mode::UltraLean,
            raw_tokens_passed_while_paused: 7,
            on_session_start: OnSessionStart::AutoResume,
            last_trimmed_id: Some("4f9a".into()),
            trimmed_outputs: 3,
            tokens_saved: 900,
            cas_dereferences: 1,
        };
        let v = serde_json::to_value(&state).unwrap();
        assert_eq!(v["status"], "paused");
        assert_eq!(v["mode"], "ultra-lean");
        assert_eq!(v["on_session_start"], "auto_resume");
        assert_eq!(v["last_trimmed_id"], "4f9a");
        assert_eq!(serde_json::from_value::<State>(v).unwrap(), state);
    }

    #[test]
    fn missing_fields_and_unknown_fields_are_tolerated() {
        let state: State = serde_json::from_str(r#"{"status":"paused","future":1}"#).unwrap();
        assert_eq!(state.status, Status::Paused);
        assert_eq!(state.mode, Mode::Balanced);
        assert_eq!(state.trimmed_outputs, 0, "a Slice 4 file has no counters");
    }

    #[test]
    fn missing_or_corrupt_file_loads_defaults() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        assert_eq!(load(&p), State::default());
        fs::create_dir_all(&p.agentdiet_home).unwrap();
        fs::write(p.state_file(), "{not json").unwrap();
        assert_eq!(load(&p), State::default());
    }

    #[test]
    fn pause_and_resume_flip_the_file() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);

        assert_eq!(set_status(&p, Status::Paused).unwrap(), Change::Changed);
        let on_disk: State = serde_json::from_slice(&fs::read(p.state_file()).unwrap()).unwrap();
        assert_eq!(on_disk.status, Status::Paused);
        assert_eq!(set_status(&p, Status::Paused).unwrap(), Change::Unchanged);

        assert_eq!(set_status(&p, Status::Active).unwrap(), Change::Changed);
        assert_eq!(load(&p).status, Status::Active);
    }

    #[test]
    fn writes_leave_no_temp_files() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        set_status(&p, Status::Paused).unwrap();
        set_mode(&p, Mode::Light).unwrap();
        let names: Vec<_> = fs::read_dir(&p.agentdiet_home)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        let mut names = names;
        names.sort();
        assert_eq!(
            names,
            vec!["state.json".to_string(), "state.json.lock".to_string()]
        );
    }

    #[test]
    fn concurrent_updates_never_lose_a_write() {
        let dir = TempDir::new().unwrap();
        let p = std::sync::Arc::new(paths(&dir));
        // Every update is a durable write (fsync of file and directory), so keep the count modest
        // and the wait generous: this test is about lost updates, not about the 2 s policy.
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let p = std::sync::Arc::clone(&p);
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        update_within(&p, Duration::from_secs(60), |s| {
                            s.raw_tokens_passed_while_paused += 1;
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(load(&p).raw_tokens_passed_while_paused, 40);
    }

    /// The scenario the lock exists for: telemetry holding an old copy must not overwrite a user's
    /// pause. Without the lock the second `update` would read before the first one wrote.
    #[test]
    fn telemetry_cannot_overwrite_a_concurrent_pause() {
        let dir = TempDir::new().unwrap();
        let p = std::sync::Arc::new(paths(&dir));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let p2 = std::sync::Arc::clone(&p);
        let telemetry = std::thread::spawn(move || {
            update_within(&p2, Duration::from_secs(5), |s| {
                // We are inside the critical section, holding a snapshot taken before the pause.
                started_tx.send(()).unwrap();
                go_rx.recv().unwrap();
                s.raw_tokens_passed_while_paused += 5;
            })
            .unwrap();
        });
        started_rx.recv().unwrap();
        let p3 = std::sync::Arc::clone(&p);
        let pauser = std::thread::spawn(move || set_status(&p3, Status::Paused).unwrap());
        std::thread::sleep(Duration::from_millis(100));
        go_tx.send(()).unwrap();
        telemetry.join().unwrap();
        pauser.join().unwrap();
        let end = load(&p);
        assert_eq!(end.status, Status::Paused, "the pause survived");
        assert_eq!(
            end.raw_tokens_passed_while_paused, 5,
            "and so did the counter"
        );
    }

    #[test]
    fn a_busy_lock_fails_explicit_changes_and_drops_telemetry_quickly() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        fs::create_dir_all(&p.agentdiet_home).unwrap();
        let held = fsutil::lock_exclusive(&p.state_lock_file(), Duration::from_secs(1)).unwrap();

        let started = std::time::Instant::now();
        let telemetry = Telemetry {
            last_trimmed_id: Some("4f9a1b2c"),
            paused_tokens: 10,
            ..Telemetry::default()
        };
        let err = record_telemetry(&p, &telemetry).unwrap_err();
        assert!(matches!(err, StateError::Busy(_)), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "telemetry waits ~100ms"
        );

        let err = update_within(&p, Duration::from_millis(50), |s| s.status = Status::Paused)
            .unwrap_err();
        assert!(err.to_string().contains("state is busy"), "{err}");
        assert!(!p.state_file().exists(), "nothing was written while busy");

        drop(held);
        record_telemetry(&p, &telemetry).unwrap();
        let s = load(&p);
        assert_eq!(s.last_trimmed_id.as_deref(), Some("4f9a1b2c"));
        assert_eq!(s.raw_tokens_passed_while_paused, 10);
    }

    #[test]
    fn trimming_and_dereference_counters_accumulate() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        assert_eq!(load(&p).dereference_rate(), None);
        for saved in [100, 250] {
            let telemetry = Telemetry {
                last_trimmed_id: Some("4f9a1b2c"),
                trimmed: true,
                tokens_saved: saved,
                ..Telemetry::default()
            };
            record_telemetry(&p, &telemetry).unwrap();
        }
        record_dereference(&p).unwrap();
        let s = load(&p);
        assert_eq!(
            (s.trimmed_outputs, s.tokens_saved, s.cas_dereferences),
            (2, 350, 1)
        );
        assert_eq!(s.dereference_rate(), Some(0.5));
        assert_eq!(s.raw_tokens_passed_while_paused, 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_at_state_json_is_unreadable_not_a_hang() {
        use std::os::unix::ffi::OsStrExt;
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        fs::create_dir_all(&p.agentdiet_home).unwrap();
        let c = std::ffi::CString::new(p.state_file().as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(load(&p));
        });
        let loaded = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("load blocked");
        assert_eq!(loaded, State::default());
    }

    #[cfg(unix)]
    #[test]
    fn the_lock_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        set_status(&p, Status::Paused).unwrap();
        let mode = fs::metadata(p.state_lock_file())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
