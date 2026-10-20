//! `agentdiet init` — connect assistants — and `agentdiet eject`.
//!
//! The library entry points ([`run`], [`eject::eject`]) take an injected
//! [`Paths`] and return structured reports; [`cmd_init`] / [`cmd_eject`] are
//! the thin printing wrappers used by the CLI.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

use crate::{Paths, fsutil};

pub mod backup;
pub mod diff;
pub mod eject;
pub mod json_patch;
pub mod rule_pointer;
pub mod shims;
pub mod targets;
pub mod toml_block;

pub use targets::Target;

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("backup manifest {}: {message}", path.display())]
    Manifest { path: PathBuf, message: String },
}

/// `map_err` adapter attaching the failing path to an I/O error.
pub(crate) fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> InitError + '_ {
    move |source| InitError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug, Clone)]
pub struct InitOptions {
    pub targets: Vec<Target>,
    pub project: bool,
    pub dry_run: bool,
}

#[derive(Debug, Default)]
pub struct InitReport {
    /// Files written (or, in a dry run, that would be written).
    pub changed: Vec<PathBuf>,
    /// Files already up to date.
    pub unchanged: Vec<PathBuf>,
    /// Targets or files skipped, with the reason.
    pub skipped: Vec<String>,
    /// Planned changes, rendered as diffs (dry run only).
    pub diffs: Vec<String>,
    /// Where originals were backed up, if anything was.
    pub backup_dir: Option<PathBuf>,
    /// The shims target ran, so the user should be told about `PATH`.
    pub shims_selected: bool,
    /// The same outcome per selected target, in [`Target::ALL`] order: what `init` prints.
    pub targets: Vec<TargetReport>,
}

/// What `init` did for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetReport {
    pub target: Target,
    /// agentdiet was already present in at least one of the target's files (or shims).
    pub was_present: bool,
    /// Files (or shim links) written, or that a dry run would write.
    pub changed: usize,
    /// Files (or shim links) already up to date.
    pub unchanged: usize,
    /// Why files were left alone, without the target label; home paths shown as `~/…`.
    pub skipped: Vec<String>,
}

/// How a [`TargetReport`] reads in one word (SPEC §2.2 "Output").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Connected,
    Updated,
    AlreadyConnected,
    PartlyConnected,
    Skipped,
}

impl TargetReport {
    fn new(target: Target) -> Self {
        Self {
            target,
            was_present: false,
            changed: 0,
            unchanged: 0,
            skipped: Vec::new(),
        }
    }

    pub fn verdict(&self) -> Verdict {
        let done = self.changed + self.unchanged;
        match (done, self.skipped.is_empty(), self.changed) {
            (0, _, _) => Verdict::Skipped,
            (_, false, _) => Verdict::PartlyConnected,
            (_, true, 0) => Verdict::AlreadyConnected,
            _ if self.was_present => Verdict::Updated,
            _ => Verdict::Connected,
        }
    }
}

