//! Output for people (SPEC §2.5): colour only on a terminal without `NO_COLOR`, a word beside every
//! symbol, `~/…` paths. Never used for agent-facing text (breadcrumbs, `show`, `stats --json`, `scan`,
//! hooks, MCP).

use std::ffi::OsStr;
use std::io::IsTerminal;
use std::path::Path;

/// How a line reads: decides the symbol and, with colour on, its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Information, nothing to do (`·`).
    Info,
    /// Fine (`✓`).
    Ok,
    /// Works, but worth a look (`⚠`).
    Warning,
    /// Does not work (`✗`).
    Broken,
}

impl Level {
    pub fn symbol(self) -> &'static str {
        match self {
            Level::Info => "·",
            Level::Ok => "✓",
            Level::Warning => "⚠",
            Level::Broken => "✗",
        }
    }

    fn ansi(self) -> &'static str {
        match self {
            Level::Info => "2",
            Level::Ok => "32",
            Level::Warning => "33",
            Level::Broken => "31",
        }
    }
}

/// The SPEC §2.5 colour rule: a terminal, `NO_COLOR` unset or empty, `TERM` not `dumb`.
pub fn color_allowed(is_terminal: bool, no_color: Option<&OsStr>, term: Option<&OsStr>) -> bool {
    is_terminal && no_color.is_none_or(OsStr::is_empty) && term.is_none_or(|t| t != "dumb")
}

/// Styles text for one stream.
#[derive(Debug, Clone, Copy)]
pub struct Painter {
    color: bool,
}

impl Painter {
    fn for_stream(is_terminal: bool) -> Self {
        let no_color = std::env::var_os("NO_COLOR");
        let term = std::env::var_os("TERM");
        Self {
            color: color_allowed(is_terminal, no_color.as_deref(), term.as_deref()),
        }
    }

    pub fn stdout() -> Self {
        Self::for_stream(std::io::stdout().is_terminal())
    }

    pub fn stderr() -> Self {
        Self::for_stream(std::io::stderr().is_terminal())
    }

    /// Never colours; for tests and captured output.
    pub fn plain() -> Self {
        Self { color: false }
    }

    fn wrap(self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub fn bold(self, text: &str) -> String {
        self.wrap("1", text)
    }

    pub fn dim(self, text: &str) -> String {
        self.wrap("2", text)
    }

    /// `text` in the level's colour.
    pub fn level(self, level: Level, text: &str) -> String {
        self.wrap(level.ansi(), text)
    }

    /// `✓ word`, coloured as one unit. `width` pads the word (outside the escape codes), so columns
    /// line up with colour on or off.
    pub fn badge(self, level: Level, word: &str, width: usize) -> String {
        let text = format!("{} {word}", level.symbol());
        let pad = width.saturating_sub(text.chars().count());
        format!("{}{}", self.level(level, &text), " ".repeat(pad))
    }
}

/// SPEC §2.5 "Glyphs": `true` when the locale (`LC_ALL`, else `LC_CTYPE`, else `LANG`: the first one
/// that is set and not empty) names a non-UTF-8 charset, or `TERM` is `dumb`. Unset means UTF-8.
pub fn ascii_wanted(
    lc_all: Option<&OsStr>,
    lc_ctype: Option<&OsStr>,
    lang: Option<&OsStr>,
    term: Option<&OsStr>,
) -> bool {
    if term.is_some_and(|t| t == "dumb") {
        return true;
    }
    [lc_all, lc_ctype, lang]
        .into_iter()
        .flatten()
        .find(|v| !v.is_empty())
        .is_some_and(|v| {
            let v = v.to_string_lossy().to_ascii_lowercase();
            !(v.contains("utf-8") || v.contains("utf8"))
        })
}

/// [`ascii_wanted`] for this process's environment.
pub fn ascii_from_env() -> bool {
    let var = |name| std::env::var_os(name);
    ascii_wanted(
        var("LC_ALL").as_deref(),
        var("LC_CTYPE").as_deref(),
        var("LANG").as_deref(),
        var("TERM").as_deref(),
    )
}

/// `✓ ⚠ ✗ · ● ■ ○ ≈ → …` as `+ ! x - * # o ~ -> ...` (the symbols this crate prints).
pub fn asciify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '✓' => out.push('+'),
            '⚠' => out.push('!'),
            '✗' => out.push('x'),
            '·' => out.push('-'),
            '●' => out.push('*'),
            '■' => out.push('#'),
            '○' => out.push('o'),
            '≈' => out.push('~'),
            '→' => out.push_str("->"),
            '…' => out.push_str("..."),
            '↑' => out.push('^'),
            '↓' => out.push('v'),
            other => out.push(other),
        }
    }
    out
}

