//! Shim mode: `~/.agentdiet/shims/<cmd>` symlinks to this binary (SPEC §3 Tier 1).
//!
//! Every call either execs the real binary untouched (the transparent path) or, when an AI agent
//! runs a command whose output the engine knows how to compress, runs it and routes its stdout and
//! stderr through the Diet Engine (the agent path). The checks that pick the path run cheapest
//! first, so a human's terminal never pays for a process walk, a config read or a DB open.

use std::ffi::{OsStr, OsString};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::diet::{Bypass, Engine, ExitStatus, Input, Settings, Source, estimate_tokens};
use crate::memory::{Origin, RunKind};
use crate::runlog;
use crate::session::{self, Key};
use crate::{Paths, basename, is_agentdiet_exe, state};

/// Commands that get a shim.
pub const SHIM_COMMANDS: &[&str] = &[
    "git", "cargo", "pytest", "npm", "pnpm", "yarn", "go", "make", "ruff", "docker", "gh",
];

/// Process names (or executable base names) that mark an AI-agent ancestor.
pub const AGENT_NAMES: &[&str] = &[
    "claude", "cursor", "windsurf", "codex", "aider", "gemini", "copilot", "opencode",
];

/// Environment variables set by coding agents for the commands they run.
pub const ENV_MARKERS: &[&str] = &["CLAUDECODE", "CURSOR_TRACE_ID", "GEMINI_CLI"];

/// The parent walk gives up after this many hops.
pub const MAX_ANCESTRY_HOPS: usize = 16;

/// Set on the agent path's child: a shim that sees it is nested and goes transparent.
pub const NESTED_ENV: &str = "AGENTDIET_SHIM_ACTIVE";
/// Force the transparent path (tests, and users who want shims out of the way).
pub const FORCE_HUMAN_ENV: &str = "AGENTDIET_FORCE_HUMAN";
/// Treat the caller as an agent without looking for one.
pub const FORCE_AGENT_ENV: &str = "AGENTDIET_FORCE_AGENT";
/// Claude Code's session id, exported to the commands it runs.
pub const CLAUDE_SESSION_ENV: &str = "CLAUDE_CODE_SESSION_ID";

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
        let is_symlink =
            std::fs::symlink_metadata(&candidate).is_ok_and(|m| m.file_type().is_symlink());
        if is_symlink && std::fs::canonicalize(&candidate).is_ok_and(|real| is_agentdiet_exe(&real))
        {
            continue;
        }
        return Some(candidate);
    }
    None
}

// ---- agent detection --------------------------------------------------------------------------

/// `true` if a process name or executable base name names an AI agent: one of [`AGENT_NAMES`],
/// case-insensitive, alone or followed by a space or `-` (`Cursor Helper (Plugin)`). Never a
/// substring match.
pub fn is_agent_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let lower = lower.strip_suffix(".exe").unwrap_or(&lower);
    AGENT_NAMES.iter().any(|agent| {
        lower
            .strip_prefix(agent)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '-']))
    })
}

/// What the parent walk needs to know about one process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    pub ppid: u32,
    /// Its process name and, where available, its executable's base name.
    pub names: Vec<String>,
}

/// Walk parent PIDs from `start` (at most [`MAX_ANCESTRY_HOPS`] processes, stopping at PID 1, a
/// PID already seen or a process `lookup` cannot read) and return the first AI agent's PID.
pub fn find_agent(start: u32, lookup: impl Fn(u32) -> Option<ProcInfo>) -> Option<u32> {
    let mut seen: Vec<u32> = Vec::with_capacity(MAX_ANCESTRY_HOPS);
    let mut pid = start;
    for _ in 0..MAX_ANCESTRY_HOPS {
        if pid <= 1 || seen.contains(&pid) {
            return None;
        }
        seen.push(pid);
        let info = lookup(pid)?;
        if info.names.iter().any(|n| is_agent_name(n)) {
            return Some(pid);
        }
        pid = info.ppid;
    }
    None
}

