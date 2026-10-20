//! What the full-screen views (the Control Center and `top`) share: entering and restoring the
//! terminal, and leaving it clean when the process is told to stop (SPEC §2.5 "Full-screen views").
//!
//! Restoring the terminal (raw mode off, main screen back, cursor shown) writes to it, which is not
//! async-signal-safe, so it never happens in a signal handler. The handler only records which signal
//! arrived ([`pending_signal`]); the event loop, which never waits longer than [`TICK`], sees it,
//! returns, lets [`Restore`] put the terminal back from ordinary code, and then dies by that same
//! signal ([`exit_for_signal`]) so the parent still sees why it ended.

use std::io;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use anyhow::Context;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::{self, Level};

/// The longest an event loop waits before looking at [`pending_signal`] again.
pub const TICK: Duration = Duration::from_millis(100);

/// The signal that asked us to stop, or 0. Written only by the handler (an atomic store is
/// async-signal-safe), read by the event loop.
static PENDING: AtomicI32 = AtomicI32::new(0);

/// Restores the terminal however a view ends (ratatui's own panic hook covers a panic).
pub struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        ratatui::restore();
        // `ratatui::restore` leaves the cursor to `Terminal`'s own drop, which never runs when the
        // process ends by a signal (`exit_for_signal`), so show it here too. Best effort.
        let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
    }
}

/// Take over the terminal for a full-screen view: stderr warnings off (they would land on the
/// screen), stop signals caught, raw mode and the alternate screen on.
pub fn enter() -> anyhow::Result<(ratatui::DefaultTerminal, Restore)> {
    crate::state::quiet_warnings();
    install_handlers();
    let terminal = ratatui::try_init().context("could not set up the terminal")?;
    Ok((terminal, Restore))
}

/// [`crossterm::event::poll`] for at most [`TICK`]. A signal interrupting the wait is "no event":
/// the caller then checks [`pending_signal`].
pub fn poll(timeout: Duration) -> io::Result<bool> {
    match crossterm::event::poll(timeout.min(TICK)) {
        Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(false),
        other => other,
    }
}

/// The stop signal (SIGTERM, SIGHUP, SIGINT) that arrived, if any.
pub fn pending_signal() -> Option<i32> {
    match PENDING.load(Ordering::Relaxed) {
        0 => None,
        sig => Some(sig),
    }
}

/// Put the terminal back, then end the process by `sig` with its default action, as if the handler
/// had never been installed. Falls back to exit status 128 + `sig`.
pub fn exit_for_signal(sig: i32, restore: Restore) -> ! {
    drop(restore);
    #[cfg(unix)]
    // SAFETY: `signal` and `raise` are called from ordinary code (not a handler) with a valid
    // signal number that this module installed a handler for.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
    std::process::exit(128 + sig)
}

#[cfg(unix)]
extern "C" fn record(sig: libc::c_int) {
    PENDING.store(sig, Ordering::Relaxed);
}

#[cfg(unix)]
fn install_handlers() {
    for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
        // SAFETY: a zeroed `sigaction` is a valid "no flags, empty mask" value; `record` is an
        // `extern "C" fn(c_int)` that only does an atomic store, which is async-signal-safe. No
        // SA_RESTART, so a blocked `poll` returns EINTR and the loop notices at once.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = record as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(sig, &action, std::ptr::null_mut());
        }
    }
}

#[cfg(not(unix))]
fn install_handlers() {}

const ASCII_BORDER: ratatui::symbols::border::Set<'static> = ratatui::symbols::border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// How this terminal can be drawn on (SPEC §2.5).
#[derive(Debug, Clone, Copy)]
pub struct Ctx {
    pub ascii: bool,
    pub color: bool,
}

impl Ctx {
    pub fn from_env() -> Self {
        let no_color = std::env::var_os("NO_COLOR");
        let term = std::env::var_os("TERM");
        Self {
            ascii: ui::ascii_from_env(),
            color: ui::color_allowed(true, no_color.as_deref(), term.as_deref()),
        }
    }

    /// Box borders: the usual line drawing, or `+ - |` on an ASCII terminal.
    pub(crate) fn border(self) -> ratatui::symbols::border::Set<'static> {
        if self.ascii {
            ASCII_BORDER
        } else {
            ratatui::symbols::border::PLAIN
        }
    }

    pub(crate) fn t(self, s: &str) -> String {
        if self.ascii {
            ui::asciify(s)
        } else {
            s.to_string()
        }
    }

    pub(crate) fn level(self, level: Level) -> Style {
        match (self.color, level) {
            (true, Level::Ok) => Style::new().fg(Color::Green),
            (true, Level::Warning) => Style::new().fg(Color::Yellow),
            (true, Level::Broken) => Style::new().fg(Color::Red),
            (true, Level::Info) => Style::new().fg(Color::DarkGray),
            // Without colour: weight, never hue.
            (false, Level::Warning | Level::Broken) => Style::new().add_modifier(Modifier::BOLD),
            (false, _) => Style::new(),
        }
    }

    pub(crate) fn dim(self) -> Style {
        if self.color {
            Style::new().fg(Color::DarkGray)
        } else {
            Style::new()
        }
    }

    pub(crate) fn bold(self) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }

    pub(crate) fn selected(self) -> Style {
        Style::new().add_modifier(Modifier::REVERSED)
    }

    pub(crate) fn span(self, text: &str, style: Style) -> Span<'static> {
        Span::styled(self.t(text), style)
    }

    pub(crate) fn line(self, text: &str, style: Style) -> Line<'static> {
        Line::from(self.span(text, style))
    }
}
