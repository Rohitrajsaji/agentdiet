//! Signature folding: tree-sitter parsed, statically linked grammars, scrubbed-copy line numbers.
//!
//! Every fixture parses cleanly before folding, the folded view parses cleanly afterwards, every
//! signature that is not nested inside another function survives, and every line outside a folded
//! range comes back verbatim. The fixtures were written for this project.

mod common;

#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Command, Stdio};

use agentdiet::diet::{
    Candidate, Disposition, Engine, Extent, FoldStyle, Input, Layout, Piece, Settings, Source,
    Transform,
};
use agentdiet::folding::{Language, count_lines, fold, parse};
#[cfg(unix)]
use agentdiet::scrubber::scrub;
use agentdiet::state::Mode;
use common::Env;
use regex::Regex;
use tree_sitter::Node;

const FIXTURES: [(&str, Language); 5] = [
    ("inventory.rs", Language::Rust),
    ("pricing.py", Language::Python),
    ("cart.js", Language::JavaScript),
    ("store.ts", Language::TypeScript),
    ("Table.tsx", Language::Tsx),
];

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/folding/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// Render a candidate the way the engine does, with `id`.
fn render(c: &Candidate, id: &str) -> String {
    let Layout::Folded { pieces } = &c.layout else {
        panic!("expected a folded layout")
    };
    let mut out = Vec::new();
    for p in pieces {
        match p {
            Piece::Text(t) => out.extend_from_slice(t),
            Piece::Fold {
                first_line,
                last_line,
                style,
            } => out.extend_from_slice(style.marker(*first_line, *last_line, id).as_bytes()),
        }
    }
    String::from_utf8(out).unwrap()
}

fn marker_re() -> Regex {
    Regex::new(r"folded L(\d+)-(\d+): agentdiet show '#([0-9a-f]{8,64})' --lines (\d+)\.\.(\d+)")
        .unwrap()
}

#[derive(Debug)]
struct Marker {
    first: usize,
    last: usize,
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "read by the Unix-only recovery tests")
    )]
    id: String,
}

fn markers(text: &str) -> Vec<Marker> {
    marker_re()
        .captures_iter(text)
        .map(|c| {
            let n = |i: usize| c[i].parse::<usize>().unwrap();
            assert_eq!(
                (n(1), n(2)),
                (n(4), n(5)),
                "the range in the text and the command agree"
            );
            Marker {
                first: n(1),
                last: n(2),
                id: c[3].to_string(),
            }
        })
        .collect()
}

const FUNCTION_KINDS: [&str; 7] = [
    "function_item",
    "function_declaration",
    "generator_function_declaration",
    "function_expression",
    "function",
    "arrow_function",
    "method_definition",
];

/// `(signature text, nested inside another function?)` for every function with a body.
fn signatures(language: Language, text: &str) -> Vec<(String, bool)> {
    let tree = parse(language, text).unwrap_or_else(|| panic!("does not parse cleanly:\n{text}"));
    fn is_fn(language: Language, n: Node<'_>) -> bool {
        if language == Language::Python {
            n.kind() == "function_definition"
        } else {
            FUNCTION_KINDS.contains(&n.kind())
        }
    }
    fn walk(
        language: Language,
        node: Node<'_>,
        text: &str,
        inside: bool,
        out: &mut Vec<(String, bool)>,
    ) {
        let mut here = inside;
        if is_fn(language, node)
            && let Some(body) = node.child_by_field_name("body")
        {
            out.push((
                text[node.start_byte()..body.start_byte()]
                    .trim()
                    .to_string(),
                inside,
            ));
            here = true;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk(language, child, text, here, out);
        }
    }
    let mut out = Vec::new();
    walk(language, tree.root_node(), text, false, &mut out);
    out
}

