//! The Control Center: bare `agentdiet` in a terminal (SPEC §2.1).
//!
//! Three parts, so the screen can be tested without a terminal:
//! * [`Data`] is what the screen shows, loaded read-only (no `state.json` write, no Vault GC);
//! * [`App`] is the view state, and [`App::on_key`] turns a key into an [`Action`] without I/O;
//! * [`apply`] performs an action through the same functions the CLI commands use, and [`draw`]
//!   renders [`Data`] and [`App`] into a ratatui frame.
//!
//! [`run`] owns the terminal.

use std::cell::Cell;
use std::io::IsTerminal;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::cas::Cas;
use crate::init::{self, InitOptions, Target, eject};
use crate::state::{Mode, State, Status};
use crate::status::{self, Check, CheckKind, Observed, Report};
use crate::top::{self, RecentItem};
use crate::tui::{self, Ctx};
use crate::ui::{self, Level, Painter};
use crate::{Paths, control, fsutil, words};

/// State is reloaded this often without a key press (the checks are not: SPEC §2.1).
pub const REFRESH: Duration = Duration::from_secs(2);
/// Below this the view is one column and the Recent panel is hidden.
const WIDE: u16 = 70;
/// Below this width or height only the status line is shown.
const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 12;

// ---- data ---------------------------------------------------------------------------------------

/// Everything the screen shows. Every text in it is already scrubbed.
pub struct Data {
    pub state: State,
    pub report: Report,
    /// Targets agentdiet is connected to at user scope (what `a` toggles).
    pub installed: Vec<Target>,
    pub recent: Vec<RecentItem>,
    /// The last trimmed output's text, once `l` loaded it.
    pub last: Option<String>,
}

/// `state.json` without `state::load`'s stderr warning, which would land on the screen. A corrupt
/// file shows up in the `State` check instead.
fn quiet_state(paths: &Paths) -> State {
    fsutil::read_optional_regular(&paths.state_file())
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

impl Data {
    /// Run the checks and read the state, the connected targets and memory.db. Creates nothing.
    pub fn load(paths: &Paths) -> Self {
        let path_var = std::env::var_os("PATH").unwrap_or_default();
        Self {
            state: quiet_state(paths),
            report: status::doctor(paths, &path_var),
            installed: init::installed(paths),
            recent: top::recent_items(paths, &Cas::new(paths.cas_dir()), SystemTime::now()),
            last: None,
        }
    }

    /// The cheap part of a refresh: counters and status, not the checks.
    pub fn reload_state(&mut self, paths: &Paths) {
        self.state = quiet_state(paths);
    }
}

// ---- view state and keys ------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    Main,
    Mode {
        cursor: usize,
    },
    Last {
        scroll: u16,
    },
    Assistants {
        cursor: usize,
        confirm: Option<Target>,
    },
    Doctor {
        scroll: u16,
    },
    Help {
        scroll: u16,
    },
}

/// What a key asks for; [`apply`] and [`run`] carry it out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    TogglePause,
    SetMode(Mode),
    Connect(Target),
    Disconnect(Target),
    ShowLast,
    Refresh,
    OpenLive,
    Quit,
}

#[derive(Debug)]
pub struct App {
    pub view: View,
    /// The result of the last action, shown above the keys.
    pub message: Option<String>,
    /// The largest scroll the current pane allows; set by [`draw`], used to clamp keys.
    cap: Cell<u16>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            view: View::Main,
            message: None,
            cap: Cell::new(0),
        }
    }

    /// An app already showing `view` (tests, and previews of a screen).
    pub fn in_view(view: View) -> Self {
        Self {
            view,
            ..Self::new()
        }
    }

    /// Handle one key. Moves between views itself; returns what needs doing outside the view.
    pub fn on_key(&mut self, key: KeyEvent, data: &Data) -> Option<Action> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Some(Action::Quit);
        }
        let back = matches!(key.code, KeyCode::Esc | KeyCode::Char('q'));
        let cap = self.cap.get();
        match &mut self.view {
            View::Main => {
                self.message = None;
                match key.code {
                    KeyCode::Char('p') => return Some(Action::TogglePause),
                    KeyCode::Char('m') => {
                        let cursor = Mode::ALL.iter().position(|m| *m == data.state.mode);
                        self.view = View::Mode {
                            cursor: cursor.unwrap_or(0),
                        };
                    }
                    KeyCode::Char('l') => return Some(Action::ShowLast),
                    KeyCode::Char('a') => {
                        self.view = View::Assistants {
                            cursor: 0,
                            confirm: None,
                        };
                    }
                    KeyCode::Char('d') => self.view = View::Doctor { scroll: 0 },
                    KeyCode::Char('t') => return Some(Action::OpenLive),
                    KeyCode::Char('r') => return Some(Action::Refresh),
                    KeyCode::Char('?') => self.view = View::Help { scroll: 0 },
                    _ if back => return Some(Action::Quit),
                    _ => {}
                }
            }
            View::Mode { cursor } => match key.code {
                KeyCode::Char(c @ '1'..='3') => {
                    let mode = Mode::ALL[usize::from(c as u8 - b'1')];
                    self.view = View::Main;
                    return Some(Action::SetMode(mode));
                }
                KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    *cursor = (*cursor + 1).min(Mode::ALL.len() - 1)
                }
                KeyCode::Enter => {
                    let mode = Mode::ALL[*cursor];
                    self.view = View::Main;
                    return Some(Action::SetMode(mode));
                }
                _ if back => self.view = View::Main,
                _ => {}
            },
            View::Assistants { cursor, confirm } => {
                if let Some(target) = *confirm {
                    *confirm = None;
                    if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                        return Some(if data.installed.contains(&target) {
                            Action::Disconnect(target)
                        } else {
                            Action::Connect(target)
                        });
                    }
                    return None;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        *cursor = (*cursor + 1).min(Target::ALL.len() - 1);
                    }
                    KeyCode::Enter => *confirm = Some(Target::ALL[*cursor]),
                    _ if back => self.view = View::Main,
                    _ => {}
                }
            }
            View::Last { scroll } | View::Doctor { scroll } | View::Help { scroll } => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => *scroll = (*scroll + 1).min(cap),
                    KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                    KeyCode::PageDown => *scroll = (*scroll + 10).min(cap),
                    KeyCode::Home => *scroll = 0,
                    KeyCode::End => *scroll = cap,
                    KeyCode::Char('?') if matches!(self.view, View::Help { .. }) => {
                        self.view = View::Main;
                    }
                    _ if back => self.view = View::Main,
                    _ => {}
                }
            }
        }
        None
    }
}

