//! Signature folding for file reads (SPEC §3 Tier 2, "Signature folding").
//!
//! For a complete read of a Rust, Python, JavaScript, TypeScript or TSX file of more than
//! [`FOLD_MIN_FILE_LINES`] lines, function and method **bodies** are replaced by a one-line marker
//! that carries the real command to get them back. Everything else (signatures, attributes, doc
//! comments, docstrings, classes, impls, traits, types, imports, constants) is left alone.
//!
//! The grammars are linked statically (the `tree-sitter-*` crates); there is no runtime grammar
//! loading, no helper program and no wasm. The parse runs on the *scrubbed* text, so every line
//! number in a marker is a line of the Vault copy, even when a multi-line redaction (a private key)
//! made the scrubbed text shorter than the file on disk.
//!
//! Any doubt means "unchanged": a read that is not [`Extent::Complete`], an unknown extension,
//! non-UTF-8 bytes, any parse error, a parse that overruns [`PARSE_BUDGET`], a first 2 KiB that does
//! not look like source (see `looks_like_source`), or a body that is not worth folding.

use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use tree_sitter::{Node, ParseOptions, Parser};

use crate::diet::{
    Candidate, Extent, FOLD_MIN_FILE_LINES, FoldStyle, Layout, ModeParams, Piece, Transform,
};
use crate::state::Mode;

/// A parse that runs longer than this is abandoned and the file is left unchanged. It is a guard
/// against hostile input, not a tuning knob: the parser does not stop at the first syntax error, so
/// without it text that is not source code would run the parse to the end of the input. A valid
/// 256 KiB source takes about 22 ms on the reference machine (SPEC §7), so on a slower or busier
/// machine the largest files may go unfolded.
///
/// An unoptimized build parses about ten times slower and the tests run in parallel, so it gets ten
/// times the time; otherwise the debug test suite would measure the build profile, not the rule.
pub const PARSE_BUDGET: Duration =
    Duration::from_millis(if cfg!(debug_assertions) { 300 } else { 30 });

/// How much of a file [`looks_like_source`] parses before deciding whether to parse all of it.
const PROBE_BYTES: usize = 2048;

/// The probe's limit, in progress-callback steps (the parser calls back once per fixed batch of its
/// internal operations, so the count is the same on every machine and in every build profile, unlike
/// a deadline). Measured over the first 2 KiB of the test fixtures in all five languages, cut at
/// every line boundary and shifted through the file: valid code needs at most 20 steps; number-and-
/// bracket text needs 71 or more. Prose and diagnostics need fewer steps than valid code (error
/// recovery is slow per step), so they are rejected by the structural rules instead.
const PROBE_MAX_STEPS: usize = 40;

/// The ID used to size markers before the real one exists (the shortest an emitted ID can be).
const PLACEHOLDER_ID: &str = "00000000";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
}

