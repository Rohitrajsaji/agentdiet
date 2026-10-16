//! The state ledger: `~/.agentdiet/state.json`.
//!
//! Hooks and shims read this on every call and must fail open, so [`load`]
//! never errors — a missing or corrupt file simply yields the defaults.

use std::io;

use serde::{Deserialize, Serialize};

use crate::{Paths, fsutil};

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Active,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Light,
    #[default]
    Balanced,
    UltraLean,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Light, Mode::Balanced, Mode::UltraLean];

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
}

/// Whether a state mutation actually changed anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Changed,
    Unchanged,
}

/// Load the state, falling back to defaults (with a stderr warning when the
/// file exists but is unreadable or corrupt).
pub fn load(paths: &Paths) -> State {
    let path = paths.state_file();
    match fsutil::read_optional(&path) {
        Ok(None) => State::default(),
        Ok(Some(bytes)) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            eprintln!(
                "agentdiet: ignoring corrupt {} ({e}); using defaults",
                path.display()
            );
            State::default()
        }),
        Err(e) => {
            eprintln!("agentdiet: cannot read {} ({e}); using defaults", path.display());
            State::default()
        }
    }
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

/// Load, apply `f`, and save only if `f` changed the state.
pub fn update(paths: &Paths, f: impl FnOnce(&mut State)) -> Result<(State, Change), StateError> {
    let before = load(paths);
    let mut after = before.clone();
    f(&mut after);
    if after == before && paths.state_file().exists() {
        return Ok((after, Change::Unchanged));
    }
    save(paths, &after)?;
    let change = if after == before { Change::Unchanged } else { Change::Changed };
    Ok((after, change))
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
  "last_trimmed_id": null
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
        assert_eq!(names, vec!["state.json".to_string()]);
    }
}
