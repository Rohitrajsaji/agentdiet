//! `agentdiet status` and `agentdiet doctor` (SPEC §3b "Status", "Doctor").
//!
//! Both are read-only: no `state.json` write, no Vault GC, memory.db opened only when it exists.
//! Every file is read and classified, never repaired; the fixes are printed as commands.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::cas::{Cas, CasError};
use crate::init::json_patch::PatchError;
use crate::init::shims::{self, Link, PathStatus};
use crate::init::targets::{self, FileSpec};
use crate::init::{self, Target};
use crate::memory::{MemoryDb, Origin};
use crate::state::{State, Status};
use crate::ui::{Level, Painter, and_list, plural, tilde};
use crate::{Paths, fsutil, words};

/// What a doctor line is about, so a view can place rows without reading their label text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    Binary,
    State,
    Assistant { target: Target, project: bool },
    Others,
    Shims,
    Path,
    Vault,
    Memory,
    Setup,
}

/// How much evidence a connected integration has (SPEC §3b "Doctor"): configuration alone is never
/// "verified".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Observed {
    /// Not an integration, or it has a problem that comes first.
    #[default]
    NotApplicable,
    /// Runs of it were recorded in this workspace.
    Verified,
    /// Configured, and AgentDiet could observe it, but has not yet.
    NotYet,
    /// Configured; AgentDiet cannot attribute runs to this assistant (MCP only).
    NotObservable,
}

/// One doctor line: `<label>  <symbol> <word>  <detail>`, then `→ <fix>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub kind: CheckKind,
    /// The missing agentdiet binary this row is broken by, so rows with one cause can share one fix.
    pub(crate) cause: Option<PathBuf>,
    pub observed: Observed,
    pub label: String,
    pub level: Level,
    pub word: &'static str,
    pub detail: String,
    pub fix: Option<String>,
}

impl Check {
    fn new(label: impl Into<String>, level: Level, word: &'static str, detail: String) -> Self {
        Self {
            kind: CheckKind::Setup,
            cause: None,
            observed: Observed::NotApplicable,
            label: label.into(),
            level,
            word,
            detail,
            fix: None,
        }
    }

    fn kind(mut self, kind: CheckKind) -> Self {
        self.kind = kind;
        self
    }

    fn observed(mut self, observed: Observed) -> Self {
        self.observed = observed;
        self
    }

    fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// What doctor found, plus the facts `status` reuses.
#[derive(Debug, Clone)]
pub struct Report {
    pub checks: Vec<Check>,
    /// Labels of the assistants (and `Shims`) agentdiet is connected to, at any scope.
    pub connected: Vec<&'static str>,
}

impl Report {
    pub fn problems(&self) -> usize {
        self.count(Level::Broken)
    }

    pub fn warnings(&self) -> usize {
        self.count(Level::Warning)
    }

    fn count(&self, level: Level) -> usize {
        self.checks.iter().filter(|c| c.level == level).count()
    }

    /// 0 unless something is `broken`.
    pub fn exit_code(&self) -> u8 {
        u8::from(self.problems() > 0)
    }

    /// The one-line verdict, shared by doctor's last line and status's Health line.
    pub fn verdict(&self) -> String {
        let parts: Vec<String> = [
            (self.problems(), "problem", "problems"),
            (self.warnings(), "warning", "warnings"),
        ]
        .into_iter()
        .filter(|(n, ..)| *n > 0)
        .map(|(n, one, many)| plural(n, one, many))
        .collect();
        if parts.is_empty() {
            "no problems found".into()
        } else {
            parts.join(", ")
        }
    }

    pub fn render(&self, p: Painter) -> String {
        let width = self
            .checks
            .iter()
            .map(|c| c.label.chars().count())
            .max()
            .unwrap_or(0)
            .max(12)
            + 2;
        let mut out = format!("{}\n\n", p.bold("AgentDiet doctor"));
        for c in &self.checks {
            let line = format!(
                "  {:<width$}{}{}",
                c.label,
                p.badge(c.level, c.word, 18),
                c.detail
            );
            out.push_str(line.trim_end());
            out.push('\n');
            if let Some(fix) = &c.fix {
                out.push_str(&format!("  {:width$}  → {fix}\n", ""));
            }
        }
        out.push('\n');
        out.push_str(&if self.problems() + self.warnings() == 0 {
            p.level(Level::Ok, "All good.")
        } else {
            let level = if self.problems() > 0 {
                Level::Broken
            } else {
                Level::Warning
            };
            p.level(level, &format!("{}.", self.verdict()))
        });
        out.push('\n');
        out
    }
}

/// Runs recorded in this workspace's run log, by origin; zero without a memory.db.
#[derive(Debug, Clone, Copy, Default)]
struct Activity {
    hook: u64,
    shim: u64,
    total: u64,
    /// How long ago the newest hook / shim run was recorded.
    hook_age: Option<Duration>,
    shim_age: Option<Duration>,
}

/// `42 s`, `5 min`, `3 h`, `2 days`.
fn age(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        3600..86_400 => format!("{} h", s / 3600),
        _ => plural(
            usize::try_from(s / 86_400).unwrap_or(usize::MAX),
            "day",
            "days",
        ),
    }
}

/// `34 hook runs recorded in this workspace, the last 2 h ago`.
fn runs_seen(n: u64, one: &str, many: &str, last: Option<Duration>) -> String {
    let n = plural(usize::try_from(n).unwrap_or(usize::MAX), one, many);
    match last {
        Some(d) => format!("{n} recorded in this workspace, the last {} ago", age(d)),
        None => format!("{n} recorded in this workspace"),
    }
}