/// The PID of this process's nearest AI-agent ancestor, if any.
pub fn nearest_agent_ancestor() -> Option<u32> {
    #[cfg(unix)]
    {
        find_agent(std::os::unix::process::parent_id(), os_lookup)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn os_lookup(pid: u32) -> Option<ProcInfo> {
    fn c_name(chars: &[libc::c_char]) -> String {
        let bytes: Vec<u8> = chars
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| *c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    let pid = i32::try_from(pid).ok()?;
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    // SAFETY: `info` is a writable buffer of exactly `size` bytes, the size passed; proc_pidinfo
    // writes at most that many bytes and returns how many it wrote.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: the buffer started zeroed (a valid value of this plain-data struct) and
    // proc_pidinfo filled all of it (checked above).
    let info = unsafe { info.assume_init() };
    let mut names = vec![c_name(&info.pbi_comm), c_name(&info.pbi_name)];

    let mut buf = vec![0u8; usize::try_from(libc::PROC_PIDPATHINFO_MAXSIZE).ok()?];
    let cap = u32::try_from(buf.len()).ok()?;
    // SAFETY: `buf` is writable for `cap` bytes, the size passed; proc_pidpath returns the length
    // it wrote, or <= 0 on failure.
    let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), cap) };
    if let Ok(len) = usize::try_from(len)
        && len > 0
    {
        let path = String::from_utf8_lossy(&buf[..len.min(buf.len())]).into_owned();
        names.push(basename(&path).to_string());
    }
    Some(ProcInfo {
        ppid: info.pbi_ppid,
        names,
    })
}

#[cfg(target_os = "linux")]
fn os_lookup(pid: u32) -> Option<ProcInfo> {
    // `<pid> (<comm>) <state> <ppid> ...`; comm may itself contain `)`, so split at the last one.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (head, rest) = stat.rsplit_once(')')?;
    let comm = head.split_once('(')?.1.to_string();
    let ppid = rest.split_whitespace().nth(1)?.parse().ok()?;
    let mut names = vec![comm];
    if let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe"))
        && let Some(name) = exe.file_name()
    {
        names.push(name.to_string_lossy().into_owned());
    }
    Some(ProcInfo { ppid, names })
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn os_lookup(_pid: u32) -> Option<ProcInfo> {
    None
}

// ---- routing ------------------------------------------------------------------------------------

/// Which path a shim call takes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Route {
    Transparent,
    Agent { argv: Vec<String>, key: Key },
}

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// stdout is a device (`> /dev/null`, a serial line): never a reader worth dieting for. An stdout
/// that cannot be inspected counts as one too: the safe answer is not to capture.
#[cfg(unix)]
fn stdout_is_device() -> bool {
    use std::os::fd::AsFd;
    use std::os::unix::fs::FileTypeExt;
    let Ok(fd) = std::io::stdout().as_fd().try_clone_to_owned() else {
        return true;
    };
    std::fs::File::from(fd)
        .metadata()
        .map_or(true, |m| m.file_type().is_char_device())
}

#[cfg(not(unix))]
fn stdout_is_device() -> bool {
    true
}

/// What an open file descriptor refers to, comparable across processes: two descriptors with the
/// same identity write to the same place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenFile {
    /// A file, FIFO or device, by device and inode.
    Vnode { dev: u64, ino: u64 },
    /// A pipe end, by its kernel handle (macOS; Linux pipes are vnodes of pipefs).
    Pipe(u64),
}

