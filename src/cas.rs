//! Content-addressed vault: `~/.agentdiet/cas/<full_sha256>.log`.
//!
//! Only scrubbed output can be stored ([`Scrubbed`]); the sha256 is taken over
//! the scrubbed bytes, so identical output dedupes. Entries live for 7 days
//! from their last write and are garbage-collected opportunistically (on `put`
//! and on every read). GC leaves a zero-byte `<sha>.expired` tombstone so an
//! expired ID reports "expired" instead of "not found".
//!
//! Identity: tombstones are part of the ID space. A short ID is unique among
//! live entries *and* tombstones, and resolving a prefix looks at both, so an
//! old breadcrumb whose entry expired can report "expired" or an ambiguity but
//! can never silently return a different entry that later landed on the same
//! first characters.
//!
//! Entries are plain regular files. Symlinks and other non-regular files in the
//! directory are never read, written or followed; see [`fsutil::open_regular`].
//!
//! Concurrency: `put`, `gc`, `resolve` and `read` each take the exclusive vault
//! lock (`.lock` in the vault directory, bounded wait, see [`Cas::lock_timeout`])
//! for their whole duration, and the private helpers assume it is held, so a lock
//! is never taken twice and a GC can never delete content a `put` is refreshing.
//!
//! Integrity: `read` hashes the bytes before returning any of them; a mismatch
//! is [`CasError::Corrupt`] and carries no content.
//!
//! Every operation takes `now` explicitly so expiry is deterministic in tests.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::fsutil;
use crate::scrubber::Scrubbed;

/// How long an entry is kept after its last write.
pub const DEFAULT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Shortest ID accepted as input (`#4f9a`).
pub const MIN_INPUT_ID_LEN: usize = 4;

/// Shortest ID AgentDiet emits (`#4f9a1b2c`): breadcrumbs, fold markers, `state.json` and
/// ambiguity candidates. Longer when a shorter one is not unique among every entry ever issued.
pub const MIN_EMIT_ID_LEN: usize = 8;

/// How long `put`/`gc`/`resolve`/`read` wait for the vault lock before giving up.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(2);

const SHA_LEN: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum CasError {
    #[error("no vault entry matches #{0}")]
    NotFound(String),
    #[error("#{0} has expired (vault entries are kept 7 days)")]
    Expired(String),
    #[error("#{prefix} is ambiguous; candidates: {}", .candidates.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "))]
    Ambiguous {
        prefix: String,
        candidates: Vec<Candidate>,
    },
    #[error("invalid ID {0:?}: expected at least {MIN_INPUT_ID_LEN} hex characters, e.g. #4f9a")]
    InvalidId(String),
    #[error(
        "vault entry #{0} is corrupt: its bytes no longer match its hash, so nothing was returned"
    )]
    Corrupt(String),
    #[error("the vault is busy: another agentdiet process held its lock for {0:?}")]
    Busy(Duration),
    #[error(
        "invalid line range {0:?}: expected <start>..<end>, 1-based and inclusive, start <= end"
    )]
    InvalidRange(String),
    #[error("vault I/O error: {0}")]
    Io(#[from] io::Error),
}

/// One entry an ambiguous prefix could mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Shortest prefix (at least [`MIN_EMIT_ID_LEN`]) that tells it apart from every other entry.
    pub id: String,
    /// Only a tombstone remains; the content is gone.
    pub expired: bool,
}

impl fmt::Display for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.id)?;
        if self.expired {
            f.write_str(" (expired)")?;
        }
        Ok(())
    }
}

/// The result of storing an output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutResult {
    /// Full 64-hex sha256 of the scrubbed output.
    pub sha: String,
    /// Shortest unique prefix, at least [`MIN_EMIT_ID_LEN`] chars (shown as `#4f9a1b2c`).
    pub short_id: String,
}

/// What [`Cas::stats`] counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CasStats {
    /// Live entries.
    pub live: usize,
    /// Tombstones: IDs whose content expired.
    pub expired: usize,
    /// Total size of the live entries.
    pub bytes: u64,
}