/// Check everything that must hold between a file and its folded view.
fn check_view(language: Language, original: &str, folded: &str) -> Vec<Marker> {
    let found = markers(folded);
    let total = count_lines(original.as_bytes());

    // Ranges are sane, ordered and disjoint.
    let mut previous_end = 0;
    for m in &found {
        assert!(
            1 <= m.first && m.first <= m.last && m.last <= total,
            "{m:?} (file has {total} lines)"
        );
        assert!(
            m.first > previous_end,
            "overlapping or unordered ranges at {m:?}"
        );
        previous_end = m.last;
    }

    // The view parses; no top-level signature was lost; nothing was invented.
    let before = signatures(language, original);
    let after = signatures(language, folded);
    let kept: Vec<&String> = before
        .iter()
        .filter(|(_, nested)| !nested)
        .map(|(s, _)| s)
        .collect();
    let mut at = 0;
    for sig in &kept {
        let pos = after[at..].iter().position(|(s, _)| s == *sig);
        assert!(pos.is_some(), "lost the signature {sig:?}");
        at += pos.unwrap() + 1;
    }
    for (sig, _) in &after {
        assert!(
            before.iter().any(|(s, _)| s == sig),
            "invented signature {sig:?}"
        );
    }

    // Lines outside the ranges come back verbatim and in order; boundary lines keep their
    // surrounding text.
    let orig: Vec<&str> = original.split_inclusive('\n').collect();
    let out: Vec<&str> = folded.split_inclusive('\n').collect();
    let re = marker_re();
    let (mut oi, mut oo) = (0usize, 0usize);
    for m in &found {
        while oi < m.first - 1 {
            assert_eq!(
                out[oo],
                orig[oi],
                "line {} changed outside any range",
                oi + 1
            );
            oi += 1;
            oo += 1;
        }
        let line = out[oo];
        let span = re.find(line).expect("the marker line");
        let (before_marker, after_marker) = (&line[..span.start()], &line[span.end()..]);
        if language == Language::Python {
            assert!(before_marker.ends_with("...  # "), "{line:?}");
            let indent = before_marker.trim_end_matches("...  # ");
            assert!(
                indent.chars().all(|c| c == ' ' || c == '\t')
                    && orig[m.first - 1].starts_with(indent)
            );
        } else {
            let prefix = before_marker.strip_suffix("{ /* ").expect("brace marker");
            let rest = after_marker
                .strip_prefix(" */ }")
                .expect("brace marker end");
            assert!(
                orig[m.first - 1].starts_with(prefix),
                "{:?} vs {prefix:?}",
                orig[m.first - 1]
            );
            assert!(
                orig[m.last - 1].ends_with(rest),
                "{:?} vs {rest:?}",
                orig[m.last - 1]
            );
        }
        oi = m.last;
        oo += 1;
    }
    assert_eq!(&out[oo..], &orig[oi..], "the tail after the last range");
    found
}

fn candidate(name: &str, language: Language, mode: Mode) -> Option<(String, Candidate)> {
    let text = fixture(name);
    assert!(
        parse(language, &text).is_some(),
        "{name} must parse cleanly before folding"
    );
    let c = fold(name, Extent::Complete, text.as_bytes(), mode)?;
    Some((text, c))
}

// ---- fixtures ---------------------------------------------------------------------------------

#[test]
fn every_fixture_folds_into_a_valid_smaller_view_in_balanced_and_ultra_lean() {
    for (name, language) in FIXTURES {
        for mode in [Mode::Balanced, Mode::UltraLean] {
            let (original, c) = candidate(name, language, mode)
                .unwrap_or_else(|| panic!("{name} {mode:?}: nothing folded"));
            assert_eq!(c.kind, Transform::Folded);
            let folded = render(&c, "abcdef12");
            assert!(folded.len() < original.len(), "{name} {mode:?}");
            let found = check_view(language, &original, &folded);
            assert!(!found.is_empty());
            let (kept, total) = c.lines.unwrap();
            assert_eq!(total, count_lines(original.as_bytes()));
            assert_eq!(
                kept,
                count_lines(folded.as_bytes()),
                "{name} {mode:?}: reported kept lines"
            );
        }
    }
}

#[test]
fn lighter_modes_fold_no_more_than_heavier_ones() {
    for (name, language) in FIXTURES {
        let count = |mode| {
            candidate(name, language, mode)
                .map_or(0, |(_, c)| markers(&render(&c, "abcdef12")).len())
        };
        let (light, balanced, ultra) = (
            count(Mode::Light),
            count(Mode::Balanced),
            count(Mode::UltraLean),
        );
        assert!(
            light <= balanced && balanced <= ultra,
            "{name}: {light} {balanced} {ultra}"
        );
        assert!(ultra > 0, "{name}");
    }
}

