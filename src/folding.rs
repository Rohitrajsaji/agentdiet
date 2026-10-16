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
//! non-UTF-8 bytes, any parse error, a parse that overruns [`PARSE_BUDGET`], or a body that is not
//! worth folding.

use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use tree_sitter::{Node, ParseOptions, Parser};

use crate::diet::{
    Candidate, Extent, FOLD_MIN_FILE_LINES, FoldStyle, Layout, ModeParams, Piece, Transform,
};
use crate::state::Mode;

/// A parse that takes longer than this is abandoned and the file is left alone.
pub const PARSE_BUDGET: Duration = Duration::from_millis(300);

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
    let mut parser = Parser::new();
    parser.set_language(&language.grammar()).ok()?;
    let deadline = Instant::now() + PARSE_BUDGET;
    let mut progress = |_: &tree_sitter::ParseState| {
        if Instant::now() >= deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut progress);
    let bytes = text.as_bytes();
    let tree = parser.parse_with_options(
        &mut |i, _| &bytes[i.min(bytes.len())..],
        None,
        Some(options),
    )?;
    (!tree.root_node().has_error()).then_some(tree)
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
