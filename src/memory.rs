//! Project-local memory index: `.agentdiet/memory.db`.
//!
//! SQLite in WAL mode with an FTS5 table over `title` and `body`, ranked by
//! BM25. It holds short summaries (command results, document reads, session
//! decisions) that point at the full output in the vault via `cas_sha`;
//! PreCompact recovery and `diet_search` query it. Only [`Scrubbed`] text can
//! be indexed.

use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, params};

use crate::fsutil;
use crate::scrubber::Scrubbed;

const SCHEMA_VERSION: i64 = 1;

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

pub struct MemoryDb {
    conn: Connection,
}

impl MemoryDb {
    /// Open (creating and migrating if needed) the database at `path`.
    pub fn open(path: &Path) -> Result<Self, MemoryError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| MemoryError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(2))?;
        // Setting journal_mode returns a row, so `execute` would fail.
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<(), MemoryError> {
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(MemoryError::UnsupportedVersion(version));
        }
        if version < 1 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(SCHEMA_V1)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Index one summary; returns its row id.
    pub fn index(
        &self,
        kind: Kind,
        title: &Scrubbed,
        body: &Scrubbed,
        cas_sha: Option<&str>,
        session: Option<&str>,
    ) -> Result<i64, MemoryError> {
        let created_ms = i64::try_from(fsutil::now_millis()).unwrap_or(i64::MAX);
        self.conn.execute(
            "INSERT INTO entries(kind, title, body, cas_sha, session, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                kind.as_str(),
                title.to_lossy_string(),
                body.to_lossy_string(),
                cas_sha,
                session,
                created_ms
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
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

    /// The active journal mode (`wal` once opened).
    pub fn journal_mode(&self) -> Result<String, MemoryError> {
        Ok(self
            .conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))?)
    }
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
}
