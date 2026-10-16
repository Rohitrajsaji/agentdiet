//! Helpers shared by the TOON conformance tests: deciding whether a decline is *explained* by the
//! input or by the reference output, so that a bug cannot hide as a decline.
#![allow(dead_code)]

use agentdiet::toon::Decline;
use serde_json::Value;

/// Number tokens of a JSON text, found by a scanner that skips strings (independent of the
/// encoder's parser).
pub fn numbers_in(json: &str) -> Vec<String> {
    let b = json.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < b.len() && matches!(b[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    i += 1;
                }
                out.push(json[start..i].to_string());
            }
            _ => i += 1,
        }
    }
    out
}

pub fn non_canonical(token: &str) -> bool {
    let (neg, rest) = token
        .strip_prefix('-')
        .map_or((false, token), |r| (true, r));
    let (int, frac) = rest.split_once('.').map_or((rest, ""), |(i, f)| (i, f));
    rest.contains(['e', 'E'])
        || frac.ends_with('0')
        || (neg && int == "0" && frac.is_empty())
        || int.trim_start_matches('0').len() + frac.len() > 15
        || (int == "0" && !frac.is_empty() && frac.bytes().take_while(|b| *b == b'0').count() > 5)
}

/// Does the official output show a form this subset documents as unsupported?
pub fn expected_uses_unsupported_form(expected: &str) -> bool {
    expected.lines().any(|l| {
        let t = l.trim_start();
        t == "-"
            || t.starts_with("- ")
            || t.contains("[0]:")
            // a keyed tabular header: `[N:]`, `[N:|]` or `[N:<tab>]`
            || t.split("[").skip(1).any(|s| {
                let digits = s.chars().take_while(char::is_ascii_digit).count();
                digits > 0 && s[digits..].starts_with(':')
            })
            // a nested field group inside a header's braces: `name{...{`
            || t.find('{').is_some_and(|i| t[i + 1..].contains('{') && t[..i].contains(']'))
    })
}

pub fn key_orders_differ(input: &str) -> bool {
    fn walk(v: &Value) -> bool {
        match v {
            Value::Array(items) => {
                let orders: Vec<Vec<&String>> = items
                    .iter()
                    .filter_map(|i| i.as_object().map(|o| o.keys().collect()))
                    .collect();
                let differs = orders.len() > 1 && orders.iter().any(|o| *o != orders[0]) && {
                    let mut a = orders[0].clone();
                    a.sort();
                    orders.iter().all(|o| {
                        let mut b = o.clone();
                        b.sort();
                        a == b
                    })
                };
                differs || items.iter().any(walk)
            }
            Value::Object(m) => m.values().any(walk),
            _ => false,
        }
    }
    serde_json::from_str::<Value>(input).is_ok_and(|v| walk(&v))
}

pub fn explained(decline: &Decline, input: &str, expected: &str) -> Result<(), String> {
    let ok = match decline {
        Decline::NumberForm => numbers_in(input).iter().any(|n| non_canonical(n)),
        Decline::Form("tabular rows would reorder keys") => key_orders_differ(input),
        Decline::Form(_) => expected_uses_unsupported_form(expected),
        Decline::NotJson | Decline::DuplicateKey | Decline::TooDeep | Decline::Cost => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("{decline:?} is not explained by the case"))
    }
}
