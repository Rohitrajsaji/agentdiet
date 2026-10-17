//! Project-local memory index: `.agentdiet/memory.db`.
//!
//! SQLite in WAL mode with an FTS5 table over `title` and `body`, ranked by
//! BM25. It holds short summaries (command results, document reads, session
//! decisions) that point at the full output in the vault via `cas_sha`;
//! PreCompact recovery and `diet_search` query it. Only [`Scrubbed`] text can
//! be indexed: titles, bodies and the session label alike, and `cas_sha` must be
//! a real sha256, so nothing free-form reaches the database unscrubbed.
//!
//! Rows are *events*. Schema v2 adds a `NOT NULL` `dedupe_key` (sha256 over the
//! length-prefixed kind, title, body, cas_sha and session) with a unique index,
//! and [`MemoryDb::index`] is a single `INSERT … ON CONFLICT DO NOTHING`, so
//! repeats collapse atomically while the same output produced by a different
//! command or session keeps its own row.

use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};

use crate::fsutil;
use crate::scrubber::Scrubbed;

const SCHEMA_VERSION: i64 = 2;

/// How long SQLite (and our WAL retry) waits on another writer before reporting `Busy`.
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

/// Longest session label accepted (bytes of scrubbed text).
pub const MAX_SESSION_LEN: usize = 256;

/// Title matches count this much more than body matches.
const TITLE_WEIGHT: f64 = 5.0;
const BODY_WEIGHT: f64 = 1.0;

const SCHEMA_V1: &str = "
CREATE TABLE entries(
    id         INTEGER PRIMARY KEY,
    kind       TEXT NOT NULL,
    title      TEXT NOT NULL,
    body       TEXT NOT NULL,
    cas_sha    TEXT,
    session    TEXT,
    created_ms INTEGER NOT NULL
);
CREATE VIRTUAL TABLE entries_fts USING fts5(
    title, body, content='entries', content_rowid='id', tokenize='unicode61'
);
CREATE TRIGGER entries_ai AFTER INSERT ON entries BEGIN
    INSERT INTO entries_fts(rowid, title, body) VALUES (new.id, new.title, new.body);
END;
CREATE TRIGGER entries_ad AFTER DELETE ON entries BEGIN
    INSERT INTO entries_fts(entries_fts, rowid, title, body)
    VALUES ('delete', old.id, old.title, old.body);
END;
CREATE TRIGGER entries_au AFTER UPDATE ON entries BEGIN
    INSERT INTO entries_fts(entries_fts, rowid, title, body)
    VALUES ('delete', old.id, old.title, old.body);
    INSERT INTO entries_fts(rowid, title, body) VALUES (new.id, new.title, new.body);
END;
";

/// Added in schema v2. The key column is `NOT NULL` so a missing `cas_sha` or `session` (an empty
/// string in the key) cannot make two identical events look distinct, which a `UNIQUE` index over
/// nullable columns would allow.
const SCHEMA_V2_COLUMN: &str = "ALTER TABLE entries ADD COLUMN dedupe_key TEXT NOT NULL DEFAULT ''";
const SCHEMA_V2_INDEX: &str = "CREATE UNIQUE INDEX entries_dedupe ON entries(dedupe_key)";

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("memory database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("could not prepare {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("memory.db schema version {0} is newer than this agentdiet understands")]
    UnsupportedVersion(i64),
    #[error("invalid {0} for the memory index")]
    InvalidMetadata(&'static str),
}

/// What [`MemoryDb::index`] did with an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indexed {
    /// A new row with this id.
    New(i64),
    /// An identical event (same kind, title, body, `cas_sha` and session) was already there.
    Duplicate,
}

/// What an indexed entry summarises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Command,
    Read,
    Decision,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Command => "command",
            Kind::Read => "read",
            Kind::Decision => "decision",
        }
    }
}

/// One search result. Lower `score` is a better match (BM25).
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub snippet: String,
    pub cas_sha: Option<String>,
    pub score: f64,
}

/// One indexed event, as [`MemoryDb::recent`] returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recent {
    pub kind: String,
    pub title: String,
    pub body: String,
    pub cas_sha: Option<String>,
}

pub struct MemoryDb {
    conn: Connection,
}