/// Run every doctor check. `path_var` is the `PATH` to check the shims against.
pub fn doctor(paths: &Paths, path_var: &OsStr) -> Report {
    run(paths, path_var, None)
}

/// The checks for just `targets` (at project scope when `project`), plus `PATH` when the shims are
/// among them: what `init` verifies right after connecting (SPEC §2.2). Cheap: it reads only those
/// targets' files and memory.db.
pub fn doctor_for(paths: &Paths, path_var: &OsStr, targets: &[Target], project: bool) -> Report {
    run(paths, path_var, Some((targets, project)))
}

fn run(paths: &Paths, path_var: &OsStr, only: Option<(&[Target], bool)>) -> Report {
    let (memory_check, activity) = memory_check(paths);
    let detected = targets::detect(paths, false);
    let project_scope = paths.project_root != paths.home;

    // Pass 1: classify every target.
    let mut assessed: Vec<(Target, bool, Assessment)> = Vec::new();
    for target in Target::ALL {
        if target == Target::Shims || only.is_some_and(|(list, _)| !list.contains(&target)) {
            continue;
        }
        match only {
            Some((_, project)) => assessed.push((target, project, assess(paths, target, project))),
            None => {
                assessed.push((target, false, assess(paths, target, false)));
                if project_scope {
                    let project = assess(paths, target, true);
                    if project.verdict.is_some() {
                        assessed.push((target, true, project));
                    }
                }
            }
        }
    }

    // One "different build" warning on the Binary row when every such integration agrees.
    let mut other_rows = assessed
        .iter()
        .filter(|(.., a)| matches!(a.verdict, Some(FileState::OtherExe(_))))
        .count();
    let mut others: Vec<PathBuf> = assessed
        .iter()
        .filter_map(|(.., a)| match &a.verdict {
            Some(FileState::OtherExe(exe)) => Some(exe.clone()),
            _ => None,
        })
        .collect();
    if only.is_none() {
        let shim_others: Vec<PathBuf> = shims::inspect(paths)
            .into_iter()
            .filter_map(|(_, l)| match l {
                Link::Other(exe) => Some(exe),
                _ => None,
            })
            .collect();
        if !shim_others.is_empty() {
            other_rows += 1;
            others.extend(shim_others);
        }
    }
    others.sort();
    others.dedup();
    let collapse =
        (only.is_none() && others.len() == 1 && other_rows >= 2).then(|| others[0].clone());

    // Pass 2: the lines.
    let mut checks = Vec::new();
    let mut connected = Vec::new();
    let mut not_installed = Vec::new();
    for (target, project, assessment) in assessed {
        let kind = CheckKind::Assistant { target, project };
        match assessment.verdict {
            None if only.is_some() || detected.contains(&target) => {
                checks.push(not_connected(target, assessment.note).kind(kind));
            }
            None => not_installed.push(target.label()),
            Some(_) => {
                if !connected.contains(&target.label()) {
                    connected.push(target.label());
                }
                let label = if project {
                    format!("{} (project)", target.label())
                } else {
                    target.label().to_string()
                };
                checks.push(
                    assessment
                        .into_check(paths, target, label, &activity, collapse.as_deref())
                        .kind(kind),
                );
            }
        }
    }
    if only.is_none() && !not_installed.is_empty() {
        checks.push(
            Check::new(
                "Others",
                Level::Info,
                "not installed",
                not_installed.join(", "),
            )
            .kind(CheckKind::Others),
        );
    }

    if only.is_none_or(|(list, project)| list.contains(&Target::Shims) && !project) {
        let mut shim = shim_checks(paths, path_var, &activity, collapse.as_deref());
        if shim.first().is_some_and(|c| c.word != "not installed") {
            connected.push("Shims");
        }
        for (i, c) in shim.iter_mut().enumerate() {
            c.kind = if i == 0 {
                CheckKind::Shims
            } else {
                CheckKind::Path
            };
        }
        checks.extend(shim);
    }

    if only.is_none() {
        checks.insert(0, state_check(paths).kind(CheckKind::State));
        let this = tilde(&paths.home, &paths.exe);
        let version = env!("CARGO_PKG_VERSION");
        checks.insert(
            0,
            match &collapse {
                Some(exe) => Check::new(
                    "Binary",
                    Level::Warning,
                    "warning",
                    format!(
                        "{this} ({version}); {} run {}, a different build",
                        plural(other_rows, "integration", "integrations"),
                        tilde(&paths.home, exe)
                    ),
                ),
                None => Check::new("Binary", Level::Ok, "ok", format!("{this} ({version})")),
            }
            .kind(CheckKind::Binary),
        );
        checks.push(vault_check(paths).kind(CheckKind::Vault));
        checks.push(memory_check.kind(CheckKind::Memory));
        if connected.is_empty() {
            checks.push(
                Check::new(
                    "Setup",
                    Level::Broken,
                    "broken",
                    "nothing is connected, so AgentDiet never sees any output".into(),
                )
                .fix("agentdiet init")
                .kind(CheckKind::Setup),
            );
        }
    }
    share_missing_binary_fix(&mut checks, paths);
    Report { checks, connected }
}

