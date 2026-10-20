//! `agentdiet top`: the live ratatui dashboard (SPEC §3b "`agentdiet top`").
//!
//! Loading ([`Snapshot::load`]) and drawing ([`draw`]) are separate, so the dashboard can be
//! rendered into a `TestBackend` without a terminal. [`run`] owns the terminal and the keys.

use std::io::IsTerminal;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::cas::{Cas, CasStats};
use crate::init;
use crate::memory::MemoryDb;
use crate::state::{self, Mode, State, Status};
use crate::tui::{self, Ctx};
use crate::ui::Level;
use crate::{Paths, words};

/// The dashboard reloads this often without a key press.
pub const REFRESH: Duration = Duration::from_secs(2);
/// memory.db rows loaded for the table.
const RECENT_ROWS: usize = 20;
/// Printed (as an error) when there is no terminal to draw on.
pub const NO_TTY: &str =
    "`agentdiet top` needs an interactive terminal (stdin and stdout must be a TTY).";

/// One row of the recent table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentItem {
    /// The emitted ID while the Vault entry is live.
    pub id: Option<String>,
    pub kind: String,
    /// The command or path.
    pub title: String,
    pub summary: String,
}

/// Everything the dashboard shows. Every text in it is already scrubbed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub state: State,
    pub vault: CasStats,
    /// Labels of the assistants connected at user scope.
    pub assistants: Vec<&'static str>,
    pub recent: Vec<RecentItem>,
    /// The result of the last key action, shown above the keys.
    pub message: Option<String>,
    /// Why `state.json` could not be used (corrupt or unreadable), shown in the footer instead of
    /// being printed over the screen.
    pub state_warning: Option<String>,
    /// Opened from the Control Center: `q` goes back there rather than quitting.
    pub from_center: bool,
}

impl Snapshot {
    /// Read the state ledger, the Vault (after a GC) and memory.db. Creates nothing: a project
    /// without memory.db and a home without a Vault simply show empty.
    pub fn load(paths: &Paths, now: SystemTime) -> Self {
        let cas = Cas::new(paths.cas_dir());
        // Best effort: `top` is one of the Vault's opportunistic GC points (SPEC §3 Tier 3).
        let _ = cas.gc(now);
        let (state, state_warning) = state::read(paths);
        Self {
            state,
            state_warning,
            from_center: false,
            vault: cas.stats().unwrap_or_default(),
            assistants: init::installed(paths)
                .into_iter()
                .map(|t| t.label())
                .collect(),
            recent: recent_items(paths, &cas, now),
            message: None,
        }
    }
}