// ---- actions ------------------------------------------------------------------------------------

/// Carry out `action` the way the matching CLI command does, and say what happened. A failure is
/// a message, never an exit (SPEC §2.1). `Quit`, `OpenLive`, `ShowLast` and `Refresh` are the
/// caller's.
pub fn apply(action: &Action, paths: &Paths, data: &Data) -> String {
    match action {
        // Short messages of its own: the CLI's sentences are written for a terminal line, and
        // would be cut off in the footer.
        Action::TogglePause => {
            let target = match data.state.status {
                Status::Active => Status::Paused,
                Status::Paused => Status::Active,
            };
            match crate::state::set_status(paths, target) {
                Ok(_) if target == Status::Paused => {
                    "Paused: output passes through untrimmed (still scrubbed). p resumes.".into()
                }
                Ok(_) => "Resumed: trimming again.".into(),
                Err(e) => format!("Could not change the status: {e}"),
            }
        }
        Action::SetMode(mode) => control::set_mode(paths, *mode)
            .unwrap_or_else(|e| format!("Could not change the mode: {e:#}")),
        Action::Connect(target) => connect(paths, *target),
        Action::Disconnect(target) => disconnect(paths, *target),
        Action::ShowLast | Action::Refresh | Action::OpenLive | Action::Quit => String::new(),
    }
}

fn connect(paths: &Paths, target: Target) -> String {
    let opts = InitOptions {
        targets: vec![target],
        project: false,
        dry_run: false,
    };
    match init::run(paths, &opts) {
        Err(e) => format!("Could not connect {}: {e}", target.label()),
        Ok(report) => match report.targets.first() {
            Some(t) if t.verdict() == init::Verdict::Skipped || !t.skipped.is_empty() => format!(
                "{} not fully connected: {}",
                target.label(),
                t.skipped.join("; ")
            ),
            Some(t) if t.changed == 0 => format!("{} was already connected.", target.label()),
            _ if target == Target::Shims => {
                "Shims linked. Add them to your PATH (see `d`) so agents reach them.".to_string()
            }
            _ => format!(
                "Connected {}. Restart it so it loads the new config.",
                target.label()
            ),
        },
    }
}

fn disconnect(paths: &Paths, target: Target) -> String {
    match eject::eject(paths, Some(&[target]), false) {
        Err(e) => format!("Could not disconnect {}: {e}", target.label()),
        Ok(report) if report.for_target(target).is_empty() => {
            format!("{}: nothing to remove.", target.label())
        }
        Ok(report) => {
            let warn = report
                .warnings
                .first()
                .map(|w| format!(" ({w})"))
                .unwrap_or_default();
            format!(
                "Disconnected {}. Restart it so it stops loading AgentDiet.{warn}",
                target.label()
            )
        }
    }
}

// ---- drawing ------------------------------------------------------------------------------------

/// `text` cut to `width` characters, with an ellipsis when it was cut.
fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Greedy word wrap at `width`; a continuation keeps the line's indentation plus two.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    for source in text.lines() {
        let indent = source.len() - source.trim_start().len();
        let mut line = String::new();
        let mut first = true;
        for word in source.split_whitespace() {
            let pad = if first { indent } else { indent + 2 };
            let sep = usize::from(!line.trim().is_empty());
            if !line.trim().is_empty() && line.chars().count() + sep + word.chars().count() > width
            {
                out.push(std::mem::take(&mut line));
                first = false;
                line = " ".repeat(indent + 2);
            } else if line.is_empty() {
                line = " ".repeat(pad);
            }
            if !line.trim().is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        out.push(line);
    }
    out
}

/// How many rows `lines` take at `width` when wrapped.
fn rows(lines: &[Line<'_>], width: u16) -> u16 {
    let width = usize::from(width.max(1));
    let n: usize = lines.iter().map(|l| l.width().div_ceil(width).max(1)).sum();
    u16::try_from(n).unwrap_or(u16::MAX)
}

fn glyph(level: Level) -> &'static str {
    level.symbol()
}

/// Draw the whole screen.
pub fn draw(frame: &mut Frame<'_>, app: &App, data: &Data, ctx: Ctx) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        return draw_tiny(frame, area, data, ctx);
    }
    match &app.view {
        View::Main => draw_main(frame, area, app, data, ctx),
        View::Mode { cursor } => draw_mode(frame, area, data, ctx, *cursor),
        View::Assistants { cursor, confirm } => {
            draw_assistants(frame, area, data, ctx, *cursor, *confirm);
        }
        View::Last { scroll } => {
            let text = data
                .last
                .as_deref()
                .unwrap_or("Nothing has been trimmed yet.");
            draw_pane(
                frame,
                area,
                app,
                ctx,
                "Last trimmed output",
                text,
                *scroll,
                "↑↓ PgUp PgDn scroll · esc back",
            );
        }
        View::Doctor { scroll } => {
            let text = data.report.render(Painter::plain());
            draw_pane(
                frame,
                area,
                app,
                ctx,
                "Checks",
                &text,
                *scroll,
                "↑↓ PgUp PgDn scroll · esc back",
            );
        }
        View::Help { scroll } => {
            draw_pane(
                frame,
                area,
                app,
                ctx,
                "Keys",
                &help_text(),
                *scroll,
                "↑↓ scroll · esc back",
            );
        }
    }
}

