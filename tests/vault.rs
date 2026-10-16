//! Slice 2 end-to-end: scrubber -> CAS vault -> memory index, all inside a TempDir.
//!
//! Synthetic keys are assembled at runtime so this file never contains a
//! literal key-shaped string (GitHub push protection, secret scanners).

mod common;

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::time::{Duration, SystemTime};

use agentdiet::cas::{Cas, CasError, LineRange};
use agentdiet::memory::{Kind, MemoryDb};
use agentdiet::scrubber::{Scrubbed, scrub};
use common::Env;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn synthetic_secrets() -> Vec<String> {
    let half = "x_Y-".repeat(18) + "zz";
    vec![
        ["AK", "IA"].concat() + &"Q7".repeat(8),
        ["sk-", "ant-", "api03-"].concat() + &"Ab1_-".repeat(19) + "AA",
        ["sk-", &"a1B2".repeat(5), "T3Blbk", "FJ", &"c3D4".repeat(5)].concat(),
        ["sk-", "proj-", &half, "T3Blbk", "FJ", &half].concat(),
        "aB3/".repeat(10), // AWS secret access key value
        "abcdefghijklmnop1234567890".to_string(), // bearer token
        "hunter2hunter2".to_string(), // dotenv value
        "MIIEvQIBADANBgkqhkiG9w0BAQEFAASC".repeat(6), // PEM body
    ]
}

fn dirty_output(secrets: &[String]) -> String {
    format!(
        "$ deploy\nid={}\nkey: {}\nOPENAI={}\nproj={}\nAWS_SECRET_ACCESS_KEY={}\n\
         Authorization: Bearer {}\nDB_PASSWORD={}\n-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\ndone\n",
        secrets[0], secrets[1], secrets[2], secrets[3], secrets[4], secrets[5], secrets[6], secrets[7]
    )
}

fn files_in(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.path().is_file())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), fs::read(e.path()).unwrap()))
        .collect()
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
}

fn backdate(path: &Path, by: Duration) {
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - by)
        .unwrap();
}

fn put_text(cas: &Cas, text: &str, now: SystemTime) -> agentdiet::cas::PutResult {
    cas.put(&scrub(text.as_bytes()), now).unwrap()
}

fn live_logs(dir: &Path) -> usize {
    files_in(dir).iter().filter(|(n, _)| n.ends_with(".log")).count()
}

#[test]
fn secrets_never_reach_the_vault_or_the_memory_db() {
    let env = Env::empty();
    let secrets = synthetic_secrets();
    let raw = dirty_output(&secrets);
    // Control: the raw output really does contain every secret.
    for s in &secrets {
        assert!(raw.contains(s.as_str()));
    }

    let scrubbed = scrub(raw.as_bytes());
    assert!(scrubbed.redactions() >= secrets.len() - 1);

    let cas = Cas::new(env.paths.cas_dir());
    let put = cas.put(&scrubbed, SystemTime::now()).unwrap();

    let db = MemoryDb::open(&env.paths.memory_db()).unwrap();
    db.index(
        Kind::Command,
        &scrub(format!("deploy {}", secrets[0]).as_bytes()),
        &scrubbed,
        Some(&put.sha),
        Some("s1"),
    )
    .unwrap();

    // Every byte on disk: vault files, memory.db, and its -wal / -shm (db is still open).
    let mut on_disk = files_in(&env.paths.cas_dir());
    on_disk.extend(files_in(&env.paths.project_data_dir()));
    assert!(on_disk.iter().any(|(n, _)| n.starts_with("memory.db")));
    for (name, bytes) in &on_disk {
        for s in &secrets {
            assert!(!contains(bytes, s), "{name} contains a raw secret");
        }
    }
    let vault = cas.read(&put.short_id, None, SystemTime::now()).unwrap();
    assert!(contains(&vault, "[REDACTED:aws-access-key-id]"));
    assert!(contains(&vault, "[REDACTED:private-key]"));
    assert!(contains(&vault, "$ deploy")); // the rest is lossless
}

