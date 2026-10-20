//! The Control Center (bare `agentdiet`) and the `pause` / `resume` commands.

use std::time::SystemTime;

use anyhow::Context;

use crate::Paths;
use crate::cas::{Cas, CasError};
use crate::state::{self, Change, Mode, State, Status};

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
            "⏸ AgentDiet paused. Output passes through untrimmed (still scrubbed) until you run 'agentdiet resume' or type '@diet resume'.".into()
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

/// `agentdiet mode <mode>` (and the Control Center's mode switch).
pub fn set_mode(paths: &Paths, mode: Mode) -> anyhow::Result<String> {
    Ok(
        match state::set_mode(paths, mode).context("could not update state.json")? {
            Change::Changed => format!("Diet Mode set to {}.", mode.label()),
            Change::Unchanged => format!("Diet Mode is already {}.", mode.label()),
        },
    )
}

/// What a mode does, from its §3 Tier 2 parameters: shared by `agentdiet mode` and the Control Center.
pub fn mode_line(mode: Mode) -> String {
    use crate::diet::ModeParams;
    use crate::ui::{plural, thousands};

    let params = ModeParams::for_mode(mode);
    format!(
        "outputs over {} est. tokens · must save {}% · {}",
        thousands(params.min_tokens as u64),
        params.saving_pct,
        plural(params.context_lines, "context line", "context lines"),
    )
}

/// One plain sentence on when to pick a mode.
pub fn mode_blurb(mode: Mode) -> &'static str {
    match mode {
        Mode::Light => "Gentlest: only very large outputs are trimmed; the agent sees the most.",
        Mode::Balanced => "The default: trims long, repetitive output and keeps every error.",
        Mode::UltraLean => {
            "Most aggressive: smallest context, but agents may re-open outputs more."
        }
    }
}

/// `agentdiet mode`: the current mode and what each one does.
pub fn mode_overview(state: &State, p: crate::ui::Painter) -> String {
    use crate::ui::Level;

    let mut out = format!("Diet Mode: {}\n\n", p.bold(state.mode.label()));
    for mode in Mode::ALL {
        let (mark, name) = if mode == state.mode {
            ("●", p.level(Level::Ok, &format!("{:<11}", mode.slug())))
        } else {
            ("○", format!("{:<11}", mode.slug()))
        };
        out.push_str(&format!("  {mark} {name} {}\n", mode_line(mode)));
    }
    out.push_str(&format!(
        "\nChange it: {}\n",
        p.bold("agentdiet mode <light|balanced|ultra-lean>")
    ));
    out
}

/// "Inspect last trimmed output" shows at most this many lines of the entry.
pub const INSPECT_MAX_LINES: usize = 400;

/// Text for "Inspect last trimmed output": the recorded ID and the Vault entry's (scrubbed)
/// content, up to [`INSPECT_MAX_LINES`] lines.
pub fn inspect_last(paths: &Paths, state: &State, now: SystemTime) -> String {
    let Some(id) = &state.last_trimmed_id else {
        return "Nothing has been trimmed yet.".into();
    };
    let show = format!("agentdiet show '#{id}'");
    let bytes = match Cas::new(paths.cas_dir()).read(id, None, now) {
        Ok(bytes) => bytes,
        Err(CasError::Expired(_)) => {
            return format!("Last trimmed output: #{id} has expired from the vault (7-day TTL).");
        }
        Err(CasError::NotFound(_)) => {
            return format!("Last trimmed output: #{id} is not in the vault.");
        }
        Err(CasError::Corrupt(_)) => {
            return format!("Last trimmed output: #{id} is corrupt in the vault; nothing shown.");
        }
        Err(e) => return format!("Last trimmed output: #{id} could not be read: {e}"),
    };
    let text = String::from_utf8_lossy(&bytes);
    let total = text.lines().count();
    let mut out = format!("Last trimmed output: #{id} ({total} lines)\n");
    for line in text.lines().take(INSPECT_MAX_LINES) {
        out.push_str(line);
        out.push('\n');
    }
    if total > INSPECT_MAX_LINES {
        out.push_str(&format!(
            "… {} more lines. Run: {show}\n",
            total - INSPECT_MAX_LINES
        ));
    } else {
        out.push_str(&format!("Run: {show}\n"));
    }
    out
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
    fn mode_overview_marks_the_current_mode_and_quotes_the_engine_parameters() {
        let state = State {
            mode: Mode::UltraLean,
            ..State::default()
        };
        let text = mode_overview(&state, crate::ui::Painter::plain());
        assert!(text.starts_with("Diet Mode: Ultra-Lean\n"));
        assert!(text.contains(
            "  ○ light       outputs over 1,500 est. tokens · must save 40% · 5 context lines\n"
        ));
        assert!(text.contains(
            "  ● ultra-lean  outputs over 250 est. tokens · must save 10% · 1 context line\n"
        ));
        assert!(
            text.lines().all(|l| l.chars().count() <= 80),
            "fits 80 columns"
        );
    }

    #[test]
    fn set_mode_reports_changes() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        assert_eq!(
            set_mode(&p, Mode::Light).unwrap(),
            "Diet Mode set to Light."
        );
        assert_eq!(
            set_mode(&p, Mode::Light).unwrap(),
            "Diet Mode is already Light."
        );
        assert_eq!(state::load(&p).mode, Mode::Light);
    }

    #[test]
    fn inspect_shows_the_last_trimmed_entry() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        let now = SystemTime::now();
        let mut s = State::default();
        assert!(inspect_last(&p, &s, now).contains("Nothing"));

        s.last_trimmed_id = Some("4f9a1b2c".into());
        assert!(inspect_last(&p, &s, now).contains("not in the vault"));

        let body: String = (1..=450).map(|i| format!("line {i}\n")).collect();
        let put = Cas::new(p.cas_dir())
            .put(&crate::scrubber::scrub(body.as_bytes()), now)
            .unwrap();
        s.last_trimmed_id = Some(put.short_id.clone());
        let shown = inspect_last(&p, &s, now);
        assert!(shown.starts_with(&format!(
            "Last trimmed output: #{} (450 lines)\n",
            put.short_id
        )));
        assert!(shown.contains("\nline 1\n") && shown.contains("\nline 400\n"));
        assert!(!shown.contains("line 401\n"));
        assert!(shown.contains(&format!(
            "… 50 more lines. Run: agentdiet show '#{}'",
            put.short_id
        )));

        let later = now + crate::cas::DEFAULT_TTL + std::time::Duration::from_secs(1);
        assert!(inspect_last(&p, &s, later).contains("expired"));
    }
}