#[test]
fn declarations_attributes_docs_and_docstrings_survive_in_every_language() {
    let keep = |name: &str, language: Language, needles: &[&str]| {
        let (_, c) = candidate(name, language, Mode::Balanced).unwrap();
        let folded = render(&c, "abcdef12");
        for n in needles {
            assert!(folded.contains(n), "{name}: lost {n:?}");
        }
    };
    keep(
        "inventory.rs",
        Language::Rust,
        &[
            "//! Inventory bookkeeping for the warehouse service.",
            "#[derive(Debug, Clone, PartialEq)]",
            "pub struct Item {",
            "    /// Total value of the stock held, in cents.",
            "    ///\n    /// Saturates instead of overflowing.",
            "    pub fn value_cents(&self) -> u64 {",
            "    #[inline(never)]\n    pub fn reconcile_stage_3(&mut self, factor: u64) -> Result<u64, String> {",
            "pub trait Audit {",
            "    fn name(&self) -> &str;",
            "pub enum Change {",
            "pub const MAX_QUANTITY: u32 = 10_000;",
            "#[cfg(test)]\nmod tests {",
        ],
    );
    keep(
        "pricing.py",
        Language::Python,
        &[
            "\"\"\"Order pricing helpers.\"\"\"",
            "@dataclasses.dataclass\nclass Line:",
            "        \"\"\"Pricing stage 4.\"\"\"\n",
            "        \"\"\"Return a short description.\n\n        The format is stable because the receipts parse it.\n        \"\"\"",
            "    def only_a_docstring(self):\n        \"\"\"Nothing but documentation.\"\"\"",
            "    def one_liner(self): return self.quantity",
            "    @property\n    def total(self) -> float:",
            "async def fetch_prices(skus):",
            "TAX_RATE = 0.2",
            "if __name__ == \"__main__\":",
        ],
    );
    keep(
        "cart.js",
        Language::JavaScript,
        &[
            "// Shopping cart logic.",
            "export class Cart {",
            "  stage2(factor = 1) {",
            "  async load(fetchLine) {",
            "export function helper3(values) {",
            "export const double = (x) => x * 2;",
            "  get total() {",
        ],
    );
    keep(
        "store.ts",
        Language::TypeScript,
        &[
            "export interface Item {",
            "export type Change =",
            "export enum Level {",
            "  abstract name(): string;",
            "export function parse(input: string): number;",
            "  public stage3(factor: number): number {",
            "export const identity = <T,>(x: T): T => x;",
        ],
    );
    keep(
        "Table.tsx",
        Language::Tsx,
        &[
            "interface Props {",
            "export function Table({ rows, onSelect }: Props) {",
            "export function useCounter3(start: number) {",
        ],
    );
}

#[test]
fn python_keeps_the_docstring_and_the_body_stays_valid_at_its_indentation() {
    let (original, c) = candidate("pricing.py", Language::Python, Mode::Balanced).unwrap();
    let folded = render(&c, "abcdef12");
    let found = check_view(Language::Python, &original, &folded);
    // `stage_1` has a docstring on line N and statements from N+1: the range starts after it.
    let stage = original
        .lines()
        .position(|l| l.contains("def stage_1"))
        .unwrap();
    let docstring_line = stage + 2; // 1-based line of the docstring
    let m = found
        .iter()
        .find(|m| m.first == docstring_line + 1)
        .expect("range after the docstring");
    assert!(
        original
            .lines()
            .nth(m.first - 1)
            .unwrap()
            .trim_start()
            .starts_with("subtotal = 0.0")
    );
    assert!(
        folded.contains("        ...  # folded L"),
        "marker keeps the body's indentation"
    );
    // A docstring-only function and a one-line function have nothing to fold.
    assert!(!found.iter().any(|m| {
        original
            .lines()
            .nth(m.first - 2)
            .is_some_and(|l| l.contains("def only_a_docstring") || l.contains("def one_liner"))
    }));
}

#[test]
fn nested_functions_are_not_folded_twice() {
    let mut src = String::from("fn outer() {\n");
    for i in 0..4 {
        src.push_str(&format!(
            "    let before_{i} = compute_something_rather_long(argument_one, argument_two);\n"
        ));
    }
    src.push_str("    fn inner() {\n");
    for i in 0..12 {
        src.push_str(&format!(
            "        let inside_{i} = compute_something_rather_long(argument_one, argument_two);\n"
        ));
    }
    src.push_str("    }\n    inner();\n}\n");
    while count_lines(src.as_bytes()) <= 305 {
        src.push_str("// padding line to get past the 300 line minimum for folding\n");
    }
    let c = fold("a.rs", Extent::Complete, src.as_bytes(), Mode::UltraLean).unwrap();
    let folded = render(&c, "abcdef12");
    let found = check_view(Language::Rust, &src, &folded);
    assert_eq!(
        found.len(),
        1,
        "one replacement for the outer body: {folded}"
    );
    assert!(!folded.contains("inside_3"));
    assert!(
        !folded.contains("fn inner"),
        "the nested function is inside the folded body"
    );
}

// ---- thresholds -------------------------------------------------------------------------------

