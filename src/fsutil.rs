//! Small filesystem helpers shared by `state.json` and every config write.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
    fs::rename(tmp, target)
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
/// creation. Directories that already exist keep their mode.
pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    private_dir_builder(true).create(path)
}

/// Create the single directory `path` as `0700`; `AlreadyExists` if it is there.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    private_dir_builder(false).create(path)
}

fn private_dir_builder(recursive: bool) -> fs::DirBuilder {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(recursive);
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

/// Open an existing **regular file** without following a symlink: a symlink or
/// any other non-regular file is an error, and on Unix the opened file must be
/// the very inode that was checked (so a swap in between is caught).
pub fn open_regular(path: &Path, write: bool) -> io::Result<fs::File> {
    let not_regular = || io::Error::new(io::ErrorKind::InvalidInput, "not a regular file");
    let before = fs::symlink_metadata(path)?;
    if !before.file_type().is_file() {
        return Err(not_regular());
    }
    let file = OpenOptions::new().read(true).write(write).open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let after = file.metadata()?;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err(not_regular());
        }
    }
    Ok(file)
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
        assert!(!fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
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
        assert!(!fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
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

        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&real).unwrap(), b"new");
        let mode = fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