impl Language {
    /// From the file extension (case-insensitive). Anything else is not folded.
    pub fn from_path(path: &str) -> Option<Self> {
        // `Path::extension` semantics: `.rs` (a dotfile) and `rs` have no extension.
        let ext = std::path::Path::new(crate::basename(path))
            .extension()?
            .to_str()?
            .to_ascii_lowercase();
        Some(match ext.as_str() {
            "rs" => Language::Rust,
            "py" | "pyi" => Language::Python,
            "js" | "mjs" | "cjs" | "jsx" => Language::JavaScript,
            "ts" | "mts" | "cts" => Language::TypeScript,
            "tsx" => Language::Tsx,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Language::Rust => "Rust",
            Language::Python => "Python",
            Language::JavaScript => "JavaScript",
            Language::TypeScript => "TypeScript",
            Language::Tsx => "TSX",
        }
    }

    fn grammar(self) -> tree_sitter::Language {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }

    fn style(self) -> FoldStyle {
        match self {
            Language::Python => FoldStyle::Python,
            _ => FoldStyle::Brace,
        }
    }
}

/// The number of lines of `text`: a final line without a terminator counts.
pub fn count_lines(text: &[u8]) -> usize {
    text.iter().filter(|b| **b == b'\n').count()
        + usize::from(!text.is_empty() && !text.ends_with(b"\n"))
}

/// Parse `text` as `language`; `None` on any error, timeout, or failure to set the grammar up.
pub fn parse(language: Language, text: &str) -> Option<tree_sitter::Tree> {
    let tree = parse_within(language, text, Limit::Time(PARSE_BUDGET))?;
    (!tree.root_node().has_error()).then_some(tree)
}

/// When a parse is abandoned.
#[derive(Debug, Clone, Copy)]
enum Limit {
    /// After this much wall-clock time.
    Time(Duration),
    /// After this many progress callbacks: a measure of the parser's work, not of the machine.
    Steps(usize),
}

/// Parse `text` as `language`, abandoning the parse at `limit`. Unlike [`parse`], a tree with
/// syntax errors is returned as it is: `None` only on a timeout or a grammar failure.
fn parse_within(language: Language, text: &str, limit: Limit) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser.set_language(&language.grammar()).ok()?;
    let started = Instant::now();
    let mut steps = 0usize;
    // The parser reports `has_error` only once the error is on its stack, which for non-source text
    // is not until the end of the input (measured: 2000+ callbacks, none with the flag set). Do not
    // count on it to end the parse early; the limit is the guard.
    let mut progress = |_: &tree_sitter::ParseState| {
        steps += 1;
        let over = match limit {
            Limit::Time(budget) => started.elapsed() >= budget,
            Limit::Steps(max) => steps > max,
        };
        if over {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut progress);
    let bytes = text.as_bytes();
    parser.parse_with_options(
        &mut |i, _| &bytes[i.min(bytes.len())..],
        None,
        Some(options),
    )
}

/// Whether `root` has an `ERROR` or `MISSING` node that starts before the start of its last child.
/// The root itself is not counted: an unclosed construct at the end of a prefix may make it an
/// `ERROR` node, and everything inside the last child belongs to that unclosed construct.
fn has_error_before_last_item(root: Node<'_>) -> bool {
    let Some(last) = root
        .child_count()
        .checked_sub(1)
        .and_then(|n| root.child(n))
    else {
        return false;
    };
    let limit = last.start_byte();
    let mut cursor = root.walk();
    if !cursor.goto_first_child() {
        return false;
    }
    let mut depth = 1;
    loop {
        let node = cursor.node();
        if (node.is_error() || node.is_missing()) && node.start_byte() < limit {
            return true;
        }
        // A subtree with no error in it has nothing to find; an error that starts at or after the
        // limit has only descendants that start there too.
        if node.has_error() && !node.is_error() && cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        while !cursor.goto_next_sibling() {
            if depth == 1 {
                return false;
            }
            cursor.goto_parent();
            depth -= 1;
        }
    }
}

/// Whether `root`'s `ERROR` children cover nearly all of a prefix of `len` bytes and none of them
/// holds a well-formed node of more than one line (a function, a block, a statement list). That is
/// text the grammar found no structure in, such as diagnostics or prose, which Rust's grammar
/// swallows into one `ERROR` node without a single error before the last item. A construct left open
/// by the cut is different: its `ERROR` node holds the complete functions and statements before the
/// cut.
fn is_mostly_unstructured_error(root: Node<'_>, len: usize) -> bool {
    let mut error_bytes = 0;
    let mut structured = false;
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if !child.is_error() {
            continue;
        }
        error_bytes += child.end_byte() - child.start_byte();
        let mut inner = child.walk();
        structured |= child.children(&mut inner).any(|node| {
            node.is_named()
                && !node.has_error()
                && node.end_position().row > node.start_position().row
        });
    }
    error_bytes * 10 >= len * 9 && !structured
}