fn rust_with_body_spans(spans: &[usize]) -> String {
    let mut src = String::new();
    for (n, span) in spans.iter().enumerate() {
        src.push_str(&format!("fn f{n}() {{\n"));
        for k in 0..span.saturating_sub(2) {
            src.push_str(&format!("    let value_{n}_{k} = compute_something_rather_long(argument_one, argument_two);\n"));
        }
        src.push_str("}\n");
    }
    while count_lines(src.as_bytes()) <= 305 {
        src.push_str("// padding line to get past the 300 line minimum for folding\n");
    }
    src
}

#[test]
fn the_mode_decides_the_smallest_body_that_is_folded() {
    // (mode, body spans that must fold, spans that must stay): a body spans the brace lines too.
    let table = [
        (Mode::Light, 20usize, 19usize),
        (Mode::Balanced, 8, 7),
        (Mode::UltraLean, 3, 2),
    ];
    for (mode, folds, stays) in table {
        let src = rust_with_body_spans(&[folds, stays]);
        let c = fold("t.rs", Extent::Complete, src.as_bytes(), mode)
            .unwrap_or_else(|| panic!("{mode:?}: a body of {folds} lines folds"));
        let found = markers(&render(&c, "abcdef12"));
        assert_eq!(found.len(), 1, "{mode:?}: only the {folds}-line body");
        assert_eq!((found[0].first, found[0].last), (1, folds), "{mode:?}");
        // And alone, the smaller one is not worth folding.
        let only_small = rust_with_body_spans(&[stays]);
        assert!(
            fold("t.rs", Extent::Complete, only_small.as_bytes(), mode).is_none(),
            "{mode:?}"
        );
    }
}

#[test]
fn a_body_is_folded_only_if_its_marker_is_shorter() {
    // Three-line bodies of a few bytes each are below the marker's size in every mode.
    let mut src = String::new();
    for n in 0..120 {
        src.push_str(&format!("fn f{n}() {{\n    a();\n}}\n"));
    }
    assert!(fold("t.rs", Extent::Complete, src.as_bytes(), Mode::UltraLean).is_none());
}

#[test]
fn files_of_300_lines_or_fewer_are_never_folded() {
    let body = |lines: usize| {
        let mut src = String::from("fn big() {\n");
        for k in 0..lines.saturating_sub(2) {
            src.push_str(&format!(
                "    let value_{k} = compute_something_rather_long(argument_one, argument_two);\n"
            ));
        }
        src.push_str("}\n");
        src
    };
    let at_300 = body(300);
    assert_eq!(count_lines(at_300.as_bytes()), 300);
    assert!(fold("t.rs", Extent::Complete, at_300.as_bytes(), Mode::UltraLean).is_none());
    let at_301 = body(301);
    assert_eq!(count_lines(at_301.as_bytes()), 301);
    assert!(fold("t.rs", Extent::Complete, at_301.as_bytes(), Mode::UltraLean).is_some());
    // A missing final newline still counts the last line.
    let no_nl = at_301.trim_end_matches('\n').to_string();
    assert_eq!(count_lines(no_nl.as_bytes()), 301);
    assert!(fold("t.rs", Extent::Complete, no_nl.as_bytes(), Mode::UltraLean).is_some());
}

#[test]
fn every_language_folds_a_long_body_in_light_mode() {
    let long = |lang: &str| -> String {
        let (open, stmt, close, pad) = match lang {
            "rs" => (
                "fn big() {",
                "    let value = compute_something_rather_long(a, b);",
                "}",
                "// padding line to get past the 300 line minimum",
            ),
            "js" | "ts" | "tsx" => (
                "function big() {",
                "  const value = computeSomethingRatherLong(a, b);",
                "}",
                "// padding line to get past the 300 line minimum",
            ),
            _ => (
                "def big():",
                "    value = compute_something_rather_long(a, b)",
                "",
                "# padding line to get past the 300 line minimum",
            ),
        };
        let mut src = format!("{open}\n");
        for _ in 0..30 {
            src.push_str(stmt);
            src.push('\n');
        }
        if !close.is_empty() {
            src.push_str(close);
            src.push('\n');
        }
        while src.lines().count() <= 305 {
            src.push_str(pad);
            src.push('\n');
        }
        src
    };
    for ext in ["rs", "py", "js", "ts", "tsx"] {
        let src = long(ext);
        let path = format!("dir/file.{ext}");
        let c = fold(&path, Extent::Complete, src.as_bytes(), Mode::Light)
            .unwrap_or_else(|| panic!("{ext}"));
        check_view(
            Language::from_path(&path).unwrap(),
            &src,
            &render(&c, "abcdef12"),
        );
    }
}