/// 1-based, inclusive line range (`--lines 10..40`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    start: usize,
    end: usize,
}

impl LineRange {
    pub fn parse(input: &str) -> Result<Self, CasError> {
        let bad = || CasError::InvalidRange(input.to_string());
        let (a, b) = input.split_once("..").ok_or_else(bad)?;
        let start: usize = a.trim().parse().map_err(|_| bad())?;
        let end: usize = b.trim().parse().map_err(|_| bad())?;
        Self::new(start, end).map_err(|_| bad())
    }

    /// `start..=end`, 1-based. A start of 0 or start > end is [`CasError::InvalidRange`].
    pub fn new(start: usize, end: usize) -> Result<Self, CasError> {
        if start == 0 || start > end {
            return Err(CasError::InvalidRange(format!("{start}..{end}")));
        }
        Ok(Self { start, end })
    }

    /// The selected lines with their line endings kept. An end past EOF
    /// clamps; a start past EOF yields nothing.
    pub fn slice(self, bytes: &[u8]) -> Vec<u8> {
        bytes
            .split_inclusive(|b| *b == b'\n')
            .skip(self.start - 1)
            .take(self.end - self.start + 1)
            .flatten()
            .copied()
            .collect()
    }
}

/// Normalise a user-supplied ID: `#4F9A` / `4f9a` -> `4f9a`. One pair of single quotes around it is
/// dropped as well: `cmd.exe` passes the breadcrumb's `'#4f9a'` on with its quotes (SPEC §10).
pub fn normalize_id(input: &str) -> Result<String, CasError> {
    let trimmed = input.trim();
    let unquoted = trimmed
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(trimmed);
    let id = unquoted
        .strip_prefix('#')
        .unwrap_or(unquoted)
        .to_ascii_lowercase();
    if id.len() < MIN_INPUT_ID_LEN
        || id.len() > SHA_LEN
        || !id.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(CasError::InvalidId(input.to_string()));
    }
    Ok(id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Live,
    Tombstone,
}

#[derive(Debug)]
struct Entry {
    sha: String,
    kind: Kind,
    modified: SystemTime,
}

/// The vault rooted at one directory (normally `Paths::cas_dir()`).
#[derive(Debug, Clone)]
pub struct Cas {
    dir: PathBuf,
    ttl: Duration,
    lock_timeout: Duration,
}

impl Cas {
    pub fn new(dir: PathBuf) -> Self {
        Self::with_ttl(dir, DEFAULT_TTL)
    }

    pub fn with_ttl(dir: PathBuf, ttl: Duration) -> Self {
        Self {
            dir,
            ttl,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        }
    }