pub(crate) fn recent_items(paths: &Paths, cas: &Cas, now: SystemTime) -> Vec<RecentItem> {
    let db_path = paths.memory_db();
    if !db_path.is_file() {
        return Vec::new();
    }
    let rows = MemoryDb::open(&db_path)
        .and_then(|db| db.recent(RECENT_ROWS))
        .unwrap_or_default();
    let shas: Vec<&str> = rows.iter().filter_map(|r| r.cas_sha.as_deref()).collect();
    let ids = cas.live_ids(&shas, now);
    rows.iter()
        .map(|r| RecentItem {
            id: r.cas_sha.as_deref().and_then(|sha| ids.get(sha)).cloned(),
            kind: r.kind.clone(),
            title: one_line(&r.title),
            summary: one_line(&r.body),
        })
        .collect()
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The next mode `m` switches to.
pub fn next_mode(mode: Mode) -> Mode {
    match mode {
        Mode::Light => Mode::Balanced,
        Mode::Balanced => Mode::UltraLean,
        Mode::UltraLean => Mode::Light,
    }
}

/// Draw the dashboard into `frame`, with `ctx`'s glyphs and colour rules (SPEC §2.5).
pub fn draw(frame: &mut Frame<'_>, snap: &Snapshot, ctx: Ctx) {
    let [header, metrics, table, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(7),
        Constraint::Min(4),
        Constraint::Length(2),
    ])
    .areas(frame.area());
    let block = |title: &str| {
        Block::new()
            .borders(Borders::ALL)
            .border_set(ctx.border())
            .title(ctx.t(title))
    };

    let s = &snap.state;
    let (status, level) = words::status_word(s.status);
    let assistants = if snap.assistants.is_empty() {
        "none".to_string()
    } else {
        snap.assistants.join(", ")
    };
    let header_line = Line::from(vec![
        ctx.span(status, ctx.level(level).add_modifier(Modifier::BOLD)),
        Span::raw("   Mode: "),
        Span::styled(s.mode.label(), ctx.bold()),
        Span::raw("   Assistants: "),
        Span::raw(assistants),
    ]);
    frame.render_widget(
        Paragraph::new(header_line).block(block(" AgentDiet ")),
        header,
    );

    let none_yet = || "none yet".to_string();
    let mut lines = vec![if s.trimmed_outputs == 0 {
        Line::from("Savings: nothing trimmed yet")
    } else {
        ctx.line(
            &format!(
                "Savings: {} in {}",
                words::tokens_saved(s),
                words::trimmed_outputs(s)
            ),
            Style::new(),
        )
    }];
    lines.push(Line::from(format!(
        "Re-opened: {}",
        words::reopens(s).unwrap_or_else(none_yet)
    )));
    match words::reopen_note(s) {
        Some((Level::Warning, note)) => lines.push(ctx.line(note, ctx.level(Level::Warning))),
        Some((_, note)) => lines.push(ctx.line(note, ctx.dim())),
        None => {}
    }
    lines.push(ctx.line(
        &format!("Passed while paused: {}", words::passed_while_paused(s)),
        Style::new(),
    ));
    lines.push(Line::from(format!(
        "Last trimmed: {}   Vault: {}",
        s.last_trimmed_id
            .as_deref()
            .map_or_else(none_yet, |id| format!("#{id}")),
        words::vault(&snap.vault)
    )));
    frame.render_widget(
        Paragraph::new(lines).block(block(" All projects (estimated) ")),
        metrics,
    );

    let head = Row::new(["#id", "kind", "command / path", "summary"]).style(ctx.bold());
    let rows = snap.recent.iter().map(|r| {
        let id =
            r.id.as_deref()
                .map_or_else(|| "-".to_string(), |id| format!("#{id}"));
        Row::new([
            Cell::from(id),
            Cell::from(r.kind.as_str()),
            Cell::from(r.title.as_str()),
            Cell::from(r.summary.as_str()),
        ])
    });
    let widths = [
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Percentage(35),
        Constraint::Fill(1),
    ];
    let title = if snap.recent.is_empty() {
        " Recent activity (this workspace): none yet "
    } else {
        " Recent activity (this workspace) "
    };
    frame.render_widget(
        Table::new(rows, widths).header(head).block(block(title)),
        table,
    );

    let note = match (&snap.message, &snap.state_warning) {
        (Some(message), _) => Line::from(message.clone()),
        (None, Some(warning)) => ctx.line(
            &format!("⚠ state.json: {warning}"),
            ctx.level(Level::Warning),
        ),
        (None, None) => Line::default(),
    };
    let keys = format!(
        "p {}  m next mode  r reload  q {}",
        match s.status {
            Status::Active => "pause",
            Status::Paused => "resume",
        },
        if snap.from_center { "back" } else { "quit" }
    );
    frame.render_widget(
        Paragraph::new(vec![note, ctx.line(&keys, ctx.dim())]),
        footer,
    );
}

/// How a dashboard session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// `q` or Esc: back to whoever opened it.
    Back,
    /// Ctrl-C: quit altogether.
    Quit,
    /// A stop signal arrived (SIGTERM, SIGHUP, SIGINT); see [`tui::exit_for_signal`].
    Signal(i32),
}

/// Open the dashboard. Fails, drawing nothing, without a terminal on stdin and stdout.
pub fn run(paths: &Paths) -> anyhow::Result<()> {
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        anyhow::bail!(NO_TTY);
    }
    let (mut terminal, restore) = tui::enter()?;
    match run_in(&mut terminal, paths, Ctx::from_env(), false)? {
        Ended::Signal(sig) => tui::exit_for_signal(sig, restore),
        Ended::Back | Ended::Quit => Ok(()),
    }
}

