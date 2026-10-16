//! `~/.agentdiet/shims/<cmd>` symlinks to the agentdiet binary.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{InitError, io_err};
use crate::shim::SHIM_COMMANDS;
use crate::{Paths, is_agentdiet_exe};

/// What `install` did, per shim.
#[derive(Debug, Default)]
pub struct Outcome {
    pub linked: Vec<PathBuf>,
    pub unchanged: usize,
    pub skipped: Vec<String>,
}

#[cfg(unix)]
fn symlink(original: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(original, link)
}

#[cfg(not(unix))]
fn symlink(_original: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "symlink shims need unix"))
}

/// Create or refresh the shim symlinks. With `dry_run`, only report.
pub fn install(paths: &Paths, dry_run: bool) -> Result<Outcome, InitError> {
    let mut outcome = Outcome::default();
    if !cfg!(unix) {
        outcome
            .skipped
            .push("shims: symlink shims are not supported on this platform".into());
        return Ok(outcome);
    }
    let dir = paths.shims_dir();
    if !dry_run {
        fs::create_dir_all(&dir).map_err(io_err(&dir))?;
    }
    for name in SHIM_COMMANDS {
        let link = dir.join(name);
        match fs::symlink_metadata(&link) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let current = fs::read_link(&link).map_err(io_err(&link))?;
                if current == paths.exe {
                    outcome.unchanged += 1;
                    continue;
                }
                if !is_agentdiet_exe(&current) {
                    outcome.skipped.push(format!(
                        "shims: {} points at {}, not agentdiet; left alone",
                        link.display(),
                        current.display()
                    ));
                    continue;
                }
                if !dry_run {
                    fs::remove_file(&link).map_err(io_err(&link))?;
                    symlink(&paths.exe, &link).map_err(io_err(&link))?;
                }
                outcome.linked.push(link);
            }
            Ok(_) => outcome.skipped.push(format!(
                "shims: {} exists and is not a symlink; left alone",
                link.display()
            )),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if !dry_run {
                    symlink(&paths.exe, &link).map_err(io_err(&link))?;
                }
                outcome.linked.push(link);
            }
            Err(e) => return Err(io_err(&link)(e)),
        }
    }
    Ok(outcome)
}

/// Remove the symlinks we own, then the shims dir if it is empty.
pub fn remove(paths: &Paths) -> Result<Vec<PathBuf>, InitError> {
    let dir = paths.shims_dir();
    let mut removed = Vec::new();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(removed);
    };
    for path in entries.flatten().map(|e| e.path()) {
        let is_ours = fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink())
            && fs::read_link(&path).is_ok_and(|t| is_agentdiet_exe(&t));
        if is_ours {
            fs::remove_file(&path).map_err(io_err(&path))?;
            removed.push(path);
        }
    }
    let _ = fs::remove_dir(&dir); // only succeeds when empty
    Ok(removed)
}

/// `true` if any shim symlink points at an agentdiet binary.
pub fn is_installed(paths: &Paths) -> bool {
    SHIM_COMMANDS.iter().any(|name| {
        fs::read_link(paths.shims_dir().join(name)).is_ok_and(|t| is_agentdiet_exe(&t))
    })
}
