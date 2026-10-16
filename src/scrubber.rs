//! Secret scrubber: redacts credentials before anything reaches disk.
//!
//! [`scrub`] is the only way to build a [`Scrubbed`], and the vault
//! ([`crate::cas`]) and memory index ([`crate::memory`]) accept only that type,
//! so "secrets are never written to disk unscrubbed" holds at compile time.
//!
//! Works on bytes, so non-UTF-8 tool output stays lossless. A secret span is
//! replaced by `[REDACTED:<rule-id>]`; surrounding text (such as the variable
//! name) is kept. Patterns marked "ported" come from Secretlint (MIT, see
//! `THIRD_PARTY_NOTICES`) and are translated to the `regex` crate's syntax,
//! which has no lookaround: those are emulated with an extra capture group or
//! an `accept` check on the bytes around the match. The Bearer and dotenv rules
//! are original to AgentDiet.

use std::sync::LazyLock;

use regex::bytes::Regex;

/// Output that has been through [`scrub`]. Cannot be constructed any other way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scrubbed {
    bytes: Vec<u8>,
    redactions: usize,
}

impl Scrubbed {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Number of secret spans that were redacted.
    pub fn redactions(&self) -> usize {
        self.redactions
    }

    /// The text for places that need a `String` (the memory index); invalid
    /// UTF-8 becomes U+FFFD. Never used for the vault, which stays byte-exact.
    pub fn to_lossy_string(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

/// Decides, from the haystack and the span to redact, whether a match counts.
type Accept = fn(haystack: &[u8], start: usize, end: usize) -> bool;

struct Rule {
    id: &'static str,
    re: Regex,
    /// Capture group holding the secret (0 = the whole match).
    group: usize,
    accept: Accept,
}

fn always(_: &[u8], _: usize, _: usize) -> bool {
    true
}

/// Emulates `(?![A-Za-z0-9_-])` after the Anthropic key.
fn not_followed_by_key_char(h: &[u8], _: usize, end: usize) -> bool {
    !h.get(end)
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
}

/// A 40-char AWS secret must not be the head of a longer base64 run.
fn not_followed_by_base64_char(h: &[u8], _: usize, end: usize) -> bool {
    !h.get(end)
        .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'+' | b'='))
}

/// dotenv values: skip short or numeric values (`MAX_TOKENS=4096`) and values
/// an earlier rule already redacted.
fn plausible_secret_value(h: &[u8], start: usize, end: usize) -> bool {
    let value = h[start..end].trim_ascii();
    let value = value
        .strip_prefix(b"\"")
        .or_else(|| value.strip_prefix(b"'"))
        .unwrap_or(value);
    !value.starts_with(b"[REDACTED:") && value.len() >= 6 && !value.iter().all(u8::is_ascii_digit)
}

fn rule(id: &'static str, pattern: &str, group: usize, accept: Accept) -> Rule {
    Rule {
        id,
        // The patterns are literals covered by unit tests; a failure here is a
        // programming error that every test run would surface.
        re: Regex::new(pattern).expect("built-in scrubber pattern must compile"),
        group,
        accept,
    }
}

