//! The Diet Engine's shared pipeline (SPEC §3 Tier 2).
//!
//! A library called by the hook router (`hook.rs`, PostToolUse) and the shims' agent path
//! (`shim.rs`). It takes the captured bytes of one tool output plus structured metadata ([`Input`])
//! and returns the bytes to emit ([`Outcome`]). It never prints and never runs a command.
//!
//! ```text
//! classify -> scrub -> candidate -> pre-check            (pure: [`transform`])
//!          -> durable Vault write -> real-ID final check (persisting: [`Engine::finish`])
//!          -> emit -> best-effort state + index updates
//! ```
//!
//! Precedence when rules conflict follows SPEC P1-P7: nothing unscrubbed reaches disk, a failure
//! yields the best safe output, redaction happens in every mode, identity is exact bytes, a
//! replacement must be strictly smaller (breadcrumb included), it is persisted first, and bypasses
//! skip transforms but not scrubbing.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::SystemTime;

use crate::Paths;
use crate::cas::{Cas, CasError};
use crate::memory::{self, MemoryDb};
use crate::scrubber::{Scrubbed, scrub};
use crate::state::{self, Mode};

/// Output above this many bytes is scrubbed and emitted, never transformed or stored (P3).
pub const OVERSIZE_BYTES: usize = 8 * 1024 * 1024;
/// A NUL byte within this prefix marks output as binary (P3).
pub const BINARY_SNIFF_BYTES: usize = 8 * 1024;
/// A paused engine appends the notice to output of more than this many estimated tokens.
pub const PAUSED_NOTICE_TOKENS: usize = 3_000;
/// The breadcrumb summary never exceeds this many bytes.
pub const MAX_SUMMARY_BYTES: usize = 160;
/// A diagnostic note is at most this many bytes.
pub const MAX_NOTE_BYTES: usize = 200;
/// Files of at most this many lines are never folded.
pub const FOLD_MIN_FILE_LINES: usize = 300;

/// The paused notice (SPEC §2.3), without its line break.
pub const PAUSED_NOTICE: &str =
    "⏸ AgentDiet is paused. Run 'agentdiet resume' or type '@diet resume' to re-enable.";

/// The only text the engine reports about a panic. The payload is never read.
pub const PANIC_NOTE: &str = "agentdiet: internal error; output passed through";

// ---- inputs ---------------------------------------------------------------------------------

/// How the command that produced shell output ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    Code(i32),
    /// Terminated by this signal.
    Signal(i32),
    Unknown,
}

impl ExitStatus {
    pub fn describe(self) -> String {
        match self {
            ExitStatus::Code(c) => format!("exit {c}"),
            ExitStatus::Signal(s) => format!("signal {s}"),
            ExitStatus::Unknown => "exit unknown".to_string(),
        }
    }
}

/// Whether a file read covers the whole file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extent {
    /// The bytes are the file's own bytes from the first to the last. Only these may be folded.
    Complete,
    /// A slice (offset/limit) of the file: its line numbers are not the file's, so it is never
    /// folded and never presented as a complete file.
    Ranged,
}

/// Where the bytes came from. The engine dispatches on this structured data, never on substrings
/// of shell text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Output of a command that has already run and whose output goes to the model.
    Shell { argv: Vec<String>, exit: ExitStatus },
    /// File contents read for the model.
    Read { path: String, extent: Extent },
    /// A JSON document the caller marks as model-facing (an MCP tool result, say). The only
    /// source that may become TOON.
    ModelJson,
}

#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    pub bytes: &'a [u8],
    pub source: &'a Source,
}

/// Why trimming is skipped for this call (SPEC P7). Scrubbing still applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Bypass {
    #[default]
    None,
    /// A prompt said `raw`, `full` or `unfiltered`.
    RawNextTurn,
    /// The same command ran twice in a row.
    RepeatedCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub mode: Mode,
    pub paused: bool,
    pub bypass: Bypass,
}

impl Settings {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            paused: false,
            bypass: Bypass::None,
        }
    }
}

// ---- modes ----------------------------------------------------------------------------------

/// The mode table of SPEC §3 Tier 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeParams {
    /// Estimated tokens below which trimming and TOON are not attempted.
    pub min_tokens: usize,
    /// Required saving, in percent, in bytes and in estimated tokens, breadcrumb included.
    pub saving_pct: u64,
    /// Lines kept on each side of an error/warning line.
    pub context_lines: usize,
    /// Trailing lines always kept.
    pub last_lines: usize,
    /// Fewest body lines for a function to be folded.
    pub fold_min_body_lines: usize,
}

impl ModeParams {
    pub fn for_mode(mode: Mode) -> Self {
        match mode {
            Mode::Light => Self {
                min_tokens: 1500,
                saving_pct: 40,
                context_lines: 5,
                last_lines: 20,
                fold_min_body_lines: 20,
            },
            Mode::Balanced => Self {
                min_tokens: 600,
                saving_pct: 20,
                context_lines: 3,
                last_lines: 10,
                fold_min_body_lines: 8,
            },
            Mode::UltraLean => Self {
                min_tokens: 250,
                saving_pct: 10,
                context_lines: 1,
                last_lines: 5,
                fold_min_body_lines: 3,
            },
        }
    }
}

// ---- token estimate ---------------------------------------------------------------------------

/// Deterministic estimate of tokens (SPEC "Estimated tokens"): not any provider's tokenizer.
///
/// A run of ASCII letters, digits and `_` counts `ceil(len / 4)`; a run of `\n`/`\r` with at least
/// one `\n` counts 1; spaces, tabs and lone `\r` runs count 0; every other scalar value and every
/// invalid byte counts 1.
pub fn estimate_tokens(bytes: &[u8]) -> usize {
    let mut total = 0;
    for chunk in bytes.utf8_chunks() {
        total += estimate_valid(chunk.valid());
        total += chunk.invalid().len();
    }
    total
}

fn estimate_valid(text: &str) -> usize {
    let mut total = 0;
    let mut word = 0usize; // length of the current word run
    let mut breaks = false; // inside a newline run
    let mut has_lf = false; // that run holds a `\n`
    for c in text.chars() {
        let is_word = c.is_ascii_alphanumeric() || c == '_';
        let is_break = c == '\n' || c == '\r';
        if !is_word && word > 0 {
            total += word.div_ceil(4);
            word = 0;
        }
        if !is_break && breaks {
            total += usize::from(has_lf);
            breaks = false;
            has_lf = false;
        }
        if is_word {
            word += 1;
        } else if is_break {
            breaks = true;
            has_lf |= c == '\n';
        } else if c != ' ' && c != '\t' {
            total += 1;
        }
    }
    total += word.div_ceil(4);
    if breaks {
        total += usize::from(has_lf);
    }
    total
}

