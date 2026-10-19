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
}

/// Connect the selected assistants. Idempotent: a file is backed up and
/// rewritten only if its patched content differs from what is on disk.
pub fn run(paths: &Paths, opts: &InitOptions) -> Result<InitReport, InitError> {
    let mut targets = opts.targets.clone();
    targets.sort();
    targets.dedup();

    let mut report = InitReport::default();
    let mut session = backup::Session::new(paths);

    for target in targets {
        if target == Target::Shims {
            report.shims_selected = true;
            run_shims(paths, opts.dry_run, &mut report)?;
            continue;
        }
        let Some(files) = targets::files(paths, target, opts.project) else {
            report.skipped.push(format!(
                "{}: no project-level config; skipped",
                target.label()
            ));
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
                    continue;
                }
            };
            match file.kind.patch(current_text, &paths.exe) {
                Err(e) => report.skipped.push(format!(
                    "{}: {}: {e}; left untouched",
                    target.label(),
                    file.path.display()
                )),
                Ok(None) => report.unchanged.push(file.path.clone()),
                Ok(Some(new)) => {
                    if opts.dry_run {
                        report.diffs.push(diff::render(
                            &file.path.display().to_string(),
                            current_text.unwrap_or(""),
                            &new,
                        ));
                    } else {
                        session.write(target, &file, current.as_deref(), new.as_bytes())?;
                    }
                    report.changed.push(file.path.clone());
                }
            }
        }
    }
    report.backup_dir = session.dir().map(Path::to_path_buf);
    Ok(report)
}

fn run_shims(paths: &Paths, dry_run: bool, report: &mut InitReport) -> Result<(), InitError> {
    let outcome = shims::install(paths, dry_run)?;
    if dry_run && !outcome.linked.is_empty() {
        let lines: Vec<String> = outcome
            .linked
            .iter()
            .map(|l| format!("+ {} -> {}", l.display(), paths.exe.display()))
            .collect();
        report
            .diffs
            .push(format!("--- shims\n{}\n", lines.join("\n")));
    }
    report.changed.extend(outcome.linked);
    report.skipped.extend(outcome.skipped);
    if outcome.unchanged > 0 {
        report.unchanged.push(paths.shims_dir());
    }
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

/// Interactive checklist of detected assistants.
pub fn choose_interactively(paths: &Paths, project: bool) -> anyhow::Result<Vec<Target>> {
    if !is_interactive() {
        bail!(
            "not a terminal: pass --all or --only <targets> (claude,cursor,windsurf,gemini,copilot,codex,shims)"
        );
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
    } else {
        choose_interactively(paths, project)?
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

    for diff in &report.diffs {
        println!("{diff}");
    }
    for note in &report.skipped {
        eprintln!("skipped: {note}");
    }
    let verb = if dry_run { "would change" } else { "changed" };
    println!(
        "{verb} {} file(s); {} already up to date.",
        report.changed.len(),
        report.unchanged.len()
    );
    if !dry_run {
        for path in &report.changed {
            println!("  patched {}", path.display());
        }
        if let Some(dir) = &report.backup_dir {
            println!("Originals backed up to {}", dir.display());
        }
    }
    if report.shims_selected {
        println!(
            "Add the shims to your PATH (agentdiet never edits shell rc files):\n  export PATH=\"{}:$PATH\"",
            paths.shims_dir().display()
        );
    }
    Ok(())
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