// `(?-u)` everywhere: ASCII-only classes that can match arbitrary bytes.
static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        // ported: secretlint/packages/@secretlint/secretlint-rule-privatekey (MIT).
        // The whole PEM block (header, body, footer) is replaced.
        rule(
            "private-key",
            r"(?-u)-----BEGIN (?:(?:RSA|DSA|EC|OPENSSH|PGP) )?PRIVATE KEY(?: BLOCK)?-----[A-Za-z0-9+/=\s\\]{100,10000}-----END (?:(?:RSA|DSA|EC|OPENSSH|PGP) )?PRIVATE KEY(?: BLOCK)?-----",
            0,
            always,
        ),
        // ported: secretlint-rule-anthropic (MIT). `(?<!\p{L})` becomes the
        // leading group; `(?![A-Za-z0-9_-])` is `not_followed_by_key_char`.
        rule(
            "anthropic-api-key",
            r"(?-u)(?:^|[^A-Za-z])(sk-ant-api0[0-9]-[A-Za-z0-9_-]{90,128}AA)",
            1,
            not_followed_by_key_char,
        ),
        // ported: secretlint-rule-openai (MIT).
        rule(
            "openai-api-key",
            r"(?-u)sk-(?:proj|svcacct|admin)-(?:[A-Za-z0-9_-]{74}|[A-Za-z0-9_-]{58})T3BlbkFJ(?:[A-Za-z0-9_-]{74}|[A-Za-z0-9_-]{58})\b|sk-[a-zA-Z0-9]{20}T3BlbkFJ[a-zA-Z0-9]{20}",
            0,
            always,
        ),
        // ported: secretlint-rule-aws, access key ID (MIT).
        rule(
            "aws-access-key-id",
            r"(?-u)\b(?:A3T[A-Z0-9]|AKIA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|ASIA)[A-Z0-9]{16}\b",
            0,
            always,
        ),
        // ported: secretlint-rule-aws, secret access key assignment (MIT). Only
        // the 40-char value (group 1) is redacted. The original's trailing
        // `\b` is replaced by `not_followed_by_base64_char`, since `\b` fails
        // after a `=` padding char.
        rule(
            "aws-secret-access-key",
            r#"(?-u)(?:AWS|aws|Aws)?_?(?:SECRET|secret|Secret)_?(?:ACCESS|access|Access)_?(?:KEY|key|Key)["']?\s*(?::|=>|=)\s*["']?([A-Za-z0-9/+=]{40})"#,
            1,
            not_followed_by_base64_char,
        ),
        // original: `Bearer <token>` in headers and logs; only the token is redacted.
        rule(
            "bearer-token",
            r"(?-u)(?i:\bBearer)[ \t]+([A-Za-z0-9._~+/-]{16,}=*)",
            1,
            always,
        ),
        // original: `.env`-style assignments to secret-looking names. Secretlint's
        // no-dotenv rule only flags the filename, so nothing is ported here.
        rule(
            "dotenv-secret",
            r"(?-u)(?m)^[ \t]*(?:export[ \t]+)?[A-Za-z0-9_]*(?:SECRET|TOKEN|PASSWORD|PASSWD|API_?KEY|PRIVATE_KEY|ACCESS_KEY)[A-Za-z0-9_]*[ \t]*=[ \t]*([^\r\n]+)",
            1,
            plausible_secret_value,
        ),
    ]
});

