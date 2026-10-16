//! Backups under `~/.agentdiet/backups/<epoch-millis>/<target>/<file>` and the
//! `manifest.json` that lets `eject` put every original back.
//!
//! Backup directories are created lazily — only once a file is actually going
//! to change — so a no-op `init` leaves no trace on disk.
//!
//! A backup is a byte-exact copy of the user's original config (so `eject` can
//! restore it exactly), which means it is **not scrubbed** and may hold secrets
//! the user already had. SPEC.md §2.2 documents this as a separate contract from
//! the vault: instead of redaction, everything here is `0700` directories and
//! `0600` files from the moment of creation, temp files included.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::targets::{FileSpec, PatchKind, Target};
use super::{InitError, io_err};
use crate::{Paths, fsutil};

pub const MANIFEST: &str = "manifest.json";
pub const MANIFEST_DONE: &str = "manifest.ejected.json";

/// One file `init` rewrote.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub target: Target,
    pub original_path: PathBuf,
    pub kind: PatchKind,
    /// Backup path relative to the backup dir; `None` if the file did not
    /// exist before `init` created it.
    pub backup: Option<String>,
    /// sha256 of the bytes `init` wrote, to detect later user edits.
    pub written_sha256: String,
    /// Directories `init` had to create for the file (outermost first).
    #[serde(default)]
    pub created_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub ejected: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub created_ms: u64,
    pub entries: Vec<Entry>,
}

/// A manifest loaded from disk, with where it lives.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub dir: PathBuf,
    pub manifest: Manifest,
}

impl Loaded {
    pub fn manifest_path(&self) -> PathBuf {
        self.dir.join(MANIFEST)
    }

    pub fn write(&self) -> Result<(), InitError> {
        write_manifest(&self.dir, &self.manifest)
    }

    /// Mark finished: rename to `manifest.ejected.json` once every entry is ejected.
    pub fn finish(&self) -> Result<(), InitError> {
        if self.manifest.entries.iter().all(|e| e.ejected) {
            write_manifest_as(&self.dir, MANIFEST_DONE, &self.manifest)?;
            let old = self.manifest_path();
            fs::remove_file(&old).map_err(io_err(&old))
        } else {
            self.write()
        }
    }
}

fn write_manifest(dir: &Path, manifest: &Manifest) -> Result<(), InitError> {
    write_manifest_as(dir, MANIFEST, manifest)
}

fn write_manifest_as(dir: &Path, name: &str, manifest: &Manifest) -> Result<(), InitError> {
    let path = dir.join(name);
    let mut json = serde_json::to_string_pretty(manifest).map_err(|e| InitError::Manifest {
        path: path.clone(),
        message: e.to_string(),
    })?;
    json.push('\n');
    fsutil::atomic_write_private(&path, json.as_bytes()).map_err(io_err(&path))
}

/// Load every active manifest, oldest first. Unreadable ones are skipped with
/// a warning rather than blocking an eject.
pub fn load_active(paths: &Paths) -> (Vec<Loaded>, Vec<String>) {
    let mut loaded = Vec::new();
    let mut warnings = Vec::new();
    let Ok(dirs) = fs::read_dir(paths.backups_dir()) else {
        return (loaded, warnings);
    };
    for dir in dirs.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let file = dir.join(MANIFEST);
        let Ok(bytes) = fs::read(&file) else { continue };
        match serde_json::from_slice::<Manifest>(&bytes) {
            Ok(manifest) => loaded.push(Loaded { dir, manifest }),
            Err(e) => warnings.push(format!("skipping unreadable {}: {e}", file.display())),
        }
    }
    loaded.sort_by(|a, b| {
        (a.manifest.created_ms, &a.dir).cmp(&(b.manifest.created_ms, &b.dir))
    });
    (loaded, warnings)
}

/// Collects the backups for one `init` run.
pub struct Session<'a> {
    paths: &'a Paths,
    dir: Option<PathBuf>,
    manifest: Manifest,
}

impl<'a> Session<'a> {
    pub fn new(paths: &'a Paths) -> Self {
        Self {
            paths,
            dir: None,
            manifest: Manifest {
                created_ms: fsutil::now_millis(),
                entries: Vec::new(),
            },
        }
    }

    /// The backup directory, if anything was backed up or created.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    fn ensure_dir(&mut self) -> Result<PathBuf, InitError> {
        if let Some(dir) = &self.dir {
            return Ok(dir.clone());
        }
        let root = self.paths.backups_dir();
        fsutil::create_private_dir_all(&root).map_err(io_err(&root))?;
        let stamp = self.manifest.created_ms;
        let mut attempt = 0u32;
        loop {
            let name = if attempt == 0 {
                stamp.to_string()
            } else {
                format!("{stamp}-{attempt}")
            };
            let candidate = root.join(name);
            match fsutil::create_private_dir(&candidate) {
                Ok(()) => {
                    self.dir = Some(candidate.clone());
                    return Ok(candidate);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => attempt += 1,
                Err(e) => return Err(io_err(&candidate)(e)),
            }
        }
    }

    /// Back up `current`, record the change, then write `new` atomically.
    pub fn write(
        &mut self,
        target: Target,
        file: &FileSpec,
        current: Option<&[u8]>,
        new: &[u8],
    ) -> Result<(), InitError> {
        let dir = self.ensure_dir()?;
        let backup = match current {
            Some(bytes) => Some(store_backup(&dir, target, &file.path, bytes)?),
            None => None,
        };
        self.manifest.entries.push(Entry {
            target,
            original_path: file.path.clone(),
            kind: file.kind.clone(),
            backup,
            written_sha256: fsutil::sha256_hex(new),
            created_dirs: missing_parents(&file.path),
            ejected: false,
        });
        write_manifest(&dir, &self.manifest)?;
        fsutil::atomic_write(&file.path, new).map_err(io_err(&file.path))
    }
}

/// Copy `bytes` to `<dir>/<target>/<file name>`, avoiding name clashes.
fn store_backup(dir: &Path, target: Target, original: &Path, bytes: &[u8]) -> Result<String, InitError> {
    let name = original
        .file_name()
        .map_or_else(|| "file".to_string(), |n| n.to_string_lossy().into_owned());
    let sub = dir.join(target.slug());
    fsutil::create_private_dir_all(&sub).map_err(io_err(&sub))?;
    let mut rel = format!("{}/{name}", target.slug());
    let mut n = 1;
    while dir.join(&rel).exists() {
        rel = format!("{}/{n}-{name}", target.slug());
        n += 1;
    }
    let dest = dir.join(&rel);
    fsutil::atomic_write_private(&dest, bytes).map_err(io_err(&dest))?;
    Ok(rel)
}

/// Ancestors of `path` that do not exist yet (outermost first).
fn missing_parents(path: &Path) -> Vec<PathBuf> {
    let mut missing: Vec<PathBuf> = path
        .ancestors()
        .skip(1)
        .take_while(|p| !p.as_os_str().is_empty() && !p.exists())
        .map(Path::to_path_buf)
        .collect();
    missing.reverse();
    missing
}
