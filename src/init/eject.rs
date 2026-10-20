//! `agentdiet eject` / `uninstall`: put configs back and remove shims.
//!
//! For each file `init` rewrote, the *earliest* backup is the pristine
//! original and the *latest* manifest entry records what `init` last wrote.
//!
//! * file still equals what `init` wrote → restore the pristine original
//!   byte-for-byte (or delete a file `init` created);
//! * the user edited it since → remove only agentdiet's entries, keep their edits.
//!
//! A final sweep strips agentdiet entries from known config locations even
//! when no manifest remembers them. Everything is idempotent.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::backup::{self, Entry};
use super::targets::{self, Target};
use super::{InitError, io_err, shims};
use crate::{Paths, fsutil};

/// What eject did to one file (or the shim links) of a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Restored,
    Deleted,
    Cleaned,
    ShimsRemoved,
}

#[derive(Debug, Default)]
pub struct EjectReport {
    /// The same outcomes per target, in the order they happened (`ShimsRemoved` carries the shims
    /// directory). The flat lists below stay for callers that only count.
    pub actions: Vec<(Target, Action, PathBuf)>,
    /// Files restored byte-for-byte from a backup.
    pub restored: Vec<PathBuf>,
    /// Files init had created, now deleted.
    pub deleted: Vec<PathBuf>,
    /// Files where only agentdiet's entries were removed.
    pub cleaned: Vec<PathBuf>,
    pub shims_removed: Vec<PathBuf>,
    /// Data removed by `--purge`.
    pub purged: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

impl EjectReport {
    fn note(&mut self, target: Target, action: Action, path: &Path) {
        self.actions.push((target, action, path.to_path_buf()));
    }

    /// The files eject touched for `target`, with what it did to each.
    pub fn for_target(&self, target: Target) -> Vec<(Action, &Path)> {
        self.actions
            .iter()
            .filter(|(t, ..)| *t == target)
            .map(|(_, a, p)| (*a, p.as_path()))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.restored.is_empty()
            && self.deleted.is_empty()
            && self.cleaned.is_empty()
            && self.shims_removed.is_empty()
            && self.purged.is_empty()
    }
}

/// Eject `only` (or everything when `None`). `purge` additionally deletes the
/// vault, project memory and the state ledger; it is honoured only for a full eject.
pub fn eject(
    paths: &Paths,
    only: Option<&[Target]>,
    purge: bool,
) -> Result<EjectReport, InitError> {
    let wanted = |t: Target| only.is_none_or(|list| list.contains(&t));
    let mut report = EjectReport::default();

    let (mut manifests, warnings) = backup::load_active(paths);
    report.warnings.extend(warnings);

    // Group active entries by file, oldest first: (manifest index, entry index).
    let mut groups: BTreeMap<PathBuf, Vec<(usize, usize)>> = BTreeMap::new();
    for (mi, loaded) in manifests.iter().enumerate() {
        for (ei, entry) in loaded.manifest.entries.iter().enumerate() {
            if !entry.ejected && wanted(entry.target) {
                groups
                    .entry(entry.original_path.clone())
                    .or_default()
                    .push((mi, ei));
            }
        }
    }

    for (path, refs) in &groups {
        let entry_at = |&(mi, ei): &(usize, usize)| manifests[mi].manifest.entries[ei].clone();
        let (Some(first), Some(last)) = (refs.first(), refs.last()) else {
            continue;
        };
        let earliest = entry_at(first);
        let latest = entry_at(last);
        let earliest_dir = manifests[first.0].dir.clone();

        match fsutil::read_optional(path).map_err(io_err(path))? {
            None => {} // already gone; nothing to restore over
            Some(bytes) if fsutil::sha256_hex(&bytes) == latest.written_sha256 => {
                restore_pristine(path, &earliest, &earliest_dir, &latest, &mut report)?;
                if earliest.backup.is_none() {
                    for entry in refs.iter().map(entry_at) {
                        remove_created_dirs(&entry.created_dirs);
                    }
                }
            }
            Some(bytes) => {
                strip_entries(path, &latest, &bytes, &mut report)?;
            }
        }
        for &(mi, ei) in refs {
            manifests[mi].manifest.entries[ei].ejected = true;
        }
    }

    sweep(paths, &wanted, &mut report)?;

    if wanted(Target::Shims) {
        let removed = shims::remove(paths)?;
        if !removed.is_empty() {
            report.note(Target::Shims, Action::ShimsRemoved, &paths.shims_dir());
        }
        report.shims_removed.extend(removed);
    }

    for loaded in &manifests {
        if loaded.manifest.entries.iter().any(|e| e.ejected) {
            loaded.finish()?;
        }
    }

    if purge && only.is_none() {
        purge_data(paths, &mut report)?;
    }
    Ok(report)
}

fn restore_pristine(
    path: &Path,
    earliest: &Entry,
    earliest_dir: &Path,
    latest: &Entry,
    report: &mut EjectReport,
) -> Result<(), InitError> {
    match &earliest.backup {
        Some(rel) => {
            let backup_file = earliest_dir.join(rel);
            match fs::read(&backup_file) {
                Ok(original) => {
                    fsutil::atomic_write(path, &original).map_err(io_err(path))?;
                    report.restored.push(path.to_path_buf());
                    report.note(earliest.target, Action::Restored, path);
                }
                Err(e) => {
                    report.warnings.push(format!(
                        "backup {} is unreadable ({e}); removing agentdiet entries instead",
                        backup_file.display()
                    ));
                    let bytes = fs::read(path).map_err(io_err(path))?;
                    strip_entries(path, latest, &bytes, report)?;
                }
            }
        }
        None => {
            fs::remove_file(path).map_err(io_err(path))?;
            report.deleted.push(path.to_path_buf());
            report.note(earliest.target, Action::Deleted, path);
        }
    }
    Ok(())
}

/// Remove directories init created, innermost first; non-empty ones stay.
fn remove_created_dirs(dirs: &[PathBuf]) {
    for dir in dirs.iter().rev() {
        let _ = fs::remove_dir(dir);
    }
}

/// Surgically remove agentdiet's entries from `bytes`, leaving the rest.
fn strip_entries(
    path: &Path,
    entry: &Entry,
    bytes: &[u8],
    report: &mut EjectReport,
) -> Result<(), InitError> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        report
            .warnings
            .push(format!("{} is not UTF-8; left untouched", path.display()));
        return Ok(());
    };
    match entry.kind.unpatch(text) {
        Ok(Some(cleaned)) => {
            fsutil::atomic_write(path, cleaned.as_bytes()).map_err(io_err(path))?;
            report.warnings.push(format!(
                "{} was edited after init; removed only agentdiet's entries",
                path.display()
            ));
            report.cleaned.push(path.to_path_buf());
            report.note(entry.target, Action::Cleaned, path);
        }
        Ok(None) => {}
        Err(e) => report
            .warnings
            .push(format!("{}: {e}; left untouched", path.display())),
    }
    Ok(())
}

