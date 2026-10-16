//! A conservative encoder for a subset of TOON (`toon-spec: 4.4`, pinned in SPEC §9).
//!
//! Input is one JSON document; output is the byte-exact TOON the normative spec prescribes, or a
//! [`Decline`]. The encoder **never emits a lesser form**: wherever the spec would require a form
//! this subset does not implement, it declines and the caller leaves the JSON alone.
//!
//! Implemented: a root object (the empty one is the empty document), a root primitive, a root
//! primitive or flat tabular array and the empty root array; nested and empty objects; primitives; `key: []`; inline primitive arrays `key[N]: a,b`;
//! flat tabular arrays `key[N]{f1,f2}:`. Comma delimiter, 2-space indent, LF, no trailing newline.
//!
//! Declined: list forms (arrays of arrays, mixed arrays, arrays with empty or non-uniform
//! objects), nested field groups, keyed tabular objects (spec §9.5, decided by the exact detection
//! rule), tabular candidates whose elements list the same keys in a different order, and any number that is not already canonical (§2): the number's
//! original text is emitted or nothing is.
//!
//! The JSON reader is `serde_json` with the `raw_value` feature, so each number keeps its original
//! lexeme. Duplicate keys, nesting deeper than [`MAX_DEPTH`], lone surrogates and trailing content
//! decline.

use std::collections::HashSet;

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;

use crate::diet::{Candidate, Layout, Transform};

/// JSON nesting deeper than this declines.
pub const MAX_DEPTH: usize = 64;
/// Each nested value is parsed again from its own text, so a document costs (about) its size times
/// its depth. Past this many passes over the input the document declines instead.
const MAX_PASSES: usize = 10;
/// ...but a document is always allowed this many bytes of parsing work, so small documents are
/// never refused for cost however deep (at most [`MAX_DEPTH`]) they nest.
const MIN_WORK: usize = 64 * 1024;
/// A number may have at most this many significant digits to be emitted.
const MAX_SIGNIFICANT_DIGITS: usize = 15;

/// Why a document was left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decline {
    /// Not one strict RFC 8259 document (syntax error, lone surrogate, BOM, trailing content).
    NotJson,
    DuplicateKey,
    TooDeep,
    /// Parsing would take more than [`MAX_PASSES`] passes over the input (deep nesting around a large
    /// value). Declining keeps the cost of this step bounded.
    Cost,
    /// A number whose text is not already canonical (exponent, `1.10`, `-0`, 16+ digits, ...).
    NumberForm,
    /// A form the spec requires but this subset does not implement.
    Form(&'static str),
}

#[derive(Debug)]
enum Node {
    Null,
    Bool(bool),
    /// The number's original text.
    Num(String),
    Str(String),
    Arr(Vec<Node>),
    Obj(Vec<(String, Node)>),
}

// ---- parsing ------------------------------------------------------------------------------

const DUPLICATE_KEY: &str = "agentdiet: duplicate key";

struct Pairs(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for Pairs {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Pairs;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Pairs, A::Error> {
                let mut seen: HashSet<String> = HashSet::new();
                let mut pairs = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    let value: Box<RawValue> = map.next_value()?;
                    if !seen.insert(key.clone()) {
                        return Err(de::Error::custom(DUPLICATE_KEY));
                    }
                    pairs.push((key, value));
                }
                Ok(Pairs(pairs))
            }
        }
        d.deserialize_map(V)
    }
}

struct Items(Vec<Box<RawValue>>);

impl<'de> Deserialize<'de> for Items {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Items;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON array")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Items, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element::<Box<RawValue>>()? {
                    items.push(item);
                }
                Ok(Items(items))
            }
        }
        d.deserialize_seq(V)
    }
}

fn map_error(e: &serde_json::Error) -> Decline {
    if e.to_string().contains(DUPLICATE_KEY) {
        Decline::DuplicateKey
    } else {
        Decline::NotJson
    }
}

