//! The Control Center (bare `agentdiet`) and the `pause` / `resume` commands.

use std::fmt;
use std::io::IsTerminal;

use anyhow::Context;
use inquire::error::InquireError;

use crate::Paths;
use crate::init::{self, InitOptions, Target};
use crate::state::{self, Change, Mode, State, Status};

/// Control Center entries, in the order SPEC.md §2.1 lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    PauseResume,
    InspectLast,
    SwitchMode,
    ManageAssistants,
    OpenTop,
}

impl MenuItem {
    pub const ALL: [MenuItem; 5] = [
        MenuItem::PauseResume,
        MenuItem::InspectLast,
        MenuItem::SwitchMode,
        MenuItem::ManageAssistants,
        MenuItem::OpenTop,
    ];

    /// Menu text; the first entry reflects the current status.
    pub fn label(self, status: Status) -> &'static str {
        match (self, status) {
            (MenuItem::PauseResume, Status::Active) => "Pause AgentDiet",
            (MenuItem::PauseResume, Status::Paused) => "Resume AgentDiet",
            (MenuItem::InspectLast, _) => "Inspect last trimmed output",
            (MenuItem::SwitchMode, _) => "Switch Diet Mode",
            (MenuItem::ManageAssistants, _) => "Manage Connected Assistants",
            (MenuItem::OpenTop, _) => "Open `agentdiet top`",
        }
    }
}

struct Choice {
    item: MenuItem,
    label: &'static str,
}

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label)
    }
}

/// One-line status summary.
pub fn status_line(state: &State) -> String {
    let status = match state.status {
        Status::Active => "active",
        Status::Paused => "paused",
    };
    format!("AgentDiet is {status} (mode: {})", state.mode.label())
}

/// `agentdiet pause`
pub fn pause(paths: &Paths) -> anyhow::Result<String> {
    let change = state::set_status(paths, Status::Paused).context("could not update state.json")?;
    Ok(match change {
        Change::Changed => {
            "⏸ AgentDiet paused. Output passes through raw until you run 'agentdiet resume'.".into()
        }
        Change::Unchanged => "AgentDiet is already paused.".into(),
    })
}

/// `agentdiet resume`
pub fn resume(paths: &Paths) -> anyhow::Result<String> {
    let change = state::set_status(paths, Status::Active).context("could not update state.json")?;
    Ok(match change {
        Change::Changed => "▶ AgentDiet resumed.".into(),
        Change::Unchanged => "AgentDiet is already active.".into(),
    })
}

/// Text for "Inspect last trimmed output". The vault itself arrives later, so
/// for now this reports the recorded short ID.
pub fn inspect_last(state: &State) -> String {
    match &state.last_trimmed_id {
        Some(id) => format!(
            "Last trimmed output: #{id}\nRun: agentdiet show #{id}  (full content viewing arrives with the vault)"
        ),
        None => "Nothing has been trimmed yet.".into(),
    }
}

/// Open the Control Center. Without a terminal, print the status and exit
/// instead of blocking on a prompt.
pub fn run(paths: &Paths) -> anyhow::Result<()> {
    let state = state::load(paths);
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        println!("{}", status_line(&state));
        println!(
            "Run `agentdiet --help` for commands, or run in a terminal for the Control Center."
        );
        return Ok(());
    }
    loop {
        let state = state::load(paths);
        let choices: Vec<Choice> = MenuItem::ALL
            .into_iter()
            .map(|item| Choice {
                item,
                label: item.label(state.status),
            })
            .collect();
        let prompt = format!("{} — Control Center", status_line(&state));
        let picked = match inquire::Select::new(&prompt, choices)
            .with_help_message("↑↓ to move, enter to select, esc to quit")
            .prompt()
        {
            Ok(choice) => choice.item,
            Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
                return Ok(());
            }
            Err(e) => return Err(e).context("menu failed"),
        };
        match handle(paths, picked, &state) {
            Ok(()) => {}
            Err(e) if is_cancel(&e) => {}
            Err(e) => eprintln!("error: {e:#}"),
        }
    }
}

fn is_cancel(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<InquireError>(),
        Some(InquireError::OperationCanceled | InquireError::OperationInterrupted)
    )
}

fn handle(paths: &Paths, item: MenuItem, state: &State) -> anyhow::Result<()> {
    match item {
        MenuItem::PauseResume => {
            let message = match state.status {
                Status::Active => pause(paths)?,
                Status::Paused => resume(paths)?,
            };
            println!("{message}");
        }
        MenuItem::InspectLast => println!("{}", inspect_last(state)),
        MenuItem::SwitchMode => switch_mode(paths, state)?,
        MenuItem::ManageAssistants => manage_assistants(paths)?,
        MenuItem::OpenTop => {
            println!("`agentdiet top` is not available yet (coming in a later slice).")
        }
    }
    Ok(())
}