// ---- inputs that must be left alone ----------------------------------------------------------

#[test]
fn unsupported_or_doubtful_input_is_left_alone() {
    let rs = fixture("inventory.rs");
    // A ranged read is not the file: never folded, whatever the content.
    assert!(
        fold(
            "inventory.rs",
            Extent::Ranged,
            rs.as_bytes(),
            Mode::UltraLean
        )
        .is_none()
    );
    // Unknown extensions, no extension, a directory named like a file.
    for path in [
        "inventory.txt",
        "inventory",
        "Makefile",
        "inventory.rs.bak",
        "inventory.rb",
        "inventory.go",
        ".rs",
        "dir.rs/file",
    ] {
        assert!(
            fold(path, Extent::Complete, rs.as_bytes(), Mode::UltraLean).is_none(),
            "{path}"
        );
    }
    // Case-insensitive extensions are folded.
    assert!(
        fold(
            "INVENTORY.RS",
            Extent::Complete,
            rs.as_bytes(),
            Mode::UltraLean
        )
        .is_some()
    );
    // Not UTF-8.
    let mut bad = rs.clone().into_bytes();
    bad[40] = 0xff;
    assert!(fold("inventory.rs", Extent::Complete, &bad, Mode::UltraLean).is_none());
    // Syntax errors anywhere.
    let broken = rs.replacen("    }\n}\n", "    }\n", 1);
    assert!(
        fold(
            "inventory.rs",
            Extent::Complete,
            broken.as_bytes(),
            Mode::UltraLean
        )
        .is_none()
    );
    let broken_py = fixture("pricing.py").replacen(
        "        return f\"{label}: {euros}.{cents:02d}\"",
        "        return f\"{label}: {euros}.{cents:02d}\" +",
        1,
    );
    assert!(
        fold(
            "pricing.py",
            Extent::Complete,
            broken_py.as_bytes(),
            Mode::UltraLean
        )
        .is_none()
    );
    // Terminal escape sequences or other control characters: not source, not even parsed.
    let with_escape = rs.replacen("pub struct Item {", "pub struct Item {\u{1b}[0m", 1);
    assert!(
        fold(
            "inventory.rs",
            Extent::Complete,
            with_escape.as_bytes(),
            Mode::UltraLean
        )
        .is_none()
    );
    // Empty and tiny.
    assert!(fold("a.rs", Extent::Complete, b"", Mode::UltraLean).is_none());
    assert!(fold("a.rs", Extent::Complete, b"fn main() {}\n", Mode::UltraLean).is_none());
    // The wrong language for the extension fails to parse cleanly.
    assert!(
        fold(
            "inventory.py",
            Extent::Complete,
            rs.as_bytes(),
            Mode::UltraLean
        )
        .is_none()
    );
}

#[test]
fn crlf_files_fold_and_keep_their_line_endings() {
    for (name, language) in FIXTURES {
        let crlf = fixture(name).replace('\n', "\r\n");
        let c = fold(name, Extent::Complete, crlf.as_bytes(), Mode::Balanced)
            .unwrap_or_else(|| panic!("{name}"));
        let folded = render(&c, "abcdef12");
        // Every line terminator outside the markers is still CRLF; a marker adds none of its own.
        assert!(
            !folded.replace("\r\n", "").contains('\n'),
            "{name}: a bare LF appeared"
        );
        assert!(folded.len() < crlf.len());
        assert!(parse(language, &folded).is_some(), "{name}");
        assert_eq!(count_lines(folded.as_bytes()), c.lines.unwrap().0);
    }
}

#[test]
fn multibyte_text_in_bodies_and_around_them_is_handled() {
    let mut src = String::from("/// 日本語のドキュメント 😀\nfn greet() {\n");
    for k in 0..10 {
        src.push_str(&format!("    let s{k} = \"héllo wörld 😀 日本語 {k}\".to_string() + &compute_something_long(a, b);\n"));
    }
    src.push_str("}\n");
    while count_lines(src.as_bytes()) <= 305 {
        src.push_str("// padding ünïcödé line to get past the 300 line minimum for folding\n");
    }
    let c = fold("g.rs", Extent::Complete, src.as_bytes(), Mode::Balanced).unwrap();
    let folded = render(&c, "abcdef12");
    assert!(
        folded.starts_with("/// 日本語のドキュメント 😀\nfn greet() { /* folded L2-13:"),
        "{folded}"
    );
    check_view(Language::Rust, &src, &folded);
}