/// `true` if this process's stdout is the very same open file as its parent's stdout: the output
/// goes wherever the parent's goes (an agent's capture file or pipe), not to a file, a device or a
/// pipe the parent set up for this one command (`> out.txt`, `| tail`). Unknown means `false`.
fn stdout_is_parents() -> bool {
    #[cfg(unix)]
    {
        let own = fd_identity(std::process::id(), 1);
        own.is_some() && own == fd_identity(std::os::unix::process::parent_id(), 1)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(target_os = "linux")]
fn fd_identity(pid: u32, fd: i32) -> Option<OpenFile> {
    use std::os::unix::fs::MetadataExt;
    // The link is followed, so a pipe resolves to its pipefs inode.
    let meta = std::fs::metadata(format!("/proc/{pid}/fd/{fd}")).ok()?;
    Some(OpenFile::Vnode {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

/// `struct proc_fileinfo` from `<sys/proc_info.h>` (not in the libc crate).
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct ProcFileInfo {
    fi_openflags: u32,
    fi_status: u32,
    fi_offset: libc::off_t,
    fi_type: i32,
    fi_guardflags: u32,
}

/// `struct vnode_fdinfowithpath` (flavor `PROC_PIDFDVNODEPATHINFO`).
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct VnodeFdInfoWithPath {
    pfi: ProcFileInfo,
    pvip: libc::vnode_info_path,
}

/// `struct pipe_info`.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct PipeInfo {
    pipe_stat: libc::vinfo_stat,
    pipe_handle: u64,
    pipe_peerhandle: u64,
    pipe_status: i32,
    rfu_1: i32,
}

/// `struct pipe_fdinfo` (flavor `PROC_PIDFDPIPEINFO`).
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct PipeFdInfo {
    pfi: ProcFileInfo,
    pipeinfo: PipeInfo,
}

#[cfg(target_os = "macos")]
const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;
#[cfg(target_os = "macos")]
const PROC_PIDFDPIPEINFO: libc::c_int = 6;

/// `proc_pidfdinfo` into a `T`, if the kernel filled exactly a whole one. Only called with the
/// plain-data `repr(C)` structs above, for which all-zero bytes are a valid value.
#[cfg(target_os = "macos")]
fn pidfd_info<T: Copy>(pid: u32, fd: i32, flavor: libc::c_int) -> Option<T> {
    let pid = i32::try_from(pid).ok()?;
    let size = i32::try_from(std::mem::size_of::<T>()).ok()?;
    let mut info = std::mem::MaybeUninit::<T>::zeroed();
    // SAFETY: `info` is a writable buffer of exactly `size` bytes, the size passed;
    // proc_pidfdinfo writes at most that many bytes and returns how many it wrote.
    let written = unsafe { libc::proc_pidfdinfo(pid, fd, flavor, info.as_mut_ptr().cast(), size) };
    // SAFETY: the buffer started zeroed (valid for these plain-data structs) and was filled.
    (written == size).then(|| unsafe { info.assume_init() })
}

#[cfg(target_os = "macos")]
fn fd_identity(pid: u32, fd: i32) -> Option<OpenFile> {
    if let Some(v) = pidfd_info::<VnodeFdInfoWithPath>(pid, fd, PROC_PIDFDVNODEPATHINFO) {
        let stat = v.pvip.vip_vi.vi_stat;
        return Some(OpenFile::Vnode {
            dev: u64::from(stat.vst_dev),
            ino: stat.vst_ino,
        });
    }
    pidfd_info::<PipeFdInfo>(pid, fd, PROC_PIDFDPIPEINFO)
        .map(|p| OpenFile::Pipe(p.pipeinfo.pipe_handle))
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn fd_identity(_pid: u32, _fd: i32) -> Option<OpenFile> {
    None
}

/// The SPEC §3 Tier 1 decision order.
fn route(name: &str, args: &[OsString]) -> Route {
    if env_set(NESTED_ENV) || env_set(FORCE_HUMAN_ENV) {
        return Route::Transparent;
    }
    if std::io::stdin().is_terminal() || std::io::stdout().is_terminal() {
        return Route::Transparent;
    }
    if stdout_is_device() {
        return Route::Transparent;
    }
    let Some(argv) = std::iter::once(Some(name.to_string()))
        .chain(args.iter().map(|a| a.to_str().map(str::to_string)))
        .collect::<Option<Vec<String>>>()
    else {
        return Route::Transparent;
    };
    if crate::compress::detect(&argv).is_none() {
        return Route::Transparent;
    }
    if !stdout_is_parents() {
        return Route::Transparent;
    }
    let session = std::env::var(CLAUDE_SESSION_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .map(Key::Session);
    let key = if env_set(FORCE_AGENT_ENV) || ENV_MARKERS.iter().any(|m| env_set(m)) {
        session
            .or_else(|| nearest_agent_ancestor().map(Key::AgentPid))
            .unwrap_or(Key::UnknownAgent)
    } else {
        let Some(pid) = nearest_agent_ancestor() else {
            return Route::Transparent;
        };
        session.unwrap_or(Key::AgentPid(pid))
    };
    Route::Agent { argv, key }
}

/// Run shim `name` with `args`. Never returns. `paths` is consulted only on the agent path.
pub fn run(name: &str, args: Vec<OsString>, paths: impl FnOnce() -> anyhow::Result<Paths>) -> ! {
    let real = find_real(name);
    match route(name, &args) {
        Route::Transparent => exec(&real, args),
        Route::Agent { argv, key } => agent(&real, args, argv, key, paths),
    }
}

/// The real `name` on `PATH`, past the shims; exits 127 if there is none.
fn find_real(name: &str) -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let shims_dir = home.map(|h| PathBuf::from(h).join(".agentdiet").join("shims"));
    let path_var = std::env::var_os("PATH").unwrap_or_default();

    let real = resolve_real(
        name,
        &path_var,
        shims_dir.as_deref().unwrap_or(Path::new("")),
    );
    let Some(real) = real else {
        eprintln!("agentdiet shim: could not find the real `{name}` on PATH");
        std::process::exit(127);
    };
    real
}

#[cfg(unix)]
fn exec(real: &Path, args: Vec<OsString>) -> ! {
    use std::os::unix::process::CommandExt;
    let err = Command::new(real).args(args).exec();
    eprintln!("agentdiet shim: failed to exec {}: {err}", real.display());
    std::process::exit(126)
}

#[cfg(not(unix))]
fn exec(real: &Path, args: Vec<OsString>) -> ! {
    match Command::new(real).args(args).status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("agentdiet shim: failed to run {}: {err}", real.display());
            std::process::exit(126)
        }
    }
}

// ---- the agent path ---------------------------------------------------------------------------

/// Read a pipe to its end. A read error keeps what arrived before it.
fn drain(pipe: Option<impl Read>) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut bytes);
    }
    bytes
}