/// Write `bytes` to stdout unchanged (data: `show`, `stats --json`, `scan`). A reader that went away
/// (`agentdiet show last | head`) ends the output quietly instead of panicking like `print!` or
/// failing; any other write error is returned (SPEC §2.5 "Pipes").
pub fn write_out(bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    match out.write_all(bytes).and_then(|()| out.flush()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }
}

/// [`write_out`] for people-facing text, in ASCII when the environment asks for it.
pub fn emit(text: &str) -> std::io::Result<()> {
    if ascii_from_env() {
        write_out(asciify(text).as_bytes())
    } else {
        write_out(text.as_bytes())
    }
}

/// One line on stderr. Never fails and never panics: a closed or broken stderr is ignored, because
/// a warning must not turn a finished command into a crash (unlike `eprintln!`).
pub fn warn(line: &str) {
    use std::io::Write;
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{line}");
}

/// `path` with the home directory shown as `~`.
pub fn tilde(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// `12345` → `12,345`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `n thing` / `n things`.
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `a`, `a and b`, `a, b and c`.
pub fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn colour_needs_a_terminal_and_no_opt_out() {
        let os = |s: &'static str| Some(OsStr::new(s));
        assert!(color_allowed(true, None, os("xterm-256color")));
        assert!(color_allowed(true, os(""), None), "empty NO_COLOR is unset");
        assert!(!color_allowed(false, None, None), "pipes never get colour");
        assert!(!color_allowed(true, os("1"), None));
        assert!(!color_allowed(true, None, os("dumb")));
    }

    #[test]
    fn plain_output_has_no_escapes_and_aligned_badges() {
        let p = Painter::plain();
        assert_eq!(p.badge(Level::Ok, "ok", 8), "✓ ok    ");
        assert_eq!(p.bold("x"), "x");
        let c = Painter { color: true };
        let badge = c.badge(Level::Broken, "broken", 10);
        assert!(badge.starts_with("\x1b[31m✗ broken\x1b[0m"));
        assert!(
            badge.ends_with("  "),
            "padding sits outside the colour codes"
        );
    }

    #[test]
    fn the_locale_decides_between_unicode_and_ascii() {
        let os = |s: &'static str| Some(OsStr::new(s));
        let w = |a, c, l, t| ascii_wanted(a, c, l, t);
        assert!(!w(None, None, None, None), "unset is UTF-8");
        assert!(!w(None, None, os("en_US.UTF-8"), os("xterm")));
        assert!(!w(None, None, os("de_DE.utf8"), None));
        assert!(w(None, None, os("C"), None));
        assert!(w(None, None, os("POSIX"), None));
        assert!(w(None, None, os("en_US.ISO8859-1"), None));
        // The first one that is set wins, and empty counts as unset.
        assert!(w(os("C"), None, os("en_US.UTF-8"), None));
        assert!(!w(os(""), None, os("en_US.UTF-8"), None));
        assert!(!w(None, os("en_US.UTF-8"), os("C"), None));
        assert!(w(None, None, os("en_US.UTF-8"), os("dumb")));
    }

    #[test]
    fn asciify_keeps_meaning_and_other_text() {
        assert_eq!(
            asciify("✓ ok · ⚠ warn → fix… ≈5 ● ○ ✗"),
            "+ ok - ! warn -> fix... ~5 * o x"
        );
        assert_eq!(asciify("plain café"), "plain café");
    }

    #[test]
    fn home_paths_use_a_tilde() {
        let home = PathBuf::from("/Users/a");
        assert_eq!(
            tilde(&home, Path::new("/Users/a/.claude.json")),
            "~/.claude.json"
        );
        assert_eq!(tilde(&home, Path::new("/Users/a")), "~");
        assert_eq!(tilde(&home, Path::new("/opt/x")), "/opt/x");
        assert_eq!(tilde(&home, Path::new("/Users/ab")), "/Users/ab");
    }

    #[test]
    fn numbers_and_lists() {
        assert_eq!(thousands(134_830), "134,830");
        assert_eq!(thousands(7), "7");
        assert_eq!(plural(1, "file", "files"), "1 file");
        assert_eq!(plural(3, "file", "files"), "3 files");
        assert_eq!(and_list(&["A"]), "A");
        assert_eq!(and_list(&["A", "B", "C"]), "A, B and C");
    }
}