/// Several rows broken by one missing binary (it was moved or deleted) get one fix, on the last of
/// them, naming every target to reconnect; the others keep their line but not a copy of the fix.
fn share_missing_binary_fix(checks: &mut [Check], paths: &Paths) {
    let causes: Vec<PathBuf> = checks.iter().filter_map(|c| c.cause.clone()).collect();
    for cause in causes.iter().collect::<std::collections::BTreeSet<_>>() {
        let rows: Vec<usize> = (0..checks.len())
            .filter(|&i| checks[i].cause.as_ref() == Some(cause))
            .collect();
        if rows.len() < 2 {
            continue;
        }
        let slugs: Vec<&str> = rows
            .iter()
            .map(|&i| match checks[i].kind {
                CheckKind::Assistant { target, .. } => target.slug(),
                _ => "shims",
            })
            .collect();
        for &i in &rows {
            checks[i].fix = None;
        }
        if let Some(&last) = rows.last() {
            checks[last].fix = Some(format!(
                "same cause for all {}: put agentdiet back at {}, or run `agentdiet init --only {}` \
                 from the agentdiet binary they should use",
                rows.len(),
                tilde(&paths.home, cause),
                slugs.join(",")
            ));
        }
    }
}

fn state_check(paths: &Paths) -> Check {
    let path = paths.state_file();
    let shown = tilde(&paths.home, &path);
    let broken = |why: String| {
        Check::new(
            "State",
            Level::Broken,
            "broken",
            format!("{shown} {why}; hooks and shims use the defaults"),
        )
        .fix(format!(
            "rm {shown}  (AgentDiet starts again from defaults; its counters are lost)"
        ))
    };
    match fsutil::read_optional_regular(&path) {
        Ok(None) => Check::new(
            "State",
            Level::Ok,
            "ok",
            "no state yet (defaults: active, Balanced mode)".into(),
        ),
        Ok(Some(bytes)) => match serde_json::from_slice::<State>(&bytes) {
            Ok(state) => Check::new("State", Level::Ok, "ok", describe_state(&state)),
            Err(e) => broken(format!("is not valid state JSON ({e})")),
        },
        Err(e) => broken(format!("cannot be read ({e})")),
    }
}

fn describe_state(state: &State) -> String {
    let status = match state.status {
        Status::Active => "active",
        Status::Paused => "paused",
    };
    format!("{status}, {} mode", state.mode.label())
}