pub(crate) fn exit_status(status: &std::process::ExitStatus) -> ExitStatus {
    if let Some(code) = status.code() {
        return ExitStatus::Code(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return ExitStatus::Signal(signal);
        }
    }
    ExitStatus::Unknown
}

/// Run the command once, capture both streams, diet each, and exit as the command did.
fn agent(
    real: &Path,
    args: Vec<OsString>,
    argv: Vec<String>,
    key: Key,
    paths: impl FnOnce() -> anyhow::Result<Paths>,
) -> ! {
    let child = Command::new(real)
        .args(&args)
        .env(NESTED_ENV, "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(err) => {
            eprintln!("agentdiet shim: failed to run {}: {err}", real.display());
            std::process::exit(126)
        }
    };
    let out = child.stdout.take();
    let err = child.stderr.take();
    let out = std::thread::spawn(move || drain(out));
    let err = std::thread::spawn(move || drain(err));
    let status = child.wait();
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let exit = status.as_ref().map_or(ExitStatus::Unknown, exit_status);

    // The command has run; from here on nothing may run it again (P2). A panic below falls back
    // to the captured bytes as they are, the best output left once the engine itself failed.
    let emitted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        diet_streams(&stdout, &stderr, argv, exit, &key, paths)
    }));
    let (out_bytes, err_bytes, notes) = match emitted {
        Ok(Some(done)) => done,
        _ => (stdout, stderr, Vec::new()),
    };
    // A reader that went away (EPIPE) must not turn into a different exit status.
    let _ = std::io::stdout().lock().write_all(&out_bytes);
    let _ = std::io::stdout().lock().flush();
    let mut stderr_handle = std::io::stderr().lock();
    let _ = stderr_handle.write_all(&err_bytes);
    for note in notes {
        let _ = writeln!(stderr_handle, "{note}");
    }
    drop(stderr_handle);
    exit_like(status.ok(), exit)
}