// ---- candidates -------------------------------------------------------------------------------

/// What kind of replacement was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    Compressed,
    Toon,
    Folded,
}

/// How a fold marker is written around the same text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldStyle {
    /// `{ /* folded L1-9: … */ }`
    Brace,
    /// `...  # folded L1-9: …`
    Python,
}

impl FoldStyle {
    /// The marker with the real ID; line numbers are 1-based and inclusive, in the Vault copy.
    pub fn marker(self, first: usize, last: usize, id: &str) -> String {
        let text =
            format!("folded L{first}-{last}: agentdiet show '#{id}' --lines {first}..{last}");
        match self {
            FoldStyle::Brace => format!("{{ /* {text} */ }}"),
            FoldStyle::Python => format!("...  # {text}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Text(Vec<u8>),
    Fold {
        first_line: usize,
        last_line: usize,
        style: FoldStyle,
    },
}

/// The shape of a replacement, so it can be rendered once the real ID is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// `body`, a line break if needed, then the breadcrumb line.
    Trailer { body: Vec<u8> },
    /// Text with fold markers; the markers carry the ID, so no trailing breadcrumb.
    Folded { pieces: Vec<Piece> },
}

/// A proposed replacement for the scrubbed original, before the ID exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: Transform,
    pub layout: Layout,
    /// What was trimmed, most important first. Only as many as fit in
    /// [`MAX_SUMMARY_BYTES`] are shown.
    pub details: Vec<String>,
    /// `(kept, total)` lines, when the transform works on whole lines.
    pub lines: Option<(usize, usize)>,
}

/// Everything a transform may look at, assembled by the engine.
#[derive(Debug, Clone, Copy)]
pub struct Selection<'a> {
    pub input: &'a Input<'a>,
    /// The scrubbed original: what every transform works on and every comparison is made against.
    pub scrubbed: &'a [u8],
    pub params: ModeParams,
    pub mode: Mode,
}

/// Production candidate selection: dispatch on the source.
fn select_candidate(sel: &Selection<'_>) -> Option<Candidate> {
    match sel.input.source {
        Source::Shell { argv, exit } => {
            crate::compress::compress(argv, *exit, sel.scrubbed, sel.mode)
        }
        Source::ModelJson => crate::toon::encode(sel.scrubbed),
        Source::Read { path, extent } => {
            crate::folding::fold(path, *extent, sel.scrubbed, sel.mode)
        }
    }
}

/// Whether the mode's minimum-token rule applies to this source (folding has its own rules).
fn needs_token_minimum(source: &Source) -> bool {
    !matches!(source, Source::Read { .. })
}

// ---- breadcrumb and framing -------------------------------------------------------------------

/// `🥗 Diet: <summary> Run: agentdiet show '#<id>'`. The ID is quoted: an unquoted `#…` starts a
/// shell comment.
pub fn breadcrumb(summary: &str, id: &str) -> String {
    format!("🥗 Diet: {summary} Run: agentdiet show '#{id}'")
}

/// The line break to use after `body`: CRLF if its last line break is CRLF, else LF.
fn line_break_for(body: &[u8]) -> &'static [u8] {
    match body.iter().rposition(|b| *b == b'\n') {
        Some(i) if i > 0 && body[i - 1] == b'\r' => b"\r\n",
        _ => b"\n",
    }
}

fn frame(body: &[u8], crumb: &str) -> Vec<u8> {
    let eol = line_break_for(body);
    let mut out = Vec::with_capacity(body.len() + crumb.len() + 4);
    out.extend_from_slice(body);
    if !body.is_empty() && !body.ends_with(b"\n") {
        out.extend_from_slice(eol);
    }
    out.extend_from_slice(crumb.as_bytes());
    out.extend_from_slice(eol);
    out
}

/// The summary: kept lines, then details while they fit, then the exit status and the redaction
/// count, which are always shown.
fn summarize(candidate: &Candidate, source: &Source, redactions: usize) -> String {
    let mut back: Vec<String> = Vec::new();
    if let Source::Shell { exit, .. } = source {
        back.push(exit.describe());
    }
    if redactions > 0 {
        back.push(format!(
            "{redactions} secret{} redacted (not recoverable)",
            if redactions == 1 { "" } else { "s" }
        ));
    }
    let mut front: Vec<String> = Vec::new();
    if let Some((kept, total)) = candidate.lines {
        front.push(format!("kept {kept}/{total} lines"));
    }
    let joined_len = |front: &[String], back: &[String]| {
        let parts = front.len() + back.len();
        front.iter().chain(back).map(String::len).sum::<usize>() + 2 * parts.saturating_sub(1)
    };
    for detail in &candidate.details {
        let mut trial = front.clone();
        trial.push(detail.clone());
        if joined_len(&trial, &back) <= MAX_SUMMARY_BYTES {
            front = trial;
        }
    }
    let summary = front.into_iter().chain(back).collect::<Vec<_>>().join("; ");
    // The summary is composed from fixed templates and sanitized names, and goes through the
    // scrubber anyway: the rule holds for text the engine itself writes. The cut is a backstop
    // for the (impossible with those templates) case of an over-long required part.
    scrub(truncate_chars(&summary, MAX_SUMMARY_BYTES).as_bytes()).to_lossy_string()
}

/// Render a candidate with `id` (the final bytes that would be emitted).
fn render(candidate: &Candidate, summary: &str, id: &str) -> Vec<u8> {
    match &candidate.layout {
        Layout::Trailer { body } => frame(body, &breadcrumb(summary, id)),
        Layout::Folded { pieces } => {
            let mut out = Vec::new();
            for piece in pieces {
                match piece {
                    Piece::Text(text) => out.extend_from_slice(text),
                    Piece::Fold {
                        first_line,
                        last_line,
                        style,
                    } => {
                        out.extend_from_slice(style.marker(*first_line, *last_line, id).as_bytes())
                    }
                }
            }
            out
        }
    }
}

/// P5: the complete final text must be strictly smaller than the scrubbed original in bytes and
/// in estimated tokens, and each must meet the mode's saving. Integer arithmetic only.
fn meets_saving(final_bytes: &[u8], original: &[u8], original_tokens: usize, pct: u64) -> bool {
    let smaller = |new: usize, old: usize| {
        new < old && (new as u128) * 100 <= (old as u128) * u128::from(100 - pct)
    };
    smaller(final_bytes.len(), original.len())
        && smaller(estimate_tokens(final_bytes), original_tokens)
}

