//! Differential test against the official reference encoder (`@toon-format/toon` 4.4.0, MIT).
//! Opt-in: it needs Node and a directory holding the package, so it is `#[ignore]`d.
//!
//! ```sh
//! mkdir /tmp/toon-js && cd /tmp/toon-js && npm init -y && npm install --ignore-scripts @toon-format/toon@4.4.0
//! TOON_REFERENCE_DIR=/tmp/toon-js cargo test --test toon_reference -- --ignored --nocapture
//! TOON_FUZZ_DOCS=200000 TOON_FUZZ_SEED=7 ...   # more documents / another seed
//! ```
//!
//! A deterministic generator writes JSON documents (a bias towards the awkward strings, keys,
//! numbers and shapes the spec singles out). For every document, either AgentDiet's output must
//! equal the reference's byte for byte, or AgentDiet must have declined for a reason the case
//! itself shows (a list form in the reference's output, a non-canonical number, reordered keys).
//! The reference is a different implementation, so this checks the encoder against something that
//! does not share its mistakes. Nothing from the package is vendored or linked.
//!
//! Measured result recorded in the commit that added this file.

mod toon_support;

use std::io::Write;
use std::process::{Command, Stdio};

use agentdiet::toon::{Decline, encode_json};
use serde_json::{Value, json};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const STRINGS: &[&str] = &[
    "",
    " ",
    "a",
    "hello",
    "Ada_99",
    "a b",
    " lead",
    "trail ",
    "a  b",
    "true",
    "false",
    "null",
    "True",
    "NULL",
    "123",
    "-5",
    "05",
    "+1",
    "1.5",
    "1e5",
    "1E-6",
    "0x10",
    ".5",
    "5.",
    "-",
    "- x",
    "-x",
    "#",
    "#tag",
    "a#b",
    "a,b",
    ",",
    "a:b",
    ":",
    "x\"y",
    "\"",
    "back\\slash",
    "\\",
    "[x]",
    "{y}",
    "[",
    "}",
    "tab\there",
    "\t",
    "nl\nx",
    "cr\rx",
    "ctrl\u{1}x",
    "\u{7f}",
    "é",
    "日本語",
    "😀",
    "\u{a0}nbsp",
    "\u{feff}bom",
    "a.b",
    "a-b",
    "key: value",
    "Infinity",
    "NaN",
    "item 1",
    "- ",
    "--",
    "a=b",
    "http://x.y/z?q=1",
    "10:30",
    "1,2",
    "\u{2028}",
    "ü,ö",
];

const KEYS: &[&str] = &[
    "a", "b", "id", "name", "x_y", "a.b", "a-b", "1x", "", "x y", "é", "A", "_", "k:1", "my key",
    "true", "[0]", "-", "n", "value", "tags", "items", "ü", "a\"b", "t\tab",
];

const NUMBERS: &[&str] = &[
    "0",
    "1",
    "-1",
    "10",
    "100",
    "1000000",
    "42",
    "-42",
    "1.5",
    "-2.25",
    "0.5",
    "0.000001",
    "0.0000001",
    "3.14159",
    "123456789012345",
    "1234567890123456",
    "9007199254740993",
    "1e3",
    "1E3",
    "1.0",
    "1.50",
    "-0",
    "-0.0",
    "0.10",
    "1e-7",
    "2.5e10",
    "100000000000000000000",
    "0.30000000000000004",
];

fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

fn primitive(rng: &mut Rng) -> String {
    match rng.below(10) {
        0 => "null".into(),
        1 => "true".into(),
        2 => "false".into(),
        3..=5 => (*rng.pick(NUMBERS)).to_string(),
        _ => json_string(rng.pick(STRINGS)),
    }
}

fn distinct_keys(rng: &mut Rng, n: usize) -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = Vec::new();
    while keys.len() < n {
        let k = *rng.pick(KEYS);
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys
}

fn object(rng: &mut Rng, depth: usize) -> String {
    let n = rng.below(4);
    let keys = distinct_keys(rng, n);
    let fields: Vec<String> = keys
        .iter()
        .map(|k| format!("{}:{}", json_string(k), value(rng, depth + 1)))
        .collect();
    format!("{{{}}}", fields.join(","))
}

/// An array of objects that is usually uniform (tabular), sometimes with a twist.
fn table(rng: &mut Rng, depth: usize) -> String {
    let width = 1 + rng.below(4);
    let keys = distinct_keys(rng, width);
    let rows = 1 + rng.below(4);
    let nested_col = rng.chance(8);
    let reorder = rng.chance(8);
    let mut out = Vec::new();
    for r in 0..rows {
        let mut ks = keys.clone();
        if reorder && r > 0 && ks.len() > 1 {
            ks.reverse();
        }
        let cells: Vec<String> = ks
            .iter()
            .map(|k| {
                let v = if nested_col && *k == keys[0] {
                    format!("{{\"p\":{},\"q\":{}}}", primitive(rng), primitive(rng))
                } else if rng.chance(3) {
                    value(rng, depth + 2) // occasionally a container: not tabular
                } else {
                    primitive(rng)
                };
                format!("{}:{v}", json_string(k))
            })
            .collect();
        out.push(format!("{{{}}}", cells.join(",")));
    }
    if rng.chance(4) {
        out.push("{}".into());
    }
    format!("[{}]", out.join(","))
}