/// Redact every secret in `raw`.
pub fn scrub(raw: &[u8]) -> Scrubbed {
    let mut bytes = raw.to_vec();
    let mut redactions = 0;
    for rule in RULES.iter() {
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for caps in rule.re.captures_iter(&bytes) {
            if let Some(m) = caps.get(rule.group)
                && (rule.accept)(&bytes, m.start(), m.end())
            {
                spans.push((m.start(), m.end()));
            }
        }
        if spans.is_empty() {
            continue;
        }
        let placeholder = format!("[REDACTED:{}]", rule.id);
        let mut out = Vec::with_capacity(bytes.len());
        let mut last = 0;
        for (start, end) in &spans {
            out.extend_from_slice(&bytes[last..*start]);
            out.extend_from_slice(placeholder.as_bytes());
            last = *end;
        }
        out.extend_from_slice(&bytes[last..]);
        redactions += spans.len();
        bytes = out;
    }
    Scrubbed { bytes, redactions }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic keys are assembled at runtime so the repo never contains
    // literal key-shaped strings (GitHub push protection, secret scanners).
    fn aws_key_id() -> String {
        ["AK", "IA"].concat() + &"Q7".repeat(8)
    }

    fn aws_secret() -> String {
        "aB3/".repeat(10)
    }

    fn anthropic_key() -> String {
        ["sk-", "ant-", "api03-"].concat() + &"Ab1_-".repeat(19) + "AA"
    }

    fn openai_legacy() -> String {
        ["sk-", &"a1B2".repeat(5), "T3Blbk", "FJ", &"c3D4".repeat(5)].concat()
    }

    fn openai_project() -> String {
        let half = "x_Y-".repeat(18) + "zz"; // 74 chars
        ["sk-", "proj-", &half, "T3Blbk", "FJ", &half].concat()
    }

    fn pem() -> String {
        let body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASC".repeat(6);
        [
            "-----BEGIN ",
            "PRIVATE KEY-----\n",
            &body,
            "\n-----END ",
            "PRIVATE KEY-----",
        ]
        .concat()
    }

    fn text(s: &Scrubbed) -> String {
        s.to_lossy_string()
    }

    #[test]
    fn clean_text_is_untouched() {
        let raw = b"running 4 tests\nok. MAX_TOKENS=4096\nBearer short\n";
        let out = scrub(raw);
        assert_eq!(out.as_bytes(), raw);
        assert_eq!(out.redactions(), 0);
    }

    #[test]
    fn aws_access_key_id() {
        let raw = format!("id={} end", aws_key_id());
        let out = scrub(raw.as_bytes());
        assert_eq!(text(&out), "id=[REDACTED:aws-access-key-id] end");
        assert_eq!(out.redactions(), 1);
    }

    #[test]
    fn aws_secret_access_key_keeps_the_name() {
        let raw = format!("AWS_SECRET_ACCESS_KEY=\"{}\"", aws_secret());
        let out = text(&scrub(raw.as_bytes()));
        assert_eq!(
            out,
            "AWS_SECRET_ACCESS_KEY=\"[REDACTED:aws-secret-access-key]\""
        );
    }

    #[test]
    fn anthropic_key_redacted_only_at_a_boundary() {
        let key = anthropic_key();
        let out = scrub(format!("key: {key}\n").as_bytes());
        assert_eq!(text(&out), "key: [REDACTED:anthropic-api-key]\n");

        // Start of input counts as a boundary.
        assert_eq!(text(&scrub(key.as_bytes())), "[REDACTED:anthropic-api-key]");

        // Lookaround emulation: a letter before, or a key char after, is no match.
        let before = format!("x{key}");
        assert_eq!(scrub(before.as_bytes()).redactions(), 0);
        let after = format!("{key}x");
        assert_eq!(scrub(after.as_bytes()).redactions(), 0);
    }

    #[test]
    fn openai_keys() {
        for key in [openai_legacy(), openai_project()] {
            let out = scrub(format!("OPENAI={key};").as_bytes());
            assert!(text(&out).contains("[REDACTED:openai-api-key]"), "{key}");
            assert!(!text(&out).contains(&key));
        }
    }

    #[test]
    fn private_key_block_redacted_whole() {
        let raw = format!("before\n{}\nafter", pem());
        let out = text(&scrub(raw.as_bytes()));
        assert_eq!(out, "before\n[REDACTED:private-key]\nafter");
    }

    #[test]
    fn bearer_redacts_only_the_token() {
        let out = text(&scrub(
            b"Authorization: Bearer abcdefghijklmnop1234567890==",
        ));
        assert_eq!(out, "Authorization: Bearer [REDACTED:bearer-token]");
        let lower = text(&scrub(b"authorization: bearer abcdefghijklmnop1234567890"));
        assert_eq!(lower, "authorization: bearer [REDACTED:bearer-token]");
    }

    #[test]
    fn dotenv_keeps_the_name_and_redacts_the_value() {
        let raw = b"DATABASE_PASSWORD=hunter2hunter2\nexport API_KEY='abcdef123456'\nDEBUG=true\n";
        let out = text(&scrub(raw));
        assert_eq!(
            out,
            "DATABASE_PASSWORD=[REDACTED:dotenv-secret]\nexport API_KEY=[REDACTED:dotenv-secret]\nDEBUG=true\n"
        );
    }

    #[test]
    fn dotenv_ignores_numeric_and_short_values() {
        let raw = b"MAX_TOKENS=4096\nSECRET_LEN=abc\n";
        assert_eq!(scrub(raw).redactions(), 0);
    }

    #[test]
    fn already_redacted_values_are_not_redacted_twice() {
        let raw = format!("AWS_SECRET_ACCESS_KEY={}\n", aws_secret());
        let out = scrub(raw.as_bytes());
        assert_eq!(out.redactions(), 1);
        assert_eq!(
            text(&out),
            "AWS_SECRET_ACCESS_KEY=[REDACTED:aws-secret-access-key]\n"
        );
    }

    #[test]
    fn non_utf8_bytes_survive() {
        let mut raw = vec![0xff, 0xfe, b'\n'];
        raw.extend_from_slice(aws_key_id().as_bytes());
        raw.extend_from_slice(b"\n\x80tail");
        let out = scrub(&raw);
        let bytes = out.as_bytes();
        assert!(bytes.starts_with(&[0xff, 0xfe, b'\n']));
        assert!(bytes.ends_with(b"\n\x80tail"));
        assert_eq!(out.redactions(), 1);
    }

    #[test]
    fn scrubbing_is_idempotent() {
        let raw = format!("{} {}\n{}", aws_key_id(), anthropic_key(), pem());
        let once = scrub(raw.as_bytes());
        let twice = scrub(once.as_bytes());
        assert_eq!(once.as_bytes(), twice.as_bytes());
        assert_eq!(twice.redactions(), 0);
    }
}