fn memory_check(paths: &Paths) -> (Check, Activity) {
    let db = paths.memory_db();
    let shown = tilde(&paths.home, &db);
    if !db.is_file() {
        let check = Check::new(
            "Memory",
            Level::Ok,
            "ok",
            "nothing recorded in this workspace yet".into(),
        );
        return (check, Activity::default());
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let ago = |ms: Option<i64>| {
        ms.map(|ms| Duration::from_millis(u64::try_from(now_ms.saturating_sub(ms)).unwrap_or(0)))
    };
    let opened = MemoryDb::open(&db).and_then(|m| {
        let hook = m.last_run_ms(Origin::Hook)?;
        let shim = m.last_run_ms(Origin::Shim)?;
        Ok((m.run_totals()?, hook, shim))
    });
    match opened {
        Ok((totals, hook_ms, shim_ms)) => {
            let mut activity = Activity {
                hook_age: ago(hook_ms),
                shim_age: ago(shim_ms),
                ..Activity::default()
            };
            for (origin, t) in totals {
                activity.total += t.runs;
                match origin {
                    Origin::Hook => activity.hook = t.runs,
                    Origin::Shim => activity.shim = t.runs,
                    Origin::Mcp => {}
                }
            }
            let detail = format!(
                "{} in {shown}",
                plural(
                    usize::try_from(activity.total).unwrap_or(usize::MAX),
                    "run",
                    "runs"
                )
            );
            (Check::new("Memory", Level::Ok, "ok", detail), activity)
        }
        Err(e) => {
            let check = Check::new(
                "Memory",
                Level::Broken,
                "broken",
                format!("{shown} cannot be opened ({e})"),
            )
            .fix(format!(
                "move {shown} aside; AgentDiet starts a new one on the next run"
            ));
            (check, Activity::default())
        }
    }
}

fn vault_check(paths: &Paths) -> Check {
    let cas = Cas::new(paths.cas_dir()).lock_timeout(std::time::Duration::from_millis(500));
    match cas.stats() {
        Ok(stats) => Check::new("Vault", Level::Ok, "ok", crate::words::vault(&stats)),
        Err(CasError::Busy(_)) => Check::new(
            "Vault",
            Level::Info,
            "busy",
            "another agentdiet process is writing to it; run doctor again".into(),
        ),
        Err(e) => Check::new(
            "Vault",
            Level::Broken,
            "broken",
            format!(
                "{} cannot be listed ({e})",
                tilde(&paths.home, &paths.cas_dir())
            ),
        ),
    }
}

// ---- assistants --------------------------------------------------------------------------------

/// One config file's state, worst first within each level (SPEC §3b "Doctor").
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum FileState {
    /// The executable the entry runs does not exist.
    MissingExe(PathBuf),
    /// Not valid JSON/TOML, or an unexpected shape: cannot be checked or updated.
    Invalid(String),
    /// Cannot be read at all.
    Unreadable(String),
    /// `init` deliberately leaves it alone.
    Conflict(String),
    /// Points at another agentdiet executable that exists.
    OtherExe(PathBuf),
    /// agentdiet's entry is absent here while present in another of the target's files, or it
    /// differs from what `init` would write.
    /// `exe` is the other build the config points at, when it is not this one: the fix must run
    /// from that binary, never from this one.
    Incomplete {
        why: String,
        exe: Option<PathBuf>,
    },
    Ok,
    /// No agentdiet entry.
    Absent,
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

fn classify(paths: &Paths, file: &FileSpec) -> FileState {
    let shown = tilde(&paths.home, &file.path);
    let bytes = match fsutil::read_optional(&file.path) {
        Ok(None) => return FileState::Absent,
        Ok(Some(bytes)) => bytes,
        Err(e) => return FileState::Unreadable(format!("{shown} cannot be read ({e})")),
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return FileState::Invalid(format!("{shown} is not UTF-8"));
    };
    // `patch` is pure: it reports what `init` would do without writing anything.
    match file.kind.patch(Some(text), &paths.exe) {
        Err(PatchError::Conflict(why)) if why.starts_with("not valid TOML") => {
            return FileState::Invalid(format!("{shown} is not valid TOML"));
        }
        Err(PatchError::Conflict(why)) => return FileState::Conflict(format!("{shown}: {why}")),
        Err(PatchError::Parse(_)) => {
            return FileState::Invalid(format!(
                "{shown} is not strict JSON (comments or trailing commas?)"
            ));
        }
        Err(e @ (PatchError::NotObject | PatchError::BadShape(_))) => {
            return FileState::Invalid(format!("{shown}: {e}"));
        }
        Ok(_) => {}
    }
    if !file.kind.is_present(text) {
        return FileState::Absent;
    }
    // Completeness is judged against the binary the config points at, so an incomplete config on
    // another build is not mistaken for a complete one (and `init` from here is never suggested).
    let (exe, other) = match file.kind.configured_exe(text) {
        None => (paths.exe.clone(), false),
        Some(exe) if !exe.exists() => return FileState::MissingExe(exe),
        Some(exe) if same_file(&exe, &paths.exe) => (paths.exe.clone(), false),
        Some(exe) => (exe, true),
    };
    match file.kind.patch(Some(text), &exe) {
        Ok(None) if other => FileState::OtherExe(exe),
        Ok(None) => FileState::Ok,
        _ => FileState::Incomplete {
            why: format!("{shown} differs from what init writes"),
            exe: other.then_some(exe),
        },
    }
}

/// A target at one scope: `verdict` is `None` when agentdiet is not connected there.
#[derive(Clone)]
struct Assessment {
    verdict: Option<FileState>,
    /// Every file's state (an absent file counts as incomplete), so the "different build" collapse can
    /// be applied per file before the worst is picked.
    files: Vec<FileState>,
    /// For a target that is not connected: an invalid file `init` would skip.
    note: Option<String>,
}

fn assess(paths: &Paths, target: Target, project: bool) -> Assessment {
    let files = targets::files(paths, target, project).unwrap_or_default();
    let states: Vec<(FileState, bool)> = files
        .iter()
        .map(|f| {
            let state = classify(paths, f);
            // An unparseable file "counts" as agentdiet's only when it mentions agentdiet: a JSONC
            // config of an assistant the user never connected is not a problem of ours.
            let mentions = matches!(state, FileState::Invalid(_) | FileState::Unreadable(_))
                && fsutil::read_optional(&f.path)
                    .ok()
                    .flatten()
                    .is_some_and(|b| String::from_utf8_lossy(&b).contains("agentdiet"));
            (state, mentions)
        })
        .collect();
    let connected = states.iter().any(|(s, mentions)| match s {
        FileState::Absent => false,
        FileState::Invalid(_) | FileState::Unreadable(_) => *mentions,
        _ => true,
    });
    if !connected {
        let note = states.iter().find_map(|(s, _)| match s {
            FileState::Invalid(why) | FileState::Unreadable(why) => Some(why.clone()),
            _ => None,
        });
        return Assessment {
            verdict: None,
            files: Vec::new(),
            note,
        };
    }
    // A file with no entry belongs to a target whose other files may run another build; the fix
    // for it has to run from that build too.
    let sibling_exe = states.iter().find_map(|(s, _)| match s {
        FileState::OtherExe(exe) => Some(exe.clone()),
        FileState::Incomplete { exe: Some(exe), .. } => Some(exe.clone()),
        _ => None,
    });
    let states: Vec<FileState> = files
        .iter()
        .zip(states)
        .map(|(f, (s, _))| match s {
            FileState::Absent => FileState::Incomplete {
                why: format!("{} has no AgentDiet entry", tilde(&paths.home, &f.path)),
                exe: sibling_exe.clone(),
            },
            other => other,
        })
        .collect();
    Assessment {
        verdict: Some(states.iter().min().cloned().unwrap_or(FileState::Ok)),
        files: states,
        note: None,
    }
}

impl Assessment {
    /// The worst file state once a "different build" that the Binary row already reports counts as
    /// this one. Applied per file, so the next-worst state (an incomplete file, say) still surfaces.
    fn resolve(&self, collapse: Option<&Path>) -> Option<FileState> {
        self.verdict.as_ref()?;
        Some(
            self.files
                .iter()
                .map(|f| match f {
                    FileState::OtherExe(exe) if collapse == Some(exe.as_path()) => FileState::Ok,
                    other => other.clone(),
                })
                .min()
                .unwrap_or(FileState::Ok),
        )
    }
}

fn not_connected(target: Target, note: Option<String>) -> Check {
    let init = format!("agentdiet init --only {}", target.slug());
    match note {
        Some(why) => Check::new(
            target.label(),
            Level::Info,
            "not connected",
            format!("detected; {why}, so init would skip it"),
        )
        .fix(format!("fix that file first, then run: {init}")),
        None => Check::new(
            target.label(),
            Level::Info,
            "not connected",
            "detected".into(),
        )
        .fix(init),
    }
}

impl Assessment {
    fn into_check(
        self,
        paths: &Paths,
        target: Target,
        label: String,
        activity: &Activity,
        collapse: Option<&Path>,
    ) -> Check {
        let init = format!("agentdiet init --only {}", target.slug());
        let this = tilde(&paths.home, &paths.exe);
        let verdict = self.resolve(collapse).unwrap_or(FileState::Ok);
        match verdict {
            FileState::MissingExe(exe) => {
                let mut check = Check::new(
                    label,
                    Level::Broken,
                    "broken",
                    format!("configured binary {} is missing", tilde(&paths.home, &exe)),
                )
                .fix(format!(
                    "put agentdiet back at {}, or run `{init}` from the agentdiet binary it should use",
                    tilde(&paths.home, &exe)
                ));
                check.cause = Some(exe);
                check
            }
            FileState::Invalid(why) => Check::new(
                label,
                Level::Warning,
                "warning",
                format!("{why}; AgentDiet cannot check or update it"),
            )
            .fix(format!("fix the file, then run: {init}")),
            FileState::Unreadable(why) => Check::new(label, Level::Warning, "warning", why),
            FileState::Conflict(why) => {
                Check::new(label, Level::Warning, "warning", format!("skipped by init: {why}"))
            }
            FileState::OtherExe(exe) => Check::new(
                label,
                Level::Warning,
                "warning",
                format!(
                    "uses a different agentdiet: {} (this is {this})",
                    tilde(&paths.home, &exe)
                ),
            ),
            FileState::Incomplete { why, exe } => {
                let fix = match exe {
                    Some(exe) => format!("run `{init}` from {}", tilde(&paths.home, &exe)),
                    None => init,
                };
                Check::new(label, Level::Warning, "warning", format!("incomplete: {why}")).fix(fix)
            }
            FileState::Ok | FileState::Absent => match target {
                Target::Claude if activity.hook > 0 => Check::new(
                    label,
                    Level::Ok,
                    "verified",
                    runs_seen(activity.hook, "hook run", "hook runs", activity.hook_age),
                )
                .observed(Observed::Verified),
                Target::Claude => Check::new(
                    label,
                    Level::Ok,
                    "configured",
                    "not yet observed: no activity recorded in this workspace; restart Claude Code if it was running"
                        .into(),
                )
                .observed(Observed::NotYet),
                _ => Check::new(label, Level::Ok, "configured", "MCP server registered".into())
                    .observed(Observed::NotObservable),
            },
        }
    }
}

// ---- shims --------------------------------------------------------------------------------------

fn shim_checks(
    paths: &Paths,
    path_var: &OsStr,
    activity: &Activity,
    collapse: Option<&Path>,
) -> Vec<Check> {
    // A "different build" that the Binary row already reports counts as this one here.
    let links: Vec<(&'static str, Link)> = shims::inspect(paths)
        .into_iter()
        .map(|(name, link)| match link {
            Link::Other(exe) if collapse == Some(exe.as_path()) => (name, Link::Current),
            other => (name, other),
        })
        .collect();
    let total = links.len();
    let current = links.iter().filter(|(_, l)| *l == Link::Current).count();
    let ours = links.iter().filter(|(_, l)| *l != Link::Missing).count();
    if ours == 0 {
        return vec![
            Check::new(
                "Shims",
                Level::Info,
                "not installed",
                "optional: trims git, cargo, npm, … output for any agent".into(),
            )
            .fix("agentdiet init --only shims"),
        ];
    }
    let fix = "run `agentdiet init --only shims` from the agentdiet binary they should use";
    let first = |pred: fn(&Link) -> Option<&PathBuf>| links.iter().find_map(|(_, l)| pred(l));
    let shim = if let Some(exe) = first(|l| match l {
        Link::Dangling(p) => Some(p),
        _ => None,
    }) {
        let n = links
            .iter()
            .filter(|(_, l)| matches!(l, Link::Dangling(_)))
            .count();
        let mut check = Check::new(
            "Shims",
            Level::Broken,
            "broken",
            format!(
                "{} point at {}, which is missing",
                plural(n, "shim", "shims"),
                tilde(&paths.home, exe)
            ),
        )
        .fix(fix);
        check.cause = Some(exe.clone());
        check
    } else if let Some(exe) = first(|l| match l {
        Link::Other(p) => Some(p),
        _ => None,
    }) {
        Check::new(
            "Shims",
            Level::Warning,
            "warning",
            format!(
                "use a different agentdiet: {} (this is {})",
                tilde(&paths.home, exe),
                tilde(&paths.home, &paths.exe)
            ),
        )
    } else if current < total {
        let missing: Vec<&str> = links
            .iter()
            .filter(|(_, l)| *l == Link::Missing)
            .map(|(n, _)| *n)
            .collect();
        Check::new(
            "Shims",
            Level::Warning,
            "warning",
            format!(
                "incomplete: {current} of {total} commands linked (missing: {})",
                missing.join(", ")
            ),
        )
        .fix("agentdiet init --only shims")
    } else if activity.shim > 0 {
        Check::new(
            "Shims",
            Level::Ok,
            "verified",
            runs_seen(activity.shim, "shim run", "shim runs", activity.shim_age),
        )
        .observed(Observed::Verified)
    } else {
        Check::new(
            "Shims",
            Level::Ok,
            "configured",
            format!(
                "{total} commands linked; not yet observed: no shim runs recorded in this workspace"
            ),
        )
        .observed(Observed::NotYet)
    };

    let dir = tilde(&paths.home, &paths.shims_dir());
    let path = match shims::path_status(path_var, &paths.shims_dir()) {
        PathStatus::First => Check::new(
            "PATH",
            Level::Ok,
            "ok",
            format!("{dir} comes first in this shell"),
        ),
        PathStatus::Missing => Check::new(
            "PATH",
            Level::Broken,
            "broken",
            format!("{dir} is not on PATH in this shell, so the shims are never reached"),
        )
        .fix(format!("add to your shell rc: {}", init::path_line(paths))),
        PathStatus::Shadowed { command, found } => Check::new(
            "PATH",
            Level::Warning,
            "warning",
            format!(
                "`{command}` resolves to {} before the shims in this shell",
                tilde(&paths.home, &found)
            ),
        )
        .fix(format!("put the shims first: {}", init::path_line(paths))),
    };
    vec![shim, path]
}

// ---- status ---------------------------------------------------------------------------------------

/// `agentdiet status`: the §3b overview, from `state.json` and a doctor run.
pub fn render_status(state: &State, report: &Report, p: Painter) -> String {
    let status = match state.status {
        Status::Active => p.level(Level::Ok, "active"),
        Status::Paused => p.level(Level::Warning, "paused"),
    };
    let mut out = format!(
        "{} {status} · {} mode\n\n",
        p.bold("AgentDiet is"),
        state.mode.label()
    );
    let row = |out: &mut String, label: &str, text: String| {
        out.push_str(&format!("  {label:<14}{text}\n"));
    };
    let hint = |cmd: &str| p.dim(&format!("· {cmd}"));

    let assistants = if report.connected.is_empty() {
        format!("none connected {}", hint("agentdiet init"))
    } else {
        and_list(&report.connected)
    };
    row(&mut out, "Assistants", assistants);

    if state.trimmed_outputs == 0 {
        row(&mut out, "Savings", "nothing trimmed yet".into());
    } else {
        row(
            &mut out,
            "Savings",
            format!(
                "{} in {} (all projects, estimated)",
                words::tokens_saved(state),
                words::trimmed_outputs(state)
            ),
        );
    }
    if let Some(mut text) = words::reopens(state) {
        if words::reopens_high(state) {
            text.push(' ');
            text.push_str(&p.level(Level::Warning, words::REOPEN_WARNING));
        }
        row(&mut out, "Re-opened", text);
    }
    if state.status == Status::Paused {
        row(
            &mut out,
            "Paused",
            format!(
                "{} {}",
                words::passed_while_paused(state),
                hint("agentdiet resume")
            ),
        );
    }
    if let Some(id) = &state.last_trimmed_id {
        row(
            &mut out,
            "Last trimmed",
            format!("#{id} {}", hint("agentdiet show last")),
        );
    }
    let health = if report.problems() + report.warnings() == 0 {
        p.badge(Level::Ok, "no problems found", 0)
    } else {
        let level = if report.problems() > 0 {
            Level::Broken
        } else {
            Level::Warning
        };
        format!(
            "{} {}",
            p.badge(level, &report.verdict(), 0),
            hint("agentdiet doctor")
        )
    };
    row(&mut out, "Health", health);
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::init::InitOptions;

    struct Fixture {
        _dir: TempDir,
        paths: Paths,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().unwrap();
        let exe = dir.path().join("bin/agentdiet");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "").unwrap();
        let paths = Paths::new(
            dir.path().join("home"),
            dir.path().join("cfg"),
            dir.path().join("proj"),
            exe,
        );
        Fixture { _dir: dir, paths }
    }

    fn connect(paths: &Paths, targets: &[Target], project: bool) {
        let opts = InitOptions {
            targets: targets.to_vec(),
            project,
            dry_run: false,
        };
        crate::init::run(paths, &opts).unwrap();
    }

    fn verdict(paths: &Paths, target: Target, project: bool) -> Option<FileState> {
        assess(paths, target, project).verdict
    }

    #[test]
    fn a_fresh_connection_is_ok_and_an_untouched_target_is_not_connected() {
        let f = fixture();
        connect(&f.paths, &[Target::Claude, Target::Codex], false);
        assert_eq!(
            verdict(&f.paths, Target::Claude, false),
            Some(FileState::Ok)
        );
        assert_eq!(verdict(&f.paths, Target::Codex, false), Some(FileState::Ok));
        assert_eq!(verdict(&f.paths, Target::Cursor, false), None);
    }

    #[test]
    fn the_worst_file_decides_and_a_missing_binary_beats_a_missing_entry() {
        let f = fixture();
        connect(&f.paths, &[Target::Claude], false);
        let rules = f.paths.home.join(".claude/rules/agentdiet.md");
        fs::remove_file(&rules).unwrap();
        assert!(matches!(
            verdict(&f.paths, Target::Claude, false),
            Some(FileState::Incomplete { why, .. }) if why.contains("agentdiet.md has no AgentDiet entry")
        ));

        let settings = f.paths.home.join(".claude/settings.json");
        let text = fs::read_to_string(&settings).unwrap();
        let exe = f.paths.exe.display().to_string();
        fs::write(&settings, text.replace(&exe, "/gone/agentdiet")).unwrap();
        assert_eq!(
            verdict(&f.paths, Target::Claude, false),
            Some(FileState::MissingExe("/gone/agentdiet".into()))
        );
    }

    #[test]
    fn a_complete_entry_for_another_binary_is_other_not_incomplete() {
        let f = fixture();
        let other = f.paths.home.join("old/agentdiet");
        fs::create_dir_all(other.parent().unwrap()).unwrap();
        fs::write(&other, "").unwrap();
        let mut old = f.paths.clone();
        old.exe = other.clone();
        connect(&old, &[Target::Cursor], false);
        assert_eq!(
            verdict(&f.paths, Target::Cursor, false),
            Some(FileState::OtherExe(other))
        );
    }

    #[test]
    fn invalid_files_matter_only_when_they_mention_agentdiet() {
        let f = fixture();
        let cursor = f.paths.home.join(".cursor/mcp.json");
        fs::create_dir_all(cursor.parent().unwrap()).unwrap();
        fs::write(&cursor, "{ // mine\n}").unwrap();
        let a = assess(&f.paths, Target::Cursor, false);
        assert_eq!(a.verdict, None);
        assert!(a.note.unwrap().contains("not strict JSON"));

        fs::write(&cursor, "{ // agentdiet\n}").unwrap();
        assert!(matches!(
            verdict(&f.paths, Target::Cursor, false),
            Some(FileState::Invalid(_))
        ));
    }

    #[test]
    fn project_scope_is_reported_only_where_it_is_connected() {
        let f = fixture();
        fs::create_dir_all(&f.paths.project_root).unwrap();
        connect(&f.paths, &[Target::Gemini], true);
        let report = doctor(&f.paths, OsStr::new(""));
        let labels: Vec<&str> = report.checks.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.contains(&"Gemini CLI (project)"), "{labels:?}");
        assert!(!labels.contains(&"Cursor (project)"), "{labels:?}");
        assert_eq!(report.connected, ["Gemini CLI"]);
        assert_eq!(report.exit_code(), 0, "{}", report.render(Painter::plain()));
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age(Duration::from_secs(5)), "5 s");
        assert_eq!(age(Duration::from_secs(300)), "5 min");
        assert_eq!(age(Duration::from_secs(7_200)), "2 h");
        assert_eq!(age(Duration::from_secs(86_400)), "1 day");
        assert_eq!(
            runs_seen(
                34,
                "hook run",
                "hook runs",
                Some(Duration::from_secs(7_200))
            ),
            "34 hook runs recorded in this workspace, the last 2 h ago"
        );
    }

    #[test]
    fn status_rows_follow_the_state() {
        let f = fixture();
        let report = doctor(&f.paths, OsStr::new(""));
        let state = State {
            status: Status::Paused,
            trimmed_outputs: 4,
            tokens_saved: 134_830,
            cas_dereferences: 1,
            raw_tokens_passed_while_paused: 2_500,
            last_trimmed_id: Some("683db530".into()),
            ..State::default()
        };
        let text = render_status(&state, &report, Painter::plain());
        assert!(
            text.starts_with("AgentDiet is paused · Balanced mode\n\n"),
            "{text}"
        );
        for line in [
            "  Savings       ≈134,830 tokens saved in 4 trimmed outputs (all projects, estimated)",
            "  Re-opened     1 re-open for 4 trimmed outputs (25%) ⚠ high: agents re-read trimmed output; consider a lighter mode",
            "  Paused        ≈2,500 tokens passed untrimmed (still scrubbed) · agentdiet resume",
            "  Last trimmed  #683db530 · agentdiet show last",
            "  Health        ✗ 1 problem · agentdiet doctor",
        ] {
            assert!(text.contains(line), "missing {line:?} in:\n{text}");
        }

        let quiet = render_status(&State::default(), &report, Painter::plain());
        assert!(quiet.contains("  Savings       nothing trimmed yet\n"));
        assert!(
            !quiet.contains("Re-opened") && !quiet.contains("Paused") && !quiet.contains("Last")
        );
    }
}

