//! `agentdiet stats`: the §2.3 counters from `state.json` plus the workspace's run log totals per
//! origin (SPEC §3b "Stats"). Read-only: memory.db is never created just to be read.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::Paths;
use crate::memory::{MemoryDb, MemoryError, Origin, RunTotals};
use crate::state::{self, State, Status};
use crate::ui::tilde;
use crate::words;

#[derive(Debug, Clone)]
pub struct Stats {
    pub state: State,
    pub workspace: PathBuf,
    /// The home directory, so the text can show `~/…` (never part of `--json`).
    pub home: PathBuf,
    /// The workspace's memory.db, when it exists.
    pub memory_db: Option<PathBuf>,
    /// Per origin, in the order shim, mcp, hook; all zero without a memory.db.
    pub runs: Vec<(Origin, RunTotals)>,
}

pub fn collect(paths: &Paths) -> Result<Stats, MemoryError> {
    let db_path = paths.memory_db();
    let (memory_db, runs) = if db_path.is_file() {
        let totals = MemoryDb::open(&db_path)?.run_totals()?;
        (Some(db_path), totals)
    } else {
        let zero = [Origin::Shim, Origin::Mcp, Origin::Hook]
            .into_iter()
            .map(|o| (o, RunTotals::default()))
            .collect();
        (None, zero)
    };
    Ok(Stats {
        state: state::load(paths),
        workspace: paths.workspace_root(),
        home: paths.home.clone(),
        memory_db,
        runs,
    })
}