// ---- outcomes ---------------------------------------------------------------------------------

/// What happened to the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Binary output, passed through byte for byte (not scrubbed, not stored).
    Binary,
    /// More than [`OVERSIZE_BYTES`]: scrubbed, not transformed or stored.
    Oversize,
    /// Paused: scrubbed only.
    Paused,
    /// A bypass applied: scrubbed only.
    Bypassed,
    /// Nothing was worth changing, or no candidate beat the gate: the scrubbed original (the
    /// exact input bytes if nothing was redacted).
    Unchanged,
    /// The output was replaced; the full scrubbed original is in the Vault.
    Replaced(Transform),
    /// A replacement was ready but the Vault write failed: the scrubbed original.
    StorageFailed,
    /// An internal error after scrubbing: the scrubbed original.
    Panicked,
    /// The scrubber itself failed: the raw output (the documented P2 exception).
    ScrubberFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub output: Vec<u8>,
    pub disposition: Disposition,
    /// Secret spans redacted. They are never stored and never recoverable.
    pub redactions: usize,
    /// The emitted ID (without `#`), for a replacement.
    pub id: Option<String>,
    /// Full sha256 of the stored scrubbed original, for a replacement.
    pub sha: Option<String>,
    /// Estimated tokens of the (scrubbed) output that was passed while paused, the notice not
    /// included; `None` when not paused or for binary output. The caller's telemetry adds it to
    /// `raw_tokens_passed_while_paused`; [`Engine::run`] does so itself.
    pub paused_tokens: Option<usize>,
    /// One bounded (at most [`MAX_NOTE_BYTES`]), scrubbed note for the caller to write to stderr,
    /// never to the stdout of `agentdiet mcp` or a hook's output channel.
    pub note: Option<String>,
}

impl Outcome {
    fn plain(output: Vec<u8>, disposition: Disposition, redactions: usize) -> Self {
        Self {
            output,
            disposition,
            redactions,
            id: None,
            sha: None,
            paused_tokens: None,
            note: None,
        }
    }
}

/// A diagnostic for stderr: at most [`MAX_NOTE_BYTES`] of `text`, scrubbed.
pub fn note(text: &str) -> String {
    scrub(truncate_chars(text, MAX_NOTE_BYTES).as_bytes()).to_lossy_string()
}

// ---- the pure step ----------------------------------------------------------------------------

/// What the pure step decided.
#[derive(Debug)]
pub enum Plan {
    /// Final: no persistence needed.
    Done(Outcome),
    /// A replacement worth persisting. Holds the scrubbed original, which must go to the Vault
    /// before anything is emitted.
    Replace(Box<Pending>),
}

#[derive(Debug)]
pub struct Pending {
    scrubbed: Scrubbed,
    candidate: Candidate,
    source: Source,
    params: ModeParams,
    original_tokens: usize,
}

impl Pending {
    /// The scrubbed original that will be stored.
    pub fn scrubbed(&self) -> &Scrubbed {
        &self.scrubbed
    }

    pub fn candidate(&self) -> &Candidate {
        &self.candidate
    }
}

/// `true` if a NUL occurs in the first [`BINARY_SNIFF_BYTES`] bytes.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0)
}

/// Append the paused notice on its own line.
fn with_paused_notice(mut emitted: Vec<u8>) -> Vec<u8> {
    let eol = line_break_for(&emitted);
    if !emitted.is_empty() && !emitted.ends_with(b"\n") {
        emitted.extend_from_slice(eol);
    }
    emitted.extend_from_slice(PAUSED_NOTICE.as_bytes());
    emitted.extend_from_slice(eol);
    emitted
}

/// The pure step: classify, scrub, build a candidate, pre-check. No I/O, no clock, no globals.
///
/// A panic is not caught here; [`Engine::run`] is the entry point that guards against one.
pub fn transform(input: &Input<'_>, settings: &Settings) -> Plan {
    if looks_binary(input.bytes) {
        return Plan::Done(Outcome::plain(input.bytes.to_vec(), Disposition::Binary, 0));
    }
    plan_scrubbed(input, settings, scrub(input.bytes), &select_candidate)
}

fn plan_scrubbed(
    input: &Input<'_>,
    settings: &Settings,
    scrubbed: Scrubbed,
    select: &dyn Fn(&Selection<'_>) -> Option<Candidate>,
) -> Plan {
    let redactions = scrubbed.redactions();
    let emit_scrubbed = |scrubbed: Scrubbed, disposition: Disposition| {
        let mut output = scrubbed.into_bytes();
        let mut paused_tokens = None;
        if settings.paused {
            let tokens = estimate_tokens(&output);
            paused_tokens = Some(tokens);
            if tokens > PAUSED_NOTICE_TOKENS {
                output = with_paused_notice(output);
            }
        }
        Plan::Done(Outcome {
            paused_tokens,
            ..Outcome::plain(output, disposition, redactions)
        })
    };

    if input.bytes.len() > OVERSIZE_BYTES {
        return emit_scrubbed(scrubbed, Disposition::Oversize);
    }
    if settings.paused {
        return emit_scrubbed(scrubbed, Disposition::Paused);
    }
    if settings.bypass != Bypass::None {
        return emit_scrubbed(scrubbed, Disposition::Bypassed);
    }

    let params = ModeParams::for_mode(settings.mode);
    let original_tokens = estimate_tokens(scrubbed.as_bytes());
    if needs_token_minimum(input.source) && original_tokens < params.min_tokens {
        return emit_scrubbed(scrubbed, Disposition::Unchanged);
    }
    let selection = Selection {
        input,
        scrubbed: scrubbed.as_bytes(),
        params,
        mode: settings.mode,
    };
    let Some(candidate) = select(&selection) else {
        return emit_scrubbed(scrubbed, Disposition::Unchanged);
    };

    // Pre-check with the shortest possible ID: if it cannot pass even then, never touch the Vault.
    let summary = summarize(&candidate, input.source, redactions);
    let shortest = "0".repeat(crate::cas::MIN_EMIT_ID_LEN);
    let attempt = render(&candidate, &summary, &shortest);
    if !meets_saving(
        &attempt,
        scrubbed.as_bytes(),
        original_tokens,
        params.saving_pct,
    ) {
        return emit_scrubbed(scrubbed, Disposition::Unchanged);
    }
    Plan::Replace(Box::new(Pending {
        scrubbed,
        candidate,
        source: input.source.clone(),
        params,
        original_tokens,
    }))
}

// ---- the engine -------------------------------------------------------------------------------

/// Where the persisting step reads and writes, and the clock it uses.
pub struct Engine<'a> {
    paths: &'a Paths,
    cas: Cas,
    now: SystemTime,
    session: Option<Scrubbed>,
}