/// Connect the selected assistants. Idempotent: a file is backed up and
/// rewritten only if its patched content differs from what is on disk.
pub fn run(paths: &Paths, opts: &InitOptions) -> Result<InitReport, InitError> {
    let mut targets = opts.targets.clone();
    targets.sort();
    targets.dedup();

    let mut report = InitReport::default();
    let mut session = backup::Session::new(paths);
    let shown = |p: &Path| crate::ui::tilde(&paths.home, p);

    for target in targets {
        let mut tr = TargetReport::new(target);
        if target == Target::Shims {
            report.shims_selected = true;
            tr.was_present = shims::is_installed(paths);
            run_shims(paths, opts.dry_run, &mut report, &mut tr)?;
            report.targets.push(tr);
            continue;
        }
        let Some(files) = targets::files(paths, target, opts.project) else {
            report.skipped.push(format!(
                "{}: no project-level config; skipped",
                target.label()
            ));
            tr.skipped.push("no project-level config".into());
            report.targets.push(tr);
            continue;
        };
        for file in files {
            let current = match fsutil::read_optional(&file.path) {
                Ok(bytes) => bytes,
                Err(e) => {
                    report.skipped.push(format!(
                        "{}: cannot read {} ({e}); left untouched",
                        target.label(),
                        file.path.display()
                    ));
                    tr.skipped.push(format!(
                        "cannot read {} ({e}); left untouched",
                        shown(&file.path)
                    ));
                    continue;
                }
            };
            let current_text = match current.as_deref().map(std::str::from_utf8).transpose() {
                Ok(text) => text,
                Err(_) => {
                    report.skipped.push(format!(
                        "{}: {} is not UTF-8; left untouched",
                        target.label(),
                        file.path.display()
                    ));
                    tr.skipped.push(format!(
                        "{} is not UTF-8; left untouched",
                        shown(&file.path)
                    ));
                    continue;
                }
            };
            tr.was_present |= current_text.is_some_and(|t| file.kind.is_present(t));
            match file.kind.patch(current_text, &paths.exe) {
                Err(e) => {
                    report.skipped.push(format!(
                        "{}: {}: {e}; left untouched",
                        target.label(),
                        file.path.display()
                    ));
                    // Codex's TOML errors already end in "left untouched".
                    let reason = e.to_string();
                    let tail = if reason.ends_with("left untouched") {
                        ""
                    } else {
                        "; left untouched"
                    };
                    tr.skipped
                        .push(format!("{}: {reason}{tail}", shown(&file.path)));
                }
                Ok(None) => {
                    report.unchanged.push(file.path.clone());
                    tr.unchanged += 1;
                }
                Ok(Some(new)) => {
                    if opts.dry_run {
                        report.diffs.push(diff::render(
                            &shown(&file.path),
                            current_text.unwrap_or(""),
                            &new,
                        ));
                    } else {
                        session.write(target, &file, current.as_deref(), new.as_bytes())?;
                    }
                    report.changed.push(file.path.clone());
                    tr.changed += 1;
                }
            }
        }
        report.targets.push(tr);
    }
    report.backup_dir = session.dir().map(Path::to_path_buf);
    Ok(report)
}