/// Parse `raw` and, recursively, every value in it. `work` counts the bytes handed to the parser
/// so far and is checked against `limit`.
fn parse(raw: &str, depth: usize, work: &mut usize, limit: usize) -> Result<Node, Decline> {
    if depth > MAX_DEPTH {
        return Err(Decline::TooDeep);
    }
    *work += raw.len();
    if *work > limit {
        return Err(Decline::Cost);
    }
    let raw = raw.trim_matches([' ', '\t', '\n', '\r']);
    match raw.as_bytes().first() {
        Some(b'{') => {
            let Pairs(pairs) = serde_json::from_str(raw).map_err(|e| map_error(&e))?;
            pairs
                .into_iter()
                .map(|(k, v)| Ok((k, parse(v.get(), depth + 1, work, limit)?)))
                .collect::<Result<_, Decline>>()
                .map(Node::Obj)
        }
        Some(b'[') => {
            let Items(items) = serde_json::from_str(raw).map_err(|e| map_error(&e))?;
            items
                .into_iter()
                .map(|v| parse(v.get(), depth + 1, work, limit))
                .collect::<Result<_, Decline>>()
                .map(Node::Arr)
        }
        Some(b'"') => serde_json::from_str::<String>(raw)
            .map(Node::Str)
            .map_err(|_| Decline::NotJson),
        Some(b't') if raw == "true" => Ok(Node::Bool(true)),
        Some(b'f') if raw == "false" => Ok(Node::Bool(false)),
        Some(b'n') if raw == "null" => Ok(Node::Null),
        Some(b'-' | b'0'..=b'9') => Ok(Node::Num(raw.to_string())),
        _ => Err(Decline::NotJson),
    }
}

// ---- canonical numbers (spec §2) ------------------------------------------------------------

/// `true` if `lexeme` is already canonical: plain decimal, no exponent, no leading zeros, no
/// trailing fractional zeros, not `-0`, at most [`MAX_SIGNIFICANT_DIGITS`] significant digits,
/// and zero or at least 1e-6 (so also below 1e21). `10` and `100` are canonical; `1.50` and `1.0`
/// are not.
pub fn is_canonical_number(lexeme: &str) -> bool {
    let (negative, digits) = match lexeme.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, lexeme),
    };
    if digits.contains(['e', 'E', '+']) {
        return false;
    }
    let (int, frac) = match digits.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (digits, None),
    };
    let int_ok = int == "0"
        || (int.starts_with(|c: char| ('1'..='9').contains(&c))
            && int.bytes().all(|b| b.is_ascii_digit()));
    if !int_ok {
        return false;
    }
    if let Some(frac) = frac
        && (frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) || frac.ends_with('0'))
    {
        return false;
    }
    if negative && int == "0" && frac.is_none() {
        return false; // -0
    }
    // Below 1e-6 the spec allows exponent form, which this subset does not emit.
    if int == "0"
        && let Some(frac) = frac
        && frac.bytes().take_while(|b| *b == b'0').count() > 5
    {
        return false;
    }
    let significant = int.trim_start_matches('0').len()
        + frac.map_or(0, |f| {
            if int == "0" {
                f.trim_start_matches('0').len()
            } else {
                f.len()
            }
        });
    significant <= MAX_SIGNIFICANT_DIGITS
}

// ---- strings and keys (spec §7) -------------------------------------------------------------