/// The dashboard loop on a terminal the caller already owns (the Control Center's `t`). Leaves the
/// terminal as it found it and says how it ended.
pub fn run_in(
    terminal: &mut ratatui::DefaultTerminal,
    paths: &Paths,
    ctx: Ctx,
    from_center: bool,
) -> anyhow::Result<Ended> {
    let load = || Snapshot {
        from_center,
        ..Snapshot::load(paths, SystemTime::now())
    };
    let mut snap = load();
    let mut loaded = Instant::now();
    loop {
        if let Some(sig) = tui::pending_signal() {
            return Ok(Ended::Signal(sig));
        }
        terminal
            .draw(|frame| draw(frame, &snap, ctx))
            .context("could not draw the dashboard")?;
        let wait = REFRESH.saturating_sub(loaded.elapsed());
        let mut reload = false;
        if tui::poll(wait).context("could not read the keyboard")? {
            // Anything but a key press (a resize, a release) only needs the redraw above.
            if let Event::Key(key) = event::read().context("could not read the keyboard")?
                && key.kind == KeyEventKind::Press
            {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(Ended::Back),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(Ended::Quit);
                    }
                    KeyCode::Char('p') => {
                        let target = match snap.state.status {
                            Status::Active => Status::Paused,
                            Status::Paused => Status::Active,
                        };
                        snap.message = Some(match state::set_status(paths, target) {
                            Ok(_) if target == Status::Paused => "Paused.".to_string(),
                            Ok(_) => "Resumed.".to_string(),
                            Err(e) => format!("Could not change status: {e}"),
                        });
                        reload = true;
                    }
                    KeyCode::Char('m') => {
                        let mode = next_mode(snap.state.mode);
                        snap.message = Some(match state::set_mode(paths, mode) {
                            Ok(_) => format!("Diet Mode set to {}.", mode.label()),
                            Err(e) => format!("Could not change mode: {e}"),
                        });
                        reload = true;
                    }
                    KeyCode::Char('r') => {
                        snap.message = Some("Reloaded.".to_string());
                        reload = true;
                    }
                    _ => {}
                }
            }
        }
        // The snapshot (and its Vault GC) is reloaded after an action or every REFRESH, never on
        // every tick.
        if reload || loaded.elapsed() >= REFRESH {
            let message = snap.message.take();
            snap = load();
            snap.message = message;
            loaded = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tempfile::TempDir;

    use super::*;

    /// The rendered 80x24 screen, one string per row.
    fn render(snap: &Snapshot) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    snap,
                    Ctx {
                        ascii: false,
                        color: true,
                    },
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    fn sample(derefs: u64, trimmed: u64) -> Snapshot {
        Snapshot {
            state: State {
                status: Status::Paused,
                mode: Mode::UltraLean,
                raw_tokens_passed_while_paused: 4321,
                last_trimmed_id: Some("4f9a1b2c".into()),
                trimmed_outputs: trimmed,
                tokens_saved: 123_456,
                cas_dereferences: derefs,
                ..State::default()
            },
            vault: CasStats {
                live: 12,
                expired: 3,
                bytes: 2048,
            },
            assistants: vec!["Claude Code", "Cursor"],
            recent: vec![
                RecentItem {
                    id: Some("4f9a1b2c".into()),
                    kind: "command".into(),
                    title: "cargo test".into(),
                    summary: "kept 42/1830 lines; exit 101".into(),
                },
                RecentItem {
                    id: None,
                    kind: "read".into(),
                    title: "src/main.rs".into(),
                    summary: "folded 12 bodies".into(),
                },
            ],
            message: None,
            state_warning: None,
            from_center: false,
        }
    }

    fn screen(snap: &Snapshot) -> String {
        render(snap).join("\n")
    }

    fn screen_with(snap: &Snapshot, ctx: Ctx) -> (String, ratatui::buffer::Buffer) {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, snap, ctx)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let text = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        (text, buffer)
    }

    #[test]
    fn ascii_terminals_get_ascii_boxes_and_glyphs() {
        let ctx = Ctx {
            ascii: true,
            color: true,
        };
        let (text, _) = screen_with(&sample(5, 10), ctx);
        // Only our own text is checked: tool output in the table could hold anything.
        assert!(text.is_ascii(), "{text}");
        assert!(
            text.contains("+ AgentDiet ---") && text.contains("# Paused"),
            "{text}"
        );
        assert!(
            text.contains("! high: trimmed output is often read back"),
            "{text}"
        );
        assert!(text.contains("p resume  m next mode"), "{text}");
        assert!(text.contains("~123,456 tokens saved"), "{text}");
    }

    #[test]
    fn no_color_means_no_hue_anywhere() {
        let ctx = Ctx {
            ascii: false,
            color: false,
        };
        let (_, buffer) = screen_with(&sample(5, 10), ctx);
        assert!(
            buffer
                .content()
                .iter()
                .all(|c| c.fg == ratatui::style::Color::Reset
                    && c.bg == ratatui::style::Color::Reset)
        );
        let (_, colored) = screen_with(
            &sample(5, 10),
            Ctx {
                ascii: false,
                color: true,
            },
        );
        assert!(
            colored
                .content()
                .iter()
                .any(|c| c.fg != ratatui::style::Color::Reset)
        );
    }

    #[test]
    fn a_corrupt_state_file_is_shown_in_the_footer_not_printed() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::new(
            dir.path().join("home"),
            dir.path().join("cfg"),
            dir.path().join("proj"),
            "/x/agentdiet".into(),
        );
        std::fs::create_dir_all(&paths.agentdiet_home).unwrap();
        std::fs::write(paths.state_file(), "{ not json").unwrap();
        let snap = Snapshot::load(&paths, SystemTime::now());
        assert_eq!(snap.state, State::default());
        let warning = snap.state_warning.clone().unwrap();
        assert!(warning.starts_with("ignoring corrupt"), "{warning}");
        let text = screen(&snap);
        assert!(text.contains("⚠ state.json: ignoring corrupt"), "{text}");
    }

    #[test]
    fn renders_status_metrics_and_recent_ids_in_80x24() {
        let text = screen(&sample(5, 20));
        for expected in [
            "■ Paused",
            "Mode: Ultra-Lean",
            "Claude Code, Cursor",
            "All projects (estimated)",
            "Savings: ≈123,456 tokens saved in 20 trimmed outputs",
            "Re-opened: 5 re-opens for 20 trimmed outputs (25%)",
            "⚠ high: trimmed output is often read back in full; consider a lighter mode",
            "Passed while paused: ≈4,321 tokens passed untrimmed (still scrubbed)",
            "Last trimmed: #4f9a1b2c   Vault: 12 entries, 2.0 KiB (3 expired)",
            "Recent activity (this workspace)",
            "#4f9a1b2c  command  cargo test",
            "src/main.rs",
            "p resume  m next mode  r reload  q quit",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
    }

    #[test]
    fn the_warning_needs_a_rate_strictly_above_20_percent() {
        let text = screen(&sample(3, 20));
        assert!(
            text.contains("Re-opened: 3 re-opens for 20 trimmed outputs (15%)"),
            "{text}"
        );
        assert!(!text.contains("⚠"), "{text}");
        let text = screen(&sample(4, 20));
        assert!(text.contains("(20%)") && !text.contains("⚠"), "{text}");
        let text = screen(&sample(0, 0));
        assert!(text.contains("Re-opened: none yet"), "{text}");
        assert!(text.contains("Savings: nothing trimmed yet"), "{text}");
        // Opened from the Control Center, `q` goes back.
        let back = screen(&Snapshot {
            from_center: true,
            ..sample(0, 0)
        });
        assert!(back.contains("q back"), "{back}");
    }

    #[test]
    fn an_empty_dashboard_and_a_message_render() {
        let snap = Snapshot {
            message: Some("Could not change status: state is busy".into()),
            ..Snapshot::default()
        };
        let text = screen(&snap);
        assert!(
            text.contains("● Active") && text.contains("Assistants: none"),
            "{text}"
        );
        assert!(
            text.contains("this workspace): none yet") && text.contains("state is busy"),
            "{text}"
        );
    }

    #[test]
    fn modes_cycle_through_all_three() {
        assert_eq!(next_mode(Mode::Light), Mode::Balanced);
        assert_eq!(next_mode(Mode::Balanced), Mode::UltraLean);
        assert_eq!(next_mode(Mode::UltraLean), Mode::Light);
    }

    #[test]
    fn load_reads_state_vault_and_memory_without_creating_them() {
        let dir = TempDir::new().unwrap();
        let p = Paths::new(
            dir.path().join("home"),
            dir.path().join("config"),
            dir.path().join("project"),
            "/x/agentdiet".into(),
        );
        let now = SystemTime::now();
        let empty = Snapshot::load(&p, now);
        assert_eq!(empty.vault, CasStats::default());
        assert!(empty.recent.is_empty() && empty.assistants.is_empty());
        assert!(!p.memory_db().exists() && !p.cas_dir().exists());

        let scrub = crate::scrubber::scrub;
        let put = Cas::new(p.cas_dir())
            .put(&scrub(b"full output\n"), now)
            .unwrap();
        let db = MemoryDb::open(&p.memory_db()).unwrap();
        db.index(
            crate::memory::Kind::Command,
            &scrub(b"cargo test"),
            &scrub(b"kept 1/9 lines"),
            Some(&put.sha),
            None,
        )
        .unwrap();
        state::update(&p, |s| {
            s.trimmed_outputs = 4;
            s.cas_dereferences = 1;
        })
        .unwrap();

        let snap = Snapshot::load(&p, now);
        assert_eq!(snap.vault.live, 1);
        assert_eq!(snap.recent[0].id.as_deref(), Some(put.short_id.as_str()));
        let text = screen(&snap);
        assert!(text.contains(&format!("#{}", put.short_id)), "{text}");
        assert!(
            text.contains("1 re-open for 4 trimmed outputs (25%)")
                && text.contains("too few trimmed outputs to judge yet")
                && !text.contains("⚠ high"),
            "{text}"
        );
    }
}
