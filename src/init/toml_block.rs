//! Codex keeps MCP servers in TOML (`~/.codex/config.toml`). agentdiet owns one
//! marker-delimited block there and edits **bytes**, so the user's comments and
//! formatting survive, but nothing is decided by matching lines alone:
//!
//! * the file is parsed with a real TOML parser, and an unparseable file is a
//!   conflict (left byte-identical);
//! * an existing `mcp_servers.agentdiet` outside our block is a conflict, however
//!   it is spelled (trailing comments, quoted or dotted keys, inline tables);
//! * a marker counts only if it is a real comment: one inside a multi-line
//!   string, a duplicate, an unbalanced pair, or a block whose body is not
//!   exactly an agentdiet entry is a conflict, never a partial edit;
//! * the prospective result is parsed again and must equal the original document
//!   plus the one agentdiet entry before it is returned.
//!
//! SPEC.md §2.2 has the rules.

use std::path::{Path, PathBuf};

use toml::{Table, Value};

use super::json_patch::PatchError;

pub const START: &str = "# >>> agentdiet >>>";
pub const END: &str = "# <<< agentdiet <<<";

const TABLE_KEY: &str = "mcp_servers";
const ENTRY_KEY: &str = "agentdiet";

fn toml_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// What goes between the markers.
fn body(exe: &Path) -> String {
    format!(
        "[{TABLE_KEY}.{ENTRY_KEY}]\ncommand = {}\nargs = [\"mcp\"]\n",
        toml_string(&exe.to_string_lossy())
    )
}

fn block(exe: &Path) -> String {
    format!("{START}\n{}{END}\n", body(exe))
}

fn conflict(why: impl Into<String>) -> PatchError {
    PatchError::Conflict(why.into())
}

fn parse(text: &str) -> Result<Table, PatchError> {
    toml::from_str::<Table>(text)
        .map_err(|e| conflict(format!("not valid TOML ({}); left untouched", e.message())))
}

/// The `[mcp_servers.agentdiet]` value of a parsed document, if any.
fn our_entry(doc: &Table) -> Option<&Value> {
    doc.get(TABLE_KEY)?.as_table()?.get(ENTRY_KEY)
}

/// `true` if `doc` is exactly `{ mcp_servers = { agentdiet = { command, args = ["mcp"] } } }`.
fn is_exactly_ours(doc: &Table) -> bool {
    let Some(servers) = doc.get(TABLE_KEY).and_then(Value::as_table) else {
        return false;
    };
    let Some(entry) = servers.get(ENTRY_KEY).and_then(Value::as_table) else {
        return false;
    };
    let args_ok = entry
        .get("args")
        .and_then(Value::as_array)
        .is_some_and(|a| a.len() == 1 && a[0].as_str() == Some("mcp"));
    doc.len() == 1
        && servers.len() == 1
        && entry.len() == 2
        && entry.get("command").is_some_and(Value::is_str)
        && args_ok
}

/// Find our block as `(start, end)` line indices. `Ok(None)` means there is no
/// block at all; anything suspicious is a conflict.
fn locate(lines: &[&str], doc: &Table) -> Result<Option<(usize, usize)>, PatchError> {
    let find = |marker: &str| -> Vec<usize> {
        lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.trim() == marker)
            .map(|(i, _)| i)
            .collect()
    };
    let (starts, ends) = (find(START), find(END));
    let (start, end) = match (starts.as_slice(), ends.as_slice()) {
        ([], []) => return Ok(None),
        ([s], [e]) if s < e => (*s, *e),
        _ => {
            return Err(conflict(
                "unbalanced, duplicated or misordered agentdiet markers; fix or delete them by hand",
            ));
        }
    };

    // A marker inside a multi-line string is not a comment: dropping that line
    // would change the document, so the document must be unchanged without it.
    for idx in [start, end] {
        let without: String = lines
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != idx)
            .map(|(_, l)| *l)
            .collect();
        if parse(&without).map_or(true, |d| &d != doc) {
            return Err(conflict(
                "an agentdiet marker is not a real comment (inside a string?); fix or delete it by hand",
            ));
        }
    }

    let inner = lines[start + 1..end].concat();
    if !parse(&inner).is_ok_and(|d| is_exactly_ours(&d)) {
        return Err(conflict(
            "the agentdiet block was edited by hand; fix or delete it by hand",
        ));
    }
    Ok(Some((start, end)))
}

