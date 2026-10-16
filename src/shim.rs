//! Shim mode: `~/.agentdiet/shims/<cmd>` symlinks to this binary.
//!
//! Until the Diet Engine ships, a shim always execs the real binary — the
//! fail-open default — so an installed shim can never break `git` & friends.
//! This path must stay tiny: no clap, no config reads, no DB.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::{basename, is_agentdiet_exe};

/// Commands that get a shim.
pub const SHIM_COMMANDS: &[&str] = &[
    "git", "cargo", "pytest", "npm", "pnpm", "yarn", "go", "make", "ruff", "docker", "gh",
];

/// If this process was invoked through a shim symlink, the command it stands in for.
pub fn shim_name(argv0: &OsStr) -> Option<&'static str> {
    let name = basename(argv0.to_str()?);
    SHIM_COMMANDS.iter().copied().find(|c| *c == name)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Find `name` on `path_var`, skipping the shims directory and any candidate
/// that is (a symlink to) an agentdiet binary — which also skips shims reached
/// through another spelling of the shims dir, and stale shims from other builds.
///
/// Kept cheap for the passthrough path: one `stat` per PATH entry, plus one
/// `lstat` (and a `realpath` only for symlinks) on the entry that matches.
pub fn resolve_real(name: &str, path_var: &OsStr, shims_dir: &Path) -> Option<PathBuf> {
    for dir in std::env::split_paths(path_var) {
        if dir == shims_dir {
            continue;
        }
        let candidate = dir.join(name);
        if !is_executable(&candidate) {
            continue;
        }
        let is_symlink = std::fs::symlink_metadata(&candidate).is_ok_and(|m| m.file_type().is_symlink());
        if is_symlink && std::fs::canonicalize(&candidate).is_ok_and(|real| is_agentdiet_exe(&real)) {
            continue;
        }
        return Some(candidate);
    }
    None
}

/// Replace this process with the real `name`, passing `args` through. Never
/// returns; exits 127 if the real binary cannot be found.
pub fn passthrough(name: &str, args: Vec<OsString>) -> ! {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let shims_dir = home.map(|h| PathBuf::from(h).join(".agentdiet").join("shims"));
    let path_var = std::env::var_os("PATH").unwrap_or_default();

    let real = resolve_real(name, &path_var, shims_dir.as_deref().unwrap_or(Path::new("")));
    let Some(real) = real else {
        eprintln!("agentdiet shim: could not find the real `{name}` on PATH");
        std::process::exit(127);
    };
    exec(&real, args)
}

#[cfg(unix)]
fn exec(real: &Path, args: Vec<OsString>) -> ! {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(real).args(args).exec();
    eprintln!("agentdiet shim: failed to exec {}: {err}", real.display());
    std::process::exit(126)
}

#[cfg(not(unix))]
fn exec(real: &Path, args: Vec<OsString>) -> ! {
    match std::process::Command::new(real).args(args).status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("agentdiet shim: failed to run {}: {err}", real.display());
            std::process::exit(126)
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::*;

    fn make_exe(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn shim_name_matches_basename_only() {
        assert_eq!(shim_name(OsStr::new("/h/.agentdiet/shims/git")), Some("git"));
        assert_eq!(shim_name(OsStr::new("gh")), Some("gh"));
        assert_eq!(shim_name(OsStr::new("docker")), Some("docker"));
        assert_eq!(shim_name(OsStr::new("ruff")), Some("ruff"));
        assert_eq!(shim_name(OsStr::new("agentdiet")), None);
        assert_eq!(shim_name(OsStr::new("/usr/bin/gitk")), None);
    }

    #[test]
    fn resolves_past_the_shims_dir() {
        let dir = TempDir::new().unwrap();
        let shims = dir.path().join("shims");
        let real = dir.path().join("real");
        make_exe(&shims.join("git"));
        make_exe(&real.join("git"));
        let path = std::env::join_paths([&shims, &real]).unwrap();

        assert_eq!(resolve_real("git", &path, &shims), Some(real.join("git")));
        assert_eq!(resolve_real("hg", &path, &shims), None);
    }

    #[test]
    fn skips_symlinks_to_agentdiet_even_via_another_dir() {
        let dir = TempDir::new().unwrap();
        let me = dir.path().join("bin/agentdiet");
        make_exe(&me);
        let a = dir.path().join("a");
        fs::create_dir_all(&a).unwrap();
        std::os::unix::fs::symlink(&me, a.join("git")).unwrap();
        let real = dir.path().join("real");
        make_exe(&real.join("git"));
        let path = std::env::join_paths([&a, &real]).unwrap();

        let found = resolve_real("git", &path, &dir.path().join("shims"));
        assert_eq!(found, Some(real.join("git")));
    }
}
