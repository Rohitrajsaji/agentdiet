//! `~/.agentdiet/shims/<cmd>` symlinks to the agentdiet binary.

use std::ffi::OsStr;
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
    /// Shims that already pointed at this binary, one per link (so a rerun counts what the first
    /// run linked).
    pub unchanged: Vec<PathBuf>,
    pub skipped: Vec<String>,
}

#[cfg(unix)]
fn symlink(original: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(original, link)
}

#[cfg(not(unix))]
fn symlink(_original: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "symlink shims need unix",
    ))
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
                    outcome.unchanged.push(link);
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
    SHIM_COMMANDS
        .iter()
        .any(|name| fs::read_link(paths.shims_dir().join(name)).is_ok_and(|t| is_agentdiet_exe(&t)))
}

/// One shim link, as [`inspect`] finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// Points at the running binary.
    Current,
    /// Points at another agentdiet executable that exists.
    Other(PathBuf),
    /// Points at an agentdiet executable that does not exist.
    Dangling(PathBuf),
    /// No link, or something that is not ours (left alone by `init`).
    Missing,
}

/// The state of every shim command's link, in [`SHIM_COMMANDS`] order. Reads only.
pub fn inspect(paths: &Paths) -> Vec<(&'static str, Link)> {
    let dir = paths.shims_dir();
    SHIM_COMMANDS
        .iter()
        .map(|&name| {
            let link = dir.join(name);
            let state = match fs::read_link(&link) {
                Ok(target) if is_agentdiet_exe(&target) => {
                    if target == paths.exe {
                        Link::Current
                    } else if fs::metadata(&link).is_ok() {
                        Link::Other(target)
                    } else {
                        Link::Dangling(target)
                    }
                }
                _ => Link::Missing,
            };
            (name, state)
        })
        .collect()
}

/// Where the shims stand on a `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathStatus {
    /// The shims directory is on `PATH` and no earlier entry has a shimmed command.
    First,
    /// An earlier `PATH` entry has `command`, at `found`, so the shim is never reached for it.
    Shadowed {
        command: &'static str,
        found: PathBuf,
    },
    /// The shims directory is not on `PATH`.
    Missing,
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// Check `path_var` (a `PATH` value) against the shims directory.
pub fn path_status(path_var: &OsStr, shims_dir: &Path) -> PathStatus {
    let mut shadow = None;
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if same_dir(&dir, shims_dir) {
            return match shadow {
                Some((command, found)) => PathStatus::Shadowed { command, found },
                None => PathStatus::First,
            };
        }
        if shadow.is_none() {
            shadow = SHIM_COMMANDS.iter().find_map(|&command| {
                let found = dir.join(command);
                found.is_file().then_some((command, found))
            });
        }
    }
    PathStatus::Missing
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use tempfile::TempDir;

    use super::*;

    fn join(dirs: &[&Path]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn path_status_finds_missing_first_and_shadowed_shims() {
        let dir = TempDir::new().unwrap();
        let shims = dir.path().join("shims");
        let bin = dir.path().join("bin");
        let empty = dir.path().join("empty");
        for d in [&shims, &bin, &empty] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(bin.join("git"), "").unwrap();

        assert_eq!(path_status(&join(&[&bin]), &shims), PathStatus::Missing);
        assert_eq!(
            path_status(&join(&[&shims, &bin]), &shims),
            PathStatus::First
        );
        assert_eq!(
            path_status(&join(&[&empty, &shims]), &shims),
            PathStatus::First
        );
        assert_eq!(
            path_status(&join(&[&bin, &shims]), &shims),
            PathStatus::Shadowed {
                command: "git",
                found: bin.join("git")
            }
        );
        // A trailing slash or `..` still names the same directory.
        let spelled = dir.path().join("bin/../shims/");
        assert_eq!(path_status(&join(&[&spelled]), &shims), PathStatus::First);
    }

    #[cfg(unix)]
    #[test]
    fn inspect_tells_current_other_dangling_and_missing_links_apart() {
        let dir = TempDir::new().unwrap();
        let exe = dir.path().join("bin/agentdiet");
        let other = dir.path().join("old/agentdiet");
        for p in [&exe, &other] {
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "").unwrap();
        }
        let paths = Paths::new(
            dir.path().join("home"),
            dir.path().join("cfg"),
            dir.path().join("proj"),
            exe.clone(),
        );
        install(&paths, false).unwrap();
        let shims = paths.shims_dir();
        fs::remove_file(shims.join("cargo")).unwrap();
        symlink(&other, &shims.join("cargo")).unwrap();
        fs::remove_file(shims.join("npm")).unwrap();
        symlink(&dir.path().join("gone/agentdiet"), &shims.join("npm")).unwrap();
        fs::remove_file(shims.join("gh")).unwrap();

        let found: std::collections::HashMap<_, _> = inspect(&paths).into_iter().collect();
        assert_eq!(found["git"], Link::Current);
        assert_eq!(found["cargo"], Link::Other(other));
        assert_eq!(
            found["npm"],
            Link::Dangling(dir.path().join("gone/agentdiet"))
        );
        assert_eq!(found["gh"], Link::Missing);
    }
}
