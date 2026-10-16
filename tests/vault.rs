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
        "aB3/".repeat(10),                            // AWS secret access key value
        "abcdefghijklmnop1234567890".to_string(),     // bearer token
        "hunter2hunter2".to_string(),                 // dotenv value
        "MIIEvQIBADANBgkqhkiG9w0BAQEFAASC".repeat(6), // PEM body
    ]
}

fn dirty_output(secrets: &[String]) -> String {
    format!(
        "$ deploy\nid={}\nkey: {}\nOPENAI={}\nproj={}\nAWS_SECRET_ACCESS_KEY={}\n\
         Authorization: Bearer {}\nDB_PASSWORD={}\n-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\ndone\n",
        secrets[0],
        secrets[1],
        secrets[2],
        secrets[3],
        secrets[4],
        secrets[5],
        secrets[6],
        secrets[7]
    )
}

fn files_in(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.path().is_file())
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
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
    files_in(dir)
        .iter()
        .filter(|(n, _)| n.ends_with(".log"))
        .count()
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
    assert_eq!(
        cas.read(&first.short_id, None, later).unwrap(),
        b"same output\n"
    );
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

    // Two entries sharing a prefix -> ambiguous, listing both by their (8+ hex) emitted IDs.
    let (a, a_content) = plant_live_with_prefix(&dir, "ab12", "a");
    let (b, _) = plant_live_with_prefix(&dir, "ab12", "b");
    let mut want = [a[..8].to_string(), b[..8].to_string()];
    want.sort();
    match cas.read("#ab12", None, now) {
        Err(CasError::Ambiguous { candidates, .. }) => {
            let ids: Vec<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
            assert_eq!(ids, want);
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
    let msg = cas.read("#ab12", None, now).unwrap_err().to_string();
    assert!(
        msg.contains(&format!("#{}", want[0])) && msg.contains(&format!("#{}", want[1])),
        "{msg}"
    );
    // A longer prefix disambiguates.
    assert_eq!(cas.read(&a[..8], None, now).unwrap(), a_content.as_bytes());

    assert!(matches!(
        cas.read("#0000", None, now),
        Err(CasError::NotFound(_))
    ));
    assert!(matches!(
        cas.read("#xyz1", None, now),
        Err(CasError::InvalidId(_))
    ));
    assert!(matches!(
        cas.read("#4f9", None, now),
        Err(CasError::InvalidId(_))
    ));
}

#[test]
fn line_slicing_is_one_based_and_inclusive() {
    let env = Env::empty();
    let cas = Cas::new(env.paths.cas_dir());
    let now = SystemTime::now();
    let put = put_text(&cas, "l1\nl2\nl3\nl4\nl5\n", now);
    let read = |range: &str| {
        cas.read(&put.short_id, Some(LineRange::parse(range).unwrap()), now)
            .unwrap()
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
    assert!(matches!(
        cas.read(&stale.sha, None, now),
        Err(CasError::Expired(_))
    ));

    // GC on write: a put deletes another stale entry.
    let older = put_text(&cas, "older\n", now - 9 * DAY);
    assert!(dir.join(format!("{}.log", older.sha)).exists()); // 9d is only stale relative to a later `now`
    let fresh = put_text(&cas, "fresh\n", now);
    assert!(!dir.join(format!("{}.log", older.sha)).exists());
    assert!(dir.join(format!("{}.expired", older.sha)).exists());
    assert_eq!(cas.read(&fresh.short_id, None, now).unwrap(), b"fresh\n");

    // A never-seen ID is a different error.
    assert!(matches!(
        cas.read("#0000", None, now),
        Err(CasError::NotFound(_))
    ));

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
        assert_eq!(
            mode(&env.paths.cas_dir().join(format!("{}.log", put.sha))),
            0o600
        );
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
            &s(&format!(
                "unrelated chatter about topic number {i} and nothing else"
            )),
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
    let body_only = db
        .index(Kind::Read, &s("build log"), &s(&long_body), None, None)
        .unwrap();
    let title_only = db
        .index(
            Kind::Decision,
            &s("cargo test notes"),
            &s("remember to check flags"),
            None,
            None,
        )
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
    db.index(
        Kind::Command,
        &s("cargo build"),
        &s("compiled fine"),
        None,
        None,
    )
    .unwrap();

    let hits = db.search("cargo test", 10).unwrap();
    let ids: Vec<i64> = hits.iter().map(|h| h.id).collect();
    assert_eq!(ids, vec![dense, title_only, body_only], "{hits:#?}");
    // bm25() is lower-is-better, so scores must be strictly ascending.
    assert!(
        hits.windows(2).all(|w| w[0].score < w[1].score),
        "{hits:#?}"
    );

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

// ---- vault identity: old IDs never resolve to an unrelated entry ----------

/// `<prefix><fill repeated>` as a 64-hex sha.
fn sha_like(prefix: &str, fill: char) -> String {
    format!("{prefix}{}", fill.to_string().repeat(64 - prefix.len()))
}

fn plant_live(dir: &Path, sha: &str, content: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(format!("{sha}.log")), content).unwrap();
}

/// Plant a live entry whose bytes really hash to a name starting with `prefix` (reads verify the
/// hash now, so a made-up name with arbitrary content would be reported as corrupt). Searching a
/// 4-hex prefix takes ~65k hashes. Returns `(sha, content)`.
fn plant_live_with_prefix(dir: &Path, prefix: &str, salt: &str) -> (String, String) {
    for i in 0u64.. {
        let content = format!("{salt} #{i}\n");
        let sha = agentdiet::fsutil::sha256_hex(content.as_bytes());
        if sha.starts_with(prefix) {
            plant_live(dir, &sha, &content);
            return (sha, content);
        }
    }
    unreachable!()
}

fn plant_tombstone(dir: &Path, sha: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(format!("{sha}.expired")), "").unwrap();
}

fn ambiguity(err: CasError) -> (String, Vec<(String, bool)>) {
    match err {
        CasError::Ambiguous { prefix, candidates } => (
            prefix,
            candidates.into_iter().map(|c| (c.id, c.expired)).collect(),
        ),
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn an_expired_id_never_returns_a_different_live_entry() {
    // The reproduced bug: `show abcd` returned the unrelated live content.
    let env = Env::empty();
    let dir = env.paths.cas_dir();
    let cas = Cas::new(dir.clone());
    let now = SystemTime::now();
    let old = sha_like("abcd", '1');
    plant_tombstone(&dir, &old);
    let (live, live_content) = plant_live_with_prefix(&dir, "abcd", "unrelated live output");

    let (prefix, candidates) = ambiguity(cas.read("abcd", None, now).unwrap_err());
    assert_eq!(prefix, "abcd");
    // Emitted IDs are at least 8 hex: the tombstone is `abcd1111…`.
    let mut want = [
        ("abcd1111".to_string(), true),
        (live[..8].to_string(), false),
    ];
    want.sort();
    assert_eq!(candidates, want);
    let msg = cas.read("#abcd", None, now).unwrap_err().to_string();
    assert!(
        msg.contains("#abcd1111 (expired)") && msg.contains(&format!("#{}", &live[..8])),
        "{msg}"
    );

    // The hint prefixes work: one is expired, the other is the live entry.
    assert!(matches!(
        cas.read("abcd1111", None, now),
        Err(CasError::Expired(_))
    ));
    assert_eq!(
        cas.read(&live[..8], None, now).unwrap(),
        live_content.as_bytes()
    );

    // For every prefix length of the expired sha, the live decoy is never returned.
    for len in 4..=64 {
        let id = &old[..len];
        let result = cas.read(id, None, now);
        assert!(result.is_err(), "prefix {len} returned {result:?}");
    }
    assert_eq!(
        files_in(&dir).iter().filter(|(n, _)| n != ".lock").count(),
        2,
        "lookups must not create or delete entries"
    );
}

#[test]
fn collisions_between_live_and_expired_entries_are_classified() {
    let env = Env::empty();
    let dir = env.paths.cas_dir();
    let cas = Cas::new(dir.clone());
    let now = SystemTime::now();

    // live / live
    let (one, _) = plant_live_with_prefix(&dir, "1111", "one");
    let (two, _) = plant_live_with_prefix(&dir, "1111", "two");
    let (_, c) = ambiguity(cas.read("1111", None, now).unwrap_err());
    let mut want = [(one[..8].to_string(), false), (two[..8].to_string(), false)];
    want.sort();
    assert_eq!(c, want);

    // expired / live
    plant_tombstone(&dir, &sha_like("2222", 'a'));
    let (three, _) = plant_live_with_prefix(&dir, "2222", "three");
    let (_, c) = ambiguity(cas.read("2222", None, now).unwrap_err());
    let mut want = [
        ("2222aaaa".to_string(), true),
        (three[..8].to_string(), false),
    ];
    want.sort();
    assert_eq!(c, want);

    // expired / expired: every meaning is gone, so "expired", and no content.
    plant_tombstone(&dir, &sha_like("3333", 'a'));
    plant_tombstone(&dir, &sha_like("3333", 'b'));
    assert!(matches!(
        cas.read("3333", None, now),
        Err(CasError::Expired(_))
    ));
    assert!(matches!(
        cas.read("3333aaaa", None, now),
        Err(CasError::Expired(_))
    ));

    // A never-issued prefix is still "not found".
    assert!(matches!(
        cas.read("4444", None, now),
        Err(CasError::NotFound(_))
    ));
}

#[test]
fn new_entries_get_short_ids_that_are_unique_against_tombstones() {
    let env = Env::empty();
    let dir = env.paths.cas_dir();
    let cas = Cas::new(dir.clone());
    let now = SystemTime::now();

    let sha = agentdiet::fsutil::sha256_hex(b"fresh output\n");
    let old = format!("{}{}", &sha[..8], "0".repeat(56));
    plant_tombstone(&dir, &old);

    let put = put_text(&cas, "fresh output\n", now);
    assert_eq!(put.sha, sha);
    assert!(
        put.short_id.len() > 8,
        "short id {} collides with a tombstone",
        put.short_id
    );
    assert!(put.sha.starts_with(&put.short_id));
    assert_eq!(
        cas.read(&put.short_id, None, now).unwrap(),
        b"fresh output\n"
    );
    // The old 8-char breadcrumb can no longer silently mean the new entry.
    assert!(matches!(
        cas.read(&sha[..8], None, now),
        Err(CasError::Ambiguous { .. })
    ));
}

#[test]
fn reputting_identical_content_revives_it_and_hints_stay_deterministic() {
    let env = Env::empty();
    let dir = env.paths.cas_dir();
    let cas = Cas::new(dir.clone());
    let now = SystemTime::now();

    let sha = agentdiet::fsutil::sha256_hex(b"comes back\n");
    let rival = format!("{}{}", &sha[..8], "f".repeat(56));
    plant_live(&dir, &rival, "rival");

    // Expire it, then put the same bytes again.
    let first = put_text(&cas, "comes back\n", now - 8 * DAY);
    assert!(matches!(
        cas.read(&first.sha, None, now),
        Err(CasError::Expired(_))
    ));
    let again = put_text(&cas, "comes back\n", now);
    assert_eq!(again.sha, first.sha);
    assert!(!dir.join(format!("{}.expired", sha)).exists());
    assert_eq!(
        files_in(&dir)
            .iter()
            .filter(|(n, _)| n.ends_with(".log"))
            .count(),
        2
    );

    // Its (longer) new ID works; the old 4-char one is an ambiguity with the rival.
    assert!(again.short_id.len() > 8);
    assert_eq!(
        cas.read(&again.short_id, None, now).unwrap(),
        b"comes back\n"
    );
    let first_err = cas.read(&sha[..8], None, now).unwrap_err().to_string();
    for _ in 0..5 {
        assert_eq!(
            cas.read(&sha[..8], None, now).unwrap_err().to_string(),
            first_err
        );
    }
    let (_, candidates) = ambiguity(cas.read(&sha[..8], None, now).unwrap_err());
    let ids: Vec<&str> = candidates.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids.len(), 2);
    assert!(
        ids.windows(2).all(|w| w[0] < w[1]),
        "hints are sorted: {ids:?}"
    );
    assert!(candidates.iter().all(|(_, expired)| !expired));
}

#[test]
fn a_log_with_a_leftover_tombstone_is_live() {
    // An interrupted GC can leave both files; the log wins.
    let env = Env::empty();
    let dir = env.paths.cas_dir();
    let cas = Cas::new(dir.clone());
    let (sha, content) = plant_live_with_prefix(&dir, "9a9a", "still here");
    plant_tombstone(&dir, &sha);
    assert_eq!(
        cas.read("9a9a", None, SystemTime::now()).unwrap(),
        content.as_bytes()
    );
}

// ---- vault entries are plain files: symlinks are never followed ------------

#[cfg(unix)]
mod symlinks {
    use std::os::unix::fs::symlink;

    use super::*;

    fn victim(env: &Env) -> std::path::PathBuf {
        let path = env.dir.path().join("victim.txt");
        fs::write(&path, "precious").unwrap();
        backdate(&path, 3 * DAY);
        path
    }

    fn mtime(path: &Path) -> SystemTime {
        fs::metadata(path).unwrap().modified().unwrap()
    }

    #[test]
    fn put_over_a_planted_symlink_replaces_it_and_leaves_the_target_alone() {
        let env = Env::empty();
        let dir = env.paths.cas_dir();
        let cas = Cas::new(dir.clone());
        let victim = victim(&env);
        let before = mtime(&victim);

        let sha = agentdiet::fsutil::sha256_hex(b"payload\n");
        fs::create_dir_all(&dir).unwrap();
        symlink(&victim, dir.join(format!("{sha}.log"))).unwrap();

        // Before the put, the symlink is not an entry at all.
        assert!(matches!(
            cas.read(&sha[..8], None, SystemTime::now()),
            Err(CasError::NotFound(_))
        ));

        let put = put_text(&cas, "payload\n", SystemTime::now());
        assert_eq!(fs::read(&victim).unwrap(), b"precious");
        assert_eq!(
            mtime(&victim),
            before,
            "the symlink target's mtime was touched"
        );
        let log = dir.join(format!("{}.log", put.sha));
        assert!(!fs::symlink_metadata(&log).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&log).unwrap(), b"payload\n");
    }

    #[test]
    fn show_never_reads_through_a_symlinked_entry() {
        let env = Env::empty();
        let dir = env.paths.cas_dir();
        let cas = Cas::new(dir.clone());
        let victim = victim(&env);
        let sha = sha_like("5e5e", '0');
        fs::create_dir_all(&dir).unwrap();
        symlink(&victim, dir.join(format!("{sha}.log"))).unwrap();

        let err = cas.read("5e5e", None, SystemTime::now()).unwrap_err();
        assert!(matches!(err, CasError::NotFound(_)), "{err:?}");
    }

    #[test]
    fn gc_does_not_truncate_through_a_planted_tombstone_symlink() {
        let env = Env::empty();
        let dir = env.paths.cas_dir();
        let cas = Cas::new(dir.clone());
        let victim = victim(&env);
        let before = mtime(&victim);
        let now = SystemTime::now();

        let stale = put_text(&cas, "old output\n", now - 8 * DAY);
        symlink(&victim, dir.join(format!("{}.expired", stale.sha))).unwrap();

        assert_eq!(cas.gc(now).unwrap(), 1);
        assert_eq!(fs::read(&victim).unwrap(), b"precious");
        assert_eq!(mtime(&victim), before);
        let tomb = dir.join(format!("{}.expired", stale.sha));
        assert!(
            !fs::symlink_metadata(&tomb)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::metadata(&tomb).unwrap().len(), 0);
        assert!(matches!(
            cas.read(&stale.sha, None, now),
            Err(CasError::Expired(_))
        ));
    }

    #[test]
    fn a_dedupe_hit_does_not_trust_corrupted_contents() {
        let env = Env::empty();
        let dir = env.paths.cas_dir();
        let cas = Cas::new(dir.clone());
        let now = SystemTime::now();
        let put = put_text(&cas, "the real output\n", now);
        let log = dir.join(format!("{}.log", put.sha));
        fs::write(&log, "tampered").unwrap();

        let again = put_text(&cas, "the real output\n", now);
        assert_eq!(again.sha, put.sha);
        assert_eq!(fs::read(&log).unwrap(), b"the real output\n");
    }
}

// ---- 3B storage: integrity, concurrency, bounded waits ---------------------

fn log_path(env: &Env, sha: &str) -> std::path::PathBuf {
    env.paths.cas_dir().join(format!("{sha}.log"))
}

#[test]
fn emitted_ids_are_at_least_eight_hex_and_input_ids_at_least_four() {
    let env = Env::empty();
    let cas = Cas::new(env.paths.cas_dir());
    let put = put_text(&cas, "an output\n", SystemTime::now());
    assert!(put.short_id.len() >= 8, "{}", put.short_id);
    assert!(put.sha.starts_with(&put.short_id));
    // A 4-hex input still resolves when it is unique.
    assert_eq!(
        cas.read(&put.sha[..4], None, SystemTime::now()).unwrap(),
        b"an output\n"
    );
    assert!(matches!(
        cas.read(&put.sha[..3], None, SystemTime::now()),
        Err(CasError::InvalidId(_))
    ));
}

#[test]
fn a_corrupted_entry_is_corrupt_not_missing_and_leaks_no_bytes() {
    let env = Env::empty();
    let cas = Cas::new(env.paths.cas_dir());
    let now = SystemTime::now();
    let put = put_text(&cas, "line one\nline two\nline three\n", now);
    let log = log_path(&env, &put.sha);

    // Bit rot / a torn write: same name, different bytes (here: the last byte flipped).
    let mut bytes = fs::read(&log).unwrap();
    *bytes.last_mut().unwrap() ^= 0x01;
    fs::write(&log, &bytes).unwrap();

    for range in [None, Some(LineRange::parse("1..2").unwrap())] {
        let err = cas.read(&put.short_id, range, now).unwrap_err();
        assert!(matches!(err, CasError::Corrupt(_)), "{err:?}");
        let text = err.to_string();
        assert!(text.contains("corrupt"), "{text}");
        assert!(
            !text.contains("line one"),
            "no content in the error: {text}"
        );
        assert!(
            !text.contains("not found") && !text.contains("expired"),
            "{text}"
        );
    }
    // Truncated to nothing is corrupt too.
    fs::write(&log, b"").unwrap();
    assert!(matches!(
        cas.read(&put.short_id, None, now),
        Err(CasError::Corrupt(_))
    ));

    // Putting the same content again repairs the entry.
    let again = put_text(&cas, "line one\nline two\nline three\n", now);
    assert_eq!(again, put);
    assert_eq!(
        cas.read(&put.short_id, None, now).unwrap(),
        b"line one\nline two\nline three\n"
    );
}

#[test]
fn the_show_command_reports_corruption_and_prints_nothing() {
    let env = Env::empty();
    let cas = Cas::new(env.paths.cas_dir());
    let put = put_text(&cas, "secretless but precious\n", SystemTime::now());
    fs::write(log_path(&env, &put.sha), b"tampered\n").unwrap();
    let exe = env!("CARGO_BIN_EXE_agentdiet");
    let out = std::process::Command::new(exe)
        .args(["show", &format!("#{}", put.short_id)])
        .env("HOME", &env.paths.home)
        .env("XDG_CONFIG_HOME", &env.paths.config_dir)
        .current_dir(&env.paths.project_root)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "no bytes on stdout");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("corrupt") && !err.contains("tampered"),
        "{err}"
    );
}