#[test]
fn identical_output_dedupes_and_refreshes_the_ttl() {
    let env = Env::empty();
    let cas = Cas::new(env.paths.cas_dir());
    let now = SystemTime::now();

    let first = put_text(&cas, "same output\n", now);
    let second = put_text(&cas, "same output\n", now);
    assert_eq!(first, second);
    assert_eq!(live_logs(&env.paths.cas_dir()), 1);

    // 6 days old, then written again: the 7 days restart.
    let log = env.paths.cas_dir().join(format!("{}.log", first.sha));
    backdate(&log, 6 * DAY);
    put_text(&cas, "same output\n", SystemTime::now());
    // Two days later it would be 8 days old without the refresh.
    let later = SystemTime::now() + 2 * DAY;
    assert_eq!(cas.read(&first.short_id, None, later).unwrap(), b"same output\n");
}

#[test]
fn prefix_recovery_and_ambiguity() {
    let env = Env::empty();
    let dir = env.paths.cas_dir();
    let cas = Cas::new(dir.clone());
    let now = SystemTime::now();
    let target = put_text(&cas, "target\n", now);
    put_text(&cas, "other one\n", now);
    put_text(&cas, "other two\n", now);

    for id in [
        format!("#{}", target.short_id),
        target.short_id.clone(),
        format!("#{}", target.short_id.to_uppercase()),
        target.sha.clone(),
    ] {
        assert_eq!(cas.read(&id, None, now).unwrap(), b"target\n", "{id}");
    }

    // Junk in the directory is ignored by the scan.
    fs::write(dir.join(".abc.agentdiet-tmp-1-2"), "x").unwrap();
    fs::write(dir.join("notasha.log"), "x").unwrap();
    fs::write(dir.join(format!("{}.log", "AB".repeat(32))), "x").unwrap();
    assert_eq!(cas.read(&target.short_id, None, now).unwrap(), b"target\n");

    // Two entries sharing a prefix -> ambiguous, listing both.
    let a = format!("ab12{}", "0".repeat(60));
    let b = format!("ab12{}", "f".repeat(60));
    fs::write(dir.join(format!("{a}.log")), "a").unwrap();
    fs::write(dir.join(format!("{b}.log")), "b").unwrap();
    match cas.read("#ab12", None, now) {
        Err(CasError::Ambiguous { candidates, .. }) => {
            assert_eq!(candidates, vec!["ab120".to_string(), "ab12f".to_string()]);
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
    let msg = cas.read("#ab12", None, now).unwrap_err().to_string();
    assert!(msg.contains("#ab120") && msg.contains("#ab12f"), "{msg}");
    // A longer prefix disambiguates.
    assert_eq!(cas.read("#ab120", None, now).unwrap(), b"a");

    assert!(matches!(cas.read("#0000", None, now), Err(CasError::NotFound(_))));
    assert!(matches!(cas.read("#xyz1", None, now), Err(CasError::InvalidId(_))));
    assert!(matches!(cas.read("#4f9", None, now), Err(CasError::InvalidId(_))));
}

#[test]
fn line_slicing_is_one_based_and_inclusive() {
    let env = Env::empty();
    let cas = Cas::new(env.paths.cas_dir());
    let now = SystemTime::now();
    let put = put_text(&cas, "l1\nl2\nl3\nl4\nl5\n", now);
    let read = |range: &str| {
        cas.read(&put.short_id, Some(LineRange::parse(range).unwrap()), now).unwrap()
    };
    assert_eq!(read("2..4"), b"l2\nl3\nl4\n");
    assert_eq!(read("1..1"), b"l1\n");
    assert_eq!(read("5..50"), b"l5\n");
    assert_eq!(read("9..12"), b"");
    assert!(LineRange::parse("0..3").is_err());
    assert!(LineRange::parse("4..2").is_err());
}

#[test]
fn expired_entries_report_expired_not_not_found() {
    let env = Env::empty();
    let dir = env.paths.cas_dir();
    let cas = Cas::new(dir.clone());
    let now = SystemTime::now();

    // Written 8 days ago (the entry's mtime is the injected `now`).
    let stale = put_text(&cas, "stale\n", now - 8 * DAY);
    assert!(matches!(
        cas.read(&stale.short_id, None, now),
        Err(CasError::Expired(_))
    ));
    assert!(!dir.join(format!("{}.log", stale.sha)).exists());
    assert!(dir.join(format!("{}.expired", stale.sha)).exists());
    // Still "expired" on the next call, and never "not found".
    assert!(matches!(cas.read(&stale.sha, None, now), Err(CasError::Expired(_))));

    // GC on write: a put deletes another stale entry.
    let older = put_text(&cas, "older\n", now - 9 * DAY);
    assert!(dir.join(format!("{}.log", older.sha)).exists()); // 9d is only stale relative to a later `now`
    let fresh = put_text(&cas, "fresh\n", now);
    assert!(!dir.join(format!("{}.log", older.sha)).exists());
    assert!(dir.join(format!("{}.expired", older.sha)).exists());
    assert_eq!(cas.read(&fresh.short_id, None, now).unwrap(), b"fresh\n");

    // A never-seen ID is a different error.
    assert!(matches!(cas.read("#0000", None, now), Err(CasError::NotFound(_))));

    // Storing the same content again revives it and clears the tombstone.
    let again = put_text(&cas, "stale\n", now);
    assert_eq!(again.sha, stale.sha);
    assert!(!dir.join(format!("{}.expired", stale.sha)).exists());
    assert_eq!(cas.read(&stale.sha, None, now).unwrap(), b"stale\n");
}

#[test]
fn vault_files_are_private() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let env = Env::empty();
        let cas = Cas::new(env.paths.cas_dir());
        let put = put_text(&cas, "private\n", SystemTime::now());
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&env.paths.cas_dir()), 0o700);
        assert_eq!(mode(&env.paths.cas_dir().join(format!("{}.log", put.sha))), 0o600);
    }
}