#[test]
fn hostile_nesting_is_handled_without_recursion_or_a_long_stall() {
    let mut src = String::from("fn deep() {\n");
    for _ in 0..4000 {
        src.push_str("{ ");
    }
    src.push_str("let x = 1;");
    for _ in 0..4000 {
        src.push_str(" }");
    }
    src.push_str("\n}\n");
    while count_lines(src.as_bytes()) <= 305 {
        src.push_str("// padding line to get past the 300 line minimum for folding\n");
    }
    let started = std::time::Instant::now();
    let result = fold("deep.rs", Extent::Complete, src.as_bytes(), Mode::UltraLean);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
    if let Some(c) = result {
        parse(Language::Rust, &render(&c, "abcdef12")).expect("the view parses");
    }
}

#[test]
fn mutated_files_never_panic_and_a_fold_always_yields_a_parsable_view() {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move |n: usize| {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        (state.wrapping_mul(0x2545_f491_4f6c_dd1d) % n as u64) as usize
    };
    let (mut folded_count, mut declined) = (0, 0);
    for (name, language) in FIXTURES {
        let original = fixture(name).into_bytes();
        for round in 0..60 {
            let mut bytes = original.clone();
            match round % 5 {
                0 => {
                    let i = next(bytes.len());
                    bytes.remove(i);
                }
                1 => {
                    let i = next(bytes.len());
                    bytes.insert(i, *b"{}()[];:\"'`\\#,<>/*".get(next(18)).unwrap());
                }
                2 => bytes.truncate(next(bytes.len())),
                3 => {
                    let a = next(bytes.len());
                    let b = (a + next(400)).min(bytes.len());
                    bytes.drain(a..b);
                }
                _ => {
                    let i = next(bytes.len());
                    bytes[i] = next(256) as u8;
                }
            }
            match fold(name, Extent::Complete, &bytes, Mode::UltraLean) {
                None => declined += 1,
                Some(c) => {
                    folded_count += 1;
                    let text = String::from_utf8(bytes).expect("only valid UTF-8 is folded");
                    let view = render(&c, "abcdef12");
                    assert!(
                        parse(language, &view).is_some(),
                        "{name} round {round}: the view does not parse\n{view}"
                    );
                    check_view(language, &text, &view);
                }
            }
        }
    }
    assert!(
        folded_count > 20 && declined > 20,
        "{folded_count} folded, {declined} declined"
    );
}

// ---- through the engine, with the real recovery command ---------------------------------------

#[cfg(unix)]
fn bin_dir(env: &Env) -> PathBuf {
    let dir = env.dir.path().join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    let link = dir.join("agentdiet");
    if !link.exists() {
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_agentdiet"), &link).unwrap();
    }
    dir
}

#[cfg(unix)]
fn sh(env: &Env, command: &str) -> Vec<u8> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(command)
        .env_clear()
        .env("HOME", &env.paths.home)
        .env("USERPROFILE", &env.paths.home)
        .env("XDG_CONFIG_HOME", &env.paths.config_dir)
        .env("APPDATA", &env.paths.config_dir)
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir(env).display()))
        .current_dir(&env.paths.project_root)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{command}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[cfg(unix)]
fn aws_key() -> String {
    ["AK", "IA"].concat() + &"Q7".repeat(8)
}

fn run_engine(
    env: &Env,
    path: &str,
    extent: Extent,
    bytes: &[u8],
    settings: Settings,
) -> agentdiet::diet::Outcome {
    let src = Source::Read {
        path: path.to_string(),
        extent,
    };
    Engine::new(&env.paths).run(
        &Input {
            bytes,
            source: &src,
        },
        &settings,
    )
}

#[cfg(unix)]
#[test]
fn markers_carry_the_real_id_and_each_literal_command_returns_exactly_the_folded_lines() {
    let env = Env::empty();
    let original = fixture("inventory.rs");
    let out = run_engine(
        &env,
        "src/inventory.rs",
        Extent::Complete,
        original.as_bytes(),
        Settings::new(Mode::Balanced),
    );
    assert_eq!(out.disposition, Disposition::Replaced(Transform::Folded));
    let id = out.id.clone().unwrap();
    assert!(id.len() >= 8);
    let text = String::from_utf8(out.output.clone()).unwrap();
    assert!(
        !text.contains("🥗 Diet:"),
        "folded output has markers, not a trailing breadcrumb"
    );
    assert!(text.len() < original.len());

    let found = markers(&text);
    assert!(found.len() >= 10);
    assert!(
        found.iter().all(|m| m.id == id),
        "every marker carries the stored ID"
    );
    let lines: Vec<&str> = original.split_inclusive('\n').collect();
    for m in &found {
        let literal = format!("agentdiet show '#{id}' --lines {}..{}", m.first, m.last);
        assert!(
            text.contains(&literal),
            "the literal command is in the text: {literal}"
        );
        let shown = sh(&env, &literal);
        let want: String = lines[m.first - 1..m.last].concat();
        assert_eq!(String::from_utf8(shown).unwrap(), want, "{literal}");
    }
    // The whole file comes back too.
    assert_eq!(
        sh(&env, &format!("agentdiet show '#{id}'")),
        original.as_bytes()
    );
}