fn is_numeric_like(s: &str) -> bool {
    // /^[+-]?[0-9]+(?:\.[0-9]+)?(?:e[+-]?[0-9]+)?$/i
    let b = s.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    let digits = |i: &mut usize| {
        let start = *i;
        while b.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i > start
    };
    if !digits(&mut i) {
        return false;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        if !digits(&mut i) {
            return false;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !digits(&mut i) {
            return false;
        }
    }
    i == b.len()
}

/// §7.2, with the comma as the document delimiter.
fn needs_quotes(s: &str) -> bool {
    s.is_empty()
        || s.starts_with([' ', '\t', '-', '#'])
        || s.ends_with([' ', '\t'])
        || matches!(s, "true" | "false" | "null")
        || is_numeric_like(s)
        || s.chars()
            .any(|c| matches!(c, ':' | '"' | '\\' | '[' | ']' | '{' | '}' | ',') || c < '\u{20}')
}

fn push_quoted(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\u{20}' => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn push_string(out: &mut String, s: &str) {
    if needs_quotes(s) {
        push_quoted(out, s);
    } else {
        out.push_str(s);
    }
}

/// §7.3: unquoted iff `^[A-Za-z_][A-Za-z0-9_.]*$`.
fn push_key(out: &mut String, key: &str) {
    let mut chars = key.chars();
    let plain = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if plain {
        out.push_str(key);
    } else {
        push_quoted(out, key);
    }
}

fn push_primitive(out: &mut String, node: &Node) -> Result<(), Decline> {
    match node {
        Node::Null => out.push_str("null"),
        Node::Bool(true) => out.push_str("true"),
        Node::Bool(false) => out.push_str("false"),
        Node::Num(lexeme) => {
            if !is_canonical_number(lexeme) {
                return Err(Decline::NumberForm);
            }
            out.push_str(lexeme);
        }
        Node::Str(s) => push_string(out, s),
        Node::Arr(_) | Node::Obj(_) => {
            return Err(Decline::Form("container where a primitive is needed"));
        }
    }
    Ok(())
}

fn is_primitive(node: &Node) -> bool {
    !matches!(node, Node::Arr(_) | Node::Obj(_))
}

// ---- shape detection (spec §9.3, §9.5) -------------------------------------------------------

type Fields = [(String, Node)];

fn key_set(fields: &Fields) -> Vec<&str> {
    let mut keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
    keys.sort_unstable();
    keys
}

/// Every object non-empty with the same key set, and every column uniform-primitive or
/// nested-uniform (recursively): the common test of tabular and keyed tabular detection.
fn uniform(objects: &[&Fields]) -> bool {
    let Some(first) = objects.first() else {
        return false;
    };
    if first.is_empty() {
        return false;
    }
    let keys = key_set(first);
    if objects.iter().any(|o| key_set(o) != keys) {
        return false;
    }
    keys.iter().all(|key| {
        let column: Vec<&Node> = objects
            .iter()
            .filter_map(|o| o.iter().find(|(k, _)| k == key).map(|(_, v)| v))
            .collect();
        if column.iter().all(|n| is_primitive(n)) {
            return true;
        }
        let nested: Option<Vec<&Fields>> = column
            .iter()
            .map(|n| match n {
                Node::Obj(f) => Some(f.as_slice()),
                _ => None,
            })
            .collect();
        nested.is_some_and(|n| uniform(&n))
    })
}

fn object_slices(items: &[Node]) -> Option<Vec<&Fields>> {
    items
        .iter()
        .map(|n| match n {
            Node::Obj(f) => Some(f.as_slice()),
            _ => None,
        })
        .collect()
}

/// Spec §9.5: an object with at least two entries, all of them non-empty objects with one shape.
fn keyed_tabular(fields: &Fields) -> bool {
    if fields.len() < 2 {
        return false;
    }
    let values: Option<Vec<&Fields>> = fields
        .iter()
        .map(|(_, v)| match v {
            Node::Obj(f) => Some(f.as_slice()),
            _ => None,
        })
        .collect();
    values.is_some_and(|v| uniform(&v))
}

// ---- encoding -------------------------------------------------------------------------------

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn line(out: &mut String, depth: usize) {
    if !out.is_empty() {
        out.push('\n');
    }
    indent(out, depth);
}

/// A header (`[N]` or `[N]{f1,f2}`) plus its content, for an array. `key` is `None` at the root.
fn encode_array(
    out: &mut String,
    depth: usize,
    key: Option<&str>,
    items: &[Node],
) -> Result<(), Decline> {
    let header = |out: &mut String| {
        line(out, depth);
        if let Some(key) = key {
            push_key(out, key);
        }
    };
    if items.is_empty() {
        header(out);
        match key {
            Some(_) => out.push_str(": []"),
            None => out.push_str("[]"),
        }
        return Ok(());
    }
    if items.iter().all(is_primitive) {
        header(out);
        out.push_str(&format!("[{}]: ", items.len()));
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            push_primitive(out, item)?;
        }
        return Ok(());
    }
    // Arrays of objects: tabular if the spec's detection holds, which this subset can write only
    // for flat columns listed in the same order by every element.
    let Some(objects) = object_slices(items) else {
        return Err(Decline::Form("list form: mixed or nested array"));
    };
    if !uniform(&objects) {
        return Err(Decline::Form("list form: objects that are not uniform"));
    }
    let first = objects[0];
    if first.iter().any(|(_, v)| !is_primitive(v)) {
        return Err(Decline::Form("nested field group"));
    }
    let order: Vec<&str> = first.iter().map(|(k, _)| k.as_str()).collect();
    if objects
        .iter()
        .any(|o| o.iter().map(|(k, _)| k.as_str()).ne(order.iter().copied()))
    {
        return Err(Decline::Form("tabular rows would reorder keys"));
    }
    header(out);
    out.push_str(&format!("[{}]{{", items.len()));
    for (i, (k, _)) in first.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_key(out, k);
    }
    out.push_str("}:");
    for object in &objects {
        line(out, depth + 1);
        for (i, (_, v)) in object.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            push_primitive(out, v)?;
        }
    }
    Ok(())
}