impl<'a> Engine<'a> {
    pub fn new(paths: &'a Paths) -> Self {
        Self {
            cas: Cas::new(paths.cas_dir()),
            paths,
            now: SystemTime::now(),
            session: None,
        }
    }

    /// Use this Vault (for a different lock timeout or TTL).
    pub fn with_cas(mut self, cas: Cas) -> Self {
        self.cas = cas;
        self
    }

    pub fn with_now(mut self, now: SystemTime) -> Self {
        self.now = now;
        self
    }

    /// A session label for memory.db rows. Only scrubbed text is accepted.
    pub fn with_session(mut self, session: Scrubbed) -> Self {
        self.session = Some(session);
        self
    }

    /// Run the whole pipeline. This is the entry point that never panics and never fails: any
    /// problem yields the best safe output (P2) and, where useful, a [`Outcome::note`].
    pub fn run(&self, input: &Input<'_>, settings: &Settings) -> Outcome {
        self.run_with(input, settings, &scrub, &select_candidate)
    }

    fn run_with(
        &self,
        input: &Input<'_>,
        settings: &Settings,
        scrub_fn: &dyn Fn(&[u8]) -> Scrubbed,
        select: &dyn Fn(&Selection<'_>) -> Option<Candidate>,
    ) -> Outcome {
        if looks_binary(input.bytes) {
            return Outcome::plain(input.bytes.to_vec(), Disposition::Binary, 0);
        }
        // A scrubber failure is the one case where privacy yields to delivering the output.
        let Ok(scrubbed) = catch_unwind(AssertUnwindSafe(|| scrub_fn(input.bytes))) else {
            return Outcome {
                note: Some(PANIC_NOTE.to_string()),
                ..Outcome::plain(input.bytes.to_vec(), Disposition::ScrubberFailed, 0)
            };
        };
        let fallback = scrubbed.clone();
        let guarded = catch_unwind(AssertUnwindSafe(|| {
            let outcome = match plan_scrubbed(input, settings, scrubbed, select) {
                Plan::Done(outcome) => outcome,
                Plan::Replace(pending) => self.finish(*pending),
            };
            self.after(outcome)
        }));
        guarded.unwrap_or_else(|_| Outcome {
            note: Some(PANIC_NOTE.to_string()),
            ..Outcome::plain(
                fallback.clone().into_bytes(),
                Disposition::Panicked,
                fallback.redactions(),
            )
        })
    }

    /// The persisting step: write the scrubbed original durably, then rebuild the replacement
    /// with the real ID and check it again. Any failure emits the scrubbed original.
    pub fn finish(&self, pending: Pending) -> Outcome {
        let Pending {
            scrubbed,
            candidate,
            source,
            params,
            original_tokens,
        } = pending;
        let redactions = scrubbed.redactions();
        let put = match self.cas.put(&scrubbed, self.now) {
            Ok(put) => put,
            Err(e) => {
                let why = match e {
                    CasError::Busy(_) => "the vault was busy",
                    _ => "the vault write failed",
                };
                return Outcome {
                    note: Some(note(&format!(
                        "agentdiet: {why}; output passed through untrimmed"
                    ))),
                    ..Outcome::plain(
                        scrubbed.into_bytes(),
                        Disposition::StorageFailed,
                        redactions,
                    )
                };
            }
        };
        let summary = summarize(&candidate, &source, redactions);
        let output = render(&candidate, &summary, &put.short_id);
        if !meets_saving(
            &output,
            scrubbed.as_bytes(),
            original_tokens,
            params.saving_pct,
        ) {
            // The Vault entry is harmless: nothing references it and GC removes it.
            return Outcome::plain(scrubbed.into_bytes(), Disposition::Unchanged, redactions);
        }
        let mut outcome = Outcome {
            output,
            disposition: Disposition::Replaced(candidate.kind),
            redactions,
            id: Some(put.short_id.clone()),
            sha: Some(put.sha.clone()),
            paused_tokens: None,
            note: None,
        };
        let saved = original_tokens.saturating_sub(estimate_tokens(&outcome.output));
        outcome.note = self.record(&source, &summary, &put.sha, &put.short_id, saved);
        outcome
    }

    /// Best-effort bookkeeping after a successful replacement. Returns a bounded note if any of
    /// it failed; the replacement is emitted regardless (P6).
    fn record(
        &self,
        source: &Source,
        summary: &str,
        sha: &str,
        id: &str,
        tokens_saved: usize,
    ) -> Option<String> {
        let mut failed: Vec<&str> = Vec::new();
        let telemetry = state::Telemetry {
            last_trimmed_id: Some(id),
            trimmed: true,
            tokens_saved: tokens_saved as u64,
            ..state::Telemetry::default()
        };
        if state::record_telemetry(self.paths, &telemetry).is_err() {
            failed.push("state update");
        }
        let (kind, title) = match source {
            Source::Shell { argv, .. } => (memory::Kind::Command, argv.join(" ")),
            Source::Read { path, .. } => (memory::Kind::Read, path.clone()),
            Source::ModelJson => (memory::Kind::Command, "model-facing JSON".to_string()),
        };
        let title = scrub(truncate_chars(&title, 240).as_bytes());
        let body = scrub(summary.as_bytes());
        let indexed = MemoryDb::open(&self.paths.memory_db()).and_then(|db| {
            db.index(kind, &title, &body, Some(sha), self.session.as_ref())
                .map(drop)
        });
        if indexed.is_err() {
            failed.push("memory index");
        }
        (!failed.is_empty()).then(|| {
            note(&format!(
                "agentdiet: output stored; {} failed (not retried)",
                failed.join(" and ")
            ))
        })
    }

    /// Paused-token accounting once the outcome is known (best effort, like every telemetry write).
    fn after(&self, mut outcome: Outcome) -> Outcome {
        if let Some(tokens) = outcome.paused_tokens
            && state::record_telemetry(
                self.paths,
                &state::Telemetry {
                    paused_tokens: tokens as u64,
                    ..state::Telemetry::default()
                },
            )
            .is_err()
            && outcome.note.is_none()
        {
            outcome.note = Some(note("agentdiet: paused-token count not updated"));
        }
        outcome
    }
}

fn truncate_chars(text: &str, max_bytes: usize) -> &str {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;
    use crate::state::Status;

    fn paths(dir: &TempDir) -> Paths {
        Paths::new(
            dir.path().join("home"),
            dir.path().join("config"),
            dir.path().join("project"),
            "/x/agentdiet".into(),
        )
    }

    fn aws_key() -> String {
        ["AK", "IA"].concat() + &"Q7".repeat(8)
    }

    fn shell(exit: ExitStatus) -> Source {
        Source::Shell {
            argv: vec!["cargo".into(), "test".into()],
            exit,
        }
    }

    /// 200 numbered lines: a few hundred tokens, plenty of room to shrink.
    fn long_text() -> Vec<u8> {
        (0..200)
            .map(|i| format!("line {i} of some verbose build output\n"))
            .collect::<String>()
            .into_bytes()
    }

    /// A stand-in transform: keep the first 20 lines.
    fn keep_head(sel: &Selection<'_>) -> Option<Candidate> {
        let lines: Vec<&[u8]> = sel.scrubbed.split_inclusive(|b| *b == b'\n').collect();
        let body: Vec<u8> = lines
            .iter()
            .take(20)
            .flat_map(|l| l.iter().copied())
            .collect();
        Some(Candidate {
            kind: Transform::Compressed,
            layout: Layout::Trailer { body },
            details: vec!["dropped the tail".to_string()],
            lines: Some((20.min(lines.len()), lines.len())),
        })
    }

    fn run(
        dir: &TempDir,
        bytes: &[u8],
        source: &Source,
        settings: Settings,
        select: &dyn Fn(&Selection<'_>) -> Option<Candidate>,
    ) -> Outcome {
        let p = paths(dir);
        let engine = Engine::new(&p);
        engine.run_with(&Input { bytes, source }, &settings, &scrub, select)
    }

    fn balanced() -> Settings {
        Settings::new(Mode::Balanced)
    }

    // ---- token estimate

    #[test]
    fn token_estimate_follows_the_spec_table() {
        let cases: &[(&[u8], usize)] = &[
            (b"", 0),
            (b"hello", 2), // ceil(5/4)
            (b"abcd", 1),
            (b"a b c", 3),
            (b"foo_bar123", 3), // one run of 10
            (b"\n\n\n", 1),
            (b"\r\n", 1),
            (b"\r", 0), // a CR run without LF
            (b"a\r\nb", 3),
            (b"  \t ", 0),
            (b"a\n \nb", 4), // 1, newline 1, space 0, newline 1, 1
            ("é".as_bytes(), 1),
            ("日本".as_bytes(), 2),
            (b"{}", 2),
            (b"\xff\xfe", 2),   // each invalid byte
            (b"ab\xffcd", 3),   // an invalid byte ends a word run
            (b"ab\xe2\x82", 3), // truncated sequence: two invalid bytes
        ];
        for (input, want) in cases {
            assert_eq!(
                estimate_tokens(input),
                *want,
                "{:?}",
                String::from_utf8_lossy(input)
            );
        }
        // 8 MiB of spaces is not 3000 tokens.
        assert_eq!(estimate_tokens(&vec![b' '; 8 * 1024 * 1024 + 1]), 0);
    }

    #[test]
    fn mode_table_matches_the_spec() {
        let l = ModeParams::for_mode(Mode::Light);
        let b = ModeParams::for_mode(Mode::Balanced);
        let u = ModeParams::for_mode(Mode::UltraLean);
        assert_eq!(
            (
                l.min_tokens,
                l.saving_pct,
                l.context_lines,
                l.last_lines,
                l.fold_min_body_lines
            ),
            (1500, 40, 5, 20, 20)
        );
        assert_eq!(
            (
                b.min_tokens,
                b.saving_pct,
                b.context_lines,
                b.last_lines,
                b.fold_min_body_lines
            ),
            (600, 20, 3, 10, 8)
        );
        assert_eq!(
            (
                u.min_tokens,
                u.saving_pct,
                u.context_lines,
                u.last_lines,
                u.fold_min_body_lines
            ),
            (250, 10, 1, 5, 3)
        );
    }

    // ---- framing and the gate

    #[test]
    fn breadcrumb_quotes_the_id_and_framing_adds_the_right_line_break() {
        let crumb = breadcrumb("kept 1/2 lines; exit 0", "4f9a1b2c");
        assert_eq!(
            crumb,
            "🥗 Diet: kept 1/2 lines; exit 0 Run: agentdiet show '#4f9a1b2c'"
        );
        assert!(!crumb.contains('\n'));
        assert_eq!(
            frame(b"a\nb\n", &crumb),
            format!("a\nb\n{crumb}\n").into_bytes()
        );
        assert_eq!(
            frame(b"a\nb", &crumb),
            format!("a\nb\n{crumb}\n").into_bytes()
        );
        assert_eq!(
            frame(b"a\r\nb\r\n", &crumb),
            format!("a\r\nb\r\n{crumb}\r\n").into_bytes()
        );
        assert_eq!(
            frame(b"a\r\nb", &crumb),
            format!("a\r\nb\r\n{crumb}\r\n").into_bytes()
        );
        assert_eq!(frame(b"", &crumb), format!("{crumb}\n").into_bytes());
    }

    #[test]
    fn the_saving_gate_is_exact_integer_arithmetic_in_bytes_and_tokens() {
        let original = "a".repeat(100); // 100 bytes, 25 tokens
        let pass = |len: usize, pct| {
            meets_saving("a".repeat(len).as_bytes(), original.as_bytes(), 25, pct)
        };
        assert!(pass(80, 20)); // exactly 20% smaller in bytes and tokens (20 vs 25)
        assert!(!pass(81, 20)); // 81 bytes / 21 tokens
        assert!(pass(60, 40));
        assert!(!pass(61, 40));
        // 10%: 88 bytes is 22 tokens (<= 22.5); 89 bytes is 23 tokens, over the token limit.
        assert!(pass(88, 10));
        assert!(!pass(89, 10));
        assert!(!pass(100, 10)); // not smaller at all
        // Fewer bytes but not fewer tokens fails: punctuation is one token each.
        let punct = "!".repeat(80);
        assert!(!meets_saving(punct.as_bytes(), original.as_bytes(), 25, 20));
    }

    // ---- classification and identity

    #[test]
    fn small_output_is_the_exact_input_including_odd_bytes() {
        let dir = TempDir::new().unwrap();
        for bytes in [
            &b"ok\n"[..],
            b"no trailing newline",
            b"crlf\r\nlines\r\n",
            b"\xff\xfe not utf-8 \x80\n",
            b"",
            b"\r\n",
        ] {
            let out = run(
                &dir,
                bytes,
                &shell(ExitStatus::Code(0)),
                balanced(),
                &keep_head,
            );
            assert_eq!(out.output, bytes);
            assert_eq!(out.disposition, Disposition::Unchanged);
            assert_eq!(out.redactions, 0);
            assert!(out.note.is_none() && out.id.is_none());
        }
        assert!(!paths(&dir).cas_dir().exists(), "nothing was stored");
    }

    #[test]
    fn redaction_alone_emits_scrubbed_text_and_stores_nothing() {
        let dir = TempDir::new().unwrap();
        let raw = format!("deploy id={}\n", aws_key());
        let out = run(
            &dir,
            raw.as_bytes(),
            &shell(ExitStatus::Code(0)),
            balanced(),
            &keep_head,
        );
        assert_eq!(out.output, b"deploy id=[REDACTED:aws-access-key-id]\n");
        assert_eq!(out.redactions, 1);
        assert_eq!(out.disposition, Disposition::Unchanged);
        assert!(!paths(&dir).cas_dir().exists());
    }

    #[test]
    fn binary_is_byte_identical_and_unscrubbed_within_the_sniff_window() {
        let dir = TempDir::new().unwrap();
        let key = aws_key();
        // NUL exactly at index 8191 (last sniffed byte) -> binary; at 8192 -> text.
        let mut binary = vec![b'x'; 9000];
        binary[BINARY_SNIFF_BYTES - 1] = 0;
        binary[100..100 + key.len()].copy_from_slice(key.as_bytes());
        let out = run(
            &dir,
            &binary,
            &shell(ExitStatus::Code(0)),
            balanced(),
            &keep_head,
        );
        assert_eq!(out.disposition, Disposition::Binary);
        assert_eq!(
            out.output, binary,
            "passed through untouched, secret included"
        );

        let mut text = vec![b'x'; 9000];
        text[BINARY_SNIFF_BYTES] = 0;
        text[99] = b' ';
        text[100..100 + key.len()].copy_from_slice(key.as_bytes());
        text[100 + key.len()] = b' ';
        assert!(!looks_binary(&text));
        let out = run(
            &dir,
            &text,
            &shell(ExitStatus::Code(0)),
            balanced(),
            &|_: &Selection<'_>| None,
        );
        assert_ne!(out.disposition, Disposition::Binary);
        assert!(!out.output.windows(key.len()).any(|w| w == key.as_bytes()));

        // UTF-16 text has NULs and is therefore binary.
        let utf16: Vec<u8> = "hello".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let out = run(
            &dir,
            &utf16,
            &shell(ExitStatus::Code(0)),
            balanced(),
            &keep_head,
        );
        assert_eq!((out.disposition, out.output), (Disposition::Binary, utf16));
        assert!(!paths(&dir).cas_dir().exists());
    }

    #[test]
    fn oversize_output_is_scrubbed_but_never_transformed_or_stored() {
        let dir = TempDir::new().unwrap();
        let key = aws_key();
        let mut big = format!("start {key}\n").into_bytes();
        big.resize(OVERSIZE_BYTES + 1, b' ');
        let out = run(
            &dir,
            &big,
            &shell(ExitStatus::Code(0)),
            balanced(),
            &keep_head,
        );
        assert_eq!(out.disposition, Disposition::Oversize);
        assert_eq!(out.redactions, 1);
        assert!(
            out.output
                .starts_with(b"start [REDACTED:aws-access-key-id]\n")
        );
        assert!(out.id.is_none() && !paths(&dir).cas_dir().exists());
        assert!(
            !out.output
                .windows(PAUSED_NOTICE.len())
                .any(|w| w == PAUSED_NOTICE.as_bytes())
        );
    }

    // ---- pause, bypass and the whisper

    #[test]
    fn paused_scrubs_only_and_whispers_only_above_3000_estimated_tokens() {
        let dir = TempDir::new().unwrap();
        let paused = Settings {
            paused: true,
            ..balanced()
        };
        let notice = |out: &Outcome| {
            out.output
                .ends_with(format!("{PAUSED_NOTICE}\n").as_bytes())
        };
        let tokens_of = |n: usize| "a ".repeat(n).into_bytes(); // `a` = 1, space = 0

        for (n, expect_notice) in [(2999, false), (3000, false), (3001, true)] {
            let bytes = tokens_of(n);
            assert_eq!(estimate_tokens(&bytes), n);
            let out = run(
                &dir,
                &bytes,
                &shell(ExitStatus::Code(0)),
                paused,
                &keep_head,
            );
            assert_eq!(out.disposition, Disposition::Paused);
            assert_eq!(notice(&out), expect_notice, "{n} tokens");
            assert_eq!(out.paused_tokens, Some(n), "the notice is not counted");
            assert!(out.id.is_none());
        }
        assert!(
            !paths(&dir).cas_dir().exists(),
            "paused writes nothing to the Vault"
        );

        // Secrets are redacted while paused.
        let raw = format!("{} {}\n", aws_key(), "a ".repeat(3500));
        let out = run(
            &dir,
            raw.as_bytes(),
            &shell(ExitStatus::Code(0)),
            paused,
            &keep_head,
        );
        assert!(!String::from_utf8_lossy(&out.output).contains(&aws_key()));
        assert!(notice(&out) && out.redactions == 1);

        // Oversize is not 3000 tokens by itself: 8 MiB of spaces gets no notice, real words do.
        let spaces = vec![b' '; OVERSIZE_BYTES + 1];
        let out = run(
            &dir,
            &spaces,
            &shell(ExitStatus::Code(0)),
            paused,
            &keep_head,
        );
        assert_eq!(
            (out.disposition, out.paused_tokens),
            (Disposition::Oversize, Some(0))
        );
        assert!(!notice(&out) && out.output == spaces);
        let words = "word ".repeat(OVERSIZE_BYTES / 5 + 10).into_bytes();
        let out = run(
            &dir,
            &words,
            &shell(ExitStatus::Code(0)),
            paused,
            &keep_head,
        );
        assert_eq!(out.disposition, Disposition::Oversize);
        assert!(notice(&out));

        // CRLF output gets a CRLF-terminated notice.
        let crlf = "a\r\n".repeat(3100).into_bytes();
        let out = run(&dir, &crlf, &shell(ExitStatus::Code(0)), paused, &keep_head);
        assert!(
            out.output
                .ends_with(format!("{PAUSED_NOTICE}\r\n").as_bytes())
        );

        // Binary is passed whole, with no notice and no token count.
        let mut bin = vec![b'a'; 20_000];
        bin[0] = 0;
        let out = run(&dir, &bin, &shell(ExitStatus::Code(0)), paused, &keep_head);
        assert_eq!(
            (out.disposition, out.paused_tokens, out.output.as_slice()),
            (Disposition::Binary, None, bin.as_slice())
        );
    }

    #[test]
    fn bypasses_skip_transforms_and_the_vault_but_not_scrubbing() {
        let dir = TempDir::new().unwrap();
        let mut text = long_text();
        text.extend_from_slice(format!("key {}\n", aws_key()).as_bytes());
        for bypass in [Bypass::RawNextTurn, Bypass::RepeatedCommand] {
            let settings = Settings {
                bypass,
                ..balanced()
            };
            let out = run(
                &dir,
                &text,
                &shell(ExitStatus::Code(0)),
                settings,
                &|_: &Selection<'_>| panic!("a bypassed call must not even look for a candidate"),
            );
            assert_eq!(out.disposition, Disposition::Bypassed);
            assert!(String::from_utf8_lossy(&out.output).contains("[REDACTED:aws-access-key-id]"));
            assert!(!String::from_utf8_lossy(&out.output).contains(&aws_key()));
            assert!(out.paused_tokens.is_none() && out.id.is_none());
        }
        assert!(!paths(&dir).cas_dir().exists());
    }

    // ---- replacement

    #[test]
    fn a_replacement_is_persisted_first_and_recoverable_byte_for_byte() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        let mut raw = long_text();
        raw.extend_from_slice(format!("token={}\n", aws_key()).as_bytes());
        raw.extend_from_slice(b"\xff not utf-8 tail");
        let src = shell(ExitStatus::Code(101));
        let engine = Engine::new(&p).with_session(scrub(b"sess-1"));
        let out = engine.run_with(
            &Input {
                bytes: &raw,
                source: &src,
            },
            &balanced(),
            &scrub,
            &keep_head,
        );

        assert_eq!(
            out.disposition,
            Disposition::Replaced(Transform::Compressed)
        );
        let id = out.id.clone().unwrap();
        assert!(id.len() >= 8 && out.sha.as_ref().unwrap().starts_with(&id));
        let text = String::from_utf8_lossy(&out.output).into_owned();
        assert!(
            text.ends_with(&format!("agentdiet show '#{id}'\n")),
            "{text}"
        );
        assert!(
            text.contains(
                "kept 20/202 lines; dropped the tail; exit 101; 1 secret redacted (not recoverable)"
            ),
            "{text}"
        );
        assert!(
            out.output.len() < raw.len() && estimate_tokens(&out.output) < estimate_tokens(&raw)
        );

        // The stored copy is the scrubbed original, byte for byte, and holds no secret.
        let stored = Cas::new(p.cas_dir())
            .read(&id, None, SystemTime::now())
            .unwrap();
        assert_eq!(stored, scrub(&raw).into_bytes());
        assert!(!String::from_utf8_lossy(&stored).contains(&aws_key()));

        // Bookkeeping: state and memory.db.
        assert_eq!(
            state::load(&p).last_trimmed_id.as_deref(),
            Some(id.as_str())
        );
        let db = MemoryDb::open(&p.memory_db()).unwrap();
        let hits = db.search("cargo test", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].cas_sha, out.sha);
        assert!(out.note.is_none());

        // Running it again is idempotent: same ID, one entry, one row, same bytes.
        let again = engine.run_with(
            &Input {
                bytes: &raw,
                source: &src,
            },
            &balanced(),
            &scrub,
            &keep_head,
        );
        assert_eq!(again.output, out.output);
        assert_eq!(db.search("cargo test", 5).unwrap().len(), 1);
    }

