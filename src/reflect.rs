//! `agentdiet reflect` and the MCP tool `diet_reflect`: turn the run log into `EXPERIENCE.md`,
//! with no model calls (SPEC §3b "Reflect").
//!
//! Three deterministic steps over the workspace's run log (§3 Tier 3, "Run log"):
//!
//! - **Propose.** Find friction: a failed command followed shortly by a different command of the same
//!   family that worked (a *recipe*); a command or read that keeps producing over 1,000 estimated
//!   tokens (*bloat*); a search or read that found nothing followed by one that did (a *miss*).
//! - **Probe.** Keep only lessons with evidence: at least 500 tokens wasted, or seen at least twice.
//!   One lesson per failing command, bloated title or missed path, so repeats add up instead of
//!   duplicating.
//! - **Compose.** Render the strongest lessons, by impact and then recency, into a marker-delimited
//!   block of `EXPERIENCE.md` at the workspace root, at most [`MAX_BYTES`] for the whole file. Each
//!   lesson cites a live Vault ID or the UTC time of its newest witness run.
//!
//! The same run log and Vault give the same bytes. Everything rendered comes from the run log,
//! which holds scrubbed text only (P1).
//!
//! pattern ref: harness2/harness2/core/library.py (behavior only): an immutable base harness kept
//! apart from a per-domain `EXPERIENCE.md` of lessons that each carry a cue, the lesson and its
//! evidence, and are admitted only when all three exist.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::Paths;
use crate::basename;
use crate::cas::Cas;
use crate::fsutil;
use crate::memory::{MemoryDb, MemoryError, RunRow};
use crate::runlog::BLOAT_TOKENS;

/// `EXPERIENCE.md` never exceeds this many bytes (unless its hand-written part alone does).
pub const MAX_BYTES: usize = 2048;
/// How many of the newest runs are analysed.
pub const WINDOW: usize = 2000;
/// A correction must come within this many runs of the same session ...
pub const LOOKAHEAD: usize = 5;
/// ... and within this long.
pub const MAX_GAP_MS: i64 = 30 * 60 * 1000;
/// Probe gate: a lesson is kept when it wasted at least this many estimated tokens ...
pub const MIN_TOKENS: u64 = 500;
/// ... or was seen at least this many times.
pub const MIN_OCCURRENCES: u32 = 2;
/// A shim's run and the PostToolUse of the same Bash call within this long are one run.
const SAME_CALL_MS: i64 = 120 * 1000;
/// Titles are shown cut to this many characters.
const TITLE_CHARS: usize = 72;

pub const BEGIN: &str = "<!-- agentdiet:experience:begin -->";
pub const END: &str = "<!-- agentdiet:experience:end -->";

#[derive(Debug, thiserror::Error)]
pub enum ReflectError {
    #[error("{0}")]
    Memory(#[from] MemoryError),
    #[error("could not write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("could not read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: io::Error,
    },
}

/// The three friction patterns, in the order their sections are rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LessonKind {
    /// A failing command and the one that worked next.
    Recipe,
    /// Output that keeps costing over [`BLOAT_TOKENS`].
    Bloat,
    /// A search or read that found nothing, and what found it.
    Miss,
}

impl LessonKind {
    fn heading(self) -> &'static str {
        match self {
            LessonKind::Recipe => "## Command recipes",
            LessonKind::Bloat => "## Context bloat",
            LessonKind::Miss => "## Search and path misses",
        }
    }
}

/// One candidate lesson, aggregated over every occurrence of its cue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lesson {
    pub kind: LessonKind,
    /// What went wrong: the failing command, the bloated title, the missed path.
    pub cue: String,
    /// What worked instead (the newest correction), when there is one.
    pub fix: Option<String>,
    /// The newest failing exit code (recipes).
    pub exit_code: Option<i64>,
    /// Estimated tokens spent on the friction: failed output, bloated output, missed results.
    pub tokens: u64,
    /// Of those, what reached the agent after AgentDiet (bloat; equal to `tokens` when untrimmed).
    pub emitted: u64,
    pub occurrences: u32,
    /// Time of the newest witness run.
    pub last_ms: i64,
    /// The newest witness's Vault entry, when it has one.
    pub witness_sha: Option<String>,
    /// The cue is a command line (not a path or pattern).
    pub shell: bool,
}