impl MemoryDb {
    /// Open (creating and migrating if needed) the database at `path`.
    ///
    /// The project directory is created `0700` and the database file `0600` *before* SQLite sees
    /// it (SQLite gives `-wal` and `-shm` the main file's mode). The final path component is
    /// opened with `O_NOFOLLOW|O_NONBLOCK`, so a symlink or a FIFO planted at `memory.db` is
    /// refused rather than followed or waited on, and after SQLite has opened the file, before
    /// anything is read or written, the path must still be the very inode that was checked.
    /// (`SQLITE_OPEN_NOFOLLOW` is not used: it also rejects a symlinked *parent*, such as macOS's
    /// `/var` or a project under a linked directory.) Only the final component is guarded; a
    /// swap in the instant between SQLite's `open` and the check is detected, not prevented.
    pub fn open(path: &Path) -> Result<Self, MemoryError> {
        let io_err = |source| MemoryError::Io {
            path: path.display().to_string(),
            source,
        };
        let path = match (
            path.parent().filter(|p| !p.as_os_str().is_empty()),
            path.file_name(),
        ) {
            (Some(parent), Some(name)) => {
                fsutil::create_private_dir_all(parent).map_err(io_err)?;
                std::fs::canonicalize(parent).map_err(io_err)?.join(name)
            }
            _ => path.to_path_buf(),
        };
        let checked = fsutil::open_or_create_private(&path).map_err(io_err)?;
        let mut conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        fsutil::require_same_file(&checked, &path).map_err(io_err)?;
        drop(checked);
        conn.busy_timeout(BUSY_TIMEOUT)?;
        enable_wal(&conn)?;
        migrate(&mut conn)?;
        Ok(Self { conn })
    }

    /// Index one event. An identical event (same kind, title, body, `cas_sha` and session) is
    /// [`Indexed::Duplicate`]; the check and the insert are one statement, so concurrent callers
    /// cannot both insert it.
    ///
    /// `cas_sha` must be 64 lowercase hex; `session` is scrubbed text of at most
    /// [`MAX_SESSION_LEN`] bytes.
    pub fn index(
        &self,
        kind: Kind,
        title: &Scrubbed,
        body: &Scrubbed,
        cas_sha: Option<&str>,
        session: Option<&Scrubbed>,
    ) -> Result<Indexed, MemoryError> {
        if let Some(sha) = cas_sha
            && !is_sha256_hex(sha)
        {
            return Err(MemoryError::InvalidMetadata("cas_sha"));
        }
        if session.is_some_and(|s| s.as_bytes().len() > MAX_SESSION_LEN) {
            return Err(MemoryError::InvalidMetadata("session"));
        }
        let title = title.to_lossy_string();
        let body = body.to_lossy_string();
        let session = session.map(Scrubbed::to_lossy_string);
        let key = dedupe_key(kind.as_str(), &title, &body, cas_sha, session.as_deref());
        let created_ms = i64::try_from(fsutil::now_millis()).unwrap_or(i64::MAX);
        let inserted = self.conn.execute(
            "INSERT INTO entries(kind, title, body, cas_sha, session, created_ms, dedupe_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(dedupe_key) DO NOTHING",
            params![
                kind.as_str(),
                title,
                body,
                cas_sha,
                session,
                created_ms,
                key
            ],
        )?;
        Ok(if inserted == 0 {
            Indexed::Duplicate
        } else {
            Indexed::New(self.conn.last_insert_rowid())
        })
    }

