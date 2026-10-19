//! memory.db schema v2: atomic provenance-preserving dedupe, a deterministic migration from
//! shipped v1 databases, validated and scrubbed metadata, and private files from creation.

mod common;

use std::fs;
use std::path::Path;
use std::sync::Arc;

use agentdiet::fsutil::sha256_hex;
use agentdiet::memory::{Indexed, Kind, MemoryDb, MemoryError};
use agentdiet::scrubber::scrub;
use common::Env;
use rusqlite::Connection;

/// The v1 schema exactly as Slice 2 shipped it (this is a historical fixture, deliberately not
/// shared with the production constant).
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
PRAGMA user_version = 1;
";

fn s(text: &str) -> agentdiet::scrubber::Scrubbed {
    scrub(text.as_bytes())
}

fn count(path: &Path) -> i64 {
    let conn = Connection::open(path).unwrap();
    conn.query_row("SELECT count(*) FROM entries", [], |r| r.get(0))
        .unwrap()
}

fn sha(tag: &str) -> String {
    sha256_hex(tag.as_bytes())
}

#[test]
fn identical_events_collapse_even_when_cas_sha_and_session_are_missing() {
    let env = Env::empty();
    let path = env.paths.memory_db();
    let db = MemoryDb::open(&path).unwrap();
    // NULLs are distinct inside a plain UNIQUE index; the NOT NULL key must not be fooled.
    let first = db
        .index(Kind::Command, &s("ls"), &s("ok"), None, None)
        .unwrap();
    assert!(matches!(first, Indexed::New(_)));
    for _ in 0..3 {
        assert_eq!(
            db.index(Kind::Command, &s("ls"), &s("ok"), None, None)
                .unwrap(),
            Indexed::Duplicate
        );
    }
    assert_eq!(count(&path), 1);
}

#[test]
fn distinct_events_over_the_same_output_each_keep_their_own_row() {
    let env = Env::empty();
    let path = env.paths.memory_db();
    let db = MemoryDb::open(&path).unwrap();
    let out = sha("the same output");
    let base =
        |db: &MemoryDb, kind, title: &str, body: &str, cas: Option<&str>, sess: Option<&str>| {
            db.index(kind, &s(title), &s(body), cas, sess.map(s).as_ref())
                .unwrap()
        };
    let rows = [
        base(
            &db,
            Kind::Command,
            "cargo test",
            "exit 0",
            Some(&out),
            Some("a"),
        ),
        // same output, different command
        base(
            &db,
            Kind::Command,
            "cargo nextest",
            "exit 0",
            Some(&out),
            Some("a"),
        ),
        // same command and output, different session
        base(
            &db,
            Kind::Command,
            "cargo test",
            "exit 0",
            Some(&out),
            Some("b"),
        ),
        // same everything but the summary (another exit status)
        base(
            &db,
            Kind::Command,
            "cargo test",
            "exit 101",
            Some(&out),
            Some("a"),
        ),
        // a different kind
        base(
            &db,
            Kind::Read,
            "cargo test",
            "exit 0",
            Some(&out),
            Some("a"),
        ),
        // different output
        base(
            &db,
            Kind::Command,
            "cargo test",
            "exit 0",
            Some(&sha("other")),
            Some("a"),
        ),
        // no output reference / no session
        base(&db, Kind::Command, "cargo test", "exit 0", None, Some("a")),
        base(&db, Kind::Command, "cargo test", "exit 0", Some(&out), None),
    ];
    assert!(
        rows.iter().all(|r| matches!(r, Indexed::New(_))),
        "{rows:?}"
    );
    assert_eq!(count(&path), 8);
    // ...and the exact repeat of the first one is the only duplicate.
    assert_eq!(
        base(
            &db,
            Kind::Command,
            "cargo test",
            "exit 0",
            Some(&out),
            Some("a")
        ),
        Indexed::Duplicate
    );
    // Field boundaries are length-prefixed: shifting a character between fields is not a repeat.
    assert!(matches!(
        base(
            &db,
            Kind::Command,
            "cargo tes",
            "t exit 0",
            Some(&out),
            Some("a")
        ),
        Indexed::New(_)
    ));
}

#[test]
fn concurrent_identical_inserts_leave_one_row() {
    let env = Env::empty();
    let path = env.paths.memory_db();
    MemoryDb::open(&path).unwrap();
    let path = Arc::new(path);
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let path = Arc::clone(&path);
            std::thread::spawn(move || {
                let db = MemoryDb::open(&path).unwrap();
                (0..10)
                    .filter(|_| {
                        matches!(
                            db.index(
                                Kind::Command,
                                &s("race"),
                                &s("same"),
                                None,
                                Some(&s("sess"))
                            )
                            .unwrap(),
                            Indexed::New(_)
                        )
                    })
                    .count()
            })
        })
        .collect();
    let inserted: usize = threads.into_iter().map(|t| t.join().unwrap()).sum();
    assert_eq!(inserted, 1, "exactly one insert wins");
    assert_eq!(count(&path), 1);
}

#[test]
fn concurrent_first_opens_all_succeed_and_agree_on_the_schema() {
    let env = Env::empty();
    let path = Arc::new(env.paths.memory_db());
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let path = Arc::clone(&path);
            std::thread::spawn(move || MemoryDb::open(&path).map(drop))
        })
        .collect();
    for t in threads {
        t.join().unwrap().expect("a concurrent first open failed");
    }
    let conn = Connection::open(&*path).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
    let indexes: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='entries_dedupe'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(indexes, 1);
}