#[test]
fn reading_a_vault_that_was_never_written_creates_nothing() {
    let env = Env::empty();
    let cas = Cas::new(env.paths.cas_dir());
    assert!(matches!(
        cas.read("abcd1234", None, SystemTime::now()),
        Err(CasError::NotFound(_))
    ));
    assert_eq!(cas.gc(SystemTime::now()).unwrap(), 0);
    assert!(!env.paths.cas_dir().exists());
}

/// The race the lock exists for: one thread keeps re-putting content (refreshing its mtime) while
/// others run GC with a clock far in the future. Without the lock a GC can scan the entry as
/// stale and delete it after the put has refreshed it, leaving the fresh breadcrumb dangling.
#[test]
fn gc_never_deletes_content_a_concurrent_put_just_refreshed() {
    let env = Env::empty();
    let dir = env.paths.cas_dir();
    let cas = Cas::new(dir.clone()).lock_timeout(Duration::from_secs(60));
    let real_now = SystemTime::now();
    // Stored 8 days ago, so a GC at `real_now` considers it stale until a put refreshes it.
    let seed = put_text(&cas, "contended\n", real_now - 8 * DAY);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let collectors: Vec<_> = (0..3)
        .map(|_| {
            let (cas, stop) = (cas.clone(), std::sync::Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    cas.gc(real_now).unwrap();
                }
            })
        })
        .collect();
    for round in 0..40 {
        // Age the entry back, put it (refresh to real_now), and demand it is readable immediately.
        let log = log_path(&env, &seed.sha);
        if log.exists() {
            backdate(&log, 8 * DAY);
        }
        let put = cas.put(&scrub(b"contended\n"), real_now).unwrap();
        let read = cas.read(&put.short_id, None, real_now);
        assert_eq!(
            read.unwrap_or_else(|e| panic!("round {round}: dangling right after put: {e}")),
            b"contended\n"
        );
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for c in collectors {
        c.join().unwrap();
    }
}