impl Lesson {
    /// Impact: tokens wasted plus a fixed weight per occurrence.
    pub fn impact(&self) -> u64 {
        self.tokens + 250 * u64::from(self.occurrences)
    }
}

/// What a reflect run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub path: PathBuf,
    pub runs: usize,
    pub proposed: usize,
    pub promoted: usize,
    pub rendered: usize,
    pub bytes: usize,
    /// The file's bytes changed (always `false` for a dry run).
    pub changed: bool,
    pub text: String,
}

impl Report {
    pub fn summary(&self) -> String {
        format!(
            "{} run(s) analysed; {} lesson(s) proposed, {} passed the evidence gate, {} rendered; \
             {} ({} bytes{}).",
            self.runs,
            self.proposed,
            self.promoted,
            self.rendered,
            self.path.display(),
            self.bytes,
            if self.changed {
                ", updated"
            } else {
                ", unchanged"
            }
        )
    }
}

// ---- propose ------------------------------------------------------------------------------------

/// Runs grouped by session (runs without one by origin), each group in log order, with the shim
/// run and the PostToolUse of one Bash call collapsed into one.
fn sequences(runs: &[RunRow]) -> Vec<Vec<&RunRow>> {
    let mut groups: BTreeMap<String, Vec<&RunRow>> = BTreeMap::new();
    for run in runs {
        let key = match &run.session {
            Some(s) => format!("s:{s}"),
            None => format!("o:{}", run.origin),
        };
        let group = groups.entry(key).or_default();
        let same_call = group.last().is_some_and(|prev| {
            prev.origin == "shim"
                && run.origin == "hook"
                && prev.title == run.title
                && run.created_ms - prev.created_ms <= SAME_CALL_MS
        });
        if !same_call {
            group.push(run);
        }
    }
    groups.into_values().collect()
}

/// The words of a command title after its executable.
fn args_of(title: &str) -> Vec<&str> {
    title.split_whitespace().skip(1).collect()
}

/// Edit distance, for telling a typo (`biuld`/`build`) from a different command.
fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// Whether `fixed` reads as a repair of `failed` rather than an unrelated command of the same
/// family: the words that differ are flags, or few and close (a typo, a renamed script).
fn is_repair(failed: &str, fixed: &str) -> bool {
    let (f, s) = (args_of(failed), args_of(fixed));
    // The words only one side has, each marked when it is a flag or the value right after a flag
    // only that side has (`--region eu-central-1`).
    let only = |mine: &[&str], other: &[&str]| -> Vec<(String, bool)> {
        let mut out = Vec::new();
        let mut after_new_flag = false;
        for w in mine {
            let new = !other.contains(w);
            let flag = w.starts_with('-');
            if new {
                out.push((w.to_string(), flag || after_new_flag));
            }
            after_new_flag = new && flag && !w.contains('=');
        }
        out
    };
    let (only_f, only_s) = (only(&f, &s), only(&s, &f));
    if only_f.is_empty() && only_s.is_empty() {
        return false; // the same words: a flaky retry, not a repair
    }
    if only_f.len() + only_s.len() > 4 {
        return false;
    }
    // Pair the differing plain words: each must be a near miss of its counterpart.
    let plain = |only: &[(String, bool)]| -> Vec<String> {
        only.iter()
            .filter(|(_, flag)| !flag)
            .map(|(w, _)| w.clone())
            .collect()
    };
    let (plain_f, plain_s) = (plain(&only_f), plain(&only_s));
    plain_f.len() == plain_s.len()
        && plain_f.iter().zip(&plain_s).all(|(a, b)| {
            let longest = a.chars().count().max(b.chars().count()).max(1);
            levenshtein(a, b) * 5 <= longest * 2
        })
}