fn status_text(data: &Data) -> (&'static str, Level) {
    words::status_word(data.state.status)
}

fn draw_tiny(frame: &mut Frame<'_>, area: Rect, data: &Data, ctx: Ctx) {
    let (status, level) = status_text(data);
    let w = usize::from(area.width);
    let lines = vec![
        Line::from(vec![
            ctx.span(status, ctx.level(level)),
            ctx.span(
                &fit(
                    &format!(" · {} mode", data.state.mode.label()),
                    w.saturating_sub(9),
                ),
                Style::new(),
            ),
        ]),
        ctx.line(&fit("Enlarge the terminal, or press q", w), ctx.dim()),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

fn header_line(data: &Data, width: u16, ctx: Ctx) -> Line<'static> {
    let (status, level) = status_text(data);
    let left = format!(" AgentDiet  {status} · {} mode", data.state.mode.label());
    let right = "? help ";
    let gap = usize::from(width).saturating_sub(left.chars().count() + right.len());
    Line::from(vec![
        ctx.span(" AgentDiet  ", ctx.bold()),
        ctx.span(status, ctx.level(level).add_modifier(Modifier::BOLD)),
        ctx.span(
            &format!(" · {} mode", data.state.mode.label()),
            Style::new(),
        ),
        Span::raw(" ".repeat(gap)),
        ctx.span(right, ctx.dim()),
    ])
}

/// `(glyph level, status text)` for an assistant row.
fn row_status(c: &Check) -> (Level, &str) {
    match (c.level, c.observed) {
        (Level::Ok, Observed::Verified) => (Level::Ok, "verified"),
        (Level::Ok, Observed::NotYet) => (Level::Ok, "not yet observed"),
        (Level::Ok, _) => (Level::Ok, "configured"),
        (level, _) => (level, c.word),
    }
}

fn assistant_lines(data: &Data, ctx: Ctx) -> Vec<Line<'static>> {
    let mut lines = vec![ctx.line(" Assistants", ctx.bold())];
    if data.report.connected.is_empty() {
        lines.push(ctx.line(" No assistants connected yet.", Style::new()));
        lines.push(ctx.line(" Press a to connect one,", ctx.dim()));
        lines.push(ctx.line(" or run `agentdiet init`.", ctx.dim()));
        return lines;
    }
    for c in &data.report.checks {
        match c.kind {
            CheckKind::Assistant { project: false, .. } | CheckKind::Shims => {
                let (level, status) = row_status(c);
                lines.push(Line::from(vec![
                    ctx.span(&format!(" {} ", glyph(level)), ctx.level(level)),
                    ctx.span(&format!("{:<18}", c.label), Style::new()),
                    ctx.span(status, ctx.level(level)),
                ]));
            }
            CheckKind::Assistant { project: true, .. } => {
                let (level, status) = row_status(c);
                lines.push(Line::from(vec![
                    ctx.span(&format!(" {} ", glyph(level)), ctx.level(level)),
                    ctx.span(&format!("{:<18}", fit(&c.label, 17)), Style::new()),
                    ctx.span(status, ctx.level(level)),
                ]));
            }
            _ => {}
        }
    }
    lines
}

fn savings_lines(data: &Data, ctx: Ctx) -> Vec<Line<'static>> {
    let s = &data.state;
    let mut lines = vec![ctx.line(" Savings (all projects, estimated)", ctx.bold())];
    if s.trimmed_outputs == 0 {
        lines.push(ctx.line(" Nothing trimmed yet.", Style::new()));
    } else {
        lines.push(ctx.line(&format!(" {}", words::tokens_saved(s)), Style::new()));
        lines.push(ctx.line(&format!(" {}", words::trimmed_outputs(s)), Style::new()));
        if let Some(reopens) = words::reopens(s) {
            lines.push(ctx.line(&format!(" {reopens}"), Style::new()));
        }
        if words::reopens_high(s) {
            // The full warning does not fit a half-width column; `status` and `top` carry it.
            lines.push(ctx.line(" ⚠ high: try a lighter mode", ctx.level(Level::Warning)));
        }
    }
    if let Some(id) = &s.last_trimmed_id {
        lines.push(ctx.line(&format!(" last #{id} · l to view"), ctx.dim()));
    }
    if s.status == Status::Paused {
        lines.push(ctx.line(
            &format!(" {}", words::passed_while_paused_short(s)),
            ctx.level(Level::Warning),
        ));
    }
    lines
}

fn attention_lines(data: &Data, ctx: Ctx, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width);
    let mut lines = Vec::new();
    for c in &data.report.checks {
        if !matches!(c.level, Level::Warning | Level::Broken) || c.kind == CheckKind::Setup {
            continue;
        }
        let text = wrap(
            &format!("{}  {}", c.label, c.detail),
            width.saturating_sub(5),
        );
        for (i, row) in text.iter().enumerate() {
            let lead = if i == 0 {
                ctx.span(&format!(" {} ", glyph(c.level)), ctx.level(c.level))
            } else {
                Span::raw("   ")
            };
            lines.push(Line::from(vec![
                lead,
                ctx.span(row.trim_start(), Style::new()),
            ]));
        }
        if let Some(fix) = &c.fix {
            for row in wrap(&format!("→ {fix}"), width.saturating_sub(5)) {
                lines.push(ctx.line(&format!("   {row}"), ctx.dim()));
            }
        }
    }
    lines
}