    /// Bound the wait for the vault lock (default [`DEFAULT_LOCK_TIMEOUT`]).
    pub fn lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join(".lock")
    }

    /// Take the vault lock, creating the (private) directory first.
    fn lock(&self) -> Result<fsutil::LockGuard, CasError> {
        fsutil::create_private_dir_all(&self.dir)?;
        self.acquire()
    }

    /// Take the vault lock only if the vault exists; a read-only caller has nothing to wait for
    /// (and nothing to create) in a vault that was never written.
    fn lock_existing(&self) -> Result<Option<fsutil::LockGuard>, CasError> {
        if !self.dir.is_dir() {
            return Ok(None);
        }
        self.acquire().map(Some)
    }

    fn acquire(&self) -> Result<fsutil::LockGuard, CasError> {
        fsutil::lock_exclusive(&self.lock_path(), self.lock_timeout).map_err(|e| match e {
            fsutil::LockError::Timeout { waited, .. } => CasError::Busy(waited),
            fsutil::LockError::Io { source, .. } => CasError::Io(source),
        })
    }

    fn log_path(&self, sha: &str) -> PathBuf {
        self.dir.join(format!("{sha}.log"))
    }

    fn tombstone_path(&self, sha: &str) -> PathBuf {
        self.dir.join(format!("{sha}.expired"))
    }

    /// Store `data`, refresh its TTL, run GC, and return its IDs.
    ///
    /// Durable before it returns: the entry's bytes, and the directory entries
    /// that name it, are on disk. The whole call holds the vault lock, so no
    /// concurrent GC can remove the entry between the write and the ID being
    /// handed out. The entry stays until the TTL has passed since this call
    /// (by each process's clock; see the SPEC for the bound).
    pub fn put(&self, data: &Scrubbed, now: SystemTime) -> Result<PutResult, CasError> {
        let _guard = self.lock()?;
        let sha = fsutil::sha256_hex(data.as_bytes());
        let path = self.log_path(&sha);
        let file = match intact_entry(&path, &sha) {
            Some(file) => file,
            None => {
                fsutil::atomic_write_private(&path, data.as_bytes())?;
                fsutil::open_regular(&path, true)?
            }
        };
        // Dedupe hit or fresh write: the entry's 7 days restart now. (A refresh of an existing
        // entry's timestamp is not fsynced: a crash can only make it expire earlier than the new
        // time, never lose the content.)
        file.set_modified(now)?;
        remove_if_present(&self.tombstone_path(&sha))?;

        // One directory scan serves both the GC and the ID selection. Entries the GC just removed
        // stay in the ID space as tombstones, so `known` is the whole scan.
        let entries = self.scan()?;
        self.gc_entries(&entries, now)?;
        let known: Vec<String> = entries.into_iter().map(|e| e.sha).collect();
        Ok(PutResult {
            short_id: shortest_unique_prefix(&sha, &known),
            sha,
        })
    }

    /// Delete entries older than the TTL, leaving tombstones. Returns how many.
    pub fn gc(&self, now: SystemTime) -> Result<usize, CasError> {
        let Some(_guard) = self.lock_existing()? else {
            return Ok(0);
        };
        self.gc_locked(now)
    }

    /// GC with the vault lock already held by the caller.
    fn gc_locked(&self, now: SystemTime) -> Result<usize, CasError> {
        let entries = self.scan()?;
        Ok(self.gc_entries(&entries, now)?.len())
    }

    /// Delete the stale live entries among `entries` (a scan taken under the lock), leaving
    /// tombstones, and return their shas. A removed entry stays in the ID space as a tombstone.
    ///
    /// Tombstones are created and made durable *before* any log is removed, so a crash can never
    /// leave an issued ID that is neither a log nor a tombstone.
    fn gc_entries(&self, entries: &[Entry], now: SystemTime) -> Result<Vec<String>, CasError> {
        let stale: Vec<String> = entries
            .iter()
            .filter(|e| e.kind == Kind::Live && self.is_stale(e.modified, now))
            .map(|e| e.sha.clone())
            .collect();
        if stale.is_empty() {
            return Ok(stale);
        }
        for sha in &stale {
            fsutil::create_private_empty(&self.tombstone_path(sha))?;
        }
        fsutil::sync_dir(&self.dir)?;
        for sha in &stale {
            remove_if_present(&self.log_path(sha))?;
        }
        fsutil::sync_dir(&self.dir)?;
        Ok(stale)
    }

    /// Resolve a (short) ID to the full sha of a live entry.
    ///
    /// Tombstones take part: a prefix that is shared by an expired entry and a
    /// live one is ambiguous, never silently the live one.
    pub fn resolve(&self, input: &str, now: SystemTime) -> Result<String, CasError> {
        let prefix = normalize_id(input)?;
        let Some(_guard) = self.lock_existing()? else {
            return Err(CasError::NotFound(prefix));
        };
        self.gc_locked(now)?;
        self.resolve_locked(prefix)
    }

    /// Prefix resolution with the vault lock held and GC already run.
    fn resolve_locked(&self, prefix: String) -> Result<String, CasError> {
        let entries = self.scan()?;

        let matches: Vec<&Entry> = entries
            .iter()
            .filter(|e| e.sha.starts_with(&prefix))
            .collect();
        match matches.as_slice() {
            [] => Err(CasError::NotFound(prefix)),
            [one] if one.kind == Kind::Live => Ok(one.sha.clone()),
            // Every possible meaning is gone, so nothing can be returned.
            all if all.iter().all(|e| e.kind == Kind::Tombstone) => Err(CasError::Expired(prefix)),
            many => {
                let known: Vec<String> = entries.iter().map(|e| e.sha.clone()).collect();
                // `scan` is sorted by sha, so the hints are stable.
                let mut candidates: Vec<Candidate> = many
                    .iter()
                    .map(|e| Candidate {
                        id: shortest_unique_prefix(&e.sha, &known),
                        expired: e.kind == Kind::Tombstone,
                    })
                    .collect();
                candidates.sort_by(|a, b| a.id.cmp(&b.id));
                Err(CasError::Ambiguous { prefix, candidates })
            }
        }
    }

    /// The ID to emit for each full sha (as [`PutResult::short_id`] would give it now), or `None`
    /// for a sha that is not a live entry. One scan under the lock serves every sha; a vault that
    /// was never written answers `None` throughout without creating anything.
    pub fn short_ids(
        &self,
        shas: &[&str],
        now: SystemTime,
    ) -> Result<Vec<Option<String>>, CasError> {
        let Some(_guard) = self.lock_existing()? else {
            return Ok(vec![None; shas.len()]);
        };
        self.gc_locked(now)?;
        let entries = self.scan()?;
        let known: Vec<String> = entries.iter().map(|e| e.sha.clone()).collect();
        Ok(shas
            .iter()
            .map(|sha| {
                entries
                    .iter()
                    .any(|e| e.sha == *sha && e.kind == Kind::Live)
                    .then(|| shortest_unique_prefix(sha, &known))
            })
            .collect())
    }

    /// [`Cas::short_ids`] as a map from sha to emitted ID, holding only the live entries. Any
    /// error reads as "nothing live" (callers only decorate output with the IDs).
    pub fn live_ids(&self, shas: &[&str], now: SystemTime) -> HashMap<String, String> {
        let ids = self
            .short_ids(shas, now)
            .unwrap_or_else(|_| vec![None; shas.len()]);
        shas.iter()
            .zip(ids)
            .filter_map(|(sha, id)| id.map(|id| (sha.to_string(), id)))
            .collect()
    }

    /// Entry counts and the live entries' total size. A vault that was never written counts zero
    /// and is not created. Does not run GC (call [`Cas::gc`] first for an up-to-date view).
    pub fn stats(&self) -> Result<CasStats, CasError> {
        let Some(_guard) = self.lock_existing()? else {
            return Ok(CasStats::default());
        };
        let mut stats = CasStats::default();
        for entry in self.scan()? {
            match entry.kind {
                Kind::Live => {
                    stats.live += 1;
                    stats.bytes += fs::symlink_metadata(self.log_path(&entry.sha))
                        .map(|m| m.len())
                        .unwrap_or(0);
                }
                Kind::Tombstone => stats.expired += 1,
            }
        }
        Ok(stats)
    }

    /// The raw (scrubbed) bytes of an entry, optionally sliced by line.
    ///
    /// The bytes are hashed first: an entry that no longer matches its name is
    /// [`CasError::Corrupt`] and nothing from it is returned, not even a slice.
    pub fn read(
        &self,
        input: &str,
        range: Option<LineRange>,
        now: SystemTime,
    ) -> Result<Vec<u8>, CasError> {
        let prefix = normalize_id(input)?;
        let Some(_guard) = self.lock_existing()? else {
            return Err(CasError::NotFound(prefix));
        };
        self.gc_locked(now)?;
        let sha = self.resolve_locked(prefix)?;
        let bytes = match read_entry(&self.log_path(&sha)) {
            Ok(bytes) => bytes,
            // Removed behind our back (not by a GC: that needs the lock we hold).
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(CasError::Expired(sha[..MIN_EMIT_ID_LEN].to_string()));
            }
            Err(e) => return Err(e.into()),
        };
        if fsutil::sha256_hex(&bytes) != sha {
            return Err(CasError::Corrupt(sha[..MIN_EMIT_ID_LEN].to_string()));
        }
        Ok(match range {
            Some(range) => range.slice(&bytes),
            None => bytes,
        })
    }

    fn is_stale(&self, modified: SystemTime, now: SystemTime) -> bool {
        // A modified time in the future (clock skew) counts as fresh.
        now.duration_since(modified).is_ok_and(|age| age > self.ttl)
    }

    /// Strict scan, sorted by sha: only regular files named
    /// `<64 lowercase hex>.log` / `.expired` count, so `atomic_write` temp
    /// files, stray files and symlinks are ignored. A sha with both a log and
    /// a tombstone (an interrupted GC) is live.
    fn scan(&self) -> Result<Vec<Entry>, CasError> {
        let reader = match fs::read_dir(&self.dir) {
            Ok(reader) => reader,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
        for item in reader {
            let item = item?;
            let name = item.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some((sha, ext)) = name.split_once('.') else {
                continue;
            };
            let kind = match ext {
                "log" => Kind::Live,
                "expired" => Kind::Tombstone,
                _ => continue,
            };
            if !is_sha(sha) {
                continue;
            }
            // `DirEntry::metadata` does not follow symlinks.
            let Ok(meta) = item.metadata() else { continue };
            if !meta.file_type().is_file() {
                continue;
            }
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let entry = Entry {
                sha: sha.to_string(),
                kind,
                modified,
            };
            match entries.get(sha) {
                Some(existing) if existing.kind == Kind::Live => {}
                _ => {
                    entries.insert(sha.to_string(), entry);
                }
            }
        }
        Ok(entries.into_values().collect())
    }
}