/// Engine settings and one engine call per stream. `None` if paths cannot be determined, in which
/// case the caller emits the captured output unchanged.
fn diet_streams(
    stdout: &[u8],
    stderr: &[u8],
    argv: Vec<String>,
    exit: ExitStatus,
    key: &Key,
    paths: impl FnOnce() -> anyhow::Result<Paths>,
) -> Option<(Vec<u8>, Vec<u8>, Vec<String>)> {
    let paths = paths().ok()?;
    let state = state::load(&paths);
    let cwd = std::env::current_dir().unwrap_or_default();
    let joined = argv.join("\0");
    let sig = session::signature(&[
        b"shim",
        cwd.as_os_str().as_encoded_bytes(),
        joined.as_bytes(),
    ]);
    // A sidecar that cannot be read or locked means "no bypass" (fail open).
    let before =
        session::update(&paths, key, |s| s.last_shim_sig = Some(sig.clone())).unwrap_or_default();
    let bypass = if before.last_shim_sig.as_deref() == Some(sig.as_str()) {
        Bypass::RepeatedCommand
    } else if before.raw_tool_use.is_some() || before.raw_turn {
        // Inside the tool call a full-output directive is bound to (or about to be, if no
        // PreToolUse ran). The shim never spends it: the call's own PostToolUse does (SPEC §2.4).
        Bypass::RawNextTurn
    } else {
        Bypass::None
    };
    let settings = Settings {
        mode: state.mode,
        paused: state.status == state::Status::Paused,
        bypass,
        head_tail: false,
    };

    let mut engine = Engine::new(&paths);
    if let Key::Session(id) = key
        && let Some(label) = session::label(id)
    {
        engine = engine.with_session(label);
    }
    let family = argv
        .first()
        .map(|a| basename(a).to_string())
        .unwrap_or_default();
    let title = argv.join(" ");
    let source = Source::Shell { argv, exit };
    let mut notes = Vec::new();
    let mut sha = None;
    let mut run = |bytes: &[u8]| {
        let outcome = engine.run(
            &Input {
                bytes,
                source: &source,
            },
            &settings,
        );
        notes.extend(outcome.note);
        sha = sha.take().or(outcome.sha);
        outcome.output
    };
    let out = run(stdout);
    let err = run(stderr);

    // Every agent-path run is logged, trimmed or not (SPEC §3 Tier 3, "Run log").
    let label = runlog::session_label(key);
    let logged = runlog::record(
        &paths,
        &runlog::Run {
            origin: Origin::Shim,
            kind: RunKind::Shell,
            family: &family,
            title: &title,
            outcome: runlog::outcome_of(exit),
            exit_code: runlog::exit_code_of(exit),
            raw_tokens: (estimate_tokens(stdout) + estimate_tokens(stderr)) as u64,
            emitted_tokens: (estimate_tokens(&out) + estimate_tokens(&err)) as u64,
            cas_sha: sha.as_deref(),
            session: label.as_ref(),
        },
    );
    notes.extend(logged.err());
    Some((out, err, notes))
}