/// Strip agentdiet entries from every known config location, manifest or not.
fn sweep(
    paths: &Paths,
    wanted: &impl Fn(Target) -> bool,
    report: &mut EjectReport,
) -> Result<(), InitError> {
    for target in Target::ALL.into_iter().filter(|&t| wanted(t)) {
        for project in [false, true] {
            for file in targets::files(paths, target, project).into_iter().flatten() {
                let Some(bytes) = fsutil::read_optional(&file.path).map_err(io_err(&file.path))?
                else {
                    continue;
                };
                let Ok(text) = std::str::from_utf8(&bytes) else {
                    continue;
                };
                if !file.kind.is_present(text) {
                    continue;
                }
                match file.kind.unpatch(text) {
                    Ok(Some(cleaned)) => {
                        fsutil::atomic_write(&file.path, cleaned.as_bytes())
                            .map_err(io_err(&file.path))?;
                        if !report.cleaned.contains(&file.path)
                            && !report.restored.contains(&file.path)
                        {
                            report.cleaned.push(file.path.clone());
                            report.note(target, Action::Cleaned, &file.path);
                        }
                    }
                    Ok(None) => {}
                    Err(e) => report
                        .warnings
                        .push(format!("{}: {e}; left untouched", file.path.display())),
                }
            }
        }
    }
    Ok(())
}

fn purge_data(paths: &Paths, report: &mut EjectReport) -> Result<(), InitError> {
    let cas = paths.cas_dir();
    if cas.exists() {
        fs::remove_dir_all(&cas).map_err(io_err(&cas))?;
        report.purged.push(cas);
    }
    let mut files = vec![paths.state_file(), paths.state_lock_file()];
    let project_data = paths.project_data_dir();
    for name in ["memory.db", "memory.db-wal", "memory.db-shm"] {
        files.push(project_data.join(name));
    }
    for file in files {
        match fs::remove_file(&file) {
            Ok(()) => report.purged.push(file),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(&file)(e)),
        }
    }
    Ok(())
}
