//! Per-session sidecars: `~/.agentdiet/sessions/<hash>.json` (SPEC §2.4).
//!
//! A sidecar holds the escape-hatch flag for the current turn and the signatures that detect a
//! command run twice in a row. It is scratch state: everything in it is a boolean or a sha256, never
//! command text (P1), it is written without `fsync`, and every failure reads as "no bypass", so a
//! missing, corrupt or busy sidecar can only ever make AgentDiet trim, never break a caller.

use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::Paths;
use crate::fsutil::{self, LockError};
use crate::scrubber::{Scrubbed, scrub};

/// How long a sidecar update waits for `sessions/.lock` before giving up (fail open).
pub const LOCK_TIMEOUT: Duration = Duration::from_millis(100);
/// Sidecars untouched for longer than this are deleted by SessionStart.
pub const STALE_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("could not write the session sidecar: {0}")]
    Io(#[from] io::Error),
    #[error("could not serialize the session sidecar: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("could not lock the session sidecars: {0}")]
    Lock(#[from] LockError),
}

/// Which sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// An agent session id (a hook payload's `session_id`, or Claude Code's
    /// `CLAUDE_CODE_SESSION_ID` seen by a shim).
    Session(String),
    /// A shim whose nearest AI-agent ancestor has this PID.
    AgentPid(u32),
    /// A shim under an agent recognised only by an environment marker.
    UnknownAgent,
}