/// A cheap test, before the full parse, that `text` is source code and not something that merely
/// has a source file's extension. Parsing the first [`PROBE_BYTES`] bytes (cut at a line boundary)
/// within [`PROBE_MAX_STEPS`] is enough: valid code takes a fraction of that, while text the grammar
/// cannot make sense of burns about a microsecond per byte in error recovery. A construct left open
/// by the cut is not evidence of anything and does not count (see [`has_error_before_last_item`]
/// and [`is_mostly_unstructured_error`]). Short texts and prefixes without a line break are not
/// probed.
fn looks_like_source(language: Language, text: &str) -> bool {
    if text.len() <= PROBE_BYTES {
        return true;
    }
    let Some(newline) = text.as_bytes()[..PROBE_BYTES]
        .iter()
        .rposition(|b| *b == b'\n')
    else {
        return true;
    };
    // The newline is one byte, so the cut is on a character boundary.
    let prefix = &text[..=newline];
    match parse_within(language, prefix, Limit::Steps(PROBE_MAX_STEPS)) {
        Some(tree) => {
            let root = tree.root_node();
            !has_error_before_last_item(root) && !is_mostly_unstructured_error(root, prefix.len())
        }
        None => false,
    }
}

/// A region of the text to replace by a marker.
#[derive(Debug, Clone, Copy)]
struct Region {
    start: usize,
    end: usize,
    first_line: usize,
    last_line: usize,
}

/// The nodes whose bodies are folded, per language, and where the foldable region of one is.
fn body_region(language: Language, node: Node<'_>, text: &str) -> Option<Region> {
    match language {
        Language::Rust => {
            if node.kind() != "function_item" {
                return None;
            }
            let body = node.child_by_field_name("body")?;
            (body.kind() == "block").then(|| brace_region(body))
        }
        Language::JavaScript | Language::TypeScript | Language::Tsx => {
            if !matches!(
                node.kind(),
                "function_declaration"
                    | "generator_function_declaration"
                    | "function_expression"
                    | "function"
                    | "generator_function"
                    | "arrow_function"
                    | "method_definition"
            ) {
                return None;
            }
            let body = node.child_by_field_name("body")?;
            (body.kind() == "statement_block").then(|| brace_region(body))
        }
        Language::Python => {
            if node.kind() != "function_definition" {
                return None;
            }
            python_region(node, text)
        }
    }
}

fn brace_region(body: Node<'_>) -> Region {
    Region {
        start: body.start_byte(),
        end: body.end_byte(),
        first_line: body.start_position().row + 1,
        last_line: body.end_position().row + 1,
    }
}

/// A Python body without its docstring: whole lines, from the first remaining statement's line
/// to the end of the last one. A body on the `def` line, or sharing a line with the docstring,
/// is not folded.
fn python_region(function: Node<'_>, text: &str) -> Option<Region> {
    let block = function.child_by_field_name("body")?;
    if block.kind() != "block" || block.start_position().row == function.start_position().row {
        return None;
    }
    let mut children = Vec::new();
    let mut cursor = block.walk();
    for child in block.children(&mut cursor) {
        if child.is_named() {
            children.push(child);
        }
    }
    let mut rest = children.as_slice();
    let mut docstring_end_row = None;
    if let Some(first) = rest.first()
        && is_docstring(*first)
    {
        docstring_end_row = Some(first.end_position().row);
        rest = &rest[1..];
    }
    let (first, last) = (rest.first()?, rest.last()?);
    if docstring_end_row.is_some_and(|row| first.start_position().row <= row) {
        return None;
    }
    // The region starts at the beginning of the first statement's line, which must hold nothing
    // but indentation before the statement.
    let line_start = text[..first.start_byte()].rfind('\n').map_or(0, |i| i + 1);
    if !text[line_start..first.start_byte()]
        .bytes()
        .all(|b| b == b' ' || b == b'\t')
    {
        return None;
    }
    Some(Region {
        start: line_start,
        end: last.end_byte(),
        first_line: first.start_position().row + 1,
        last_line: last.end_position().row + 1,
    })
}