fn recent_lines(data: &Data, ctx: Ctx, width: u16, max: usize) -> Vec<Line<'static>> {
    let mut lines = vec![ctx.line(" Recent activity (this workspace)", ctx.bold())];
    if data.recent.is_empty() {
        lines.push(ctx.line(" No activity in this workspace yet.", ctx.dim()));
        return lines;
    }
    for item in data.recent.iter().take(max) {
        let id = item
            .id
            .as_deref()
            .map_or("-".to_string(), |id| format!("#{id}"));
        let text = format!(" {id:<10} {}  {}", item.title, item.summary);
        lines.push(ctx.line(&fit(&text, usize::from(width)), Style::new()));
    }
    lines
}

fn keys_line(data: &Data, width: u16) -> String {
    let p = match data.state.status {
        Status::Active => "pause",
        Status::Paused => "resume",
    };
    let options = [
        format!("p {p}  m mode  l last output  a assistants  d doctor  t live  ? help  q quit"),
        format!("p {p}  m mode  l last  a assist  d doctor  t live  ? help  q quit"),
        "p m l a d t ? q".to_string(),
    ];
    let fits = |s: &String| s.chars().count() < usize::from(width);
    options
        .iter()
        .find(|s| fits(s))
        .or(options.last())
        .map(|s| format!(" {s}"))
        .unwrap_or_default()
}

fn draw_main(frame: &mut Frame<'_>, area: Rect, app: &App, data: &Data, ctx: Ctx) {
    let wide = area.width >= WIDE;
    let assistants = assistant_lines(data, ctx);
    let savings = savings_lines(data, ctx);
    let attention = attention_lines(data, ctx, area.width);

    let half = area.width / 2;
    let body_h = if wide {
        rows(&assistants, half).max(rows(&savings, area.width - half))
    } else {
        rows(&assistants, area.width) + rows(&savings, area.width)
    };
    let fixed = 1 + 1 + 2; // header, description, footer
    let avail = area.height.saturating_sub(fixed + body_h);
    let att_full = if attention.is_empty() {
        0
    } else {
        1 + rows(&attention, area.width)
    };
    let att_h = att_full.min(avail);
    let left = avail - att_h;
    let show_recent = wide && left >= 3;
    let recent_h = if show_recent { left } else { 0 };

    let [head, desc, body, att, recent, _rest, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(body_h),
        Constraint::Length(att_h),
        Constraint::Length(recent_h),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .areas(area);

    frame.render_widget(Paragraph::new(header_line(data, area.width, ctx)), head);
    let blurb = format!(" Trims {}", words::mode_line(data.state.mode));
    frame.render_widget(
        Paragraph::new(ctx.line(&fit(&blurb, usize::from(area.width)), ctx.dim())),
        desc,
    );
    if wide {
        let [l, r] = Layout::horizontal([Constraint::Length(half), Constraint::Min(0)]).areas(body);
        frame.render_widget(Paragraph::new(assistants), l);
        frame.render_widget(Paragraph::new(savings), r);
    } else {
        let split = rows(&assistants, area.width);
        let [l, r] = Layout::vertical([Constraint::Length(split), Constraint::Min(0)]).areas(body);
        frame.render_widget(Paragraph::new(assistants), l);
        frame.render_widget(Paragraph::new(savings), r);
    }
    if att_h > 0 {
        let mut lines = vec![ctx.line(" Needs attention", ctx.bold())];
        lines.extend(attention);
        frame.render_widget(Paragraph::new(lines), att);
    }
    if show_recent {
        let max = usize::from(recent_h).saturating_sub(1);
        frame.render_widget(
            Paragraph::new(recent_lines(data, ctx, area.width, max)),
            recent,
        );
    }

    let note = match (&app.message, data.state.status) {
        (Some(m), _) => (
            m.clone(),
            ctx.level(Level::Info).add_modifier(Modifier::BOLD),
        ),
        (None, Status::Paused) => (
            "Paused. Output passes through untrimmed (still scrubbed).".to_string(),
            ctx.level(Level::Warning),
        ),
        (None, Status::Active) => (String::new(), Style::new()),
    };
    let footer = vec![
        ctx.line(
            &fit(&format!(" {}", note.0), usize::from(area.width)),
            note.1,
        ),
        ctx.line(&keys_line(data, area.width), ctx.dim()),
    ];
    frame.render_widget(Paragraph::new(footer), foot);
}

/// A full-screen pane: title, a list, a hint line.
fn pane_layout(area: Rect) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area)
}

fn draw_mode(frame: &mut Frame<'_>, area: Rect, data: &Data, ctx: Ctx, cursor: usize) {
    let [title, list, hint] = pane_layout(area);
    frame.render_widget(Paragraph::new(ctx.line(" Diet Mode", ctx.bold())), title);
    let mut lines = Vec::new();
    for (i, mode) in Mode::ALL.into_iter().enumerate() {
        let current = if mode == data.state.mode {
            "  (current)"
        } else {
            ""
        };
        let style = if i == cursor {
            ctx.selected()
        } else {
            Style::new()
        };
        lines.push(ctx.line(
            &format!(
                " {} {}  {}{current}",
                if i == cursor { ">" } else { " " },
                i + 1,
                mode.slug()
            ),
            style,
        ));
        lines.push(ctx.line(&format!("      {}", words::mode_blurb(mode)), Style::new()));
        lines.push(ctx.line(&format!("      {}", words::mode_line(mode)), ctx.dim()));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        list,
    );
    frame.render_widget(
        Paragraph::new(ctx.line(" ↑↓ move · enter or 1-3 choose · esc back", ctx.dim())),
        hint,
    );
}

