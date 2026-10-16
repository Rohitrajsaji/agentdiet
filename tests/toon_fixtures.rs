//! The official TOON encode fixtures (pinned in SPEC §9, vendored under tests/fixtures/toon, MIT,
//! Copyright (c) 2025-PRESENT Johann Schopplich) against AgentDiet's subset encoder.
//!
//! Three rules keep this from passing vacuously:
//!
//! 1. every case the encoder produces output for must match the official expected text byte for
//!    byte;
//! 2. every case it declines must be *explained*: the official output (or the input) has to show
//!    a form, option or number the subset documents as unsupported, so a bug cannot hide as a
//!    decline;
//! 3. the set of cases that are encoded is recorded in `supported.txt`, and the test fails when it
//!    differs in either direction, so the supported set cannot quietly shrink (or grow unreviewed).
//!    `UPDATE_TOON_SUPPORTED=1 cargo test --test toon_fixtures` rewrites the file.

use std::collections::BTreeSet;
use std::path::PathBuf;

mod toon_support;

use agentdiet::toon::encode_json;
use serde::Deserialize;
use serde_json::Value;
use serde_json::value::RawValue;

#[derive(Deserialize)]
struct File {
    tests: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    input: Box<RawValue>,
    expected: String,
    #[serde(default)]
    options: Option<Value>,
}

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/toon")
}

fn files() -> Vec<(String, File)> {
    let mut names: Vec<_> = std::fs::read_dir(dir().join("encode"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".json"))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|n| {
            let text = std::fs::read_to_string(dir().join("encode").join(&n)).unwrap();
            let file: File = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{n}: {e}"));
            (n, file)
        })
        .collect()
}

/// Options other than the defaults this encoder implements (comma, 2 spaces).
fn non_default_options(options: &Option<Value>) -> Option<String> {
    let Some(Value::Object(map)) = options else {
        return None;
    };
    for (k, v) in map {
        let default = match k.as_str() {
            "delimiter" => v == ",",
            "indent" => v == 2,
            _ => false,
        };
        if !default {
            return Some(format!("{k}={v}"));
        }
    }
    None
}

#[test]
fn official_encode_fixtures_are_reproduced_or_explainedly_declined() {
    let mut encoded: BTreeSet<String> = BTreeSet::new();
    let mut declined = 0;
    let mut skipped = 0;
    let mut problems: Vec<String> = Vec::new();
    for (file, f) in files() {
        for case in &f.tests {
            let id = format!("{file}::{}", case.name);
            if let Some(opt) = non_default_options(&case.options) {
                let _ = opt;
                skipped += 1;
                continue;
            }
            match encode_json(case.input.get().as_bytes()) {
                Ok(text) => {
                    if text == case.expected {
                        encoded.insert(id);
                    } else {
                        problems.push(format!(
                            "{id}: output differs\n--- expected\n{}\n--- got\n{text}",
                            case.expected
                        ));
                    }
                }
                Err(decline) => {
                    declined += 1;
                    if std::env::var_os("TOON_LIST").is_some() {
                        println!("DECLINE {decline:?} | {id}");
                    }
                    if let Err(why) =
                        toon_support::explained(&decline, case.input.get(), &case.expected)
                    {
                        problems.push(format!("{id}: {why}"));
                    }
                }
            }
        }
    }
    println!(
        "official encode fixtures: {} encoded exactly, {declined} declined (each explained), {skipped} skipped for non-default options",
        encoded.len()
    );
    assert!(problems.is_empty(), "\n{}", problems.join("\n\n"));

    let path = dir().join("supported.txt");
    let listing: String = encoded.iter().map(|l| format!("{l}\n")).collect();
    if std::env::var_os("UPDATE_TOON_SUPPORTED").is_some() {
        std::fs::write(&path, &listing).unwrap();
        return;
    }
    let recorded = std::fs::read_to_string(&path).unwrap_or_default();
    let recorded: BTreeSet<String> = recorded.lines().map(str::to_string).collect();
    let lost: Vec<_> = recorded.difference(&encoded).collect();
    let gained: Vec<_> = encoded.difference(&recorded).collect();
    assert!(
        lost.is_empty() && gained.is_empty(),
        "supported set changed.\nno longer encoded: {lost:#?}\nnewly encoded (rerun with UPDATE_TOON_SUPPORTED=1 after review): {gained:#?}"
    );
    assert!(
        recorded.len() >= 60,
        "the supported set must not be vacuous"
    );
}
