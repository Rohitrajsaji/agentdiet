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
//!
//! Schema v3 adds the **run log** (`runs`): one row per command or tool call the hooks, shims and
//! `diet_exec` saw, trimmed or not, with its exit status and token counts. `agentdiet stats`
//! counts it and `agentdiet reflect` mines it (SPEC §3 Tier 3, "Run log"). Like everything here it
//! holds scrubbed text only.

use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};

use crate::fsutil;
use crate::scrubber::Scrubbed;

const SCHEMA_VERSION: i64 = 3;

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

/// Added in schema v3: the run log.
const SCHEMA_V3: &str = "
CREATE TABLE runs(
    id             INTEGER PRIMARY KEY,
    created_ms     INTEGER NOT NULL,
    origin         TEXT NOT NULL,
    kind           TEXT NOT NULL,
    family         TEXT NOT NULL,
    title          TEXT NOT NULL,
    outcome        TEXT NOT NULL,
    exit_code      INTEGER,
    raw_tokens     INTEGER NOT NULL,
    emitted_tokens INTEGER NOT NULL,
    cas_sha        TEXT,
    session        TEXT
);
CREATE INDEX runs_session ON runs(session, id);
";

/// The run log keeps at most this many rows; older ones are dropped on insert.
pub const MAX_RUNS: i64 = 10_000;
/// How long recording a run waits for another writer before it is dropped (a hook must not wait).
pub const RUN_BUSY_TIMEOUT: Duration = Duration::from_millis(100);

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

/// Which entry point saw a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A shim's agent path.
    Shim,
    /// The MCP tool `diet_exec`.
    Mcp,
    /// A Claude Code hook (PostToolUse, PostToolUseFailure).
    Hook,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Shim => "shim",
            Origin::Mcp => "mcp",
            Origin::Hook => "hook",
        }
    }
}

/// What a run was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    /// A command (argv or shell text).
    Shell,
    /// A file read.
    Read,
    /// A file or content search (Grep, Glob).
    Search,
}

impl RunKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RunKind::Shell => "shell",
            RunKind::Read => "read",
            RunKind::Search => "search",
        }
    }
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Ok,
    /// A command that exited non-zero, was killed, or a tool call that failed.
    Fail,
    /// A search that found nothing, or a read of a path that is not there.
    Miss,
}

impl RunOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            RunOutcome::Ok => "ok",
            RunOutcome::Fail => "fail",
            RunOutcome::Miss => "miss",
        }
    }
}

/// One run to record. Text is scrubbed by construction.
#[derive(Debug, Clone, Copy)]
pub struct NewRun<'a> {
    pub origin: Origin,
    pub kind: RunKind,
    /// The executable's base name (`npm`), or the tool name (`Read`, `Grep`).
    pub family: &'a Scrubbed,
    /// The command line, path or pattern.
    pub title: &'a Scrubbed,
    pub outcome: RunOutcome,
    pub exit_code: Option<i32>,
    /// Estimated tokens of the output before AgentDiet ([`crate::diet::estimate_tokens`]).
    pub raw_tokens: u64,
    /// Estimated tokens of what the agent was given.
    pub emitted_tokens: u64,
    /// The Vault entry holding the full output, when one was stored.
    pub cas_sha: Option<&'a str>,
    pub session: Option<&'a Scrubbed>,
}

/// One recorded run, as [`MemoryDb::runs`] returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub id: i64,
    pub created_ms: i64,
    pub origin: String,
    pub kind: String,
    pub family: String,
    pub title: String,
    pub outcome: String,
    pub exit_code: Option<i64>,
    pub raw_tokens: i64,
    pub emitted_tokens: i64,
    pub cas_sha: Option<String>,
    pub session: Option<String>,
}