fn run_shims(
    paths: &Paths,
    dry_run: bool,
    report: &mut InitReport,
    tr: &mut TargetReport,
) -> Result<(), InitError> {
    let outcome = shims::install(paths, dry_run)?;
    if dry_run && !outcome.linked.is_empty() {
        let names: Vec<String> = outcome
            .linked
            .iter()
            .filter_map(|l| l.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        report.diffs.push(format!(
            "--- shims\n+ {}/{{{}}} -> {}\n",
            crate::ui::tilde(&paths.home, &paths.shims_dir()),
            names.join(","),
            paths.exe.display()
        ));
    }
    tr.changed = outcome.linked.len();
    tr.unchanged = outcome.unchanged.len();
    let home = paths.home.display().to_string();
    tr.skipped = outcome
        .skipped
        .iter()
        .map(|s| s.strip_prefix("shims: ").unwrap_or(s).replace(&home, "~"))
        .collect();
    report.changed.extend(outcome.linked);
    report.skipped.extend(outcome.skipped);
    report.unchanged.extend(outcome.unchanged);
    Ok(())
}

/// Targets that currently have agentdiet connected at user scope.
pub fn installed(paths: &Paths) -> Vec<Target> {
    Target::ALL
        .into_iter()
        .filter(|&t| match t {
            Target::Shims => shims::is_installed(paths),
            _ => targets::files(paths, t, false)
                .into_iter()
                .flatten()
                .any(|f| {
                    fsutil::read_optional(&f.path)
                        .ok()
                        .flatten()
                        .and_then(|b| String::from_utf8(b).ok())
                        .is_some_and(|text| f.kind.is_present(&text))
                }),
        })
        .collect()
}

fn is_interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Why `init` without `--only`/`--all` cannot run here, and what to pass instead.
fn no_terminal_message(paths: &Paths, project: bool) -> String {
    let detected: Vec<&str> = targets::detect(paths, project)
        .into_iter()
        .map(Target::slug)
        .collect();
    let found = if detected.is_empty() {
        "nothing detected here".to_string()
    } else {
        format!("detected here: {}", detected.join(","))
    };
    format!(
        "init needs a terminal for the checklist. Pass --only <targets> or --all, or add --dry-run \
         to preview ({found}; all targets: claude,cursor,windsurf,gemini,copilot,codex,shims)"
    )
}

/// Interactive checklist of detected assistants.
pub fn choose_interactively(paths: &Paths, project: bool) -> anyhow::Result<Vec<Target>> {
    if !is_interactive() {
        bail!(no_terminal_message(paths, project));
    }
    let detected = targets::detect(paths, project);
    let options: Vec<Target> = Target::ALL
        .into_iter()
        .filter(|&t| !(project && matches!(t, Target::Windsurf | Target::Shims)))
        .collect();
    if options.is_empty() {
        return Ok(Vec::new());
    }
    let defaults: Vec<usize> = options
        .iter()
        .enumerate()
        .filter(|(_, t)| detected.contains(t))
        .map(|(i, _)| i)
        .collect();
    let labels: Vec<&str> = options.iter().map(|t| t.label()).collect();
    let picked = inquire::MultiSelect::new("Connect AgentDiet to which assistants?", labels)
        .with_default(&defaults)
        .with_help_message("space toggles, enter confirms; detected assistants are pre-selected")
        .raw_prompt()
        .context("selection cancelled")?;
    Ok(picked.into_iter().map(|o| options[o.index]).collect())
}

/// `agentdiet init`.
pub fn cmd_init(
    paths: &Paths,
    only: Vec<Target>,
    all: bool,
    project: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let targets = if all {
        Target::ALL.to_vec()
    } else if !only.is_empty() {
        only
    } else if is_interactive() {
        choose_interactively(paths, project)?
    } else if dry_run {
        // A preview is always safe: show what the checklist would pre-select (SPEC §2.2).
        targets::detect(paths, project)
    } else {
        bail!(no_terminal_message(paths, project));
    };
    if targets.is_empty() {
        println!("Nothing selected; nothing to do.");
        return Ok(());
    }

    let report = run(
        paths,
        &InitOptions {
            targets,
            project,
            dry_run,
        },
    )
    .context("init failed")?;

    let mut text: String = report.diffs.iter().map(|d| format!("{d}\n")).collect();
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let on_path = shims::path_status(&path_var, &paths.shims_dir());
    text.push_str(&render(
        &report,
        paths,
        dry_run,
        &on_path,
        crate::ui::Painter::stdout(),
    ));
    crate::ui::emit(&text).context("could not write to stdout")?;
    Ok(())
}

/// The `export PATH=…` line for the shims, with `$HOME` where it applies.
pub fn path_line(paths: &Paths) -> String {
    let dir = paths.shims_dir();
    let shown = match dir.strip_prefix(&paths.home) {
        Ok(rest) => format!("$HOME/{}", rest.display()),
        Err(_) => dir.display().to_string(),
    };
    format!("export PATH=\"{shown}:$PATH\"")
}

/// The SPEC §2.2 summary: one line per target, the backup directory, then the next steps.
pub fn render(
    report: &InitReport,
    paths: &Paths,
    dry_run: bool,
    on_path: &shims::PathStatus,
    p: crate::ui::Painter,
) -> String {
    use crate::ui::{Level, and_list, plural, tilde};

    let label = |t: Target| match t {
        Target::Shims => "Shims",
        _ => t.label(),
    };
    let width = report
        .targets
        .iter()
        .map(|t| label(t.target).len())
        .max()
        .unwrap_or(0)
        .max(12)
        + 2;
    let mut out = String::new();
    for tr in &report.targets {
        let verdict = tr.verdict();
        let (level, word) = match (verdict, dry_run) {
            (Verdict::Connected, false) => (Level::Ok, "connected"),
            (Verdict::Connected, true) => (Level::Ok, "would connect"),
            (Verdict::Updated, false) => (Level::Ok, "updated"),
            (Verdict::Updated, true) => (Level::Ok, "would update"),
            (Verdict::AlreadyConnected, _) => (Level::Ok, "already connected"),
            (Verdict::PartlyConnected, false) => (Level::Warning, "partly connected"),
            (Verdict::PartlyConnected, true) => (Level::Warning, "would partly connect"),
            (Verdict::Skipped, _) => (Level::Warning, "skipped"),
        };
        let total = tr.changed + tr.unchanged;
        let detail = match (tr.target, verdict) {
            (_, Verdict::Skipped) => String::new(),
            (Target::Shims, _) => format!(
                "{} linked in {}",
                plural(total, "command", "commands"),
                tilde(&paths.home, &paths.shims_dir())
            ),
            (_, Verdict::Updated) => format!(
                "{} of {} updated",
                tr.changed,
                plural(total, "file", "files")
            ),
            (Target::Claude, _) => "hooks, MCP server, EXPERIENCE.md pointer".into(),
            _ => "MCP server".into(),
        };
        let name = format!("{:<width$}", label(tr.target));
        let line = format!("  {name}{}{detail}", p.badge(level, word, 22));
        out.push_str(line.trim_end());
        out.push('\n');
        for reason in &tr.skipped {
            out.push_str(&format!("  {:width$}  {}\n", "", p.dim(reason)));
        }
    }
    out.push('\n');
    if dry_run {
        out.push_str(
            "Dry run: nothing was written. Run the same command without --dry-run to apply.\n",
        );
        return out;
    }
    if let Some(dir) = &report.backup_dir {
        out.push_str(&format!(
            "Backed up the originals to {}\n\n",
            tilde(&paths.home, dir)
        ));
    }

    let mut next = Vec::new();
    let shims_ok = report
        .targets
        .iter()
        .any(|t| t.target == Target::Shims && t.verdict() != Verdict::Skipped);
    if shims_ok {
        match on_path {
            shims::PathStatus::First => {}
            shims::PathStatus::Missing => next.push(format!(
                "Add the shims to your PATH (AgentDiet never edits shell rc files):\n       {}",
                path_line(paths)
            )),
            shims::PathStatus::Shadowed { command, found } => next.push(format!(
                "Put the shims first on your PATH (`{command}` resolves to {} first):\n       {}",
                tilde(&paths.home, found),
                path_line(paths)
            )),
        }
    }
    let restart: Vec<&str> = report
        .targets
        .iter()
        .filter(|t| t.target != Target::Shims && t.changed > 0)
        .map(|t| t.target.label())
        .collect();
    if !restart.is_empty() {
        let it = if restart.len() == 1 {
            "it loads"
        } else {
            "they load"
        };
        next.push(format!(
            "Restart {} so {it} the new config.",
            and_list(&restart)
        ));
    }
    next.push(format!("Check the setup: {}", p.bold("agentdiet doctor")));
    out.push_str("Next:\n");
    for (i, step) in next.iter().enumerate() {
        out.push_str(&format!("  {}. {step}\n", i + 1));
    }
    out
}

/// `agentdiet eject` / `uninstall`.
pub fn cmd_eject(paths: &Paths, purge: bool) -> anyhow::Result<()> {
    let report = eject::eject(paths, None, purge).context("eject failed")?;
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    if report.is_empty() {
        println!("Nothing to eject.");
        return Ok(());
    }
    for (label, files) in [
        ("restored", &report.restored),
        ("deleted", &report.deleted),
        ("cleaned", &report.cleaned),
        ("removed shim", &report.shims_removed),
        ("purged", &report.purged),
    ] {
        for file in files {
            println!("{label}: {}", file.display());
        }
    }
    println!("Backups are kept in {}", paths.backups_dir().display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(changed: usize, unchanged: usize, skipped: usize, was_present: bool) -> TargetReport {
        TargetReport {
            target: Target::Cursor,
            was_present,
            changed,
            unchanged,
            skipped: vec!["why".into(); skipped],
        }
    }

    #[test]
    fn verdicts_cover_every_outcome() {
        assert_eq!(report(1, 0, 0, false).verdict(), Verdict::Connected);
        assert_eq!(report(1, 2, 0, true).verdict(), Verdict::Updated);
        assert_eq!(report(0, 3, 0, true).verdict(), Verdict::AlreadyConnected);
        assert_eq!(report(1, 0, 1, false).verdict(), Verdict::PartlyConnected);
        assert_eq!(report(0, 0, 1, false).verdict(), Verdict::Skipped);
    }

    #[test]
    fn summary_explains_skips_and_ends_a_dry_run_plainly() {
        let paths = Paths::new(
            "/h".into(),
            "/h/cfg".into(),
            "/p".into(),
            "/h/bin/agentdiet".into(),
        );
        let mut r = InitReport::default();
        r.targets.push(TargetReport {
            skipped: vec!["~/.cursor/mcp.json: not valid JSON; left untouched".into()],
            ..report(0, 0, 0, false)
        });
        let text = render(
            &r,
            &paths,
            true,
            &shims::PathStatus::Missing,
            crate::ui::Painter::plain(),
        );
        let expected = [
            "  Cursor        ⚠ skipped",
            "                  ~/.cursor/mcp.json: not valid JSON; left untouched",
            "",
            "Dry run: nothing was written. Run the same command without --dry-run to apply.",
            "",
        ];
        assert_eq!(text, expected.join("\n"));
        assert_eq!(
            path_line(&paths),
            "export PATH=\"$HOME/.agentdiet/shims:$PATH\""
        );
    }
}
