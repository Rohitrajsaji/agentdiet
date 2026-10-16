//! Small filesystem helpers shared by `state.json` and every config write.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// Lowercase hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Milliseconds since the Unix epoch (0 if the clock is before it).
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Atomically replace a user's config file `path` with `bytes`: write a
/// sibling temp file, fsync it, then rename over the target.
///
/// * The temp file lives in the **same directory** so the rename never crosses
///   a filesystem boundary.
/// * The temp file is created `0600`, so a private config (`~/.claude.json`)
///   is never visible at a wider mode, not even briefly. An existing file's
///   permissions are then restored onto it before the rename; a file that did
///   not exist stays `0600`.
/// * If `path` is a symlink (dotfile managers), the write goes through to the
///   link's target instead of replacing the link.
///
/// For agentdiet's own private storage use [`atomic_write_private`].
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = resolve_symlink(path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    write_via_temp(&target, bytes, Flavor::Config)
}

/// Atomically write agentdiet-owned private data (config backups, manifests,
/// vault entries): `0600` from the moment the temp file is created, missing
/// parent directories created `0700`, and a symlink at `path` is **replaced,
/// never followed**.
pub fn atomic_write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent)?;
    }
    write_via_temp(path, bytes, Flavor::Private)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavor {
    /// Carry over the permissions of the file being replaced.
    Config,
    /// Always `0600`.
    Private,
}

fn write_via_temp(target: &Path, bytes: &[u8], flavor: Flavor) -> io::Result<()> {
    let tmp = temp_sibling(target);
    let result = write_and_rename(target, &tmp, bytes, flavor);
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_and_rename(target: &Path, tmp: &Path, bytes: &[u8], flavor: Flavor) -> io::Result<()> {
    let mut file = private_options().create_new(true).open(tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if flavor == Flavor::Config
        && let Ok(meta) = fs::metadata(target)
    {
        fs::set_permissions(tmp, meta.permissions())?;
    }
    fs::rename(tmp, target)?;
    // The rename is only a promise until the directory entry is on disk.
    match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => sync_dir(dir),
        _ => Ok(()),
    }
}

/// `OpenOptions` for a write-only file that is `0600` from creation.
fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Create `path` and any missing parents, each new directory `0700` from
/// creation. Directories that already exist keep their mode. Every directory
/// this call creates is made durable: its parent is `fsync`ed, so a crash
/// cannot leave the new entry unrecorded.
pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    // Missing ancestors, nearest-to-root last.
    let mut missing: Vec<&Path> = Vec::new();
    let mut cursor = Some(path);
    while let Some(dir) = cursor {
        if dir.as_os_str().is_empty() || dir.is_dir() {
            break;
        }
        missing.push(dir);
        cursor = dir.parent();
    }
    for dir in missing.into_iter().rev() {
        match create_private_dir(dir) {
            Ok(()) => {}
            // Lost a race with another process creating the same directory.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => continue,
            Err(e) => return Err(e),
        }
        if let Some(parent) = dir.parent()
            && !parent.as_os_str().is_empty()
        {
            sync_dir(parent)?;
        }
    }
    Ok(())
}

/// Create the single directory `path` as `0700`; `AlreadyExists` if it is there.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    private_dir_builder().create(path)
}

fn private_dir_builder() -> fs::DirBuilder {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
}

/// Create an empty private file at `path`, replacing whatever is there
/// (including a symlink, which is unlinked rather than followed).
pub fn create_private_empty(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    private_options().create_new(true).open(path).map(drop)
}

/// Open an existing **regular file** without following a symlink and without
/// ever blocking on it.
///
/// On Unix the open itself carries `O_NOFOLLOW` (a symlink as the last path
/// component fails with `ELOOP`) and `O_NONBLOCK` (opening a FIFO whose other
/// end is absent returns instead of waiting), and the descriptor that came back
/// is then checked with `fstat`: anything but a regular file is an error. There
/// is no separate check-then-open step, so there is no window to swap a symlink
/// or FIFO into. `O_NOFOLLOW` covers the final component only; a symlinked
/// parent directory is followed (SPEC §3 Tier 3 details).
pub fn open_regular(path: &Path, write: bool) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.read(true).write(write);
    harden(&mut options);
    let file = options.open(path)?;
    require_regular(&file)?;
    Ok(file)
}

/// Open (creating `0600` if needed) a lock or database file with the same
/// hardening as [`open_regular`]. Existing contents and mode are left alone.
pub fn open_or_create_private(path: &Path) -> io::Result<fs::File> {
    let mut options = private_options();
    options.read(true).create(true);
    harden(&mut options);
    let file = options.open(path)?;
    require_regular(&file)?;
    Ok(file)
}

fn harden(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    let _ = options;
}

fn require_regular(file: &fs::File) -> io::Result<()> {
    if file.metadata()?.file_type().is_file() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ))
    }
}