fn is_docstring(statement: Node<'_>) -> bool {
    statement.kind() == "expression_statement"
        && statement.named_child_count() == 1
        && statement
            .named_child(0)
            .is_some_and(|c| matches!(c.kind(), "string" | "concatenated_string"))
}

/// Every foldable region, outermost only (a folded body hides what is inside it), in document order.
fn collect(language: Language, root: Node<'_>, text: &str, params: &ModeParams) -> Vec<Region> {
    let style = language.style();
    let mut regions = Vec::new();
    let mut cursor = root.walk();
    'walk: loop {
        let node = cursor.node();
        let mut descend = true;
        if let Some(region) = body_region(language, node, text) {
            let lines = region.last_line - region.first_line + 1;
            let marker = style
                .marker(region.first_line, region.last_line, PLACEHOLDER_ID)
                .len();
            // Python's marker line also carries the body's indentation.
            let indent = if style == FoldStyle::Python {
                text[region.start..]
                    .bytes()
                    .take_while(|b| *b == b' ' || *b == b'\t')
                    .count()
            } else {
                0
            };
            if lines >= params.fold_min_body_lines && marker + indent < region.end - region.start {
                regions.push(region);
                descend = false;
            }
        }
        if descend && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    regions
}

/// Fold the function bodies of a complete file read, or `None` to leave it unchanged.
pub fn fold(path: &str, extent: Extent, scrubbed: &[u8], mode: Mode) -> Option<Candidate> {
    if extent != Extent::Complete {
        return None;
    }
    let language = Language::from_path(path)?;
    let total_lines = count_lines(scrubbed);
    if total_lines <= FOLD_MIN_FILE_LINES {
        return None;
    }
    let text = std::str::from_utf8(scrubbed).ok()?;
    // Text with control characters other than tab, line breaks and form feed is not source code
    // (terminal escape sequences, binary-ish data). Parsing it is the slow way to find that out.
    if scrubbed
        .iter()
        .any(|b| *b < 0x20 && !matches!(*b, b'\t' | b'\n' | b'\r' | 0x0c))
    {
        return None;
    }
    if !looks_like_source(language, text) {
        return None;
    }
    let tree = parse(language, text)?;
    let params = ModeParams::for_mode(mode);
    let regions = collect(language, tree.root_node(), text, &params);
    if regions.is_empty() {
        return None;
    }

    let style = language.style();
    let mut pieces: Vec<Piece> = Vec::with_capacity(regions.len() * 2 + 1);
    let mut cursor = 0;
    let mut folded_lines = 0;
    for region in &regions {
        pieces.push(Piece::Text(scrubbed[cursor..region.start].to_vec()));
        if style == FoldStyle::Python {
            // Keep the body's indentation in front of the marker.
            let indent: Vec<u8> = text[region.start..]
                .bytes()
                .take_while(|b| *b == b' ' || *b == b'\t')
                .collect();
            pieces.push(Piece::Text(indent));
        }
        pieces.push(Piece::Fold {
            first_line: region.first_line,
            last_line: region.last_line,
            style,
        });
        folded_lines += region.last_line - region.first_line + 1;
        cursor = region.end;
    }
    pieces.push(Piece::Text(scrubbed[cursor..].to_vec()));

    let kept = total_lines - folded_lines + regions.len();
    Some(Candidate {
        kind: Transform::Folded,
        layout: Layout::Folded { pieces },
        details: vec![format!(
            "folded {} {} function bodies ({folded_lines} lines)",
            regions.len(),
            language.name()
        )],
        lines: Some((kept, total_lines)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Language; 5] = [
        Language::Rust,
        Language::Python,
        Language::JavaScript,
        Language::TypeScript,
        Language::Tsx,
    ];

    fn numbers_and_punctuation(target: usize) -> String {
        repeated(target, |i| {
            format!("{i}: [{}] {{{}}} ({})\n", i * 7, i * 13, i % 5)
        })
    }

    fn repeated(target: usize, line: impl Fn(usize) -> String) -> String {
        let mut s = String::new();
        let mut i = 0;
        while s.len() < target {
            s.push_str(&line(i));
            i += 1;
        }
        s
    }

    fn fixture(name: &str) -> String {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/folding/");
        std::fs::read_to_string(format!("{path}{name}")).unwrap()
    }

    #[test]
    fn text_that_is_not_source_is_rejected_by_the_probe_without_a_full_parse() {
        // The first goes through the step limit, the other two are what Rust's grammar swallows
        // into a single `ERROR` node, with no error before the last item and few steps.
        let texts = [
            ("numbers", numbers_and_punctuation(256 * 1024)),
            (
                "diagnostics",
                repeated(256 * 1024, |i| {
                    format!("error: something failed at step {i}: warning, fatal\n")
                }),
            ),
            (
                "multibyte prose",
                repeated(256 * 1024, |i| {
                    format!("日本語のテスト {i} — héllo wörld 😀 ok\n")
                }),
            ),
        ];
        for (kind, text) in &texts {
            for language in ALL {
                assert!(
                    !looks_like_source(language, text),
                    "{kind} as {}",
                    language.name()
                );
            }
        }
    }

    #[test]
    fn short_text_and_text_without_a_line_break_are_not_probed() {
        let short = numbers_and_punctuation(PROBE_BYTES - 100);
        assert!(short.len() <= PROBE_BYTES);
        let one_line = "x ".repeat(PROBE_BYTES);
        for language in ALL {
            assert!(looks_like_source(language, &short));
            assert!(looks_like_source(language, &one_line));
        }
    }

    #[test]
    fn real_sources_pass_the_probe_at_every_cut_through_the_first_2_kib() {
        for (name, language) in [
            ("inventory.rs", Language::Rust),
            ("pricing.py", Language::Python),
            ("cart.js", Language::JavaScript),
            ("store.ts", Language::TypeScript),
            ("Table.tsx", Language::Tsx),
        ] {
            let text = fixture(name);
            // Shifting the file by a few characters moves the cut through every kind of construct
            // the fixtures contain (open functions, impls, classes, multi-line strings, JSX).
            for shift in 0..48 {
                let shifted = format!("{}{text}", "\n".repeat(shift));
                assert!(
                    looks_like_source(language, &shifted),
                    "{name} shifted by {shift} lines"
                );
            }
            let mut lim = 512;
            while lim <= PROBE_BYTES.min(text.len()) {
                let cut = text[..lim].rfind('\n').map_or(0, |i| i + 1);
                lim += 29;
                if cut == 0 {
                    continue;
                }
                let tree =
                    parse_within(language, &text[..cut], Limit::Time(Duration::from_secs(5)))
                        .unwrap();
                assert!(
                    !has_error_before_last_item(tree.root_node()),
                    "{name} cut at {cut} is a valid file cut short"
                );
            }
        }
    }

    #[test]
    fn an_error_before_the_last_top_level_node_is_found_and_one_inside_it_is_not() {
        let broken_first = "fn a() { let = ; }\nfn b() {}\nfn c() {}\n";
        let tree = parse_within(
            Language::Rust,
            broken_first,
            Limit::Time(Duration::from_secs(5)),
        )
        .unwrap();
        assert!(has_error_before_last_item(tree.root_node()));
        let open_last = "fn a() {}\nfn b() {\n    let x = 1;\n";
        let tree = parse_within(
            Language::Rust,
            open_last,
            Limit::Time(Duration::from_secs(5)),
        )
        .unwrap();
        assert!(tree.root_node().has_error());
        assert!(!has_error_before_last_item(tree.root_node()));
    }

    #[test]
    fn a_clean_parse_and_an_empty_tree_have_no_error_before_the_last_item() {
        for text in ["", "fn a() {}\nfn b() {}\n"] {
            let tree =
                parse_within(Language::Rust, text, Limit::Time(Duration::from_secs(5))).unwrap();
            assert!(!has_error_before_last_item(tree.root_node()));
        }
    }
}