    #[test]
    fn the_final_check_uses_the_real_id_and_falls_back_to_the_scrubbed_original() {
        // A candidate with a fixed 151-byte body, so only the original's length decides the gate.
        // Scan for an original that is just big enough for the 8-hex rendering to pass but too
        // small for the rendering with the full 64-hex ID (forced by planting a tombstone that
        // shares 63 hex digits with the content's sha).
        let src = shell(ExitStatus::Code(0));
        let select = |sel: &Selection<'_>| {
            Some(Candidate {
                kind: Transform::Compressed,
                layout: Layout::Trailer {
                    body: format!("{}\n", "0123456789".repeat(15)).into_bytes(),
                },
                details: vec![],
                lines: Some((1, sel.scrubbed.split(|b| *b == b'\n').count())),
            })
        };
        let ultra = Settings::new(Mode::UltraLean); // 10% saving, 250-token minimum
        let attempt = |bytes: &[u8], plant: bool| {
            let dir = TempDir::new().unwrap();
            let p = paths(&dir);
            if plant {
                let sha = crate::fsutil::sha256_hex(bytes);
                fs::create_dir_all(p.cas_dir()).unwrap();
                let flipped = if sha.ends_with('0') { '1' } else { '0' };
                let other = format!("{}{flipped}", &sha[..63]);
                fs::write(p.cas_dir().join(format!("{other}.expired")), "").unwrap();
            }
            Engine::new(&p).run_with(
                &Input {
                    bytes,
                    source: &src,
                },
                &ultra,
                &scrub,
                &select,
            )
        };