fn encode_fields(out: &mut String, depth: usize, fields: &Fields) -> Result<(), Decline> {
    for (key, value) in fields {
        match value {
            Node::Arr(items) => encode_array(out, depth, Some(key), items)?,
            Node::Obj(inner) => {
                if keyed_tabular(inner) {
                    return Err(Decline::Form("keyed tabular object"));
                }
                line(out, depth);
                push_key(out, key);
                out.push(':');
                encode_fields(out, depth + 1, inner)?;
            }
            primitive => {
                line(out, depth);
                push_key(out, key);
                out.push_str(": ");
                push_primitive(out, primitive)?;
            }
        }
    }
    Ok(())
}

/// `true` if brackets nest deeper than `limit` anywhere outside a string. One pass over the bytes,
/// no allocation: it decides hostile nesting before any parser (or recursion) touches the input.
/// Malformed input is left to the real parser; this only counts `{`/`[` against `}`/`]`.
fn nesting_exceeds(bytes: &[u8], limit: usize) -> bool {
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for &b in bytes {
        if in_string {
            match (escaped, b) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > limit + 1 {
                    return true;
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

/// Encode one JSON document as TOON, byte for byte as the spec prescribes, or decline.
pub fn encode_json(bytes: &[u8]) -> Result<String, Decline> {
    if nesting_exceeds(bytes, MAX_DEPTH) {
        return Err(Decline::TooDeep);
    }
    let raw: Box<RawValue> = serde_json::from_slice(bytes).map_err(|e| map_error(&e))?;
    let mut work = 0;
    let root = parse(
        raw.get(),
        0,
        &mut work,
        bytes.len().saturating_mul(MAX_PASSES).max(MIN_WORK),
    )?;
    let mut out = String::new();
    match &root {
        // An empty root object is the empty document (§8).
        Node::Obj(fields) if fields.is_empty() => {}
        Node::Obj(fields) => {
            if keyed_tabular(fields) {
                return Err(Decline::Form("keyed tabular object"));
            }
            encode_fields(&mut out, 0, fields)?;
        }
        Node::Arr(items) => encode_array(&mut out, 0, None, items)?,
        // A root string that starts with U+FEFF is quoted: a decoder would take it for a BOM (§7.2).
        Node::Str(s) if s.starts_with('\u{feff}') => push_quoted(&mut out, s),
        primitive => push_primitive(&mut out, primitive)?,
    }
    Ok(out)
}

/// The engine's entry point: a TOON replacement for a JSON document, or `None`.
pub fn encode(scrubbed: &[u8]) -> Option<Candidate> {
    let toon = encode_json(scrubbed).ok()?;
    if toon.is_empty() {
        return None; // `{}` is the empty document: nothing to gain
    }
    Some(Candidate {
        kind: Transform::Toon,
        layout: Layout::Trailer {
            body: toon.into_bytes(),
        },
        details: vec!["JSON converted to TOON".to_string()],
        lines: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(json: &str) -> Result<String, Decline> {
        encode_json(json.as_bytes())
    }

    fn ok(json: &str) -> String {
        enc(json).unwrap_or_else(|d| panic!("{json} declined: {d:?}"))
    }

    #[test]
    fn spec_examples_encode_exactly() {
        assert_eq!(
            ok(
                r#"{"users":[{"id":1,"name":"Ada","role":"admin"},{"id":2,"name":"Bob","role":"user"}]}"#
            ),
            "users[2]{id,name,role}:\n  1,Ada,admin\n  2,Bob,user"
        );
        assert_eq!(
            ok(r#"{"a":1,"b":{"c":"x","d":[1,2,3],"e":[]},"f":{}}"#),
            "a: 1\nb:\n  c: x\n  d[3]: 1,2,3\n  e: []\nf:"
        );
        assert_eq!(ok("[1,2,3]"), "[3]: 1,2,3");
        assert_eq!(ok("[]"), "[]");
        assert_eq!(
            ok(r#"[{"a":1,"b":"x"},{"a":2,"b":"y"}]"#),
            "[2]{a,b}:\n  1,x\n  2,y"
        );
        assert_eq!(ok("{}"), "", "the empty root object is the empty document");
        assert_eq!(ok(r#"{"tags":[]}"#), "tags: []");
        assert_eq!(
            ok(" \n{\"a\" :\t1 }\n"),
            "a: 1",
            "JSON whitespace is irrelevant"
        );
    }

    #[test]
    fn root_primitives() {
        assert_eq!(ok("42"), "42");
        assert_eq!(ok("-1.5"), "-1.5");
        assert_eq!(ok("true"), "true");
        assert_eq!(ok("null"), "null");
        assert_eq!(ok("\"hello\""), "hello");
        assert_eq!(ok("\"\""), "\"\"");
        assert_eq!(
            ok("\"\\ufeffx\""),
            "\"\u{feff}x\"",
            "a root string starting with U+FEFF is quoted"
        );
        assert_eq!(
            ok("{\"a\":\"\\ufeffx\"}"),
            "a: \u{feff}x",
            "...but only at the root"
        );
    }

    #[test]
    fn strict_parsing_declines_everything_that_is_not_one_clean_document() {
        for bad in [
            "",
            "   ",
            "{",
            "{\"a\":1,}",
            "[1,]",
            "{'a':1}",
            "{a:1}",
            "{\"a\":1} x",
            "{\"a\":1}{\"b\":2}",
            "[1] [2]",
            "// c\n{}",
            "NaN",
            "Infinity",
            "-Infinity",
            "01",
            "-01",
            ".5",
            "1.",
            "+1",
            "0x10",
            "\"unterminated",
            "\"bad \\q escape\"",
            "\"\\ud800\"",
            "\"\\udc00\"",
            "\"\\ud800\\u0041\"",
            "\u{feff}{\"a\":1}",
            "{\"a\":1}\u{0}",
            "tru",
            "nul",
            "\"a\nb\"", // raw control character inside a string
        ] {
            assert_eq!(enc(bad), Err(Decline::NotJson), "{bad:?}");
        }
        // A valid surrogate pair is fine.
        assert_eq!(ok("\"\\ud83d\\ude00\""), "😀");
        // Invalid UTF-8 never reaches a JSON parser in this subset.
        assert_eq!(encode_json(b"{\"a\":\"\xff\"}"), Err(Decline::NotJson));
    }

    #[test]
    fn duplicate_keys_decline_at_any_depth_but_the_same_key_in_two_objects_is_fine() {
        assert_eq!(enc(r#"{"a":1,"a":2}"#), Err(Decline::DuplicateKey));
        assert_eq!(
            enc(r#"{"x":{"a":1,"b":2,"a":3}}"#),
            Err(Decline::DuplicateKey)
        );
        assert_eq!(enc(r#"[{"a":1,"a":1}]"#), Err(Decline::DuplicateKey));
        assert!(enc(r#"{"a":1,"a\u0000":2}"#).is_ok());
        assert_eq!(
            enc("{\"a\":1,\"\\u0061\":2}"),
            Err(Decline::DuplicateKey),
            "an escaped key is the same key"
        );
        assert!(enc(r#"{"x":{"a":1},"y":{"b":2}}"#).is_ok());
    }

    #[test]
    fn depth_is_capped_without_overflowing_the_stack() {
        let nest = |n: usize| format!("{}1{}", "{\"a\":".repeat(n), "}".repeat(n));
        assert!(enc(&nest(MAX_DEPTH)).is_ok());
        assert_eq!(enc(&nest(MAX_DEPTH + 2)), Err(Decline::TooDeep));
        assert!(matches!(
            enc(&nest(1000)),
            Err(Decline::TooDeep | Decline::NotJson)
        ));
        // serde_json's own recursion guard, far beyond any realistic input.
        let deep = format!("{}{}", "[".repeat(200_000), "]".repeat(200_000));
        assert_eq!(enc(&deep), Err(Decline::TooDeep));
        let deep_obj = "{\"a\":".repeat(200_000);
        assert_eq!(
            enc(&deep_obj),
            Err(Decline::TooDeep),
            "even when it is also truncated"
        );
        // Brackets inside strings (escaped quotes included) are not nesting.
        let brackets = format!("{{\"a\":\"{}\\\"{}\"}}", "[".repeat(5000), "{".repeat(5000));
        assert_eq!(
            enc(&brackets).unwrap(),
            format!("a: \"{}\\\"{}\"", "[".repeat(5000), "{".repeat(5000))
        );
    }

    #[test]
    fn deep_nesting_around_a_large_value_is_bounded_by_construction() {
        let leaf = format!("[{}]", vec!["1"; 50_000].join(","));
        let nest = |n: usize| format!("{}{leaf}{}", "{\"a\":".repeat(n), "}".repeat(n));
        // A realistic depth costs a handful of passes and encodes.
        assert!(enc(&nest(4)).is_ok());
        // Sixty levels around the same leaf would take sixty passes: declined, quickly.
        let started = std::time::Instant::now();
        assert_eq!(enc(&nest(60)), Err(Decline::Cost));
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
        // Small documents are never refused for cost, however deep (within the depth cap).
        assert!(
            enc(&format!("{}1{}", "[".repeat(40), "]".repeat(40)))
                .is_err_and(|d| d != Decline::Cost)
        );
        assert!(enc(&format!("{}1{}", "{\"a\":".repeat(30), "}".repeat(30))).is_ok());
    }

    #[test]
    fn canonical_numbers_pass_and_everything_else_declines_the_whole_document() {
        for n in [
            "0",
            "1",
            "-1",
            "10",
            "100",
            "1000000",
            "1.5",
            "-2.25",
            "0.5",
            "0.000001",
            "123456789012345",
            "99999999999999.9",
            "-0.000123",
        ] {
            assert!(is_canonical_number(n), "{n}");
            assert_eq!(ok(&format!("{{\"n\":{n}}}")), format!("n: {n}"));
        }
        for n in [
            "1.0",
            "1.50",
            "0.10",
            "-0",
            "-0.0",
            "0.0",
            "1e3",
            "1E3",
            "1e-7",
            "2.5e10",
            "1e+2",
            "0.0000001",
            "1234567890123456",
            "9007199254740993",
            "100000000000000000000",
            "1000000000000000",
            "-1234567890123456",
        ] {
            assert!(!is_canonical_number(n), "{n}");
            assert_eq!(
                enc(&format!("{{\"n\":{n}}}")),
                Err(Decline::NumberForm),
                "{n}"
            );
            assert_eq!(enc(&format!("[{n}]")), Err(Decline::NumberForm), "{n}");
            assert_eq!(
                enc(&format!("[{{\"n\":{n}}}]")),
                Err(Decline::NumberForm),
                "{n}"
            );
        }
        // A number elsewhere in the document is enough.
        assert_eq!(enc(r#"{"a":1,"b":[1,2,1.50]}"#), Err(Decline::NumberForm));
        // Numbers that look numeric but sit in strings are strings.
        assert_eq!(ok(r#"{"n":"1.50"}"#), "n: \"1.50\"");
    }

    #[test]
    fn strings_are_quoted_exactly_when_the_spec_says() {
        let quoted = [
            "",
            " ",
            " a",
            "a ",
            "\ta",
            "true",
            "false",
            "null",
            "42",
            "-3.14",
            "05",
            "+1",
            "1e-6",
            "1E5",
            "a:b",
            "a\"b",
            "a\\b",
            "[x]",
            "{x}",
            "a[",
            "a}",
            "a,b",
            "-",
            "-x",
            "- x",
            "#",
            "#x",
            "line\nbreak",
            "tab\there",
            "cr\r",
            "\u{1}",
            "\u{1f}",
        ];
        for s in quoted {
            let out = ok(&format!("{{\"k\":{}}}", serde_json::to_string(s).unwrap()));
            assert!(
                out.starts_with("k: \"") && out.ends_with('"'),
                "{s:?} -> {out}"
            );
        }
        let bare = [
            "a", "a b", "a  b", "Ada_99", "x-y", "a#b", "1.5.2", "0x10", ".5", "5.", "1e", "e5",
            "Infinity", "NaN", "é", "日本", "😀", "\u{a0}x", "x\u{a0}", "\u{7f}", "\u{2028}",
            "a/b", "a=b", "True", "NULL", "10-30",
        ];
        for s in bare {
            let out = ok(&format!("{{\"k\":{}}}", serde_json::to_string(s).unwrap()));
            assert_eq!(out, format!("k: {s}"), "{s:?}");
        }
    }

    #[test]
    fn escapes_are_exactly_the_five_named_ones_and_lowercase_u_escapes() {
        assert_eq!(
            ok(r#"{"k":"a\"b\\c\nd\re\tf"}"#),
            "k: \"a\\\"b\\\\c\\nd\\re\\tf\""
        );
        assert_eq!(
            ok(r#"{"k":"\u0000\u0001\u001f\u000b\u000c\u0008"}"#),
            "k: \"\\u0000\\u0001\\u001f\\u000b\\u000c\\u0008\""
        );
        // Non-control characters are never escaped, quoted or not.
        assert_eq!(
            ok("{\"k\":\"a:\u{e9}\u{1f600}\u{2028}\"}"),
            "k: \"a:\u{e9}\u{1f600}\u{2028}\""
        );
        // The escape table has no other entries: no \b, \f, \/ or \u00XX for printable text.
        assert!(!ok(r#"{"k":"\u0008"}"#).contains("\\b"));
    }

    #[test]
    fn keys_are_unquoted_only_when_the_spec_allows() {
        assert_eq!(ok(r#"{"a":1,"A_b.c":2,"_x":3}"#), "a: 1\nA_b.c: 2\n_x: 3");
        for k in [
            "", "1a", "a-b", "a b", "é", "a:b", "a\"b", "a\nb", ".a", "a,b",
        ] {
            let out = ok(&format!("{{{}:1}}", serde_json::to_string(k).unwrap()));
            assert!(out.starts_with('"'), "{k:?} -> {out}");
        }
        assert_eq!(ok(r#"[{"a-b":1,"c":2}]"#), "[1]{\"a-b\",c}:\n  1,2");
        assert_eq!(ok(r#"{"my-key":[1,2]}"#), "\"my-key\"[2]: 1,2");
        assert_eq!(ok(r#"{"":[]}"#), "\"\": []");
    }

    #[test]
    fn tabular_detection_matches_the_spec_and_never_writes_a_lesser_form() {
        // Uniform and ordered: tabular. A single element is tabular too.
        assert_eq!(ok(r#"{"r":[{"a":1}]}"#), "r[1]{a}:\n  1");
        assert_eq!(
            ok(r#"{"r":[{"a":null,"b":true},{"a":"x","b":false}]}"#),
            "r[2]{a,b}:\n  null,true\n  x,false"
        );
        // The same key set in a different order must be tabular (spec §9.3) and so must reorder: declined.
        assert_eq!(
            enc(r#"{"r":[{"a":1,"b":2},{"b":3,"a":4}]}"#),
            Err(Decline::Form("tabular rows would reorder keys"))
        );
        // Everything below needs a form this subset does not implement.
        for doc in [
            r#"{"r":[{"a":1},{"b":2}]}"#, // different key sets: list form
            r#"{"r":[{"a":1},{}]}"#,      // empty object: list form
            r#"{"r":[{}]}"#,
            r#"{"r":[{"a":[1]},{"a":[2]}]}"#, // array-valued column
            r#"{"r":[{"a":{}},{"a":{}}]}"#,   // empty-object column
            r#"{"r":[{"a":1},{"a":{"p":1}}]}"#, // column mixing primitive and object
            r#"{"r":[{"a":{"p":1}},{"a":{"p":2}}]}"#, // nested field group
            r#"{"r":[1,{"a":1}]}"#,           // mixed
            r#"{"r":[[1],[2]]}"#,             // arrays of arrays
            r#"{"r":[[],[]]}"#,
            r#"[[1,2],[3]]"#,
            r#"[1,[2]]"#,
            r#"[{"a":1},[2]]"#,
        ] {
            assert!(
                matches!(enc(doc), Err(Decline::Form(_))),
                "{doc}: {:?}",
                enc(doc)
            );
        }
    }

    #[test]
    fn keyed_tabular_objects_are_detected_exactly_and_everything_else_nests() {
        // Detection holds: declined, at the root and in a field position.
        for doc in [
            r#"{"a":{"x":1,"y":2},"b":{"x":3,"y":4}}"#,
            r#"{"w":{"a":{"x":1},"b":{"x":2}}}"#,
            r#"{"a":{"x":1,"y":{"p":1}},"b":{"x":2,"y":{"p":2}}}"#, // nested-uniform column
        ] {
            assert_eq!(
                enc(doc),
                Err(Decline::Form("keyed tabular object")),
                "{doc}"
            );
        }
        // Detection fails: ordinary nesting, which is what the spec prescribes.
        assert_eq!(ok(r#"{"a":{"x":1},"b":{"y":2}}"#), "a:\n  x: 1\nb:\n  y: 2");
        assert_eq!(ok(r#"{"a":{"x":1}}"#), "a:\n  x: 1", "a single entry nests");
        assert_eq!(
            ok(r#"{"a":{},"b":{}}"#),
            "a:\nb:",
            "empty objects are not tabular entries"
        );
        assert_eq!(ok(r#"{"a":{"x":1},"b":3}"#), "a:\n  x: 1\nb: 3");
        assert_eq!(
            ok(r#"{"a":{"x":[1]},"b":{"x":[2]}}"#),
            "a:\n  x[1]: 1\nb:\n  x[1]: 2",
            "array columns defeat tabular detection"
        );
        assert_eq!(
            ok(r#"{"a":{"x":1,"y":2},"b":{"x":3}}"#),
            "a:\n  x: 1\n  y: 2\nb:\n  x: 3"
        );
    }

    #[test]
    fn output_has_no_trailing_spaces_blank_lines_newline_or_tabs() {
        for doc in [
            r#"{"a":1,"b":{"c":[1,2],"d":{"e":"x"}},"f":[{"g":1},{"g":2}],"h":{}}"#,
            r#"[{"a":1,"b":"two words"}]"#,
            r#"{"a":"","b":" "}"#,
        ] {
            let out = ok(doc);
            assert!(!out.ends_with('\n'));
            for l in out.split('\n') {
                assert!(!l.is_empty(), "blank line in {out:?}");
                assert!(!l.ends_with(' '), "trailing space in {l:?}");
                assert!(!l.starts_with('\t'));
                let lead = l.len() - l.trim_start_matches(' ').len();
                assert_eq!(lead % 2, 0, "indent is two spaces per level");
            }
        }
    }

    #[test]
    fn the_engine_entry_point_skips_the_empty_document() {
        assert!(encode(b"{}").is_none());
        assert!(encode(b"not json").is_none());
        let c = encode(br#"{"a":[1,2]}"#).unwrap();
        assert_eq!(c.kind, Transform::Toon);
        assert!(matches!(&c.layout, Layout::Trailer { body } if body == b"a[2]: 1,2"));
    }
}