#[cfg(unix)]
#[test]
fn a_multi_line_redaction_shifts_the_numbers_and_markers_use_the_scrubbed_copy() {
    let env = Env::empty();
    let pem_body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7VJTUt9Us8cKj\n".repeat(6);
    let key_block = format!(
        "const KEY: &str = \"-----BEGIN PRIVATE KEY-----\n{pem_body}-----END PRIVATE KEY-----\";\nconst NOTE: &str = \"id={}\";\n\n",
        aws_key()
    );
    let on_disk = format!("{key_block}{}", fixture("inventory.rs"));
    let scrubbed = scrub(on_disk.as_bytes());
    assert!(scrubbed.redactions() >= 2);
    let scrubbed_text = scrubbed.to_lossy_string();
    let shift = count_lines(on_disk.as_bytes()) - count_lines(scrubbed_text.as_bytes());
    assert!(
        shift >= 7,
        "the private key spans several lines: shift {shift}"
    );

    let out = run_engine(
        &env,
        "src/inventory.rs",
        Extent::Complete,
        on_disk.as_bytes(),
        Settings::new(Mode::Balanced),
    );
    assert_eq!(out.disposition, Disposition::Replaced(Transform::Folded));
    let text = String::from_utf8(out.output.clone()).unwrap();
    assert!(!text.contains(&aws_key()) && !text.contains("MIIEvQ"));
    let id = out.id.clone().unwrap();
    let found = markers(&text);
    let scrubbed_lines: Vec<&str> = scrubbed_text.split_inclusive('\n').collect();
    let disk_lines: Vec<&str> = on_disk.split_inclusive('\n').collect();
    let mut differs_from_disk = 0;
    for m in &found {
        let shown = String::from_utf8(sh(
            &env,
            &format!("agentdiet show '#{id}' --lines {}..{}", m.first, m.last),
        ))
        .unwrap();
        assert_eq!(
            shown,
            scrubbed_lines[m.first - 1..m.last].concat(),
            "markers index the stored (scrubbed) copy"
        );
        if disk_lines.get(m.first - 1) != scrubbed_lines.get(m.first - 1) {
            differs_from_disk += 1;
        }
    }
    assert!(
        differs_from_disk >= found.len() / 2,
        "the on-disk numbers would have pointed elsewhere"
    );
}

#[test]
fn ranged_reads_paused_and_bypassed_reads_are_not_folded() {
    let env = Env::empty();
    let original = fixture("inventory.rs");
    let ranged = run_engine(
        &env,
        "src/inventory.rs",
        Extent::Ranged,
        original.as_bytes(),
        Settings::new(Mode::UltraLean),
    );
    assert_eq!(ranged.output, original.as_bytes());
    assert_eq!(ranged.disposition, Disposition::Unchanged);
    let paused = run_engine(
        &env,
        "src/inventory.rs",
        Extent::Complete,
        original.as_bytes(),
        Settings {
            paused: true,
            ..Settings::new(Mode::UltraLean)
        },
    );
    assert_eq!(paused.disposition, Disposition::Paused);
    // Untouched, apart from the paused notice a long output earns (SPEC §2.3).
    let notice = format!("{}\n", agentdiet::diet::PAUSED_NOTICE);
    assert_eq!(paused.output, format!("{original}{notice}").as_bytes());
    assert!(
        !env.paths.cas_dir().exists(),
        "none of these touched the Vault"
    );
}

#[test]
fn folding_ignores_the_token_minimum_but_still_needs_a_real_saving() {
    // A small file is skipped for its line count, a large one folds regardless of the token floor.
    let env = Env::empty();
    let tiny = "fn main() {}\n";
    let out = run_engine(
        &env,
        "a.rs",
        Extent::Complete,
        tiny.as_bytes(),
        Settings::new(Mode::UltraLean),
    );
    assert_eq!(out.output, tiny.as_bytes());
    // A 300+ line file whose only function body saves less than the mode demands is unchanged.
    let mut src = String::from("fn big() {\n");
    for k in 0..4 {
        src.push_str(&format!(
            "    let value_{k} = compute_something_rather_long(argument_one, argument_two);\n"
        ));
    }
    src.push_str("}\n");
    while count_lines(src.as_bytes()) <= 320 {
        src.push_str("// padding line to get past the 300 line minimum for folding\n");
    }
    let out = run_engine(
        &env,
        "big.rs",
        Extent::Complete,
        src.as_bytes(),
        Settings::new(Mode::Light),
    );
    assert_eq!(
        out.output,
        src.as_bytes(),
        "a 4-line body in a 320-line file does not meet 40%"
    );
}