fn tokens_of(run: &RunRow) -> u64 {
    u64::try_from(run.raw_tokens).unwrap_or(0)
}

/// Every candidate lesson in `runs` (oldest first), aggregated per cue. Not yet gated.
pub fn propose(runs: &[RunRow]) -> Vec<Lesson> {
    let mut lessons: HashMap<(LessonKind, String), Lesson> = HashMap::new();
    let mut add = |kind: LessonKind,
                   cue: &str,
                   fix: Option<&str>,
                   witness: &RunRow,
                   tokens: u64,
                   exit: Option<i64>| {
        let lesson = lessons
            .entry((kind, cue.to_string()))
            .or_insert_with(|| Lesson {
                kind,
                cue: cue.to_string(),
                fix: None,
                exit_code: None,
                tokens: 0,
                emitted: 0,
                occurrences: 0,
                last_ms: i64::MIN,
                witness_sha: None,
                shell: witness.kind == "shell",
            });
        lesson.tokens += tokens;
        lesson.emitted += u64::try_from(witness.emitted_tokens)
            .unwrap_or(0)
            .min(tokens);
        lesson.occurrences += 1;
        if witness.created_ms >= lesson.last_ms {
            lesson.last_ms = witness.created_ms;
            lesson.fix = fix.map(str::to_string).or(lesson.fix.take());
            lesson.exit_code = exit.or(lesson.exit_code);
            lesson.witness_sha = witness.cas_sha.clone().or(lesson.witness_sha.take());
        }
    };

    for seq in sequences(runs) {
        for (i, run) in seq.iter().enumerate() {
            let next = seq
                .iter()
                .skip(i + 1)
                .take(LOOKAHEAD)
                .take_while(|n| n.created_ms - run.created_ms <= MAX_GAP_MS);
            match (run.kind.as_str(), run.outcome.as_str()) {
                ("shell", "fail") => {
                    let fixed = next
                        .filter(|n| n.kind == "shell" && n.family == run.family)
                        .find(|n| n.outcome == "ok" && is_repair(&run.title, &n.title));
                    if let Some(fixed) = fixed {
                        add(
                            LessonKind::Recipe,
                            &run.title,
                            Some(&fixed.title),
                            fixed,
                            tokens_of(run),
                            run.exit_code,
                        );
                    }
                }
                ("search" | "read", "miss") => {
                    let found = next
                        .filter(|n| matches!(n.kind.as_str(), "search" | "read"))
                        .find(|n| n.outcome == "ok" && n.title != run.title);
                    add(
                        LessonKind::Miss,
                        &run.title,
                        found.map(|f| f.title.as_str()),
                        found.copied().unwrap_or(*run),
                        tokens_of(run),
                        None,
                    );
                }
                _ => {}
            }
            // A failure's output is counted by its recipe, not as bloat.
            if tokens_of(run) >= BLOAT_TOKENS && run.outcome == "ok" {
                add(
                    LessonKind::Bloat,
                    &run.title,
                    None,
                    run,
                    tokens_of(run),
                    None,
                );
            }
        }
    }
    lessons.into_values().collect()
}

// ---- probe --------------------------------------------------------------------------------------

/// The evidence gate, then the ranking: impact, then recency (newest first), then the cue, so the
/// order is total and the output deterministic. Bloat needs repetition by definition; a miss
/// without a correction is kept only if it repeats.
pub fn probe(lessons: Vec<Lesson>) -> Vec<Lesson> {
    let mut kept: Vec<Lesson> = lessons
        .into_iter()
        .filter(|l| match l.kind {
            LessonKind::Bloat => l.occurrences >= MIN_OCCURRENCES,
            LessonKind::Recipe | LessonKind::Miss => {
                l.tokens >= MIN_TOKENS || l.occurrences >= MIN_OCCURRENCES
            }
        })
        .collect();
    kept.sort_by(|a, b| {
        b.impact()
            .cmp(&a.impact())
            .then(b.last_ms.cmp(&a.last_ms))
            .then(a.kind.cmp(&b.kind))
            .then(a.cue.cmp(&b.cue))
    });
    kept
}