/// Ensure the agentdiet block is present and current.
pub fn patch(current: Option<&str>, exe: &Path) -> Result<Option<String>, PatchError> {
    let text = current.unwrap_or("");
    let doc = parse(text)?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let located = locate(&lines, &doc)?;

    // Everything that is not our block must not already define the entry.
    let outside = match located {
        Some((start, end)) => format!("{}{}", lines[..start].concat(), lines[end + 1..].concat()),
        None => text.to_string(),
    };
    let mut expected = parse(&outside)?;
    match expected.get(TABLE_KEY) {
        None => {}
        Some(Value::Table(servers)) if !servers.contains_key(ENTRY_KEY) => {}
        Some(Value::Table(_)) => {
            return Err(conflict(
                "`mcp_servers.agentdiet` is already defined by hand",
            ));
        }
        Some(_) => return Err(conflict("`mcp_servers` is not a table; cannot extend it")),
    }

    let new = match located {
        Some((start, end)) => format!(
            "{}{}{}",
            lines[..start].concat(),
            block(exe),
            lines[end + 1..].concat()
        ),
        None => {
            let mut out = text.to_string();
            if !out.is_empty() {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
            }
            out.push_str(&block(exe));
            out
        }
    };

    // The result must be the original document plus exactly our entry.
    let ours = parse(&body(exe))?;
    let entry = our_entry(&ours)
        .cloned()
        .ok_or_else(|| conflict("internal error: agentdiet entry did not parse"))?;
    let servers = expected
        .entry(TABLE_KEY)
        .or_insert_with(|| Value::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| conflict("`mcp_servers` is not a table; cannot extend it"))?;
    servers.insert(ENTRY_KEY.to_string(), entry);
    let result = toml::from_str::<Table>(&new).map_err(|e| {
        conflict(format!(
            "adding the agentdiet entry would make the file invalid TOML ({}); \
             is `mcp_servers` an inline table?",
            e.message()
        ))
    })?;
    if result != expected {
        return Err(conflict(
            "adding the agentdiet entry would change other settings; left untouched",
        ));
    }
    Ok((new != text).then_some(new))
}

/// Remove the agentdiet block (and the blank separator line before it). A file
/// that is not valid TOML, or whose markers or block are not provably ours, is
/// an error and is left alone.
pub fn unpatch(text: &str) -> Result<Option<String>, PatchError> {
    let doc = parse(text)?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let Some((start, end)) = locate(&lines, &doc)? else {
        return Ok(None);
    };
    let keep_to = if start > 0 && lines[start - 1].trim().is_empty() {
        start - 1
    } else {
        start
    };
    let cleaned = format!("{}{}", lines[..keep_to].concat(), lines[end + 1..].concat());

    // Only our entry may disappear.
    let outside = format!("{}{}", lines[..start].concat(), lines[end + 1..].concat());
    if parse(&cleaned)? != parse(&outside)? {
        return Err(conflict(
            "removing the agentdiet block would change other settings",
        ));
    }
    Ok(Some(cleaned))
}

/// The executable the `[mcp_servers.agentdiet]` entry runs, when it has a string `command`.
pub fn block_command(text: &str) -> Option<PathBuf> {
    let doc = parse(text).ok()?;
    our_entry(&doc)?.get("command")?.as_str().map(PathBuf::from)
}

