//! `agentdiet stats`: the §2.3 counters from `state.json` plus the workspace's run log totals per
//! origin (SPEC §3b "Stats"). Read-only: memory.db is never created just to be read.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::Paths;
use crate::memory::{MemoryDb, MemoryError, Origin, RunTotals};
use crate::state::{self, State, Status};

#[derive(Debug, Clone)]
pub struct Stats {
    pub state: State,
    pub workspace: PathBuf,
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

    pub fn render(&self) -> String {
        let s = &self.state;
        let status = match s.status {
            Status::Active => "active",
            Status::Paused => "paused",
        };
        let rate = s
            .dereference_rate()
            .map_or_else(|| "n/a".to_string(), |r| format!("{:.0}%", r * 100.0));
        let mut out = format!(
            "AgentDiet: {status}, {} mode\n\n\
             All projects (state.json):\n\
             \x20 trimmed outputs     {}\n\
             \x20 tokens saved        {}\n\
             \x20 CAS dereferences    {} (rate {rate})\n\
             \x20 passed while paused {} tokens\n\
             \x20 last trimmed        {}\n\n",
            s.mode.label(),
            grouped(s.trimmed_outputs),
            grouped(s.tokens_saved),
            grouped(s.cas_dereferences),
            grouped(s.raw_tokens_passed_while_paused),
            s.last_trimmed_id
                .as_deref()
                .map_or_else(|| "none".to_string(), |id| format!("#{id}")),
        );
        out.push_str(&format!(
            "Run log, workspace {}:\n",
            self.workspace.display()
        ));
        if self.memory_db.is_none() {
            out.push_str("  no memory.db yet: nothing has been recorded here.\n");
            return out;
        }
        out.push_str("  origin    runs  failed  trimmed  tokens in -> out\n");
        for (origin, t) in &self.runs {
            out.push_str(&format!(
                "  {:<6} {:>7} {:>7} {:>8}  {} -> {}\n",
                origin.as_str(),
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

    #[test]
    fn numbers_are_grouped() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }
}