        // Find the size analytically (no I/O): one token per punctuation byte, so bytes and
        // tokens move together. `short`/`long` are the renderings with an 8-hex and a 64-hex ID.
        let pct = ModeParams::for_mode(Mode::UltraLean).saving_pct;
        let separating = (250..2000).find(|&size| {
            let bytes = "!".repeat(size).into_bytes();
            let tokens = estimate_tokens(&bytes);
            let cand = select(&Selection {
                input: &Input {
                    bytes: &bytes,
                    source: &src,
                },
                scrubbed: &bytes,
                params: ModeParams::for_mode(Mode::UltraLean),
                mode: Mode::UltraLean,
            })
            .unwrap();
            let summary = summarize(&cand, &src, 0);
            let short = render(&cand, &summary, &"a".repeat(8));
            let long = render(&cand, &summary, &"a".repeat(64));
            meets_saving(&short, &bytes, tokens, pct) && !meets_saving(&long, &bytes, tokens, pct)
        });
        let size = separating.expect("no size separates the 8-hex and 64-hex renderings");

        let bytes = "!".repeat(size).into_bytes();
        let plain = attempt(&bytes, false);
        let forced = attempt(&bytes, true);
        assert!(
            matches!(plain.disposition, Disposition::Replaced(_)),
            "{plain:?}"
        );
        assert!(plain.id.as_ref().unwrap().len() < 64);
        // It passed the pre-check (and was persisted), the real 64-hex ID pushed it over, and the
        // fallback is exactly the scrubbed original with no breadcrumb.
        assert_eq!(forced.disposition, Disposition::Unchanged);
        assert_eq!(forced.output, scrub(&bytes).into_bytes());
        assert!(forced.id.is_none() && forced.note.is_none());
    }

    // ---- failures

    #[test]
    fn a_failed_vault_write_emits_the_scrubbed_original_with_a_bounded_note() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        fs::create_dir_all(&p.agentdiet_home).unwrap();
        fs::write(p.cas_dir(), "a file where the vault directory should be").unwrap();
        let mut raw = long_text();
        raw.extend_from_slice(format!("k={}\n", aws_key()).as_bytes());
        let src = shell(ExitStatus::Code(0));
        let out = Engine::new(&p).run_with(
            &Input {
                bytes: &raw,
                source: &src,
            },
            &balanced(),
            &scrub,
            &keep_head,
        );
        assert_eq!(out.disposition, Disposition::StorageFailed);
        assert_eq!(out.output, scrub(&raw).into_bytes());
        assert!(out.id.is_none());
        let note = out.note.unwrap();
        assert!(
            note.len() <= MAX_NOTE_BYTES && note.contains("untrimmed"),
            "{note}"
        );
        assert!(!note.contains(&aws_key()));
    }

    #[test]
    fn a_busy_vault_is_a_failed_write_not_a_wait() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        fs::create_dir_all(p.cas_dir()).unwrap();
        let _held =
            crate::fsutil::lock_exclusive(&p.cas_dir().join(".lock"), Duration::from_secs(1))
                .unwrap();
        let raw = long_text();
        let src = shell(ExitStatus::Code(0));
        let cas = Cas::new(p.cas_dir()).lock_timeout(Duration::from_millis(50));
        let started = std::time::Instant::now();
        let out = Engine::new(&p).with_cas(cas).run_with(
            &Input {
                bytes: &raw,
                source: &src,
            },
            &balanced(),
            &scrub,
            &keep_head,
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            (out.disposition, out.output),
            (Disposition::StorageFailed, raw)
        );
        assert!(out.note.unwrap().contains("busy"));
    }

    #[test]
    fn state_and_index_failures_keep_the_replacement_and_add_one_note() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        // state: the lock is held; index: `.agentdiet` is a file, so memory.db cannot be created.
        fs::create_dir_all(&p.agentdiet_home).unwrap();
        let _held =
            crate::fsutil::lock_exclusive(&p.state_lock_file(), Duration::from_secs(1)).unwrap();
        fs::create_dir_all(&p.project_root).unwrap();
        fs::write(p.project_data_dir(), "not a directory").unwrap();
        let raw = long_text();
        let src = shell(ExitStatus::Code(0));
        let out = Engine::new(&p).run_with(
            &Input {
                bytes: &raw,
                source: &src,
            },
            &balanced(),
            &scrub,
            &keep_head,
        );
        assert!(matches!(out.disposition, Disposition::Replaced(_)));
        assert!(out.output.len() < raw.len());
        let note = out.note.unwrap();
        assert!(note.len() <= MAX_NOTE_BYTES, "{note}");
        assert!(
            note.contains("state update") && note.contains("memory index"),
            "{note}"
        );
        // The Vault copy is intact regardless.
        assert_eq!(
            Cas::new(p.cas_dir())
                .read(out.id.as_ref().unwrap(), None, SystemTime::now())
                .unwrap(),
            raw
        );
    }

    #[test]
    fn a_panic_after_scrubbing_emits_the_scrubbed_original_and_never_the_payload() {
        let dir = TempDir::new().unwrap();
        let mut raw = long_text();
        raw.extend_from_slice(format!("k={}\n", aws_key()).as_bytes());
        let src = shell(ExitStatus::Code(0));
        let boom = |_: &Selection<'_>| -> Option<Candidate> {
            panic!("PAYLOAD-WITH-SECRET {}", aws_key())
        };
        let out = run(&dir, &raw, &src, balanced(), &boom);
        assert_eq!(out.disposition, Disposition::Panicked);
        assert_eq!(out.output, scrub(&raw).into_bytes());
        assert_eq!(out.note.as_deref(), Some(PANIC_NOTE));
        assert!(!out.note.unwrap().contains("PAYLOAD"));
    }

    #[test]
    fn a_failing_scrubber_is_the_one_case_that_passes_raw_output() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        let raw = format!("k={}\n", aws_key()).into_bytes();
        let src = shell(ExitStatus::Code(0));
        let broken = |_: &[u8]| -> Scrubbed { panic!("scrubber bug") };
        let out = Engine::new(&p).run_with(
            &Input {
                bytes: &raw,
                source: &src,
            },
            &balanced(),
            &broken,
            &keep_head,
        );
        assert_eq!(out.disposition, Disposition::ScrubberFailed);
        assert_eq!(out.output, raw);
        assert_eq!(out.note.as_deref(), Some(PANIC_NOTE));
        assert!(
            !p.cas_dir().exists(),
            "and nothing unscrubbed was persisted"
        );
    }

    #[test]
    fn telemetry_counts_paused_tokens_into_state() {
        let dir = TempDir::new().unwrap();
        let p = paths(&dir);
        let raw = "a ".repeat(100).into_bytes();
        let src = shell(ExitStatus::Code(0));
        let paused = Settings {
            paused: true,
            ..balanced()
        };
        let engine = Engine::new(&p);
        for _ in 0..3 {
            engine.run_with(
                &Input {
                    bytes: &raw,
                    source: &src,
                },
                &paused,
                &scrub,
                &keep_head,
            );
        }
        assert_eq!(state::load(&p).raw_tokens_passed_while_paused, 300);
        assert_eq!(
            state::load(&p).status,
            Status::Active,
            "telemetry never changes the pause state"
        );
    }
}