impl Key {
    fn label(&self) -> String {
        match self {
            Key::Session(id) => format!("session:{id}"),
            Key::AgentPid(pid) => format!("pid:{pid}"),
            Key::UnknownAgent => "agent:unknown".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sidecar {
    /// The last prompt carried a full-output directive (`@diet:full`) that no tool call has taken
    /// yet: the next PreToolUse binds it to its call (SPEC §2.4).
    pub raw_turn: bool,
    /// sha256 of the `tool_use_id` the directive is bound to: that one call is bypassed.
    pub raw_tool_use: Option<String>,
    /// Signature of the previous tool call seen by PreToolUse.
    pub last_hook_sig: Option<String>,
    /// sha256 of the `tool_use_id` PreToolUse marked as a retry.
    pub retry_tool_use: Option<String>,
    /// Signature of the previous shim run.
    pub last_shim_sig: Option<String>,
}

/// The memory.db session label for an agent session id: its scrubbed text, or `None` if that is
/// longer than [`crate::memory::MAX_SESSION_LEN`].
pub fn label(session_id: &str) -> Option<Scrubbed> {
    let label = scrub(session_id.as_bytes());
    (label.as_bytes().len() <= crate::memory::MAX_SESSION_LEN).then_some(label)
}

/// sha256 over length-prefixed parts, so `["ab", "c"]` and `["a", "bc"]` differ.
pub fn signature(parts: &[&[u8]]) -> String {
    let mut buf = Vec::new();
    for part in parts {
        buf.extend_from_slice(&(part.len() as u64).to_le_bytes());
        buf.extend_from_slice(part);
    }
    fsutil::sha256_hex(&buf)
}

fn path(paths: &Paths, key: &Key) -> PathBuf {
    let hash = fsutil::sha256_hex(key.label().as_bytes());
    paths.sessions_dir().join(format!("{}.json", &hash[..32]))
}

/// The sidecar for `key`, or the default (no bypass) if it is missing or unreadable.
pub fn load(paths: &Paths, key: &Key) -> Sidecar {
    fsutil::read_optional_regular(&path(paths, key))
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Load, apply `f`, and write back if it changed, holding `sessions/.lock` throughout. Returns the
/// sidecar as it was *before* `f`.
pub fn update(
    paths: &Paths,
    key: &Key,
    f: impl FnOnce(&mut Sidecar),
) -> Result<Sidecar, SessionError> {
    let dir = paths.sessions_dir();
    fsutil::create_private_dir_all(&dir)?;
    let _guard = fsutil::lock_exclusive(&dir.join(".lock"), LOCK_TIMEOUT)?;
    let before = load(paths, key);
    let mut after = before.clone();
    f(&mut after);
    if after != before {
        let json = serde_json::to_vec(&after)?;
        fsutil::atomic_write_scratch(&path(paths, key), &json)?;
    }
    Ok(before)
}

/// Forget everything about `key` (SessionStart).
pub fn clear(paths: &Paths, key: &Key) -> io::Result<()> {
    match std::fs::remove_file(path(paths, key)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Delete sidecars last written more than [`STALE_AFTER`] before `now`. Best effort: returns how
/// many were removed.
pub fn gc(paths: &Paths, now: SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(paths.sessions_dir()) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let is_sidecar = name.to_str().is_some_and(|n| {
            n.strip_suffix(".json")
                .is_some_and(|h| h.len() == 32 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        });
        // `DirEntry::metadata` does not follow symlinks.
        let stale = entry.metadata().is_ok_and(|m| {
            m.file_type().is_file()
                && m.modified()
                    .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > STALE_AFTER))
        });
        if is_sidecar && stale && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn paths(dir: &TempDir) -> Paths {
        Paths::new(
            dir.path().join("home"),
            dir.path().join("config"),
            dir.path().join("project"),
            "/x/agentdiet".into(),
        )
    }

    #[test]
    fn signatures_are_length_prefixed() {
        assert_ne!(signature(&[b"ab", b"c"]), signature(&[b"a", b"bc"]));
        assert_eq!(signature(&[b"x"]), signature(&[b"x"]));
        assert_eq!(signature(&[b"x"]).len(), 64);
    }

    #[test]
    fn update_round_trips_and_keys_are_separate() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        let a = Key::Session("abc".into());
        let b = Key::AgentPid(42);
        let before = update(&p, &a, |s| s.raw_turn = true).unwrap();
        assert_eq!(before, Sidecar::default());
        assert!(load(&p, &a).raw_turn);
        assert!(!load(&p, &b).raw_turn);

        let text = std::fs::read_to_string(path(&p, &a)).unwrap();
        assert!(!text.contains("abc"), "the key is hashed: {text}");

        clear(&p, &a).unwrap();
        assert_eq!(load(&p, &a), Sidecar::default());
        clear(&p, &a).unwrap(); // idempotent
    }

    #[test]
    fn a_corrupt_or_unreadable_sidecar_means_no_bypass() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        let key = Key::UnknownAgent;
        fsutil::create_private_dir_all(&p.sessions_dir()).unwrap();
        std::fs::write(path(&p, &key), b"{not json").unwrap();
        assert_eq!(load(&p, &key), Sidecar::default());
        // An update over a corrupt file starts from the default and repairs it.
        update(&p, &key, |s| s.last_shim_sig = Some("s".into())).unwrap();
        assert_eq!(load(&p, &key).last_shim_sig.as_deref(), Some("s"));
    }

    #[cfg(unix)]
    #[test]
    fn a_held_lock_fails_fast_instead_of_waiting() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        fsutil::create_private_dir_all(&p.sessions_dir()).unwrap();
        let _held = fsutil::lock_exclusive(&p.sessions_dir().join(".lock"), LOCK_TIMEOUT).unwrap();
        let start = std::time::Instant::now();
        let err = update(&p, &Key::UnknownAgent, |s| s.raw_turn = true).unwrap_err();
        assert!(matches!(err, SessionError::Lock(_)), "{err}");
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn gc_removes_only_stale_sidecars() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        update(&p, &Key::AgentPid(1), |s| s.raw_turn = true).unwrap();
        let now = SystemTime::now();
        assert_eq!(gc(&p, now), 0);
        assert_eq!(gc(&p, now + STALE_AFTER + Duration::from_secs(60)), 1);
        assert!(!load(&p, &Key::AgentPid(1)).raw_turn);
        assert!(
            p.sessions_dir().join(".lock").exists(),
            "the lock file is never deleted"
        );
    }
}