// ---- compose ------------------------------------------------------------------------------------

/// A title as inline code: whitespace collapsed, backticks replaced, cut to [`TITLE_CHARS`]. A
/// command's executable path is shortened to its base name (`/x/bin/npm ls` is `npm ls`).
fn code(title: &str, command: bool) -> String {
    let mut words: Vec<String> = title
        .split_whitespace()
        .map(|w| w.replace('`', "'"))
        .collect();
    if command && let Some(first) = words.first_mut() {
        *first = basename(first).to_string();
    }
    let joined = words.join(" ");
    let cut: String = joined.chars().take(TITLE_CHARS).collect();
    let cut = if cut.len() < joined.len() {
        format!("{cut}…")
    } else {
        cut
    };
    format!("`{cut}`")
}

/// `2026-10-19 01:02Z` from milliseconds since the epoch (UTC; proleptic Gregorian calendar).
pub fn utc_minute(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's days-from-civil, inverted.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60
    )
}

fn approx_tokens(n: u64) -> String {
    if n >= 10_000 {
        format!("~{}k", n / 1000)
    } else if n >= 1000 {
        format!("~{}.{}k", n / 1000, (n % 1000) / 100)
    } else {
        format!("~{n}")
    }
}

/// One rendered lesson line. `ids` maps a Vault sha to its live short ID.
fn render_line(lesson: &Lesson, ids: &HashMap<String, String>) -> String {
    let times = if lesson.occurrences > 1 {
        format!(", {}×", lesson.occurrences)
    } else {
        String::new()
    };
    let evidence = lesson
        .witness_sha
        .as_deref()
        .and_then(|sha| ids.get(sha))
        .map_or_else(|| utc_minute(lesson.last_ms), |id| format!("#{id}"));
    let body = match lesson.kind {
        LessonKind::Recipe => {
            let exit = lesson
                .exit_code
                .map_or_else(|| "failed".to_string(), |c| format!("exit {c}"));
            let fix = lesson.fix.as_deref().unwrap_or_default();
            let mut line = format!(
                "{} worked after {} ({exit}{times})",
                code(fix, true),
                code(&lesson.cue, true)
            );
            // A cut title can hide the very words that changed: name them.
            if [fix, lesson.cue.as_str()].iter().any(|t| {
                t.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .count()
                    > TITLE_CHARS
            }) {
                let (cue_words, fix_words) = (args_of(&lesson.cue), args_of(fix));
                let added: Vec<&str> = fix_words
                    .iter()
                    .filter(|w| !cue_words.contains(w))
                    .copied()
                    .collect();
                let removed: Vec<&str> = cue_words
                    .iter()
                    .filter(|w| !fix_words.contains(w))
                    .copied()
                    .collect();
                if !added.is_empty() {
                    line.push_str(&format!("; added {}", code(&added.join(" "), false)));
                }
                if !removed.is_empty() {
                    line.push_str(&format!("; dropped {}", code(&removed.join(" "), false)));
                }
            }
            line
        }
        LessonKind::Bloat => {
            let per_run = |n: u64| approx_tokens(n / u64::from(lesson.occurrences.max(1)));
            let trimmed = if lesson.emitted < lesson.tokens {
                format!(" ({} after AgentDiet)", per_run(lesson.emitted))
            } else {
                String::new()
            };
            let advice = if lesson.shell {
                "narrow it, or run it through `diet_exec`"
            } else {
                "read a line range or search it instead"
            };
            format!(
                "{} emits {} tokens per run{trimmed}{times}: {advice}",
                code(&lesson.cue, lesson.shell),
                per_run(lesson.tokens)
            )
        }
        LessonKind::Miss => match &lesson.fix {
            Some(fix) => format!(
                "{} found nothing{times}; {} did",
                code(&lesson.cue, false),
                code(fix, false)
            ),
            None => format!("{} found nothing{times}", code(&lesson.cue, false)),
        },
    };
    format!("- {body} — {evidence}")
}

