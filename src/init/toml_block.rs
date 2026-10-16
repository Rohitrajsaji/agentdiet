//! Codex keeps MCP servers in TOML (`~/.codex/config.toml`). Rather than pull
//! in a TOML editor, agentdiet owns one marker-delimited block and refuses to
//! touch files where the user already defined `mcp_servers.agentdiet`
//! themselves.

use std::path::Path;

use super::json_patch::PatchError;

pub const START: &str = "# >>> agentdiet >>>";
pub const END: &str = "# <<< agentdiet <<<";

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

fn block(exe: &Path) -> String {
    format!(
        "{START}\n[mcp_servers.agentdiet]\ncommand = {}\nargs = [\"mcp\"]\n{END}\n",
        toml_string(&exe.to_string_lossy())
    )
}

/// Line indices of the start and end markers, if present.
fn markers(lines: &[&str]) -> (Option<usize>, Option<usize>) {
    let find = |marker: &str| lines.iter().position(|l| l.trim() == marker);
    (find(START), find(END))
}

/// Detect a user-defined `mcp_servers.agentdiet` (or an inline `mcp_servers`
/// table we cannot safely extend) in the text outside our markers.
fn conflict(outside: &str) -> Option<String> {
    let mut table = String::new();
    for line in outside.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with('[') {
            table = t
                .trim_start_matches('[')
                .trim_end_matches(']')
                .replace(['"', '\''], "")
                .replace(' ', "");
            if table == "mcp_servers.agentdiet" || table.starts_with("mcp_servers.agentdiet.") {
                return Some("`[mcp_servers.agentdiet]` is already defined by hand".into());
            }
            continue;
        }
        let key = t.split('=').next().unwrap_or("").replace(['"', '\''], "");
        let key = key.trim().replace(' ', "");
        if table.is_empty() && (key == "mcp_servers" || key.starts_with("mcp_servers.agentdiet")) {
            return Some("`mcp_servers` is defined inline; cannot extend it safely".into());
        }
        if table == "mcp_servers" && (key == "agentdiet" || key.starts_with("agentdiet.")) {
            return Some("`mcp_servers.agentdiet` is already defined by hand".into());
        }
    }
    None
}

/// Ensure the agentdiet block is present and current.
pub fn patch(current: Option<&str>, exe: &Path) -> Result<Option<String>, PatchError> {
    let text = current.unwrap_or("");
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let new = match markers(&lines) {
        (Some(start), Some(end)) if start < end => {
            let before: String = lines[..start].concat();
            let after: String = lines[end + 1..].concat();
            if let Some(why) = conflict(&format!("{before}{after}")) {
                return Err(PatchError::Conflict(why));
            }
            format!("{before}{}{after}", block(exe))
        }
        (None, None) => {
            if let Some(why) = conflict(text) {
                return Err(PatchError::Conflict(why));
            }
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
        _ => {
            return Err(PatchError::Conflict(
                "unbalanced agentdiet markers; fix or delete them by hand".into(),
            ));
        }
    };
    Ok((new != text).then_some(new))
}

/// Remove the agentdiet block (and the blank separator line before it).
pub fn unpatch(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let (Some(start), Some(end)) = markers(&lines) else {
        return None;
    };
    if start >= end {
        return None;
    }
    let keep_to = if start > 0 && lines[start - 1].trim().is_empty() {
        start - 1
    } else {
        start
    };
    Some(format!("{}{}", lines[..keep_to].concat(), lines[end + 1..].concat()))
}

pub fn has_block(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    matches!(markers(&lines), (Some(s), Some(e)) if s < e)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn exe() -> PathBuf {
        PathBuf::from("/opt/bin/agentdiet")
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
        let updated = patch(Some(&first), &PathBuf::from("/new/agentdiet")).unwrap().unwrap();
        assert_eq!(updated.matches(START).count(), 1);
        assert!(updated.contains("/new/agentdiet"));
        assert!(!updated.contains("/opt/bin"));
        assert!(updated.starts_with("a = 1\n"));
    }

    #[test]
    fn unpatch_round_trips_exactly() {
        for original in ["", "a = 1\n", "a = 1", "a = 1\n\n[t]\nb = 2\n"] {
            let patched = patch(Some(original), &exe()).unwrap().unwrap();
            let restored = unpatch(&patched).unwrap();
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
        assert!(matches!(patch(Some(text), &exe()), Err(PatchError::Conflict(_))));
        let dotted = "[mcp_servers]\nagentdiet.command = \"x\"\n";
        assert!(matches!(patch(Some(dotted), &exe()), Err(PatchError::Conflict(_))));
        let inline = "mcp_servers = { a = { command = \"x\" } }\n";
        assert!(matches!(patch(Some(inline), &exe()), Err(PatchError::Conflict(_))));
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
        assert!(matches!(patch(Some(&text), &exe()), Err(PatchError::Conflict(_))));
    }

    #[test]
    fn windows_paths_are_escaped() {
        let out = patch(None, &PathBuf::from("C:\\bin\\agentdiet.exe")).unwrap().unwrap();
        assert!(out.contains("command = \"C:\\\\bin\\\\agentdiet.exe\""));
    }
}