#[test]
fn concurrent_puts_of_different_content_all_survive() {
    let env = Env::empty();
    let cas = Cas::new(env.paths.cas_dir()).lock_timeout(Duration::from_secs(60));
    let now = SystemTime::now();
    let threads: Vec<_> = (0..4)
        .map(|t| {
            let cas = cas.clone();
            std::thread::spawn(move || {
                (0..10)
                    .map(|i| {
                        let text = format!("thread {t} item {i}\n");
                        (text.clone(), cas.put(&scrub(text.as_bytes()), now).unwrap())
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let puts: Vec<_> = threads
        .into_iter()
        .flat_map(|t| t.join().unwrap())
        .collect();
    assert_eq!(puts.len(), 40);
    for (text, put) in puts {
        assert_eq!(cas.read(&put.sha, None, now).unwrap(), text.as_bytes());
    }
}

#[test]
fn a_held_vault_lock_makes_operations_fail_busy_within_the_timeout() {
    let env = Env::empty();
    let dir = env.paths.cas_dir();
    let cas = Cas::new(dir.clone()).lock_timeout(Duration::from_millis(100));
    let put = put_text(&cas, "x\n", SystemTime::now());
    let held =
        agentdiet::fsutil::lock_exclusive(&dir.join(".lock"), Duration::from_secs(1)).unwrap();

    let started = std::time::Instant::now();
    let now = SystemTime::now();
    assert!(matches!(
        cas.put(&scrub(b"y\n"), now),
        Err(CasError::Busy(_))
    ));
    assert!(matches!(
        cas.read(&put.short_id, None, now),
        Err(CasError::Busy(_))
    ));
    assert!(matches!(
        cas.resolve(&put.short_id, now),
        Err(CasError::Busy(_))
    ));
    assert!(matches!(cas.gc(now), Err(CasError::Busy(_))));
    assert!(started.elapsed() < Duration::from_secs(2), "bounded wait");
    assert_eq!(live_logs(&dir), 1, "nothing was written while busy");

    drop(held);
    assert_eq!(cas.read(&put.short_id, None, now).unwrap(), b"x\n");
}

/// A FIFO swapped in where an entry's log should be must never block a put or a read.
#[cfg(unix)]
#[test]
fn a_fifo_in_place_of_an_entry_never_blocks() {
    use std::os::unix::ffi::OsStrExt;
    let env = Env::empty();
    let dir = env.paths.cas_dir();
    let cas = Cas::new(dir.clone());
    let sha = agentdiet::fsutil::sha256_hex(b"payload\n");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{sha}.log"));
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let now = SystemTime::now();
        // Not an entry (scan ignores non-regular files), so a read says "not found"...
        let before = cas.read(&sha[..8], None, now);
        // ...and a put replaces the FIFO with a regular file.
        let put = cas.put(&scrub(b"payload\n"), now);
        let after = cas.read(&sha[..8], None, now);
        let _ = tx.send((before, put, after));
    });
    let (before, put, after) = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("blocked on a FIFO");
    assert!(matches!(before, Err(CasError::NotFound(_))));
    put.unwrap();
    assert_eq!(after.unwrap(), b"payload\n");
}

/// Child half of [`two_processes_putting_and_collecting_do_not_corrupt_the_vault`].
#[test]
#[ignore = "child-process helper; run by two_processes_putting_and_collecting_do_not_corrupt_the_vault"]
fn vault_child_hammers() {
    let Some(dir) = std::env::var_os("AD_VAULT_HAMMER_DIR") else {
        return;
    };
    let id = std::env::var("AD_VAULT_HAMMER_ID").unwrap();
    let cas = Cas::new(dir.into()).lock_timeout(Duration::from_secs(60));
    let now = SystemTime::now();
    for i in 0..15 {
        let text = format!("child {id} item {i}\n");
        let put = cas.put(&scrub(text.as_bytes()), now).unwrap();
        assert_eq!(cas.read(&put.short_id, None, now).unwrap(), text.as_bytes());
        cas.gc(now + DAY).unwrap(); // never expires anything: entries are fresh
    }
}

#[test]
fn two_processes_putting_and_collecting_do_not_corrupt_the_vault() {
    let env = Env::empty();
    let exe = std::env::current_exe().unwrap();
    let children: Vec<_> = (0..3)
        .map(|n| {
            std::process::Command::new(&exe)
                .args(["--ignored", "--exact", "vault_child_hammers", "--nocapture"])
                .env("AD_VAULT_HAMMER_DIR", env.paths.cas_dir())
                .env("AD_VAULT_HAMMER_ID", n.to_string())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(live_logs(&env.paths.cas_dir()), 45);
}