const PREAMBLE: &str = "# Experience\n\nRepo-specific command recipes and pitfalls that \
`agentdiet reflect` found in this workspace's run log (no LLM; regenerated on every run; \
evidence: a Vault ID for `agentdiet show`, or the UTC time of the newest witness).\n";

const EMPTY: &str = "_Nothing met the evidence bar yet (500+ tokens wasted, or seen twice)._\n";

/// The generated block for `lessons` (already ranked), within `budget` bytes. Lessons are added in
/// rank order while the whole block fits; one that does not fit is skipped (impact pruning), and
/// the rendered ones keep their sections in a fixed order. Returns the block and how many lessons
/// it holds.
pub fn render_block(
    lessons: &[Lesson],
    ids: &HashMap<String, String>,
    budget: usize,
) -> (String, usize) {
    let assemble = |chosen: &[(LessonKind, String)]| -> String {
        let mut out = format!("{BEGIN}\n{PREAMBLE}");
        if chosen.is_empty() {
            out.push('\n');
            out.push_str(EMPTY);
        }
        for kind in [LessonKind::Recipe, LessonKind::Bloat, LessonKind::Miss] {
            let lines: Vec<&String> = chosen
                .iter()
                .filter(|(k, _)| *k == kind)
                .map(|(_, l)| l)
                .collect();
            if lines.is_empty() {
                continue;
            }
            out.push('\n');
            out.push_str(kind.heading());
            out.push('\n');
            for line in lines {
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str(END);
        out.push('\n');
        out
    };
    let mut chosen: Vec<(LessonKind, String)> = Vec::new();
    for lesson in lessons {
        let mut trial = chosen.clone();
        trial.push((lesson.kind, render_line(lesson, ids)));
        if assemble(&trial).len() <= budget {
            chosen = trial;
        }
    }
    let n = chosen.len();
    (assemble(&chosen), n)
}

/// The new file text: `existing` with its generated block replaced (or the block appended, or the
/// block alone for a new file), the block sized so the whole file stays within [`MAX_BYTES`].
pub fn compose(
    lessons: &[Lesson],
    ids: &HashMap<String, String>,
    existing: Option<&str>,
) -> (String, usize) {
    let (before, after) = match existing {
        None => (String::new(), String::new()),
        Some(text) => match (text.find(BEGIN), text.find(END)) {
            (Some(b), Some(e)) if b < e => {
                let rest = &text[e + END.len()..];
                let rest = rest.strip_prefix('\n').unwrap_or(rest);
                (text[..b].to_string(), rest.to_string())
            }
            _ if text.trim().is_empty() => (String::new(), String::new()),
            _ => {
                let mut kept = text.to_string();
                if !kept.ends_with('\n') {
                    kept.push('\n');
                }
                kept.push('\n');
                (kept, String::new())
            }
        },
    };
    let budget = MAX_BYTES.saturating_sub(before.len() + after.len());
    let (block, n) = render_block(lessons, ids, budget);
    (format!("{before}{block}{after}"), n)
}

// ---- the command --------------------------------------------------------------------------------

/// Analyse the workspace's run log and (unless `dry_run`) write `EXPERIENCE.md`. memory.db is never
/// created just to be read: with none, there are no runs and the file says so.
pub fn reflect(paths: &Paths, dry_run: bool, now: SystemTime) -> Result<Report, ReflectError> {
    let db_path = paths.memory_db();
    let runs = if db_path.is_file() {
        MemoryDb::open(&db_path)?.runs(WINDOW)?
    } else {
        Vec::new()
    };
    let proposed = propose(&runs);
    let n_proposed = proposed.len();
    let promoted = probe(proposed);
    let shas: Vec<&str> = promoted
        .iter()
        .filter_map(|l| l.witness_sha.as_deref())
        .collect();
    let ids = if shas.is_empty() {
        HashMap::new()
    } else {
        Cas::new(paths.cas_dir()).live_ids(&shas, now)
    };

    let path = paths.experience_file();
    let read_err = |source| ReflectError::Read {
        path: path.display().to_string(),
        source,
    };
    let existing = fsutil::read_optional_regular(&path).map_err(read_err)?;
    let existing_text = existing.as_deref().map(String::from_utf8_lossy);
    let (text, rendered) = compose(&promoted, &ids, existing_text.as_deref());
    let changed = existing.as_deref() != Some(text.as_bytes());
    if changed && !dry_run {
        fsutil::atomic_write(&path, text.as_bytes()).map_err(|source| ReflectError::Write {
            path: path.display().to_string(),
            source,
        })?;
    }
    Ok(Report {
        path,
        runs: runs.len(),
        proposed: n_proposed,
        promoted: promoted.len(),
        rendered,
        bytes: text.len(),
        changed: changed && !dry_run,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: i64, kind: &str, family: &str, title: &str, outcome: &str, tokens: i64) -> RunRow {
        RunRow {
            id,
            created_ms: 1_760_835_720_000 + id * 1000,
            origin: "hook".into(),
            kind: kind.into(),
            family: family.into(),
            title: title.into(),
            outcome: outcome.into(),
            exit_code: match outcome {
                "fail" => Some(1),
                "ok" if kind == "shell" => Some(0),
                _ => None,
            },
            raw_tokens: tokens,
            emitted_tokens: tokens,
            cas_sha: None,
            session: Some("s1".into()),
        }
    }

    #[test]
    fn utc_minutes_are_exact() {
        assert_eq!(utc_minute(0), "1970-01-01 00:00Z");
        assert_eq!(utc_minute(1_760_835_720_000), "2025-10-19 01:02Z");
        assert_eq!(utc_minute(951_782_400_000), "2000-02-29 00:00Z");
    }

    #[test]
    fn repairs_are_flag_changes_or_near_misses_not_unrelated_commands() {
        assert!(is_repair("npm run biuld", "npm run build"));
        assert!(is_repair("cargo test --lib --frozen", "cargo test --lib"));
        assert!(is_repair("npx playwright tests", "npx playwright test"));
        assert!(!is_repair("npm run build", "npm run build"), "a retry");
        assert!(!is_repair("git commit -m wip", "git push origin main"));
        assert!(!is_repair("npm run lint", "npm run typecheck"));
        assert!(is_repair(
            "aws s3 cp a b",
            "aws s3 cp a b --region eu-central-1"
        ));
        assert!(!is_repair(
            "aws s3 cp a b",
            "aws s3 cp a b --profile prod --region eu-central-1 --x"
        ));
    }

    #[test]
    fn the_three_patterns_are_found_and_gated() {
        let runs = vec![
            run(1, "shell", "npm", "npm run biuld", "fail", 40),
            run(2, "shell", "npm", "npm run build", "ok", 300),
            run(3, "shell", "npm", "npm run biuld", "fail", 40),
            run(4, "shell", "npm", "npm run build", "ok", 300),
            run(5, "shell", "npm", "npm ls --all", "ok", 24_000),
            run(6, "search", "Glob", "src/**/*.tsx", "miss", 5),
            run(7, "search", "Glob", "app/**/*.tsx", "ok", 20),
            run(8, "shell", "npm", "npm ls --all", "ok", 24_000),
            run(9, "search", "Glob", "pages/**", "miss", 5),
            run(10, "shell", "git", "git push", "fail", 30),
        ];
        let lessons = probe(propose(&runs));
        let cues: Vec<(LessonKind, &str)> =
            lessons.iter().map(|l| (l.kind, l.cue.as_str())).collect();
        assert_eq!(
            cues,
            vec![
                (LessonKind::Bloat, "npm ls --all"),
                (LessonKind::Recipe, "npm run biuld"),
            ],
            "a single miss and an uncorrected failure do not pass the gate"
        );
        let recipe = &lessons[1];
        assert_eq!(recipe.fix.as_deref(), Some("npm run build"));
        assert_eq!(recipe.occurrences, 2);
    }

    #[test]
    fn a_shim_run_and_its_hook_echo_count_once() {
        let mut shim = run(1, "shell", "npm", "npm ls", "ok", 5000);
        shim.origin = "shim".into();
        let hook = run(2, "shell", "npm", "npm ls", "ok", 5000);
        assert_eq!(
            probe(propose(&[shim, hook])),
            Vec::new(),
            "one occurrence, not two"
        );
    }

    #[test]
    fn rendering_is_bounded_ordered_and_keeps_user_text() {
        let lessons = probe(propose(&[
            run(1, "shell", "npm", "npm run biuld", "fail", 900),
            run(2, "shell", "npm", "npm run build", "ok", 10),
        ]));
        let ids = HashMap::new();
        let (text, n) = compose(&lessons, &ids, None);
        assert_eq!(n, 1);
        assert!(text.starts_with(BEGIN) && text.ends_with(&format!("{END}\n")));
        assert!(
            text.contains(
                "- `npm run build` worked after `npm run biuld` (exit 1) — 2025-10-19 01:02Z"
            ),
            "{text}"
        );

        let mine = "# Notes\n\nKeep this.\n";
        let (with_user, _) = compose(&lessons, &ids, Some(mine));
        assert!(with_user.starts_with("# Notes\n\nKeep this.\n\n<!-- agentdiet"));
        let (again, _) = compose(&lessons, &ids, Some(&with_user));
        assert_eq!(again, with_user, "regenerating replaces only the block");
    }

    mod prop {
        use proptest::prelude::*;

        use super::super::*;
        use super::run;

        fn arb_run() -> impl Strategy<Value = (u8, u8, String, u8, i64)> {
            (0u8..3, 0u8..3, "[a-z/*. -]{0,90}", 0u8..3, 0i64..40_000)
        }

        fn build(spec: Vec<(u8, u8, String, u8, i64)>) -> Vec<RunRow> {
            spec.into_iter()
                .enumerate()
                .map(|(i, (k, f, title, o, tokens))| {
                    let kind = ["shell", "search", "read"][usize::from(k)];
                    let family = ["npm", "git", "Glob"][usize::from(f)];
                    let outcome = ["ok", "fail", "miss"][usize::from(o)];
                    run(
                        i as i64 + 1,
                        kind,
                        family,
                        &format!("{family} {title}"),
                        outcome,
                        tokens,
                    )
                })
                .collect()
        }

        proptest! {
            #[test]
            fn experience_md_is_always_within_2_kib_and_deterministic(
                spec in prop::collection::vec(arb_run(), 0..120),
                user in "[ -~\n]{0,600}",
            ) {
                let runs = build(spec);
                let ids = HashMap::new();
                let (a, _) = compose(&probe(propose(&runs)), &ids, None);
                prop_assert!(a.len() <= MAX_BYTES, "{} bytes", a.len());
                let (b, _) = compose(&probe(propose(&runs)), &ids, None);
                prop_assert_eq!(&a, &b);
                let (with_user, _) = compose(&probe(propose(&runs)), &ids, Some(&user));
                prop_assert!(with_user.len() <= MAX_BYTES, "{} bytes", with_user.len());
            }
        }
    }
}