/// `12,345`.
fn grouped(n: u64) -> String {
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

impl Stats {
    pub fn to_json(&self) -> Value {
        let runs: serde_json::Map<String, Value> = self
            .runs
            .iter()
            .map(|(origin, t)| {
                (
                    origin.as_str().to_string(),
                    json!({
                        "runs": t.runs,
                        "failed": t.failed,
                        "trimmed": t.trimmed,
                        "raw_tokens": t.raw_tokens,
                        "emitted_tokens": t.emitted_tokens,
                    }),
                )
            })
            .collect();
        json!({
            "status": self.state.status,
            "mode": self.state.mode,
            "trimmed_outputs": self.state.trimmed_outputs,
            "tokens_saved": self.state.tokens_saved,
            "cas_dereferences": self.state.cas_dereferences,
            "raw_tokens_passed_while_paused": self.state.raw_tokens_passed_while_paused,
            "last_trimmed_id": self.state.last_trimmed_id,
            "workspace": self.workspace,
            "memory_db": self.memory_db,
            "runs": runs,
        })
    }

    /// The people-facing text (SPEC §3b "Stats"), in the §2.5 words. `--json` is [`Stats::to_json`].
    pub fn render(&self) -> String {
        let s = &self.state;
        let status = match s.status {
            Status::Active => "active",
            Status::Paused => "paused",
        };
        let none_yet = || "none yet".to_string();
        let mut out = format!(
            "AgentDiet is {status} · {} mode\n\nAll projects (estimated)\n",
            s.mode.label()
        );
        let mut row = |label: &str, value: String| {
            out.push_str(&format!("  {label:<21}{value}\n"));
        };
        row("trimmed outputs", grouped(s.trimmed_outputs));
        row("tokens saved", format!("≈{}", grouped(s.tokens_saved)));
        row(
            "re-opened by agents",
            words::reopens(s).unwrap_or_else(none_yet),
        );
        row("while paused", words::passed_while_paused(s));
        row(
            "last trimmed",
            s.last_trimmed_id
                .as_deref()
                .map_or_else(none_yet, |id| format!("#{id} · agentdiet show last")),
        );
        out.push_str(&format!(
            "\nThis workspace ({})\n",
            tilde(&self.home, &self.workspace)
        ));
        if self.memory_db.is_none() {
            out.push_str("  Nothing recorded in this workspace yet.\n");
            return out;
        }
        out.push_str(&format!(
            "  {:<18}{:>6}{:>8}{:>9}  est. tokens before → after\n",
            "via", "runs", "failed", "trimmed"
        ));
        for (origin, t) in &self.runs {
            let via = match origin {
                Origin::Shim => "shims",
                Origin::Mcp => "MCP (diet_exec)",
                Origin::Hook => "Claude Code hooks",
            };
            out.push_str(&format!(
                "  {via:<18}{:>6}{:>8}{:>9}  {} → {}\n",
                grouped(t.runs),
                grouped(t.failed),
                grouped(t.trimmed),
                grouped(t.raw_tokens),
                grouped(t.emitted_tokens),
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(memory: bool) -> Stats {
        Stats {
            state: State {
                trimmed_outputs: 4,
                tokens_saved: 134_830,
                cas_dereferences: 1,
                last_trimmed_id: Some("683db530".into()),
                ..State::default()
            },
            workspace: PathBuf::from("/home/u/code/app"),
            home: PathBuf::from("/home/u"),
            memory_db: memory.then(|| PathBuf::from("/home/u/code/app/.agentdiet/memory.db")),
            runs: vec![
                (
                    Origin::Shim,
                    RunTotals {
                        runs: 4,
                        ..RunTotals::default()
                    },
                ),
                (
                    Origin::Mcp,
                    RunTotals {
                        runs: 1,
                        failed: 0,
                        trimmed: 1,
                        raw_tokens: 8_000,
                        emitted_tokens: 120,
                    },
                ),
                (
                    Origin::Hook,
                    RunTotals {
                        runs: 34,
                        failed: 2,
                        trimmed: 0,
                        raw_tokens: 32_080,
                        emitted_tokens: 32_080,
                    },
                ),
            ],
        }
    }

    /// `--json` is a contract (SPEC §3b "Stats"): these exact bytes, whatever the text says.
    #[test]
    fn json_is_unchanged() {
        assert_eq!(
            sample(true).to_json().to_string(),
            concat!(
                r#"{"status":"active","mode":"balanced","trimmed_outputs":4,"tokens_saved":134830,"#,
                r#""cas_dereferences":1,"raw_tokens_passed_while_paused":0,"last_trimmed_id":"683db530","#,
                r#""workspace":"/home/u/code/app","memory_db":"/home/u/code/app/.agentdiet/memory.db","#,
                r#""runs":{"shim":{"runs":4,"failed":0,"trimmed":0,"raw_tokens":0,"emitted_tokens":0},"#,
                r#""mcp":{"runs":1,"failed":0,"trimmed":1,"raw_tokens":8000,"emitted_tokens":120},"#,
                r#""hook":{"runs":34,"failed":2,"trimmed":0,"raw_tokens":32080,"emitted_tokens":32080}}}"#
            )
        );
    }

    #[test]
    fn text_uses_plain_words() {
        let text = sample(true).render();
        assert_eq!(
            text,
            [
                "AgentDiet is active · Balanced mode",
                "",
                "All projects (estimated)",
                "  trimmed outputs      4",
                "  tokens saved         ≈134,830",
                "  re-opened by agents  1 re-open for 4 trimmed outputs (25%)",
                "  while paused         ≈0 tokens passed untrimmed (still scrubbed)",
                "  last trimmed         #683db530 · agentdiet show last",
                "",
                "This workspace (~/code/app)",
                "  via                 runs  failed  trimmed  est. tokens before → after",
                "  shims                  4       0        0  0 → 0",
                "  MCP (diet_exec)        1       0        1  8,000 → 120",
                "  Claude Code hooks     34       2        0  32,080 → 32,080",
                "",
            ]
            .join("\n")
        );
        let empty = sample(false).render();
        assert!(
            empty.ends_with(
                "This workspace (~/code/app)\n  Nothing recorded in this workspace yet.\n"
            )
        );
    }

    #[test]
    fn numbers_are_grouped() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }
}