/// An existing entry that really holds the bytes its name claims, opened
/// writable (to refresh its mtime). A symlink, a non-regular file, a read
/// error or a hash mismatch all mean "not intact": the caller rewrites it.
fn intact_entry(path: &Path, sha: &str) -> Option<fs::File> {
    let mut file = fsutil::open_regular(path, true).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    (fsutil::sha256_hex(&bytes) == sha).then_some(file)
}

/// Read an entry's bytes without following a symlink.
fn read_entry(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fsutil::open_regular(path, false)?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn is_sha(s: &str) -> bool {
    s.len() == SHA_LEN && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Shortest prefix of `sha` (at least [`MIN_EMIT_ID_LEN`]) shared with no other sha, live or
/// expired: the ID AgentDiet emits for it.
pub fn shortest_unique_prefix(sha: &str, all: &[String]) -> String {
    let mut len = MIN_EMIT_ID_LEN;
    while len < sha.len() && all.iter().any(|o| o != sha && o.starts_with(&sha[..len])) {
        len += 1;
    }
    sha[..len.min(sha.len())].to_string()
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_accepts_hash_prefix_and_case() {
        assert_eq!(normalize_id("#4F9A").unwrap(), "4f9a");
        assert_eq!(normalize_id("4f9a").unwrap(), "4f9a");
        assert_eq!(normalize_id(" #4f9a1b ").unwrap(), "4f9a1b");
    }

    #[test]
    fn normalize_drops_one_pair_of_single_quotes() {
        assert_eq!(normalize_id("'#4F9A1b2c'").unwrap(), "4f9a1b2c");
        assert_eq!(normalize_id("'4f9a'").unwrap(), "4f9a");
        assert!(normalize_id("''#4f9a''").is_err());
        assert!(normalize_id("'#4f9a").is_err());
    }

    #[test]
    fn normalize_rejects_short_and_non_hex() {
        for bad in ["4f9", "#", "", "xyz!", "#4f9g", &"a".repeat(65)] {
            assert!(
                matches!(normalize_id(bad), Err(CasError::InvalidId(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn range_parsing() {
        assert_eq!(
            LineRange::parse("2..5").unwrap(),
            LineRange { start: 2, end: 5 }
        );
        assert_eq!(
            LineRange::parse("3..3").unwrap(),
            LineRange { start: 3, end: 3 }
        );
        for bad in ["0..3", "5..2", "3", "a..b", "..", "1..", "-1..3"] {
            assert!(
                matches!(LineRange::parse(bad), Err(CasError::InvalidRange(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn slicing_is_one_based_inclusive_and_keeps_endings() {
        let data = b"one\ntwo\nthree\nfour\n";
        let r = |s| LineRange::parse(s).unwrap();
        assert_eq!(r("1..1").slice(data), b"one\n");
        assert_eq!(r("2..3").slice(data), b"two\nthree\n");
        assert_eq!(r("3..99").slice(data), b"three\nfour\n"); // end clamps
        assert_eq!(r("9..10").slice(data), b""); // start past EOF
    }

    #[test]
    fn slicing_preserves_crlf_and_missing_trailing_newline() {
        let data = b"a\r\nb\r\nc";
        let r = LineRange::parse("2..3").unwrap();
        assert_eq!(r.slice(data), b"b\r\nc");
    }

    #[test]
    fn shortest_prefix_grows_until_unique() {
        // Never shorter than the 8-hex emit minimum, however unique a shorter one would be.
        let a = format!("4f9a1b2c0{}", "0".repeat(55));
        let b = format!("4f9a1b2c1{}", "0".repeat(55));
        let c = format!("beef{}", "0".repeat(60));
        let all = vec![a.clone(), b.clone(), c.clone()];
        assert_eq!(shortest_unique_prefix(&c, &all), "beef0000");
        assert_eq!(shortest_unique_prefix(&b, &all), "4f9a1b2c1");
        assert_eq!(shortest_unique_prefix(&a, &all), "4f9a1b2c0");
        // Two entries identical for 70 chars are impossible; identical for 63 needs the full hash.
        let x = format!("{}0", "a".repeat(63));
        let y = format!("{}1", "a".repeat(63));
        assert_eq!(shortest_unique_prefix(&x, &[x.clone(), y]), x);
    }

    #[test]
    fn short_ids_match_put_and_skip_dead_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let now = SystemTime::now();
        let cas = Cas::new(dir.path().join("cas"));
        assert_eq!(cas.short_ids(&["ab"], now).unwrap(), vec![None]);
        assert!(
            !dir.path().join("cas").exists(),
            "a read never creates the vault"
        );

        let put = cas.put(&crate::scrubber::scrub(b"hello\n"), now).unwrap();
        let missing = "0".repeat(64);
        assert_eq!(
            cas.short_ids(&[&put.sha, &missing], now).unwrap(),
            vec![Some(put.short_id), None]
        );
        let later = now + DEFAULT_TTL + Duration::from_secs(1);
        assert_eq!(cas.short_ids(&[&put.sha], later).unwrap(), vec![None]);
    }

    #[test]
    fn line_range_new_validates_like_parse() {
        assert_eq!(
            LineRange::new(2, 5).unwrap(),
            LineRange::parse("2..5").unwrap()
        );
        assert!(matches!(
            LineRange::new(0, 3),
            Err(CasError::InvalidRange(_))
        ));
        assert!(matches!(
            LineRange::new(4, 3),
            Err(CasError::InvalidRange(_))
        ));
    }

    #[test]
    fn stats_and_live_ids_count_without_creating_the_vault() {
        let dir = tempfile::TempDir::new().unwrap();
        let now = SystemTime::now();
        let cas = Cas::new(dir.path().join("cas"));
        assert_eq!(cas.stats().unwrap(), CasStats::default());
        assert!(cas.live_ids(&["ab"], now).is_empty());
        assert!(!dir.path().join("cas").exists());

        let old = cas.put(&crate::scrubber::scrub(b"old\n"), now).unwrap();
        let later = now + DEFAULT_TTL + Duration::from_secs(1);
        let new = cas
            .put(&crate::scrubber::scrub(b"fresh!\n"), later)
            .unwrap();
        assert_eq!(
            cas.stats().unwrap(),
            CasStats {
                live: 1,
                expired: 1,
                bytes: 7
            }
        );
        let ids = cas.live_ids(&[&old.sha, &new.sha], later);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids.get(&new.sha), Some(&new.short_id));
    }
}
