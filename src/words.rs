//! The phrases people see in more than one place (SPEC §2.5 "Words"). `status`, `doctor`, `stats`,
//! the Control Center and `top` all take them from here, so the same fact always reads the same way.

use crate::cas::CasStats;
use crate::diet::ModeParams;
use crate::state::{DEREFERENCE_WARN_RATE, Mode, State, Status};
use crate::ui::{Level, plural, thousands};

/// What "paused" means for output, wherever it is explained.
pub const PAUSED_MEANING: &str = "output passes through untrimmed (still scrubbed)";

/// The warning for a high re-open rate (above [`DEREFERENCE_WARN_RATE`]).
pub const REOPEN_WARNING: &str = "⚠ high: agents re-read trimmed output; consider a lighter mode";

/// `● Active` / `■ Paused`, with the level it is shown at.
pub fn status_word(status: Status) -> (&'static str, Level) {
    match status {
        Status::Active => ("● Active", Level::Ok),
        Status::Paused => ("■ Paused", Level::Warning),
    }
}

/// `1,234 things`, for counters.
pub fn count(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

/// `≈134,830 tokens saved`.
pub fn tokens_saved(state: &State) -> String {
    format!("≈{} tokens saved", thousands(state.tokens_saved))
}

/// `4 trimmed outputs`.
pub fn trimmed_outputs(state: &State) -> String {
    count(state.trimmed_outputs, "trimmed output", "trimmed outputs")
}

/// `1 re-open for 4 trimmed outputs (25%)`, or `None` while nothing has been trimmed. Raw counts are
/// kept so a rate from a small sample is visibly one; it can exceed 100%.
pub fn reopens(state: &State) -> Option<String> {
    state.dereference_rate().map(|rate| {
        format!(
            "{} for {} ({:.0}%)",
            count(state.cas_dereferences, "re-open", "re-opens"),
            trimmed_outputs(state),
            rate * 100.0
        )
    })
}

/// Whether [`REOPEN_WARNING`] applies.
pub fn reopens_high(state: &State) -> bool {
    state
        .dereference_rate()
        .is_some_and(|rate| rate > DEREFERENCE_WARN_RATE)
}

/// `≈2,500 tokens passed untrimmed (still scrubbed)`.
pub fn passed_while_paused(state: &State) -> String {
    format!("{} (still scrubbed)", passed_while_paused_short(state))
}

/// `≈2,500 tokens passed untrimmed`, for a half-width column.
pub fn passed_while_paused_short(state: &State) -> String {
    format!(
        "≈{} tokens passed untrimmed",
        thousands(state.raw_tokens_passed_while_paused)
    )
}

/// `12 entries, 2.0 KiB (3 expired)`, or `empty`.
pub fn vault(stats: &CasStats) -> String {
    if stats.live == 0 && stats.expired == 0 {
        return "empty".into();
    }
    let mut out = format!(
        "{}, {:.1} KiB",
        plural(stats.live, "entry", "entries"),
        stats.bytes as f64 / 1024.0
    );
    if stats.expired > 0 {
        out.push_str(&format!(" ({} expired)", thousands(stats.expired as u64)));
    }
    out
}

/// What a mode does, from its §3 Tier 2 parameters.
pub fn mode_line(mode: Mode) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases_read_naturally_at_every_count() {
        let mut s = State::default();
        assert_eq!(reopens(&s), None);
        s.trimmed_outputs = 4;
        s.cas_dereferences = 1;
        assert_eq!(
            reopens(&s).unwrap(),
            "1 re-open for 4 trimmed outputs (25%)"
        );
        assert!(reopens_high(&s));
        s.trimmed_outputs = 1;
        s.cas_dereferences = 3;
        assert_eq!(
            reopens(&s).unwrap(),
            "3 re-opens for 1 trimmed output (300%)"
        );
        s.cas_dereferences = 0;
        s.trimmed_outputs = 10;
        assert!(!reopens_high(&s));
        s.tokens_saved = 134_830;
        assert_eq!(tokens_saved(&s), "≈134,830 tokens saved");
        s.raw_tokens_passed_while_paused = 2_500;
        assert_eq!(
            passed_while_paused(&s),
            "≈2,500 tokens passed untrimmed (still scrubbed)"
        );
        let v = CasStats {
            live: 12,
            expired: 3,
            bytes: 2048,
        };
        assert_eq!(vault(&v), "12 entries, 2.0 KiB (3 expired)");
        assert_eq!(vault(&CasStats::default()), "empty");
        assert_eq!(status_word(Status::Paused).0, "■ Paused");
    }
}