/// Read a whole file that may legitimately be a symlink (user-visible config
/// such as `state.json`, which dotfile managers link), mapping "not found" to
/// `None`. The link is resolved first and the target opened with
/// [`open_regular`], so a FIFO or device planted there is an error, never a hang.
pub fn read_optional_regular(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let target = resolve_symlink(path);
    match open_regular(&target, false) {
        Ok(mut file) => {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            Ok(Some(bytes))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// `fsync` a directory, so entries created, renamed or removed in it survive a
/// crash. Filesystems that cannot sync a directory report `Unsupported` or
/// `EINVAL`; that is not a failure to persist anything we could do better.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    match fs::File::open(dir).and_then(|d| d.sync_all()) {
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
            ) =>
        {
            Ok(())
        }
        other => other,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("timed out after {waited:?} waiting for the lock on {path}")]
    Timeout { path: String, waited: Duration },
    #[error("cannot lock {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
}

/// An exclusive advisory lock, released when dropped (so on every return path,
/// error and unwind included) or when the process dies.
///
/// `flock` locks belong to the open file description, so two guards on the same
/// path conflict even inside one process; callers must therefore never take the
/// same lock twice in a call chain (SPEC §3 Tier 3 details).
#[derive(Debug)]
pub struct LockGuard {
    file: fs::File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Take an exclusive lock on the private lock file `path`, creating it `0600`
/// if needed (the directory must exist) and never deleting it. Waits at most
/// `timeout`; a zero timeout tries exactly once.
pub fn lock_exclusive(path: &Path, timeout: Duration) -> Result<LockGuard, LockError> {
    let io_err = |source| LockError::Io {
        path: path.display().to_string(),
        source,
    };
    let file = open_or_create_private(path).map_err(io_err)?;
    let start = Instant::now();
    let mut pause = Duration::from_micros(200);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(LockGuard { file }),
            Err(fs::TryLockError::Error(e)) => return Err(io_err(e)),
            Err(fs::TryLockError::WouldBlock) => {}
        }
        let waited = start.elapsed();
        if waited >= timeout {
            return Err(LockError::Timeout {
                path: path.display().to_string(),
                waited,
            });
        }
        std::thread::sleep(pause.min(timeout - waited));
        pause = (pause * 2).min(Duration::from_millis(20));
    }
}

fn resolve_symlink(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
}

fn temp_sibling(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp_name = format!(".{name}.agentdiet-tmp-{}-{nanos}", std::process::id());
    target.with_file_name(tmp_name)
}