#[cfg(test)]
mod p1_tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::init::InitOptions;

    fn setup(dir: &TempDir) -> Paths {
        let exe = dir.path().join("bin/agentdiet");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "").unwrap();
        Paths::new(
            dir.path().join("home"),
            dir.path().join("cfg"),
            dir.path().join("proj"),
            exe,
        )
    }

    fn connect(paths: &Paths, targets: &[Target]) {
        let opts = InitOptions {
            targets: targets.to_vec(),
            project: false,
            dry_run: false,
        };
        crate::init::run(paths, &opts).unwrap();
    }

    fn row<'a>(report: &'a Report, label: &str) -> &'a Check {
        report
            .checks
            .iter()
            .find(|c| c.label == label)
            .unwrap_or_else(|| panic!("no {label} row in {:?}", report.checks))
    }

    #[test]
    fn configuration_alone_is_never_verified() {
        let dir = TempDir::new().unwrap();
        let paths = setup(&dir);
        connect(&paths, &[Target::Claude, Target::Cursor, Target::Shims]);
        let on_path = std::env::join_paths([paths.shims_dir()]).unwrap();
        let report = doctor(&paths, &on_path);

        let claude = row(&report, "Claude Code");
        assert_eq!(
            (claude.word, claude.observed),
            ("configured", Observed::NotYet)
        );
        assert!(
            claude.detail.starts_with("not yet observed"),
            "{}",
            claude.detail
        );
        let cursor = row(&report, "Cursor");
        assert_eq!(
            (cursor.word, cursor.observed),
            ("configured", Observed::NotObservable)
        );
        let shims = row(&report, "Shims");
        assert_eq!(
            (shims.word, shims.observed),
            ("configured", Observed::NotYet)
        );
        assert!(report.checks.iter().all(|c| c.word != "verified"));
    }

    #[test]
    fn many_different_builds_are_reported_once_on_the_binary_row() {
        let dir = TempDir::new().unwrap();
        let paths = setup(&dir);
        let old = dir.path().join("old/agentdiet");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::write(&old, "").unwrap();
        let mut older = paths.clone();
        older.exe = old.clone();
        connect(
            &older,
            &[
                Target::Claude,
                Target::Cursor,
                Target::Gemini,
                Target::Shims,
            ],
        );

        let on_path = std::env::join_paths([paths.shims_dir()]).unwrap();
        let report = doctor(&paths, &on_path);
        assert_eq!(report.warnings(), 1, "{}", report.render(Painter::plain()));
        assert_eq!(report.problems(), 0);
        let binary = row(&report, "Binary");
        assert_eq!(binary.level, Level::Warning);
        assert!(
            binary.detail.contains("4 integrations run"),
            "{}",
            binary.detail
        );
        assert!(binary.detail.contains("old/agentdiet"));
        assert_eq!(row(&report, "Cursor").word, "configured");

        // A single odd one out keeps its own, more specific, warning.
        let single = TempDir::new().unwrap();
        let paths = setup(&single);
        let mut older = paths.clone();
        older.exe = single.path().join("bin/other");
        fs::write(&older.exe, "").unwrap();
        connect(&older, &[Target::Cursor]);
        connect(&paths, &[Target::Gemini]);
        let report = doctor(&paths, OsStr::new(""));
        assert_eq!(row(&report, "Binary").level, Level::Ok);
        assert_eq!(row(&report, "Cursor").word, "warning");
    }

    #[test]
    fn an_incomplete_config_on_another_build_is_never_hidden_by_the_collapse() {
        let dir = TempDir::new().unwrap();
        let paths = setup(&dir);
        let old = dir.path().join("old/agentdiet");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::write(&old, "").unwrap();
        let mut older = paths.clone();
        older.exe = old.clone();
        connect(&older, &[Target::Claude, Target::Cursor, Target::Gemini]);
        // Claude's EXPERIENCE.md pointer goes missing: that config is incomplete, on the old build.
        fs::remove_file(paths.home.join(".claude/rules/agentdiet.md")).unwrap();

        let report = doctor(&paths, OsStr::new(""));
        let claude = row(&report, "Claude Code");
        assert_eq!(
            claude.level,
            Level::Warning,
            "{}",
            report.render(Painter::plain())
        );
        assert!(
            claude.detail.starts_with("incomplete:"),
            "{}",
            claude.detail
        );
        let fix = claude.fix.as_deref().unwrap();
        assert!(
            fix.contains("agentdiet init --only claude") && fix.contains("old/agentdiet"),
            "{fix}"
        );
        assert!(
            !fix.contains("bin/agentdiet"),
            "never from this build: {fix}"
        );
        // All three run the old build, so the Binary row says so once; Claude Code's own problem
        // stays on its own row.
        assert_eq!(row(&report, "Binary").level, Level::Warning);
        assert!(row(&report, "Binary").detail.contains("3 integrations run"));
        assert_eq!(row(&report, "Cursor").word, "configured");

        // The same file with only the other file of the target missing is found too.
        let report = doctor(&older, OsStr::new(""));
        assert_eq!(row(&report, "Claude Code").level, Level::Warning);
        assert!(
            !row(&report, "Claude Code")
                .fix
                .as_deref()
                .unwrap()
                .contains(" from ")
        );
    }

    #[test]
    fn one_missing_binary_behind_several_rows_gets_one_fix() {
        let dir = TempDir::new().unwrap();
        let paths = setup(&dir);
        let gone = dir.path().join("moved/agentdiet");
        fs::create_dir_all(gone.parent().unwrap()).unwrap();
        fs::write(&gone, "").unwrap();
        let mut moved = paths.clone();
        moved.exe = gone.clone();
        connect(&moved, &[Target::Claude, Target::Cursor, Target::Shims]);
        fs::remove_file(&gone).unwrap();

        let report = doctor(&paths, OsStr::new(""));
        let broken: Vec<&Check> = report.checks.iter().filter(|c| c.cause.is_some()).collect();
        assert_eq!(broken.len(), 3, "{}", report.render(Painter::plain()));
        assert!(
            broken.iter().all(|c| c.level == Level::Broken),
            "still each reported"
        );
        let fixes: Vec<&str> = broken.iter().filter_map(|c| c.fix.as_deref()).collect();
        assert_eq!(fixes.len(), 1, "one shared fix: {fixes:?}");
        assert!(
            fixes[0].contains("agentdiet init --only claude,cursor,shims"),
            "{}",
            fixes[0]
        );
        assert!(fixes[0].contains("put agentdiet back at"), "{}", fixes[0]);
    }

    #[test]
    fn doctor_for_checks_only_the_selected_targets() {
        let dir = TempDir::new().unwrap();
        let paths = setup(&dir);
        connect(&paths, &[Target::Claude, Target::Cursor, Target::Codex]);
        let report = doctor_for(&paths, OsStr::new(""), &[Target::Cursor], false);
        let kinds: Vec<_> = report.checks.iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds,
            [CheckKind::Assistant {
                target: Target::Cursor,
                project: false
            }]
        );

        // The shims bring PATH along; nothing else does.
        connect(&paths, &[Target::Shims]);
        let report = doctor_for(&paths, OsStr::new("/usr/bin"), &[Target::Shims], false);
        let kinds: Vec<_> = report.checks.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [CheckKind::Shims, CheckKind::Path]);
        assert_eq!(row(&report, "PATH").level, Level::Broken);
    }
}