    /// BM25-ranked search, best match first. Free text is safe: it can never
    /// produce an FTS5 syntax error (see [`fts_query`]).
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Hit>, MemoryError> {
        let Some(match_expr) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.kind, e.title,
                    snippet(entries_fts, 1, '[', ']', '…', 12),
                    e.cas_sha,
                    bm25(entries_fts, ?2, ?3) AS score
             FROM entries_fts
             JOIN entries e ON e.id = entries_fts.rowid
             WHERE entries_fts MATCH ?1
             ORDER BY score ASC, e.id ASC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![match_expr, TITLE_WEIGHT, BODY_WEIGHT, limit],
            |row| {
                Ok(Hit {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    title: row.get(2)?,
                    snippet: row.get(3)?,
                    cas_sha: row.get(4)?,
                    score: row.get(5)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// The `limit` most recently indexed events, newest first (the post-compaction snapshot).
    pub fn recent(&self, limit: usize) -> Result<Vec<Recent>, MemoryError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self
            .conn
            .prepare("SELECT kind, title, body, cas_sha FROM entries ORDER BY id DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(Recent {
                kind: row.get(0)?,
                title: row.get(1)?,
                body: row.get(2)?,
                cas_sha: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// The active journal mode (`wal` once opened).
    pub fn journal_mode(&self) -> Result<String, MemoryError> {
        Ok(self
            .conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))?)
    }
}

/// Switch to WAL. SQLite's busy handler is not consulted for a journal-mode change made while
/// another connection is mid-initialisation, so a concurrent first open can see `SQLITE_BUSY`
/// immediately; retry for as long as the connection's own busy timeout would have waited. Once
/// the mode is WAL it is persistent and later opens are no-ops.
fn enable_wal(conn: &Connection) -> Result<(), MemoryError> {
    let deadline = std::time::Instant::now() + BUSY_TIMEOUT;
    loop {
        // Setting journal_mode returns a row, so `execute` would fail.
        match conn
            .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))
        {
            Ok(_) => return Ok(()),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// sha256 over the length-prefixed fields. A missing `cas_sha`/`session` is the empty string.
fn dedupe_key(
    kind: &str,
    title: &str,
    body: &str,
    cas_sha: Option<&str>,
    session: Option<&str>,
) -> String {
    let mut buf = Vec::with_capacity(title.len() + body.len() + 160);
    for field in [
        kind,
        title,
        body,
        cas_sha.unwrap_or(""),
        session.unwrap_or(""),
    ] {
        buf.extend_from_slice(&(field.len() as u64).to_le_bytes());
        buf.extend_from_slice(field.as_bytes());
    }
    fsutil::sha256_hex(&buf)
}

/// Bring the database to [`SCHEMA_VERSION`].
///
/// An already-current database is detected without taking a write lock. Otherwise the work runs
/// in an `IMMEDIATE` transaction that re-reads `user_version` *after* the lock is held, so two
/// processes opening a fresh or v1 database at once serialise: the second finds the work done.
fn migrate(conn: &mut Connection) -> Result<(), MemoryError> {
    let read_version = |conn: &Connection| -> Result<i64, MemoryError> {
        Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
    };
    let version = read_version(conn)?;
    if version > SCHEMA_VERSION {
        return Err(MemoryError::UnsupportedVersion(version));
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version = read_version(&tx)?;
    if version > SCHEMA_VERSION {
        return Err(MemoryError::UnsupportedVersion(version));
    }
    if version < 1 {
        tx.execute_batch(SCHEMA_V1)?;
    }
    if version < 2 {
        tx.execute_batch(SCHEMA_V2_COLUMN)?;
        fill_dedupe_keys(&tx)?;
        tx.execute_batch(SCHEMA_V2_INDEX)?;
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

/// v1 -> v2: give every row its key and delete repeats, keeping the lowest `id` of each group so
/// the outcome does not depend on scan order. The FTS delete trigger keeps the index in sync.
fn fill_dedupe_keys(tx: &rusqlite::Transaction<'_>) -> Result<(), MemoryError> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut duplicates: Vec<i64> = Vec::new();
    {
        let mut select = tx.prepare(
            "SELECT id, kind, title, body, cas_sha, session FROM entries ORDER BY id ASC",
        )?;
        let mut update = tx.prepare("UPDATE entries SET dedupe_key = ?1 WHERE id = ?2")?;
        let mut rows = select.query([])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let key = dedupe_key(
                &row.get::<_, String>(1)?,
                &row.get::<_, String>(2)?,
                &row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?.as_deref(),
                row.get::<_, Option<String>>(5)?.as_deref(),
            );
            if seen.insert(key.clone()) {
                update.execute(params![key, id])?;
            } else {
                duplicates.push(id);
            }
        }
    }
    let mut delete = tx.prepare("DELETE FROM entries WHERE id = ?1")?;
    for id in duplicates {
        delete.execute([id])?;
    }
    Ok(())
}

/// Turn free text into a safe FTS5 expression: each whitespace-separated token
/// becomes a quoted phrase (inner quotes doubled) and the phrases are ANDed.
/// Operators like `-`, `:`, `*`, `NOT` are therefore plain text. Tokens with no
/// letters or digits are dropped; `None` means nothing searchable was left.
pub fn fts_query(query: &str) -> Option<String> {
    let phrases: Vec<String> = query
        .split_whitespace()
        .filter(|tok| tok.chars().any(char::is_alphanumeric))
        .map(|tok| format!("\"{}\"", tok.replace('"', "\"\"")))
        .collect();
    (!phrases.is_empty()).then(|| phrases.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrubber::scrub;

    #[test]
    fn fts_query_quotes_every_token() {
        assert_eq!(fts_query("cargo test").unwrap(), "\"cargo\" \"test\"");
        assert_eq!(fts_query("full-stack").unwrap(), "\"full-stack\"");
        assert_eq!(fts_query("say \"hi\"").unwrap(), "\"say\" \"\"\"hi\"\"\"");
        assert_eq!(fts_query("  * - :  "), None);
        assert_eq!(fts_query(""), None);
    }

    /// `rusqlite` is built with `bundled` only (no `bundled-full`); this pins down that the
    /// bundled SQLite still has FTS5 and its built-in ranking, by option flag and by behavior.
    #[test]
    fn bundled_sqlite_has_fts5_bm25_and_wal() {
        let conn = Connection::open_in_memory().unwrap();
        let fts5: i64 = conn
            .query_row("SELECT sqlite_compileoption_used('ENABLE_FTS5')", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fts5, 1, "bundled SQLite must be compiled with FTS5");

        conn.execute_batch(
            "CREATE VIRTUAL TABLE t USING fts5(title, body, tokenize='unicode61');
             INSERT INTO t(title, body) VALUES ('alpha', 'rust cargo build');
             INSERT INTO t(title, body) VALUES ('beta', 'rust rust rust cargo');
             INSERT INTO t(title, body) VALUES ('gamma', 'python pytest');",
        )
        .unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT title, bm25(t), snippet(t, 1, '[', ']', '…', 8)
                 FROM t WHERE t MATCH 'rust' ORDER BY bm25(t)",
            )
            .unwrap();
        let rows: Vec<(String, f64, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows.len(), 2, "only the two `rust` rows match");
        assert_eq!(rows[0].0, "beta", "denser match ranks first (lower bm25)");
        assert!(rows[0].1 < rows[1].1 && rows[0].1 < 0.0);
        assert!(rows[0].2.contains("[rust]"), "snippet: {}", rows[0].2);

        let dir = tempfile::TempDir::new().unwrap();
        let db = MemoryDb::open(&dir.path().join("m.db")).unwrap();
        assert_eq!(db.journal_mode().unwrap(), "wal");
    }

    #[test]
    fn hostile_queries_never_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = MemoryDb::open(&dir.path().join("m.db")).unwrap();
        db.index(
            Kind::Command,
            &scrub(b"full-stack build"),
            &scrub(b"ok"),
            None,
            None,
        )
        .unwrap();
        for q in [
            "full-stack",
            "a:b",
            "NOT",
            "AND OR",
            "\"",
            "(",
            "col:*",
            "-x",
            "*",
            "^a",
            "",
        ] {
            db.search(q, 10)
                .unwrap_or_else(|e| panic!("query {q:?} failed: {e}"));
        }
        assert_eq!(db.search("full-stack", 10).unwrap().len(), 1);
    }

    #[test]
    fn recent_is_newest_first_and_bounded() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = MemoryDb::open(&dir.path().join("m.db")).unwrap();
        let sha = "a".repeat(64);
        for (title, cas) in [
            ("first", None),
            ("second", Some(sha.as_str())),
            ("third", None),
        ] {
            db.index(
                Kind::Command,
                &scrub(title.as_bytes()),
                &scrub(b"ok"),
                cas,
                None,
            )
            .unwrap();
        }
        let rows = db.recent(2).unwrap();
        let titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["third", "second"]);
        assert_eq!(rows[1].cas_sha.as_deref(), Some(sha.as_str()));
        assert_eq!(rows[0].kind, "command");
    }
}