fn s(text: &str) -> Scrubbed {
    scrub(text.as_bytes())
}

#[test]
fn memory_search_is_ranked_by_bm25() {
    let env = Env::empty();
    let db = MemoryDb::open(&env.paths.memory_db()).unwrap();
    assert_eq!(db.journal_mode().unwrap(), "wal");

    // FTS5 clamps IDF to ~0 when a term is in over half the rows, so ranking
    // needs plenty of unrelated rows to be meaningful.
    for i in 0..16 {
        db.index(
            Kind::Read,
            &s(&format!("notes {i}")),
            &s(&format!("unrelated chatter about topic number {i} and nothing else")),
            None,
            None,
        )
        .unwrap();
    }
    let long_body = format!(
        "{} cargo {} test {}",
        "lorem ipsum dolor sit amet ".repeat(12),
        "consectetur adipiscing elit ".repeat(12),
        "sed do eiusmod tempor ".repeat(12)
    );
    let body_only = db.index(Kind::Read, &s("build log"), &s(&long_body), None, None).unwrap();
    let title_only = db
        .index(Kind::Decision, &s("cargo test notes"), &s("remember to check flags"), None, None)
        .unwrap();
    let dense = db
        .index(
            Kind::Command,
            &s("cargo test failed"),
            &s("cargo test failed: 3 errors in cargo test run"),
            Some("deadbeef"),
            Some("s1"),
        )
        .unwrap();
    // Only one of the two terms: excluded by the implicit AND.
    db.index(Kind::Command, &s("cargo build"), &s("compiled fine"), None, None).unwrap();

    let hits = db.search("cargo test", 10).unwrap();
    let ids: Vec<i64> = hits.iter().map(|h| h.id).collect();
    assert_eq!(ids, vec![dense, title_only, body_only], "{hits:#?}");
    // bm25() is lower-is-better, so scores must be strictly ascending.
    assert!(hits.windows(2).all(|w| w[0].score < w[1].score), "{hits:#?}");

    assert_eq!(hits[0].kind, "command");
    assert_eq!(hits[0].cas_sha.as_deref(), Some("deadbeef"));
    assert!(hits[0].snippet.contains('['));

    // limit is honoured; hostile text does not error.
    assert_eq!(db.search("cargo test", 2).unwrap().len(), 2);
    assert!(db.search("full-stack: \"x\" NOT *", 10).unwrap().is_empty());
    assert!(db.search("   ", 10).unwrap().is_empty());

    // Persists across reopen.
    drop(db);
    let db = MemoryDb::open(&env.paths.memory_db()).unwrap();
    assert_eq!(db.search("cargo test", 10).unwrap().len(), 3);
}