fn switch_mode(paths: &Paths, state: &State) -> anyhow::Result<()> {
    struct ModeChoice(Mode);
    impl fmt::Display for ModeChoice {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0.label())
        }
    }
    let options: Vec<ModeChoice> = Mode::ALL.into_iter().map(ModeChoice).collect();
    let cursor = Mode::ALL.iter().position(|m| *m == state.mode).unwrap_or(0);
    let ModeChoice(mode) = inquire::Select::new("Diet Mode", options)
        .with_starting_cursor(cursor)
        .prompt()?;
    match state::set_mode(paths, mode).context("could not update state.json")? {
        Change::Changed => println!("Diet Mode set to {}.", mode.label()),
        Change::Unchanged => println!("Diet Mode is already {}.", mode.label()),
    }
    Ok(())
}

fn manage_assistants(paths: &Paths) -> anyhow::Result<()> {
    let installed = init::installed(paths);
    let all = Target::ALL;
    let defaults: Vec<usize> = all
        .iter()
        .enumerate()
        .filter(|(_, t)| installed.contains(t))
        .map(|(i, _)| i)
        .collect();
    let labels: Vec<&str> = all.iter().map(|t| t.label()).collect();
    let picked = inquire::MultiSelect::new("Connected assistants (checked = connected)", labels)
        .with_default(&defaults)
        .raw_prompt()?;
    let selected: Vec<Target> = picked.iter().map(|o| all[o.index]).collect();

    let to_add: Vec<Target> = selected
        .iter()
        .copied()
        .filter(|t| !installed.contains(t))
        .collect();
    let to_remove: Vec<Target> = installed
        .iter()
        .copied()
        .filter(|t| !selected.contains(t))
        .collect();

    if to_add.is_empty() && to_remove.is_empty() {
        println!("No changes.");
        return Ok(());
    }
    if !to_add.is_empty() {
        let report = init::run(
            paths,
            &InitOptions {
                targets: to_add,
                project: false,
                dry_run: false,
            },
        )?;
        for note in &report.skipped {
            eprintln!("skipped: {note}");
        }
        println!("Connected: {} file(s) changed.", report.changed.len());
    }
    if !to_remove.is_empty() {
        let report = init::eject::eject(paths, Some(&to_remove), false)?;
        for warning in &report.warnings {
            eprintln!("warning: {warning}");
        }
        let count = report.restored.len()
            + report.deleted.len()
            + report.cleaned.len()
            + report.shims_removed.len();
        println!("Disconnected: {count} item(s) restored or removed.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    fn paths(dir: &TempDir) -> Paths {
        Paths::new(
            dir.path().join("home"),
            dir.path().join("config"),
            dir.path().join("project"),
            PathBuf::from("/x/agentdiet"),
        )
    }

    #[test]
    fn menu_order_matches_spec_section_2_1() {
        let labels: Vec<_> = MenuItem::ALL
            .iter()
            .map(|i| i.label(Status::Active))
            .collect();
        assert_eq!(
            labels,
            [
                "Pause AgentDiet",
                "Inspect last trimmed output",
                "Switch Diet Mode",
                "Manage Connected Assistants",
                "Open `agentdiet top`",
            ]
        );
    }

    #[test]
    fn first_menu_label_reflects_status() {
        assert_eq!(
            MenuItem::PauseResume.label(Status::Paused),
            "Resume AgentDiet"
        );
        assert_eq!(
            MenuItem::PauseResume.label(Status::Active),
            "Pause AgentDiet"
        );
    }

    #[test]
    fn pause_then_resume_report_changes() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        assert!(pause(&p).unwrap().contains("paused"));
        assert!(pause(&p).unwrap().contains("already"));
        assert_eq!(state::load(&p).status, Status::Paused);
        assert!(resume(&p).unwrap().contains("resumed"));
        assert!(resume(&p).unwrap().contains("already"));
        assert_eq!(state::load(&p).status, Status::Active);
    }

    #[test]
    fn inspect_reports_recorded_id() {
        let mut s = State::default();
        assert!(inspect_last(&s).contains("Nothing"));
        s.last_trimmed_id = Some("4f9a".into());
        assert!(inspect_last(&s).contains("#4f9a"));
    }
}