/// Read a file, mapping "not found" to `None`.
pub fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn atomic_write_creates_parents_and_replaces() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("a/b/file.json");
        atomic_write(&path, b"one").unwrap();
        atomic_write(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "temp file left behind: {leftovers:?}");
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn private_dirs_and_files_are_created_private() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("a/b/c/secret.json");
        atomic_write_private(&file, b"x").unwrap();
        for d in ["a", "a/b", "a/b/c"] {
            assert_eq!(mode(&dir.path().join(d)), 0o700, "{d}");
        }
        assert_eq!(mode(&file), 0o600);
        // Temp files are created with the same options as the final file.
        let tmp = dir.path().join("tmp");
        private_options().create_new(true).open(&tmp).unwrap();
        assert_eq!(mode(&tmp), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_private_replaces_a_symlink_instead_of_following_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, "precious").unwrap();
        let link = dir.path().join("entry");
        std::os::unix::fs::symlink(&victim, &link).unwrap();

        atomic_write_private(&link, b"new").unwrap();

        assert_eq!(fs::read(&victim).unwrap(), b"precious");
        assert!(
            !fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&link).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn open_regular_and_create_private_empty_never_follow_symlinks() {
        let dir = tempfile::TempDir::new().unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, "precious").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&victim, &link).unwrap();

        assert!(open_regular(&link, false).is_err());
        assert!(open_regular(dir.path(), false).is_err());
        assert!(open_regular(&victim, true).is_ok());

        create_private_empty(&link).unwrap();
        assert_eq!(fs::read(&victim).unwrap(), b"precious");
        assert_eq!(fs::metadata(&link).unwrap().len(), 0);
        assert!(
            !fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    fn mkfifo(path: &Path) {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    }

    /// A FIFO with no writer would block a plain `File::open` forever. Run the open on a thread so
    /// a regression fails the test instead of hanging it.
    #[cfg(unix)]
    fn finishes_within(secs: u64, f: impl FnOnce() + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            f();
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(secs))
            .expect("blocked on a FIFO");
    }

    #[cfg(unix)]
    #[test]
    fn opens_never_block_on_a_fifo_or_follow_a_symlink() {
        let dir = tempfile::TempDir::new().unwrap();
        let fifo = dir.path().join("fifo");
        mkfifo(&fifo);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path().join("target"), &link).unwrap();
        fs::write(dir.path().join("target"), "x").unwrap();
        let (f2, l2) = (fifo.clone(), link.clone());
        finishes_within(5, move || {
            assert!(open_regular(&f2, false).is_err());
            assert!(open_regular(&f2, true).is_err());
            assert!(open_regular(&l2, false).is_err());
            assert!(open_or_create_private(&f2).is_err());
            assert!(open_or_create_private(&l2).is_err());
            assert!(read_optional_regular(&f2).is_err());
        });
    }

    #[cfg(unix)]
    #[test]
    fn read_optional_regular_follows_a_config_symlink_but_not_to_a_fifo() {
        let dir = tempfile::TempDir::new().unwrap();
        let real = dir.path().join("real.json");
        fs::write(&real, "{}").unwrap();
        let link = dir.path().join("state.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(read_optional_regular(&link).unwrap().unwrap(), b"{}");
        assert_eq!(
            read_optional_regular(&dir.path().join("none")).unwrap(),
            None
        );

        let fifo = dir.path().join("fifo");
        mkfifo(&fifo);
        let linked_fifo = dir.path().join("linked");
        std::os::unix::fs::symlink(&fifo, &linked_fifo).unwrap();
        finishes_within(5, move || {
            assert!(read_optional_regular(&linked_fifo).is_err())
        });
    }

    /// `umask` is process-wide; nextest (CLAUDE.md) runs each test in its own process, and the
    /// value is restored anyway.
    #[cfg(unix)]
    #[test]
    fn lock_and_db_files_are_0600_from_creation_even_under_umask_0() {
        let dir = tempfile::TempDir::new().unwrap();
        let old = unsafe { libc::umask(0) };
        let lock = lock_exclusive(&dir.path().join("a.lock"), Duration::ZERO);
        let db = open_or_create_private(&dir.path().join("m.db"));
        let subdir = create_private_dir_all(&dir.path().join("x/y"));
        unsafe { libc::umask(old) };
        drop(lock.unwrap());
        db.unwrap();
        subdir.unwrap();
        assert_eq!(mode(&dir.path().join("a.lock")), 0o600);
        assert_eq!(mode(&dir.path().join("m.db")), 0o600);
        assert_eq!(mode(&dir.path().join("x")), 0o700);
        assert_eq!(mode(&dir.path().join("x/y")), 0o700);
    }

    #[test]
    fn lock_is_exclusive_bounded_and_released_on_drop() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("l.lock");
        let first = lock_exclusive(&path, Duration::from_secs(1)).unwrap();

        // Same process, second open file description: must conflict, and must time out.
        let started = Instant::now();
        let err = lock_exclusive(&path, Duration::from_millis(80)).unwrap_err();
        assert!(matches!(err, LockError::Timeout { .. }), "{err}");
        assert!(started.elapsed() >= Duration::from_millis(80));
        assert!(started.elapsed() < Duration::from_secs(2), "bounded wait");
        assert!(matches!(
            lock_exclusive(&path, Duration::ZERO),
            Err(LockError::Timeout { .. })
        ));

        drop(first);
        lock_exclusive(&path, Duration::ZERO).expect("free after drop");
    }

    #[test]
    fn lock_is_released_when_the_holder_panics() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("p.lock");
        let p2 = path.clone();
        let result = std::thread::spawn(move || {
            let _guard = lock_exclusive(&p2, Duration::from_secs(1)).unwrap();
            panic!("holder dies");
        })
        .join();
        assert!(result.is_err());
        lock_exclusive(&path, Duration::ZERO).expect("unlocked by unwind");
    }

    #[test]
    fn a_waiting_locker_gets_the_lock_when_the_holder_lets_go() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("w.lock");
        let held = lock_exclusive(&path, Duration::from_secs(1)).unwrap();
        let p2 = path.clone();
        let waiter =
            std::thread::spawn(move || lock_exclusive(&p2, Duration::from_secs(5)).map(drop));
        std::thread::sleep(Duration::from_millis(50));
        drop(held);
        waiter.join().unwrap().expect("acquired after release");
    }

    #[cfg(unix)]
    #[test]
    fn directory_creation_syncs_without_error_and_is_idempotent() {
        let dir = tempfile::TempDir::new().unwrap();
        let deep = dir.path().join("a/b/c");
        create_private_dir_all(&deep).unwrap();
        create_private_dir_all(&deep).unwrap();
        sync_dir(&deep).unwrap();
        // A file where a directory is needed is an error, not a panic.
        fs::write(dir.path().join("f"), "x").unwrap();
        assert!(create_private_dir_all(&dir.path().join("f/g")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn config_rewrites_keep_the_old_mode_and_new_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let existing = dir.path().join("existing.json");
        fs::write(&existing, "old").unwrap();
        fs::set_permissions(&existing, fs::Permissions::from_mode(0o640)).unwrap();
        atomic_write(&existing, b"new").unwrap();
        assert_eq!(mode(&existing), 0o640);

        let fresh = dir.path().join("fresh.json");
        atomic_write(&fresh, b"new").unwrap();
        assert_eq!(mode(&fresh), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_permissions_and_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let real = dir.path().join("real.json");
        fs::write(&real, "old").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        atomic_write(&link, b"new").unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&real).unwrap(), b"new");
        let mode = fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