fn array(rng: &mut Rng, depth: usize) -> String {
    match rng.below(10) {
        0 => "[]".into(),
        1..=4 => {
            let n = 1 + rng.below(5);
            let items: Vec<String> = (0..n).map(|_| primitive(rng)).collect();
            format!("[{}]", items.join(","))
        }
        5..=7 => table(rng, depth),
        8 => {
            let n = 1 + rng.below(3);
            let items: Vec<String> = (0..n).map(|_| array(rng, depth + 1)).collect();
            format!("[{}]", items.join(","))
        }
        _ => {
            let n = 1 + rng.below(4);
            let items: Vec<String> = (0..n).map(|_| value(rng, depth + 1)).collect();
            format!("[{}]", items.join(","))
        }
    }
}

/// An object whose values are the same shape: the keyed tabular case.
fn keyed(rng: &mut Rng) -> String {
    let entries = 1 + rng.below(3);
    let keys = distinct_keys(rng, entries);
    let ncols = 1 + rng.below(3);
    let cols = distinct_keys(rng, ncols);
    let fields: Vec<String> = keys
        .iter()
        .map(|k| {
            let cells: Vec<String> = cols
                .iter()
                .map(|c| format!("{}:{}", json_string(c), primitive(rng)))
                .collect();
            format!("{}:{{{}}}", json_string(k), cells.join(","))
        })
        .collect();
    format!("{{{}}}", fields.join(","))
}

fn value(rng: &mut Rng, depth: usize) -> String {
    if depth > 3 {
        return primitive(rng);
    }
    match rng.below(10) {
        0..=2 => primitive(rng),
        3..=5 => array(rng, depth),
        6 => keyed(rng),
        _ => object(rng, depth),
    }
}

fn document(rng: &mut Rng) -> String {
    match rng.below(10) {
        0 => primitive(rng),
        1 => array(rng, 0),
        2 => keyed(rng),
        3 => table(rng, 0),
        _ => object(rng, 0),
    }
}

const NODE_SCRIPT: &str = r#"
import { encode } from "@toon-format/toon";
import { createInterface } from "node:readline";
const rl = createInterface({ input: process.stdin });
const out = [];
for await (const line of rl) {
  const { doc } = JSON.parse(line);
  try { out.push({ ok: encode(JSON.parse(doc)) }); }
  catch (e) { out.push({ err: String(e) }); }
}
process.stdout.write(JSON.stringify(out));
"#;

#[test]
#[ignore = "needs Node and @toon-format/toon; set TOON_REFERENCE_DIR (see the file header)"]
fn encoder_agrees_with_the_reference_implementation() {
    let dir = std::env::var("TOON_REFERENCE_DIR")
        .expect("set TOON_REFERENCE_DIR to a directory with @toon-format/toon installed");
    let count: usize = std::env::var("TOON_FUZZ_DOCS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let seed: u64 = std::env::var("TOON_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let docs: Vec<String> = (0..count).map(|_| document(&mut rng)).collect();
    // Every generated document must itself be valid JSON (the generator is part of the test).
    for d in &docs {
        serde_json::from_str::<Value>(d).unwrap_or_else(|e| panic!("generator bug: {e}: {d}"));
    }

    let mut child = Command::new("node")
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("node not found");
    {
        let mut stdin = child.stdin.take().unwrap();
        for d in &docs {
            writeln!(stdin, "{}", json!({ "doc": d })).unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "node failed");
    let reference: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reference.len(), docs.len());

    let (mut same, mut declined, mut bad) = (0usize, 0usize, Vec::<String>::new());
    let mut reasons = std::collections::BTreeMap::<String, usize>::new();
    for (doc, reference) in docs.iter().zip(&reference) {
        let Some(want) = reference.get("ok").and_then(Value::as_str) else {
            bad.push(format!(
                "the reference rejected valid JSON: {doc}\n{reference}"
            ));
            continue;
        };
        match encode_json(doc.as_bytes()) {
            Ok(got) if got == want => same += 1,
            Ok(got) => bad.push(format!(
                "DIFFERENT for {doc}\n--- reference\n{want}\n--- ours\n{got}"
            )),
            Err(decline) => {
                declined += 1;
                *reasons.entry(format!("{decline:?}")).or_default() += 1;
                if let Err(why) = toon_support::explained(&decline, doc, want) {
                    bad.push(format!(
                        "UNEXPLAINED {decline:?} ({why}) for {doc}\n--- reference\n{want}"
                    ));
                }
                assert!(!matches!(decline, Decline::NotJson | Decline::DuplicateKey));
            }
        }
    }
    println!(
        "seed {seed}: {count} documents: {same} identical to the reference, {declined} declined"
    );
    for (reason, n) in &reasons {
        println!("  declined {n:6} x {reason}");
    }
    assert!(
        bad.is_empty(),
        "{} problems, first ones:\n{}",
        bad.len(),
        bad.iter().take(5).cloned().collect::<Vec<_>>().join("\n\n")
    );
    assert!(
        same > count / 4,
        "too few documents were encodable ({same}/{count}): the generator or the encoder is off"
    );
}