/// Totals over the run log for one origin.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunTotals {
    pub runs: u64,
    /// Runs whose outcome is `fail` (a search `miss` is not a failure).
    pub failed: u64,
    /// Runs whose output was replaced (a Vault entry was referenced).
    pub trimmed: u64,
    pub raw_tokens: u64,
    pub emitted_tokens: u64,
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
        Self::open_with_timeout(path, BUSY_TIMEOUT)
    }

    /// [`MemoryDb::open`], waiting at most `busy` for another writer (instead of 2 s).
    pub fn open_with_timeout(path: &Path, busy: Duration) -> Result<Self, MemoryError> {
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
        require_opened_checked(&conn, &checked, &path).map_err(io_err)?;
        drop(checked);
        conn.busy_timeout(busy)?;
        enable_wal(&conn, busy)?;
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

    /// Append one run to the run log, dropping rows beyond the newest [`MAX_RUNS`]. `cas_sha`
    /// must be 64 lowercase hex; `session` scrubbed text of at most [`MAX_SESSION_LEN`] bytes.
    pub fn record_run(&self, run: &NewRun<'_>) -> Result<i64, MemoryError> {
        if let Some(sha) = run.cas_sha
            && !is_sha256_hex(sha)
        {
            return Err(MemoryError::InvalidMetadata("cas_sha"));
        }
        if run
            .session
            .is_some_and(|s| s.as_bytes().len() > MAX_SESSION_LEN)
        {
            return Err(MemoryError::InvalidMetadata("session"));
        }
        let created_ms = i64::try_from(fsutil::now_millis()).unwrap_or(i64::MAX);
        let tokens = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
        self.conn.execute(
            "INSERT INTO runs(created_ms, origin, kind, family, title, outcome, exit_code,
                              raw_tokens, emitted_tokens, cas_sha, session)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                created_ms,
                run.origin.as_str(),
                run.kind.as_str(),
                run.family.to_lossy_string(),
                run.title.to_lossy_string(),
                run.outcome.as_str(),
                run.exit_code,
                tokens(run.raw_tokens),
                tokens(run.emitted_tokens),
                run.cas_sha,
                run.session.map(Scrubbed::to_lossy_string),
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn
            .execute("DELETE FROM runs WHERE id <= ?1", params![id - MAX_RUNS])?;
        Ok(id)
    }

    /// The newest `limit` runs, oldest first.
    pub fn runs(&self, limit: usize) -> Result<Vec<RunRow>, MemoryError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT id, created_ms, origin, kind, family, title, outcome, exit_code, raw_tokens,
                    emitted_tokens, cas_sha, session
             FROM (SELECT * FROM runs ORDER BY id DESC LIMIT ?1) ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(RunRow {
                id: row.get(0)?,
                created_ms: row.get(1)?,
                origin: row.get(2)?,
                kind: row.get(3)?,
                family: row.get(4)?,
                title: row.get(5)?,
                outcome: row.get(6)?,
                exit_code: row.get(7)?,
                raw_tokens: row.get(8)?,
                emitted_tokens: row.get(9)?,
                cas_sha: row.get(10)?,
                session: row.get(11)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// When the newest run of `origin` was recorded (ms since the epoch), if there is one.
    pub fn last_run_ms(&self, origin: Origin) -> Result<Option<i64>, MemoryError> {
        Ok(self.conn.query_row(
            "SELECT max(created_ms) FROM runs WHERE origin = ?1",
            params![origin.as_str()],
            |row| row.get(0),
        )?)
    }

    /// Totals per origin (`shim`, `mcp`, `hook`), in that order; an origin with no runs is all 0.
    pub fn run_totals(&self) -> Result<Vec<(Origin, RunTotals)>, MemoryError> {
        let mut out = Vec::new();
        for origin in [Origin::Shim, Origin::Mcp, Origin::Hook] {
            let totals = self.conn.query_row(
                "SELECT count(*),
                        coalesce(sum(outcome = 'fail'), 0),
                        coalesce(sum(cas_sha IS NOT NULL), 0),
                        coalesce(sum(raw_tokens), 0),
                        coalesce(sum(emitted_tokens), 0)
                 FROM runs WHERE origin = ?1",
                params![origin.as_str()],
                |row| {
                    let n = |i: usize| row.get::<_, i64>(i).map(|v| u64::try_from(v).unwrap_or(0));
                    Ok(RunTotals {
                        runs: n(0)?,
                        failed: n(1)?,
                        trimmed: n(2)?,
                        raw_tokens: n(3)?,
                        emitted_tokens: n(4)?,
                    })
                },
            )?;
            out.push((origin, totals));
        }
        Ok(out)
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
fn enable_wal(conn: &Connection, busy: Duration) -> Result<(), MemoryError> {
    let deadline = std::time::Instant::now() + busy;
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
    if version < 3 {
        tx.execute_batch(SCHEMA_V3)?;
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

/// Record one run in the project's memory.db at `db_path`, creating it if needed and waiting at
/// most [`RUN_BUSY_TIMEOUT`] for another writer. Best effort: the caller drops a failure.
pub fn record_run(db_path: &Path, run: &NewRun<'_>) -> Result<(), MemoryError> {
    MemoryDb::open_with_timeout(db_path, RUN_BUSY_TIMEOUT)?
        .record_run(run)
        .map(drop)
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

/// Fail unless the file SQLite opened is the one `checked` (opened `O_NOFOLLOW` beforehand) holds:
/// SQLite's own handle still matches the path (`SQLITE_FCNTL_HAS_MOVED`, which compares the
/// inode it opened with a fresh `stat`), and the path matches `checked`. Comparing the path with
/// `checked` alone would miss a file swapped in for SQLite's `open` and swapped back before the
/// check (SPEC §3 Tier 3 "memory.db metadata").
fn require_opened_checked(
    conn: &Connection,
    checked: &std::fs::File,
    path: &Path,
) -> io::Result<()> {
    if sqlite_file_has_moved(conn)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file was replaced while opening",
        ));
    }
    fsutil::require_same_file(checked, path)
}

/// `SQLITE_FCNTL_HAS_MOVED` on the main database. `false` where the VFS does not implement it
/// (Windows), which SPEC §10 states.
fn sqlite_file_has_moved(conn: &Connection) -> io::Result<bool> {
    use rusqlite::ffi;
    let mut moved: std::ffi::c_int = 0;
    // SAFETY: `conn.handle()` is the open connection's live `sqlite3*`, "main" is a NUL-terminated
    // schema name, and HAS_MOVED writes exactly one `int` through the pointer it is given.
    let rc = unsafe {
        ffi::sqlite3_file_control(
            conn.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_HAS_MOVED,
            (&raw mut moved).cast(),
        )
    };
    match rc {
        ffi::SQLITE_OK => Ok(moved != 0),
        ffi::SQLITE_NOTFOUND => Ok(false),
        _ => Err(io::Error::other("could not check the opened database file")),
    }
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

    /// A file swapped in for SQLite's `open` and swapped back before the check: the path names
    /// the checked file again, so comparing the path alone passes; SQLite's handle does not.
    #[cfg(unix)]
    #[test]
    fn a_file_swapped_in_and_back_around_sqlites_open_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("memory.db");
        drop(MemoryDb::open(&path).unwrap());
        let checked = fsutil::open_or_create_private(&path).unwrap();

        let aside = dir.path().join("aside.db");
        std::fs::rename(&path, &aside).unwrap();
        drop(MemoryDb::open(&path).unwrap()); // another database at the path
        let conn = Connection::open(&path).unwrap();
        std::fs::rename(&path, dir.path().join("planted.db")).unwrap();
        std::fs::rename(&aside, &path).unwrap();

        fsutil::require_same_file(&checked, &path).expect("the path alone looks unchanged");
        let err = require_opened_checked(&conn, &checked, &path).unwrap_err();
        assert_eq!(err.to_string(), "file was replaced while opening");

        // And the normal case passes.
        let conn = Connection::open(&path).unwrap();
        require_opened_checked(&conn, &checked, &path).unwrap();
    }

    /// A cloned repository can carry `.agentdiet/memory.db-wal` / `-shm` as symlinks. The bundled
    /// SQLite opens both `O_NOFOLLOW`, so the targets are never written: indexing fails instead.
    #[cfg(unix)]
    #[test]
    fn planted_wal_and_shm_symlinks_are_never_written_through() {
        let dir = tempfile::TempDir::new().unwrap();
        let victim = dir.path().join("victim");
        std::fs::create_dir(&victim).unwrap();
        let db_dir = dir.path().join(".agentdiet");
        std::fs::create_dir(&db_dir).unwrap();
        for sibling in ["wal", "shm"] {
            std::fs::write(victim.join(sibling), b"untouched").unwrap();
            std::os::unix::fs::symlink(
                victim.join(sibling),
                db_dir.join(format!("memory.db-{sibling}")),
            )
            .unwrap();
        }
        let indexed = MemoryDb::open(&db_dir.join("memory.db"))
            .and_then(|db| db.index(Kind::Command, &scrub(b"t"), &scrub(b"b"), None, None));
        assert!(indexed.is_err(), "a symlinked -shm cannot be used");
        for sibling in ["wal", "shm"] {
            assert_eq!(std::fs::read(victim.join(sibling)).unwrap(), b"untouched");
        }
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