#[test]
fn fold_marker_text_matches_the_spec_for_both_styles() {
    assert_eq!(
        FoldStyle::Brace.marker(120, 188, "4f9a1b2c"),
        "{ /* folded L120-188: agentdiet show '#4f9a1b2c' --lines 120..188 */ }"
    );
    assert_eq!(
        FoldStyle::Python.marker(120, 188, "4f9a1b2c"),
        "...  # folded L120-188: agentdiet show '#4f9a1b2c' --lines 120..188"
    );
}

// ---- bounded parse ----------------------------------------------------------------------------

/// A source whose first top-level item is several KiB long, so the 2 KiB probe cuts through it.
/// `pad` shifts where the cut falls.
fn long_first_item(language: Language, pad: usize) -> String {
    let lead = "x".repeat(pad);
    let mut src = String::new();
    match language {
        Language::Rust => {
            src.push_str(&format!(
                "// {lead}\nuse std::collections::BTreeMap;\n\nimpl Store {{\n"
            ));
            for n in 0..30 {
                src.push_str(&format!(
                    "    pub fn method_{n}(&self, input: &BTreeMap<String, u64>) -> u64 {{\n"
                ));
                for k in 0..14 {
                    src.push_str(&format!(
                        "        let value_{n}_{k} = compute_something_rather_long(input, {k});\n"
                    ));
                }
                src.push_str("        0\n    }\n\n");
            }
            src.push_str("}\n");
        }
        Language::Python => {
            src.push_str(&format!("# {lead}\nclass Store:\n"));
            for n in 0..30 {
                src.push_str(&format!("    def method_{n}(self, items):\n"));
                src.push_str("        message = \"\"\"stage\n        a multi-line string with def inside: def fake(): pass\n        \"\"\"\n");
                for k in 0..12 {
                    src.push_str(&format!(
                        "        value_{n}_{k} = compute_something_rather_long(items, {k})\n"
                    ));
                }
                src.push_str("        return message\n\n");
            }
        }
        _ => {
            src.push_str(&format!("// {lead}\nexport class Store {{\n"));
            for n in 0..30 {
                src.push_str(&format!("  method{n}(input) {{\n"));
                for k in 0..14 {
                    src.push_str(&format!(
                        "    const value_{n}_{k} = computeSomethingRatherLong(input, {k});\n"
                    ));
                }
                src.push_str("    return 0;\n  }\n\n");
            }
            src.push_str("}\n");
        }
    }
    src
}

#[test]
fn a_function_or_class_that_spans_the_probe_cut_is_still_folded() {
    for (name, language) in [
        ("store.rs", Language::Rust),
        ("store.py", Language::Python),
        ("store.js", Language::JavaScript),
        ("store.ts", Language::TypeScript),
    ] {
        for pad in [0, 1, 5, 17, 33, 60] {
            let src = long_first_item(language, pad);
            assert!(
                src.len() > 8 * 2048,
                "{name}: the first item must span the probe's cut"
            );
            assert!(parse(language, &src).is_some(), "{name} pad {pad} is valid");
            let folded = fold(name, Extent::Complete, src.as_bytes(), Mode::UltraLean)
                .unwrap_or_else(|| panic!("{name} pad {pad} was not folded"));
            let view = render(&folded, "abcdef12");
            assert!(view.len() < src.len() / 2, "{name} pad {pad}");
            parse(language, &view).unwrap_or_else(|| panic!("{name} pad {pad}: view must parse"));
        }
    }
}

#[test]
fn text_that_is_not_source_is_left_unchanged_in_every_language() {
    let mut text = String::new();
    let mut i = 0;
    while text.len() < 256 * 1024 {
        text.push_str(&format!("{i}: [{}] {{{}}} ({})\n", i * 7, i * 13, i % 5));
        i += 1;
    }
    for name in ["x.rs", "x.py", "x.js", "x.ts", "x.tsx"] {
        assert!(
            fold(name, Extent::Complete, text.as_bytes(), Mode::UltraLean).is_none(),
            "{name}"
        );
    }
}
