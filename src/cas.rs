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
//! Every operation takes `now` explicitly so expiry is deterministic in tests.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::fsutil;
use crate::scrubber::Scrubbed;

/// How long an entry is kept after its last write.
pub const DEFAULT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Shortest ID shown to the user (`#4f9a`).
pub const MIN_ID_LEN: usize = 4;

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
    #[error("invalid ID {0:?}: expected at least {MIN_ID_LEN} hex characters, e.g. #4f9a")]
    InvalidId(String),
    #[error("invalid line range {0:?}: expected <start>..<end>, 1-based and inclusive, start <= end")]
    InvalidRange(String),
    #[error("vault I/O error: {0}")]
    Io(#[from] io::Error),
}

/// One entry an ambiguous prefix could mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Shortest prefix (at least [`MIN_ID_LEN`]) that tells it apart from every other entry.
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
    /// Shortest unique prefix, at least [`MIN_ID_LEN`] chars (shown as `#4f9a`).
    pub short_id: String,
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
        if start == 0 || start > end {
            return Err(bad());
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

/// Normalise a user-supplied ID: `#4F9A` / `4f9a` -> `4f9a`.
pub fn normalize_id(input: &str) -> Result<String, CasError> {
    let trimmed = input.trim();
    let id = trimmed.strip_prefix('#').unwrap_or(trimmed).to_ascii_lowercase();
    if id.len() < MIN_ID_LEN || id.len() > SHA_LEN || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
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
}

impl Cas {
    pub fn new(dir: PathBuf) -> Self {
        Self::with_ttl(dir, DEFAULT_TTL)
    }

    pub fn with_ttl(dir: PathBuf, ttl: Duration) -> Self {
        Self { dir, ttl }
    }

    fn log_path(&self, sha: &str) -> PathBuf {
        self.dir.join(format!("{sha}.log"))
    }

    fn tombstone_path(&self, sha: &str) -> PathBuf {
        self.dir.join(format!("{sha}.expired"))
    }

    /// Store `data`, refresh its TTL, run GC, and return its IDs.
    pub fn put(&self, data: &Scrubbed, now: SystemTime) -> Result<PutResult, CasError> {
        fsutil::create_private_dir_all(&self.dir)?;
        let sha = fsutil::sha256_hex(data.as_bytes());
        let path = self.log_path(&sha);
        let file = match intact_entry(&path, &sha) {
            Some(file) => file,
            None => {
                fsutil::atomic_write_private(&path, data.as_bytes())?;
                fsutil::open_regular(&path, true)?
            }
        };
        // Dedupe hit or fresh write: the entry's 7 days restart now.
        file.set_modified(now)?;
        remove_if_present(&self.tombstone_path(&sha))?;

        self.gc(now)?;
        let known = self.known_shas()?;
        Ok(PutResult {
            short_id: shortest_unique_prefix(&sha, &known),
            sha,
        })
    }

    /// Delete entries older than the TTL, leaving tombstones. Returns how many.
    pub fn gc(&self, now: SystemTime) -> Result<usize, CasError> {
        let mut removed = 0;
        for entry in self.scan()? {
            if entry.kind == Kind::Live && self.is_stale(entry.modified, now) {
                fsutil::create_private_empty(&self.tombstone_path(&entry.sha))?;
                remove_if_present(&self.log_path(&entry.sha))?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Resolve a (short) ID to the full sha of a live entry.
    ///
    /// Tombstones take part: a prefix that is shared by an expired entry and a
    /// live one is ambiguous, never silently the live one.
    pub fn resolve(&self, input: &str, now: SystemTime) -> Result<String, CasError> {
        let prefix = normalize_id(input)?;
        self.gc(now)?;
        let entries = self.scan()?;

        let matches: Vec<&Entry> = entries.iter().filter(|e| e.sha.starts_with(&prefix)).collect();
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

    /// The raw (scrubbed) bytes of an entry, optionally sliced by line.
    pub fn read(
        &self,
        input: &str,
        range: Option<LineRange>,
        now: SystemTime,
    ) -> Result<Vec<u8>, CasError> {
        let sha = self.resolve(input, now)?;
        let bytes = match read_entry(&self.log_path(&sha)) {
            Ok(bytes) => bytes,
            // Removed by a concurrent GC between resolve and read.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(CasError::Expired(sha[..MIN_ID_LEN].to_string()));
            }
            Err(e) => return Err(e.into()),
        };
        Ok(match range {
            Some(range) => range.slice(&bytes),
            None => bytes,
        })
    }

    fn is_stale(&self, modified: SystemTime, now: SystemTime) -> bool {
        // A modified time in the future (clock skew) counts as fresh.
        now.duration_since(modified).is_ok_and(|age| age > self.ttl)
    }

    /// Every sha the vault knows, live or expired: the ID space short IDs must
    /// be unique within.
    fn known_shas(&self) -> Result<Vec<String>, CasError> {
        Ok(self.scan()?.into_iter().map(|e| e.sha).collect())
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
            let Some((sha, ext)) = name.split_once('.') else { continue };
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
            let Ok(modified) = meta.modified() else { continue };
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

/// Shortest prefix of `sha` (at least [`MIN_ID_LEN`]) shared with no other sha.
pub fn shortest_unique_prefix(sha: &str, all: &[String]) -> String {
    let mut len = MIN_ID_LEN;
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
    fn normalize_rejects_short_and_non_hex() {
        for bad in ["4f9", "#", "", "xyz!", "#4f9g", &"a".repeat(65)] {
            assert!(matches!(normalize_id(bad), Err(CasError::InvalidId(_))), "{bad}");
        }
    }

    #[test]
    fn range_parsing() {
        assert_eq!(LineRange::parse("2..5").unwrap(), LineRange { start: 2, end: 5 });
        assert_eq!(LineRange::parse("3..3").unwrap(), LineRange { start: 3, end: 3 });
        for bad in ["0..3", "5..2", "3", "a..b", "..", "1..", "-1..3"] {
            assert!(matches!(LineRange::parse(bad), Err(CasError::InvalidRange(_))), "{bad}");
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
        let a = format!("4f9a{}", "0".repeat(60));
        let b = format!("4f9a1{}", "0".repeat(59));
        let c = format!("beef{}", "0".repeat(60));
        let all = vec![a.clone(), b.clone(), c.clone()];
        assert_eq!(shortest_unique_prefix(&c, &all), "beef");
        assert_eq!(shortest_unique_prefix(&b, &all), "4f9a1");
        assert_eq!(shortest_unique_prefix(&a, &all), "4f9a0");
    }
}