#[test]
fn migration_from_v1_dedupes_deterministically_and_keeps_search_working() {
    let env = Env::empty();
    let path = env.paths.memory_db();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        let sha = sha("o");
        let insert = |title: &str, body: &str, cas: Option<&str>, sess: Option<&str>| {
            conn.execute(
                "INSERT INTO entries(kind, title, body, cas_sha, session, created_ms)
                 VALUES ('command', ?1, ?2, ?3, ?4, 1)",
                rusqlite::params![title, body, cas, sess],
            )
            .unwrap();
        };
        insert("build", "ok", Some(&sha), None); // id 1: kept
        insert("build", "ok", Some(&sha), None); // id 2: duplicate (NULL session)
        insert("build", "ok", Some(&sha), Some("s")); // id 3: different session, kept
        insert("build", "ok", Some(&sha), None); // id 4: duplicate of 1
        insert("test", "failed", None, None); // id 5: kept
        insert("test", "failed", None, None); // id 6: duplicate of 5
    }
    let db = MemoryDb::open(&path).unwrap();
    let conn = Connection::open(&path).unwrap();
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM entries ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(ids, [1, 3, 5], "lowest id of each group survives");
    let empty_keys: i64 = conn
        .query_row(
            "SELECT count(*) FROM entries WHERE dedupe_key = ''",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(empty_keys, 0);

    // The deleted rows are gone from the full-text index too.
    assert_eq!(db.search("build", 10).unwrap().len(), 2);
    assert_eq!(db.search("failed", 10).unwrap().len(), 1);
    // The migrated rows now dedupe against new inserts, and a second open is a no-op.
    assert_eq!(
        db.index(Kind::Command, &s("test"), &s("failed"), None, None)
            .unwrap(),
        Indexed::Duplicate
    );
    drop(db);
    MemoryDb::open(&path).unwrap();
    assert_eq!(count(&path), 3);
}

#[test]
fn a_database_from_a_newer_version_is_refused() {
    let env = Env::empty();
    let path = env.paths.memory_db();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    assert!(matches!(
        MemoryDb::open(&path),
        Err(MemoryError::UnsupportedVersion(99))
    ));
}

#[test]
fn metadata_is_validated_and_the_session_is_scrubbed_text() {
    let env = Env::empty();
    let db = MemoryDb::open(&env.paths.memory_db()).unwrap();
    for bad in [
        "",
        "deadbeef",
        &"A".repeat(64),
        &format!("{}\n", "a".repeat(63)),
        &"g".repeat(64),
        &"a".repeat(65),
        "../../etc/passwd",
    ] {
        assert!(
            matches!(
                db.index(Kind::Read, &s("t"), &s("b"), Some(bad), None),
                Err(MemoryError::InvalidMetadata("cas_sha"))
            ),
            "{bad:?}"
        );
    }
    let long = s(&"x".repeat(257));
    assert!(matches!(
        db.index(Kind::Read, &s("t"), &s("b"), None, Some(&long)),
        Err(MemoryError::InvalidMetadata("session"))
    ));
    db.index(
        Kind::Read,
        &s("t"),
        &s("b"),
        Some(&"a".repeat(64)),
        Some(&s(&"x".repeat(256))),
    )
    .unwrap();

    // A secret in the session label is redacted before it reaches the file, like title and body.
    let secret = ["AK", "IA"].concat() + &"Q7".repeat(8);
    let path = env.paths.memory_db();
    db.index(
        Kind::Command,
        &s("deploy"),
        &s("ok"),
        None,
        Some(&scrub(format!("run {secret}").as_bytes())),
    )
    .unwrap();
    let conn = Connection::open(&path).unwrap();
    let stored: String = conn
        .query_row(
            "SELECT session FROM entries WHERE title = 'deploy'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, "run [REDACTED:aws-access-key-id]");
    drop(db);
    for entry in fs::read_dir(path.parent().unwrap()).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        assert!(!bytes.windows(secret.len()).any(|w| w == secret.as_bytes()));
    }
}

#[cfg(unix)]
mod unix {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn db_wal_shm_and_directory_are_private_even_under_umask_0() {
        // umask is process-wide; nextest runs each test in its own process, and it is restored.
        let env = Env::empty();
        let path = env.paths.memory_db();
        let old = unsafe { libc::umask(0) };
        let db = MemoryDb::open(&path);
        let wrote = match &db {
            Ok(db) => Some(db.index(Kind::Command, &s("a"), &s("b"), None, None)),
            Err(_) => None,
        };
        unsafe { libc::umask(old) };
        let db = db.unwrap();
        wrote.unwrap().unwrap();

        let dir = path.parent().unwrap();
        assert_eq!(mode(dir), 0o700);
        assert_eq!(mode(&path), 0o600);
        let mut wal_seen = 0;
        for suffix in ["-wal", "-shm"] {
            let side = dir.join(format!("memory.db{suffix}"));
            if side.exists() {
                wal_seen += 1;
                assert_eq!(mode(&side), 0o600, "{suffix}");
            }
        }
        assert_eq!(
            wal_seen, 2,
            "WAL and SHM exist while the connection is open"
        );
        drop(db);
    }

    #[test]
    fn a_symlinked_or_fifo_database_path_is_refused_without_hanging() {
        let env = Env::empty();
        let dir = env.paths.project_data_dir();
        fs::create_dir_all(&dir).unwrap();

        let victim = env.dir.path().join("victim.db");
        fs::write(&victim, "precious").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join("memory.db")).unwrap();
        assert!(MemoryDb::open(&dir.join("memory.db")).is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"precious");

        let fifo = dir.join("fifo.db");
        let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(MemoryDb::open(&fifo).is_err());
        });
        assert!(
            rx.recv_timeout(Duration::from_secs(10))
                .expect("blocked on a FIFO")
        );
    }
}