pub fn has_block(text: &str) -> bool {
    let Ok(doc) = parse(text) else { return false };
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    matches!(locate(&lines, &doc), Ok(Some(_)))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn exe() -> PathBuf {
        PathBuf::from("/opt/bin/agentdiet")
    }

    #[test]
    fn reads_the_configured_executable_back() {
        let patched = patch(Some("model = \"o3\"\n"), &exe()).unwrap().unwrap();
        assert_eq!(block_command(&patched), Some(exe()));
        assert_eq!(block_command("model = \"o3\"\n"), None);
    }

    #[test]
    fn appends_block_once() {
        let original = "model = \"o3\"\n\n[profiles.fast]\nx = 1";
        let first = patch(Some(original), &exe()).unwrap().unwrap();
        assert!(first.starts_with(original));
        assert_eq!(first.matches(START).count(), 1);
        assert!(first.contains("command = \"/opt/bin/agentdiet\""));
        assert!(patch(Some(&first), &exe()).unwrap().is_none());
        assert!(has_block(&first));
    }

    #[test]
    fn creates_file_from_nothing() {
        let out = patch(None, &exe()).unwrap().unwrap();
        assert!(out.starts_with(START));
    }

    #[test]
    fn replaces_stale_block_in_place() {
        let first = patch(Some("a = 1\n"), &exe()).unwrap().unwrap();
        let updated = patch(Some(&first), &PathBuf::from("/new/agentdiet"))
            .unwrap()
            .unwrap();
        assert_eq!(updated.matches(START).count(), 1);
        assert!(updated.contains("/new/agentdiet"));
        assert!(!updated.contains("/opt/bin"));
        assert!(updated.starts_with("a = 1\n"));
    }

    #[test]
    fn unpatch_round_trips_exactly() {
        for original in ["", "a = 1\n", "a = 1", "a = 1\n\n[t]\nb = 2\n"] {
            let patched = patch(Some(original), &exe()).unwrap().unwrap();
            let restored = unpatch(&patched).unwrap().unwrap();
            let expected = if original.is_empty() || original.ends_with('\n') {
                original.to_string()
            } else {
                format!("{original}\n")
            };
            assert_eq!(restored, expected, "for {original:?}");
        }
    }

    #[test]
    fn hand_written_table_is_a_conflict() {
        let text = "[mcp_servers.agentdiet]\ncommand = \"x\"\n";
        assert!(matches!(
            patch(Some(text), &exe()),
            Err(PatchError::Conflict(_))
        ));
        let dotted = "[mcp_servers]\nagentdiet.command = \"x\"\n";
        assert!(matches!(
            patch(Some(dotted), &exe()),
            Err(PatchError::Conflict(_))
        ));
        let inline = "mcp_servers = { a = { command = \"x\" } }\n";
        assert!(matches!(
            patch(Some(inline), &exe()),
            Err(PatchError::Conflict(_))
        ));
    }

    #[test]
    fn other_mcp_servers_are_not_a_conflict() {
        let text = "[mcp_servers.context7]\ncommand = \"npx\"\n";
        let out = patch(Some(text), &exe()).unwrap().unwrap();
        assert!(out.contains("[mcp_servers.context7]"));
        assert!(out.contains("[mcp_servers.agentdiet]"));
    }

    #[test]
    fn unbalanced_markers_are_a_conflict() {
        let text = format!("{START}\nstuff\n");
        assert!(matches!(
            patch(Some(&text), &exe()),
            Err(PatchError::Conflict(_))
        ));
    }

    #[test]
    fn windows_paths_are_escaped() {
        let out = patch(None, &PathBuf::from("C:\\bin\\agentdiet.exe"))
            .unwrap()
            .unwrap();
        assert!(out.contains("command = \"C:\\\\bin\\\\agentdiet.exe\""));
    }

    fn is_conflict(r: Result<Option<String>, PatchError>) -> bool {
        matches!(r, Err(PatchError::Conflict(_)))
    }

    fn valid(text: &str) -> Table {
        toml::from_str(text).unwrap_or_else(|e| panic!("not valid TOML: {e}\n{text}"))
    }

    #[test]
    fn trailing_comment_on_a_hand_written_table_is_a_conflict() {
        // Reproduced bug: this used to get a second `[mcp_servers.agentdiet]` appended.
        let text =
            "[mcp_servers.agentdiet] # existing user config\ncommand = \"existing-binary\"\n";
        assert!(is_conflict(patch(Some(text), &exe())));
    }

    #[test]
    fn every_spelling_of_a_hand_written_entry_is_a_conflict() {
        for text in [
            "[mcp_servers.agentdiet]\ncommand = \"x\"\n",
            "[mcp_servers.agentdiet] # note\ncommand = \"x\"\n",
            "[mcp_servers.\"agentdiet\"]\ncommand = \"x\"\n",
            "[mcp_servers.'agentdiet']\ncommand = \"x\"\n",
            "[ mcp_servers . agentdiet ]\ncommand = \"x\"\n",
            "[\"mcp_servers\".agentdiet]\ncommand = \"x\"\n",
            "[mcp_servers]\nagentdiet.command = \"x\"\n",
            "[mcp_servers]\n\"agentdiet\" = { command = \"x\" }\n",
            "mcp_servers.agentdiet.command = \"x\"\n",
            "mcp_servers = { agentdiet = { command = \"x\" } }\n",
            "[mcp_servers.agentdiet.env]\nK = \"v\"\n",
            "[mcp_servers.agentdiet]\ncommand = \"x\"\n\n# >>> agentdiet >>>\n[mcp_servers.agentdiet]\ncommand = \"y\"\nargs = [\"mcp\"]\n# <<< agentdiet <<<\n",
        ] {
            assert!(is_conflict(patch(Some(text), &exe())), "{text:?}");
        }
    }

    #[test]
    fn an_inline_or_non_table_mcp_servers_is_a_conflict_not_a_corruption() {
        for text in [
            "mcp_servers = { other = { command = \"x\" } }\n",
            "mcp_servers = 3\n",
        ] {
            assert!(is_conflict(patch(Some(text), &exe())), "{text:?}");
        }
    }

    #[test]
    fn invalid_toml_is_never_touched() {
        for text in [
            "this is = = not toml",
            "a = 1\na = 2\n",
            "[t\nx = 1\n",
            "s = \"\"\"unterminated\n",
        ] {
            assert!(is_conflict(patch(Some(text), &exe())), "{text:?}");
            assert!(is_conflict(unpatch(text)), "{text:?}");
            assert!(!has_block(text));
        }
    }

    #[test]
    fn markers_inside_multiline_strings_are_conflicts_and_never_stripped() {
        for text in [
            // Both markers inside one literal string.
            format!("note = \'\'\'\n{START}\nstuff\n{END}\n\'\'\'\n"),
            // Only the start marker inside a string.
            format!("note = \"\"\"\n{START}\n\"\"\"\n"),
            // A real block, plus a lookalike inside a string.
            format!("{}\nnote = \'\'\'\n{START}\n\'\'\'\n", block(&exe())),
        ] {
            assert!(is_conflict(patch(Some(&text), &exe())), "{text:?}");
            assert!(is_conflict(unpatch(&text)), "{text:?}");
            assert!(!has_block(&text), "{text:?}");
        }
    }

    #[test]
    fn duplicate_unbalanced_and_misordered_markers_are_conflicts() {
        let good = block(&exe());
        for text in [
            format!("{good}\n{good}"),
            format!("{START}\n{START}\n{END}\n"),
            format!("{END}\nx = 1\n{START}\n"),
            format!("x = 1\n{END}\n"),
        ] {
            assert!(is_conflict(patch(Some(&text), &exe())), "{text:?}");
            assert!(is_conflict(unpatch(&text)), "{text:?}");
        }
    }

    #[test]
    fn a_hand_edited_block_is_a_conflict_and_is_never_removed() {
        let edited = format!(
            "x = 1\n\n{START}\n[mcp_servers.agentdiet]\ncommand = \"a\"\nargs = [\"mcp\"]\nenv = {{ K = \"v\" }}\n{END}\ny = 2\n"
        );
        assert!(is_conflict(patch(Some(&edited), &exe())));
        assert!(is_conflict(unpatch(&edited)));
        // Foreign content wedged between the markers is not ours to delete.
        let wedge = format!(
            "{START}\n[mcp_servers.agentdiet]\ncommand = \"a\"\nargs = [\"mcp\"]\n[other]\nkeep = true\n{END}\n"
        );
        assert!(is_conflict(unpatch(&wedge)));
    }

    #[test]
    fn user_content_after_the_block_is_not_swallowed_into_our_table() {
        // Root-level keys can only follow the block if they were already ours to
        // lose; refuse rather than re-scope them into [mcp_servers.agentdiet].
        let text = format!("{}stray = 1\n", block(&exe()));
        assert!(is_conflict(patch(
            Some(&text),
            &PathBuf::from("/new/agentdiet")
        )));
    }

    #[test]
    fn valid_configs_stay_valid_and_keep_every_byte_of_user_content() {
        let originals = [
            "",
            "model = \"o3\" # my model\n",
            "# top comment\n\n[profiles.fast]\nx = 1 # inline\n\n[profiles.slow]\ny = [\n  1,\n  2, # two\n]\n",
            "a = 1",
            "crlf = true\r\n[t]\r\nb = 2\r\n",
            "[mcp_servers.context7]\ncommand = \"npx\"\nargs = [\"-y\", \"ctx\"]\n",
            "[[array]]\nn = 1\n[[array]]\nn = 2\n",
            "s = \"\"\"\nmulti\nline # not a comment\n\"\"\"\n",
            "[mcp_servers]\nother = { command = \"x\" }\n",
        ];
        for original in originals {
            let before = valid(original);
            let patched = patch(Some(original), &exe()).unwrap().unwrap();
            assert!(patched.starts_with(original), "{original:?}");
            let after = valid(&patched);
            // Same settings plus the one entry.
            let mut expected = before.clone();
            let servers = expected
                .entry("mcp_servers")
                .or_insert_with(|| Value::Table(Table::new()))
                .as_table_mut()
                .unwrap();
            servers.insert("agentdiet".into(), our_entry(&after).unwrap().clone());
            assert_eq!(after, expected, "{original:?}");
            assert!(has_block(&patched));
            // Idempotent, and a refresh keeps user content and validity.
            assert!(patch(Some(&patched), &exe()).unwrap().is_none());
            let refreshed = patch(Some(&patched), &PathBuf::from("/new/agentdiet"))
                .unwrap()
                .unwrap();
            valid(&refreshed);
            assert!(refreshed.starts_with(original));
            // And removal restores the original document.
            assert_eq!(
                valid(&unpatch(&patched).unwrap().unwrap()),
                before,
                "{original:?}"
            );
        }
    }

    #[test]
    fn replacing_a_block_in_the_middle_keeps_what_follows() {
        let text = format!("a = 1\n\n{}\n[later]\nz = 3 # keep\n", block(&exe()));
        let out = patch(Some(&text), &PathBuf::from("/new/agentdiet"))
            .unwrap()
            .unwrap();
        assert!(out.contains("/new/agentdiet") && !out.contains("/opt/bin"));
        assert!(out.ends_with("[later]\nz = 3 # keep\n"));
        valid(&out);
    }

    #[test]
    fn a_lookalike_comment_that_is_not_a_marker_is_just_a_comment() {
        let text = "# >>> agentdiet >>> (my own note)\nx = 1\n";
        let out = patch(Some(text), &exe()).unwrap().unwrap();
        assert!(out.starts_with(text));
        valid(&out);
    }
}