fn draw_assistants(
    frame: &mut Frame<'_>,
    area: Rect,
    data: &Data,
    ctx: Ctx,
    cursor: usize,
    confirm: Option<Target>,
) {
    let [title, list, hint] = pane_layout(area);
    frame.render_widget(Paragraph::new(ctx.line(" Assistants", ctx.bold())), title);
    let mut lines = Vec::new();
    for (i, target) in Target::ALL.into_iter().enumerate() {
        let on = data.installed.contains(&target);
        let (level, state) = if on {
            (Level::Ok, "connected")
        } else {
            (Level::Info, "not connected")
        };
        let marker = if i == cursor { ">" } else { " " };
        let row = format!(" {marker} {} {:<28}{state}", glyph(level), target.label());
        lines.push(ctx.line(
            &row,
            if i == cursor {
                ctx.selected()
            } else {
                Style::new()
            },
        ));
    }
    if let Some(target) = confirm {
        let (verb, tail) = if data.installed.contains(&target) {
            ("Disconnect", "Your original config is restored.")
        } else {
            ("Connect", "A backup of your config is made first.")
        };
        lines.push(Line::default());
        lines.push(ctx.line(
            &format!(" {verb} {}? {tail}  [y/N]", target.label()),
            ctx.level(Level::Warning).add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        list,
    );
    let hint_text = if confirm.is_some() {
        " y confirms · any other key cancels"
    } else {
        " ↑↓ move · enter connect or disconnect · esc back"
    };
    frame.render_widget(Paragraph::new(ctx.line(hint_text, ctx.dim())), hint);
}

/// A scrollable text pane. Wraps `text` at the pane width and tells `app` how far it can scroll.
#[allow(clippy::too_many_arguments)] // a draw helper; a struct would only rename the arguments
fn draw_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    ctx: Ctx,
    title: &str,
    text: &str,
    scroll: u16,
    hint: &str,
) {
    let [head, body, foot] = pane_layout(area);
    let lines = wrap(text, usize::from(body.width).saturating_sub(2));
    let max = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_sub(body.height);
    app.cap.set(max);
    let scroll = scroll.min(max);
    frame.render_widget(
        Paragraph::new(ctx.line(&format!(" {title}"), ctx.bold())),
        head,
    );
    let shown: Vec<Line<'static>> = lines
        .iter()
        .skip(usize::from(scroll))
        .take(usize::from(body.height))
        .map(|l| ctx.line(&format!(" {l}"), Style::new()))
        .collect();
    frame.render_widget(Paragraph::new(shown), body);
    let more = if scroll < max { "  ↓ more" } else { "" };
    frame.render_widget(
        Paragraph::new(ctx.line(&format!(" {hint}{more}"), ctx.dim())),
        foot,
    );
}

fn help_text() -> String {
    [
        "p   pause or resume trimming",
        "      agentdiet pause | resume",
        "m   choose the Diet Mode",
        "      agentdiet mode <light|balanced|ultra-lean>",
        "l   read the last trimmed output",
        "      agentdiet show last",
        "a   connect or disconnect an assistant",
        "      agentdiet init --only <name> | agentdiet disconnect <name>",
        "d   run the checks and see the fixes",
        "      agentdiet doctor",
        "t   live dashboard (q returns here)",
        "      agentdiet top",
        "r   reload everything",
        "q   quit; esc closes a view first",
        "",
        "Nothing changes unless you press p, pick a mode, or confirm an assistant.",
    ]
    .join("\n")
}

// ---- the terminal -------------------------------------------------------------------------------

/// Open the Control Center. Without a terminal, print the status and exit instead of blocking.
pub fn run(paths: &Paths) -> anyhow::Result<()> {
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        let data = Data::load(paths);
        let text = status::render_status(&data.state, &data.report, Painter::stdout());
        // The status is already on screen: point at the next step, not at `status` again.
        let next = if data.report.connected.is_empty() {
            "Next: `agentdiet init --dry-run` previews what it would connect; `agentdiet --help` lists the commands."
        } else {
            "Open `agentdiet` in a terminal for the Control Center; `agentdiet --help` lists the commands."
        };
        ui::emit(&format!("{text}\n{next}\n")).context("could not write to stdout")?;
        return Ok(());
    }
    let ctx = Ctx::from_env();
    let (mut terminal, restore) = tui::enter()?;
    let mut data = Data::load(paths);
    let mut app = App::new();
    let mut loaded = Instant::now();
    loop {
        if let Some(sig) = tui::pending_signal() {
            tui::exit_for_signal(sig, restore);
        }
        terminal
            .draw(|frame| draw(frame, &app, &data, ctx))
            .context("could not draw the Control Center")?;
        let wait = REFRESH.saturating_sub(loaded.elapsed());
        if tui::poll(wait).context("could not read the keyboard")? {
            match event::read().context("could not read the keyboard")? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if let Some(action) = app.on_key(key, &data) {
                        match action {
                            Action::Quit => return Ok(()),
                            Action::OpenLive => {
                                match top::run_in(&mut terminal, paths, ctx, true)? {
                                    top::Ended::Quit => return Ok(()),
                                    top::Ended::Signal(sig) => tui::exit_for_signal(sig, restore),
                                    top::Ended::Back => {}
                                }
                                // No `terminal.clear()`: it asks the terminal for the cursor position and
                                // waits for the answer. The buffer still holds the dashboard's last
                                // frame, so the next draw diffs against what is really on screen.
                                data = Data::load(paths);
                            }
                            Action::ShowLast => {
                                let text =
                                    control::inspect_last(paths, &data.state, SystemTime::now());
                                data.last = Some(text);
                                app.view = View::Last { scroll: 0 };
                            }
                            Action::Refresh => {
                                data = Data::load(paths);
                                app.message = Some("Reloaded.".into());
                            }
                            other => {
                                app.message = Some(apply(&other, paths, &data));
                                data = Data::load(paths);
                            }
                        }
                        loaded = Instant::now();
                    }
                }
                // A resize needs no handling: the next loop iteration draws at the new size.
                _ => {}
            }
        }
        if loaded.elapsed() >= REFRESH {
            data.reload_state(paths);
            loaded = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;

    use crossterm::event::KeyEventState;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tempfile::TempDir;

    use super::*;
    use crate::state;
    use ratatui::style::Color;

    const UNICODE: Ctx = Ctx {
        ascii: false,
        color: false,
    };

    fn paths(dir: &TempDir) -> Paths {
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

    fn data(paths: &Paths) -> Data {
        Data {
            state: State::default(),
            report: status::doctor(paths, OsStr::new("")),
            installed: init::installed(paths),
            recent: Vec::new(),
            last: None,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn press(app: &mut App, data: &Data, keys: &str) -> Vec<Option<Action>> {
        keys.chars()
            .map(|c| app.on_key(key(KeyCode::Char(c)), data))
            .collect()
    }

    fn screen(app: &App, data: &Data, ctx: Ctx, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app, data, ctx)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn text(lines: &[String]) -> String {
        lines.join("\n")
    }

    #[test]
    fn main_keys_ask_for_actions_or_open_views() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        let d = data(&p);
        let mut app = App::new();
        assert_eq!(press(&mut app, &d, "p"), [Some(Action::TogglePause)]);
        assert_eq!(press(&mut app, &d, "l"), [Some(Action::ShowLast)]);
        assert_eq!(press(&mut app, &d, "t"), [Some(Action::OpenLive)]);
        assert_eq!(press(&mut app, &d, "r"), [Some(Action::Refresh)]);
        assert_eq!(press(&mut app, &d, "q"), [Some(Action::Quit)]);
        assert_eq!(app.on_key(key(KeyCode::Esc), &d), Some(Action::Quit));
        let ctrl_c = KeyEvent {
            modifiers: KeyModifiers::CONTROL,
            ..key(KeyCode::Char('c'))
        };
        assert_eq!(app.on_key(ctrl_c, &d), Some(Action::Quit));
        // Keys with no meaning do nothing.
        assert_eq!(press(&mut app, &d, "xz"), [None, None]);
        assert_eq!(app.view, View::Main);
    }

    #[test]
    fn the_mode_picker_chooses_by_number_cursor_or_cancels() {
        let dir = TempDir::new().unwrap();
        let d = data(&paths(&dir));
        let mut app = App::new();
        assert_eq!(press(&mut app, &d, "m"), [None]);
        assert_eq!(
            app.view,
            View::Mode { cursor: 1 },
            "starts on the current mode"
        );
        assert_eq!(
            press(&mut app, &d, "3"),
            [Some(Action::SetMode(Mode::UltraLean))]
        );
        assert_eq!(app.view, View::Main);

        press(&mut app, &d, "m");
        app.on_key(key(KeyCode::Up), &d);
        assert_eq!(
            app.on_key(key(KeyCode::Enter), &d),
            Some(Action::SetMode(Mode::Light))
        );

        press(&mut app, &d, "m");
        assert_eq!(app.on_key(key(KeyCode::Esc), &d), None);
        assert_eq!(
            app.view,
            View::Main,
            "esc leaves the picker, it does not quit"
        );
        // Down stops at the last mode.
        press(&mut app, &d, "m");
        for _ in 0..5 {
            app.on_key(key(KeyCode::Down), &d);
        }
        assert_eq!(app.view, View::Mode { cursor: 2 });
    }

    #[test]
    fn connecting_and_disconnecting_needs_a_yes() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        let mut d = data(&p);
        let mut app = App::new();
        press(&mut app, &d, "a");
        app.on_key(key(KeyCode::Down), &d); // Cursor
        assert_eq!(app.on_key(key(KeyCode::Enter), &d), None);
        assert!(matches!(
            app.view,
            View::Assistants {
                confirm: Some(Target::Cursor),
                ..
            }
        ));
        // Anything but y cancels, and nothing is asked for.
        assert_eq!(press(&mut app, &d, "n"), [None]);
        assert!(matches!(app.view, View::Assistants { confirm: None, .. }));

        app.on_key(key(KeyCode::Enter), &d);
        assert_eq!(
            press(&mut app, &d, "y"),
            [Some(Action::Connect(Target::Cursor))]
        );

        d.installed.push(Target::Cursor);
        app.on_key(key(KeyCode::Enter), &d);
        assert_eq!(
            press(&mut app, &d, "y"),
            [Some(Action::Disconnect(Target::Cursor))]
        );

        // q goes back one view; it only quits from the main view.
        assert_eq!(press(&mut app, &d, "q"), [None]);
        assert_eq!(app.view, View::Main);
        assert_eq!(press(&mut app, &d, "q"), [Some(Action::Quit)]);
    }

    #[test]
    fn panes_scroll_within_their_bounds_and_close_with_esc() {
        let dir = TempDir::new().unwrap();
        let d = {
            let mut d = data(&paths(&dir));
            d.last = Some((1..=100).map(|i| format!("line {i}\n")).collect());
            d
        };
        let mut app = App::new();
        assert_eq!(press(&mut app, &d, "l"), [Some(Action::ShowLast)]);
        app.view = View::Last { scroll: 0 };
        screen(&app, &d, UNICODE, 80, 24); // the first draw tells the app how far it may scroll
        app.on_key(key(KeyCode::End), &d);
        let View::Last { scroll } = app.view else {
            panic!()
        };
        assert_eq!(scroll, 100 - 22, "100 lines in a 22-row pane");
        app.on_key(key(KeyCode::Down), &d);
        assert_eq!(
            app.view,
            View::Last { scroll },
            "cannot scroll past the end"
        );
        app.on_key(key(KeyCode::PageUp), &d);
        assert_eq!(
            app.view,
            View::Last {
                scroll: scroll - 10
            }
        );
        let shown = text(&screen(&app, &d, UNICODE, 80, 24));
        assert!(
            shown.contains("line 69") && !shown.contains("line 1\n"),
            "{shown}"
        );
        app.on_key(key(KeyCode::Esc), &d);
        assert_eq!(app.view, View::Main);
    }

    #[test]
    fn actions_reach_state_and_configs_and_failures_become_messages() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        let d = data(&p);

        assert!(apply(&Action::TogglePause, &p, &d).starts_with("Paused: output passes"));
        assert_eq!(state::load(&p).status, Status::Paused);
        let mut d2 = data(&p);
        d2.state = state::load(&p);
        assert_eq!(
            apply(&Action::TogglePause, &p, &d2),
            "Resumed: trimming again."
        );
        assert_eq!(
            apply(&Action::SetMode(Mode::Light), &p, &d),
            "Diet Mode set to Light."
        );
        assert_eq!(state::load(&p).mode, Mode::Light);

        let cursor = p.home.join(".cursor/mcp.json");
        fs::create_dir_all(cursor.parent().unwrap()).unwrap();
        fs::write(&cursor, "{\"mcpServers\":{}}").unwrap();
        let msg = apply(&Action::Connect(Target::Cursor), &p, &d);
        assert!(msg.starts_with("Connected Cursor."), "{msg}");
        assert!(fs::read_to_string(&cursor).unwrap().contains("agentdiet"));
        let again = apply(&Action::Connect(Target::Cursor), &p, &d);
        assert!(again.contains("already connected"), "{again}");

        let msg = apply(&Action::Disconnect(Target::Cursor), &p, &d);
        assert!(msg.starts_with("Disconnected Cursor."), "{msg}");
        assert_eq!(fs::read_to_string(&cursor).unwrap(), "{\"mcpServers\":{}}");
        assert_eq!(
            apply(&Action::Disconnect(Target::Cursor), &p, &d),
            "Cursor: nothing to remove."
        );

        // A config AgentDiet cannot edit is a message, and the file is left alone.
        let gemini = p.home.join(".gemini/settings.json");
        fs::create_dir_all(gemini.parent().unwrap()).unwrap();
        fs::write(&gemini, "{ // comment\n}").unwrap();
        let msg = apply(&Action::Connect(Target::Gemini), &p, &d);
        assert!(
            msg.contains("not fully connected") && msg.contains("left untouched"),
            "{msg}"
        );
        assert_eq!(fs::read_to_string(&gemini).unwrap(), "{ // comment\n}");
    }

    #[test]
    fn an_unwritable_state_is_a_message_not_an_exit() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        // `.agentdiet` as a file: every write under it fails.
        fs::create_dir_all(&p.home).unwrap();
        fs::write(&p.agentdiet_home, "in the way").unwrap();
        let msg = apply(&Action::SetMode(Mode::Light), &p, &data(&p));
        assert!(msg.starts_with("Could not change the mode"), "{msg}");
    }

    #[test]
    fn a_fresh_install_explains_what_to_do() {
        let dir = TempDir::new().unwrap();
        let d = data(&paths(&dir));
        let shown = text(&screen(&App::new(), &d, UNICODE, 80, 24));
        for expected in [
            "AgentDiet  ● Active · Balanced mode",
            "? help",
            "Trims outputs over 600 est. tokens · must save 20% · 3 context lines",
            "No assistants connected yet.",
            "Press a to connect one,",
            "Savings (all projects, estimated)",
            "Nothing trimmed yet.",
            "No activity in this workspace yet.",
            "p pause  m mode  l last output  a assistants  d doctor  t live  ? help  q quit",
        ] {
            assert!(
                shown.contains(expected),
                "missing {expected:?} in:\n{shown}"
            );
        }
    }

    fn busy(dir: &TempDir) -> (Paths, Data) {
        let p = paths(dir);
        for (target, file, body) in [
            (Target::Cursor, ".cursor/mcp.json", "{}"),
            (Target::Claude, ".claude/settings.json", "{}"),
        ] {
            let _ = target;
            let path = p.home.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        let opts = InitOptions {
            targets: vec![Target::Cursor, Target::Claude, Target::Shims],
            project: false,
            dry_run: false,
        };
        init::run(&p, &opts).unwrap();
        let mut d = data(&p);
        d.state = State {
            status: Status::Paused,
            trimmed_outputs: 4,
            tokens_saved: 134_830,
            cas_dereferences: 1,
            raw_tokens_passed_while_paused: 2_500,
            last_trimmed_id: Some("683db530".into()),
            ..State::default()
        };
        d.recent = vec![RecentItem {
            id: Some("683db530".into()),
            kind: "shell".into(),
            title: "cargo test".into(),
            summary: "kept 14/412 lines; 398 passing lines collapsed".into(),
        }];
        (p, d)
    }

    #[test]
    fn a_busy_install_shows_status_assistants_savings_attention_and_activity() {
        let dir = TempDir::new().unwrap();
        let (_, d) = busy(&dir);
        let shown = text(&screen(&App::new(), &d, UNICODE, 80, 24));
        for expected in [
            "■ Paused · Balanced mode",
            "✓ Claude Code",
            "not yet observed",
            "✓ Cursor",
            "configured",
            "≈134,830 tokens saved",
            "1 re-open for 4 trimmed outputs (25%)",
            "⚠ high: try a lighter mode",
            "last #683db530 · l to view",
            "≈2,500 tokens passed untrimmed",
            "Needs attention",
            "PATH",
            "→ add to your shell rc: export PATH=",
            "Recent activity (this workspace)",
            "#683db530",
            "cargo test",
            "Paused. Output passes through untrimmed (still scrubbed).",
            "p resume",
        ] {
            assert!(
                shown.contains(expected),
                "missing {expected:?} in:\n{shown}"
            );
        }
    }

    #[test]
    fn every_row_fits_at_every_size_and_small_screens_degrade() {
        let dir = TempDir::new().unwrap();
        let (_, d) = busy(&dir);
        for (w, h) in [
            (80, 24),
            (100, 40),
            (70, 14),
            (60, 20),
            (45, 12),
            (39, 11),
            (20, 5),
            (200, 60),
        ] {
            for view in [
                View::Main,
                View::Mode { cursor: 0 },
                View::Assistants {
                    cursor: 1,
                    confirm: Some(Target::Cursor),
                },
                View::Doctor { scroll: 0 },
                View::Help { scroll: 0 },
            ] {
                let app = App { view, ..App::new() };
                for row in screen(&app, &d, UNICODE, w, h) {
                    assert!(row.chars().count() <= usize::from(w), "{w}x{h}: {row:?}");
                }
            }
        }
        let tiny = text(&screen(&App::new(), &d, UNICODE, 39, 11));
        assert!(tiny.contains("Enlarge the terminal, or press q"), "{tiny}");
        let narrow = text(&screen(&App::new(), &d, UNICODE, 60, 24));
        assert!(!narrow.contains("Recent activity"), "{narrow}");
        assert!(
            narrow.contains("Needs attention") && narrow.contains("≈134,830"),
            "{narrow}"
        );
        assert!(narrow.contains("p m l a d t ? q"), "{narrow}");
        let medium = text(&screen(&App::new(), &d, UNICODE, 72, 24));
        assert!(
            medium.contains("p resume  m mode  l last  a assist  d doctor"),
            "{medium}"
        );
        // A long fix wraps under its own indent instead of running off the edge.
        assert!(
            narrow.contains("   → add to your shell rc: export"),
            "{narrow}"
        );
        let cramped = text(&screen(&App::new(), &d, UNICODE, 44, 14));
        assert!(cramped.contains("p m l a d t ? q"), "{cramped}");
    }

    #[test]
    fn ascii_terminals_get_ascii_and_no_colour_means_no_hue() {
        let dir = TempDir::new().unwrap();
        let (_, d) = busy(&dir);
        let ascii = Ctx {
            ascii: true,
            color: false,
        };
        let shown = text(&screen(&App::new(), &d, ascii, 80, 24));
        assert!(shown.is_ascii(), "{shown}");
        assert!(shown.contains("# Paused - Balanced mode"), "{shown}");
        assert!(shown.contains("+ Claude Code"), "{shown}");

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| draw(f, &App::new(), &d, UNICODE))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert!(
            buffer
                .content()
                .iter()
                .all(|c| c.fg == Color::Reset && c.bg == Color::Reset),
            "NO_COLOR: weight and reverse video only"
        );
    }

    #[test]
    fn views_render_their_content() {
        let dir = TempDir::new().unwrap();
        let (_, mut d) = busy(&dir);
        d.last = Some("Last trimmed output: #683db530 (3 lines)\none\ntwo\n".into());

        let mode = text(&screen(
            &App {
                view: View::Mode { cursor: 0 },
                ..App::new()
            },
            &d,
            UNICODE,
            80,
            24,
        ));
        for expected in [
            "Diet Mode",
            "> 1  light",
            "2  balanced  (current)",
            "3  ultra-lean",
            "Gentlest",
            "must save 10%",
        ] {
            assert!(mode.contains(expected), "missing {expected:?} in:\n{mode}");
        }

        let assistants = text(&screen(
            &App {
                view: View::Assistants {
                    cursor: 1,
                    confirm: Some(Target::Cursor),
                },
                ..App::new()
            },
            &d,
            UNICODE,
            80,
            24,
        ));
        assert!(assistants.contains("> ✓ Cursor"), "{assistants}");
        assert!(
            assistants.contains("Disconnect Cursor?") && assistants.contains("[y/N]"),
            "{assistants}"
        );

        let last = text(&screen(
            &App {
                view: View::Last { scroll: 0 },
                ..App::new()
            },
            &d,
            UNICODE,
            80,
            24,
        ));
        assert!(
            last.contains("Last trimmed output: #683db530 (3 lines)") && last.contains("two"),
            "{last}"
        );

        let doctor = text(&screen(
            &App {
                view: View::Doctor { scroll: 0 },
                ..App::new()
            },
            &d,
            UNICODE,
            100,
            30,
        ));
        assert!(
            doctor.contains("AgentDiet doctor") && doctor.contains("PATH"),
            "{doctor}"
        );

        let help = text(&screen(
            &App {
                view: View::Help { scroll: 0 },
                ..App::new()
            },
            &d,
            UNICODE,
            80,
            24,
        ));
        assert!(
            help.contains("agentdiet show last") && help.contains("agentdiet doctor"),
            "{help}"
        );
    }

    #[test]
    fn fit_and_wrap_behave() {
        assert_eq!(fit("abcdef", 6), "abcdef");
        assert_eq!(fit("abcdefg", 6), "abcde…");
        assert_eq!(wrap("aaa bbb ccc", 7), ["aaa bbb", "  ccc"]);
        assert_eq!(
            wrap("  indented text here", 12),
            ["  indented", "    text", "    here"]
        );
        assert!(wrap("", 10).is_empty());
        assert_eq!(wrap("a\n\nb", 10), ["a", "", "b"], "blank lines survive");
        assert!(
            wrap("x".repeat(30).as_str(), 10)
                .iter()
                .all(|l| !l.is_empty())
        );
    }
}