/// Exit with the child's code, or die of the child's signal.
fn exit_like(status: Option<std::process::ExitStatus>, exit: ExitStatus) -> ! {
    match exit {
        ExitStatus::Code(code) => std::process::exit(code),
        ExitStatus::Signal(signal) => {
            #[cfg(unix)]
            // SAFETY: restoring a signal's default disposition and raising it in this process has
            // no memory-safety preconditions. If the default action does not end the process
            // (a stop or ignored signal), execution simply continues to the exit below.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                libc::raise(signal);
            }
            std::process::exit(128 + signal)
        }
        ExitStatus::Unknown => std::process::exit(status.and_then(|s| s.code()).unwrap_or(1)),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::HashMap;
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
        assert_eq!(
            shim_name(OsStr::new("/h/.agentdiet/shims/git")),
            Some("git")
        );
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

    #[test]
    fn agent_names_match_whole_names_only() {
        for yes in [
            "claude",
            "Claude",
            "cursor",
            "Cursor Helper (Plugin)",
            "codex",
            "codex-cli",
            "aider",
            "gemini",
            "copilot",
            "opencode",
            "windsurf",
            "Windsurf.exe",
        ] {
            assert!(is_agent_name(yes), "{yes}");
        }
        for no in [
            "claudette",
            "zsh",
            "Code Helper (Plugin)",
            "node",
            "2.1.302",
            "xclaude",
            "cursorless",
            "",
        ] {
            assert!(!is_agent_name(no), "{no}");
        }
    }

    fn table(rows: &[(u32, u32, &str)]) -> HashMap<u32, ProcInfo> {
        rows.iter()
            .map(|(pid, ppid, name)| {
                (
                    *pid,
                    ProcInfo {
                        ppid: *ppid,
                        names: vec![name.to_string()],
                    },
                )
            })
            .collect()
    }

    #[test]
    fn walk_finds_the_nearest_agent() {
        // zsh(10) <- claude(9) <- zsh(8) <- Code(7) <- launchd(1)
        let t = table(&[
            (10, 9, "zsh"),
            (9, 8, "claude"),
            (8, 7, "zsh"),
            (7, 1, "Code"),
        ]);
        assert_eq!(find_agent(10, |p| t.get(&p).cloned()), Some(9));
        let human = table(&[(10, 8, "zsh"), (8, 7, "Terminal"), (7, 1, "launchd")]);
        assert_eq!(find_agent(10, |p| human.get(&p).cloned()), None);
    }

    #[test]
    fn walk_matches_any_of_a_process_names() {
        let mut t = table(&[(10, 9, "zsh")]);
        t.insert(
            9,
            ProcInfo {
                ppid: 1,
                names: vec!["2.1.302".into(), "claude".into()],
            },
        );
        assert_eq!(find_agent(10, |p| t.get(&p).cloned()), Some(9));
    }

    #[test]
    fn walk_survives_cycles_unreadable_processes_and_long_chains() {
        let cycle = table(&[(10, 11, "a"), (11, 10, "b")]);
        assert_eq!(find_agent(10, |p| cycle.get(&p).cloned()), None);

        let gap = table(&[(10, 99, "zsh")]); // 99 cannot be read
        assert_eq!(find_agent(10, |p| gap.get(&p).cloned()), None);

        // An agent 16 hops up is found; 17 hops up is past the bound.
        let chain = |depth: u32| {
            let mut rows: Vec<(u32, u32, &str)> =
                (0..depth).map(|i| (100 + i, 101 + i, "sh")).collect();
            rows.push((100 + depth, 1, "claude"));
            table(&rows)
        };
        let ok = chain(15);
        assert_eq!(find_agent(100, |p| ok.get(&p).cloned()), Some(115));
        let far = chain(16);
        assert_eq!(find_agent(100, |p| far.get(&p).cloned()), None);
        assert_eq!(find_agent(1, |_| panic!("PID 1 is never looked up")), None);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn fd_identity_tells_open_files_apart() {
        use std::os::fd::AsRawFd;
        let me = std::process::id();
        let dir = TempDir::new().unwrap();
        let a = fs::File::create(dir.path().join("a")).unwrap();
        let a2 = a.try_clone().unwrap();
        let b = fs::File::create(dir.path().join("b")).unwrap();
        let (_r1, w1) = std::io::pipe().unwrap();
        let w1b = w1.try_clone().unwrap();
        let (_r2, w2) = std::io::pipe().unwrap();

        let id = |fd: i32| fd_identity(me, fd).expect("readable");
        assert_eq!(id(a.as_raw_fd()), id(a2.as_raw_fd()));
        assert_ne!(id(a.as_raw_fd()), id(b.as_raw_fd()));
        assert_eq!(id(w1.as_raw_fd()), id(w1b.as_raw_fd()));
        assert_ne!(id(w1.as_raw_fd()), id(w2.as_raw_fd()));
        assert_ne!(id(w1.as_raw_fd()), id(a.as_raw_fd()));
        assert_eq!(fd_identity(me, 9999), None, "not open");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn os_lookup_reads_this_process() {
        let me = os_lookup(std::process::id()).expect("own process is readable");
        assert_eq!(me.ppid, std::os::unix::process::parent_id());
        assert!(me.names.iter().any(|n| !n.is_empty()), "{me:?}");
    }
}
