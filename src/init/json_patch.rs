//! Idempotent patching of JSON-based assistant configs.
//!
//! Every function works on text and returns `Ok(None)` when the file is
//! already in the desired state, so callers can skip the backup and the write.
//! A config that does not parse as strict JSON (JSONC comments, trailing
//! commas) is an error — callers skip the target rather than rewrite it.
//!
//! // pattern ref: rtk/src/hooks/init/claude.rs (behavior only) — BOM tolerance
//! // and "detect our own hook entry, update in place" semantics.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::basename;

/// Claude Code events agentdiet registers a hook for.
pub const CLAUDE_EVENTS: [&str; 5] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PreCompact",
];

/// Name of our entry in an MCP `servers` map.
pub const SERVER_NAME: &str = "agentdiet";

#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    #[error("not valid JSON ({0}); it may use comments or trailing commas")]
    Parse(String),
    #[error("the top-level value is not an object")]
    NotObject,
    #[error("`{0}` exists but has an unexpected shape")]
    BadShape(String),
    #[error("{0}")]
    Conflict(String),
}

struct Parsed {
    root: Value,
    bom: bool,
}

fn parse(text: Option<&str>) -> Result<Parsed, PatchError> {
    let text = text.unwrap_or("");
    let (body, bom) = match text.strip_prefix('\u{feff}') {
        Some(rest) => (rest, true),
        None => (text, false),
    };
    if body.trim().is_empty() {
        return Ok(Parsed {
            root: Value::Object(Map::new()),
            bom,
        });
    }
    let root: Value = serde_json::from_str(body).map_err(|e| PatchError::Parse(e.to_string()))?;
    if !root.is_object() {
        return Err(PatchError::NotObject);
    }
    Ok(Parsed { root, bom })
}

fn render(root: &Value, bom: bool) -> String {
    // Serializing a `Value` cannot fail.
    let mut out = serde_json::to_string_pretty(root).unwrap_or_default();
    out.push('\n');
    if bom {
        out.insert(0, '\u{feff}');
    }
    out
}

fn object_mut<'a>(
    parent: &'a mut Map<String, Value>,
    key: &str,
) -> Result<&'a mut Map<String, Value>, PatchError> {
    parent
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| PatchError::BadShape(key.to_string()))
}

fn root_object(root: &mut Value) -> Result<&mut Map<String, Value>, PatchError> {
    root.as_object_mut().ok_or(PatchError::NotObject)
}

fn finish(before: &Value, after: Value, bom: bool) -> Option<String> {
    (*before != after).then(|| render(&after, bom))
}

// ---- MCP server entries -------------------------------------------------

fn mcp_entry(exe: &Path, typed: bool) -> Value {
    let command = exe.to_string_lossy();
    if typed {
        json!({ "type": "stdio", "command": command, "args": ["mcp"] })
    } else {
        json!({ "command": command, "args": ["mcp"] })
    }
}

/// Ensure `<key>.agentdiet` is our MCP stdio entry.
pub fn patch_mcp(
    current: Option<&str>,
    key: &str,
    typed: bool,
    exe: &Path,
) -> Result<Option<String>, PatchError> {
    let Parsed { root: before, bom } = parse(current)?;
    let mut root = before.clone();
    object_mut(root_object(&mut root)?, key)?.insert(SERVER_NAME.into(), mcp_entry(exe, typed));
    Ok(finish(&before, root, bom))
}

/// Remove `<key>.agentdiet`, dropping `key` if that leaves it empty.
pub fn unpatch_mcp(text: &str, key: &str) -> Result<Option<String>, PatchError> {
    let Parsed { root: before, bom } = parse(Some(text))?;
    let mut root = before.clone();
    let obj = root_object(&mut root)?;
    if let Some(servers) = obj.get_mut(key).and_then(Value::as_object_mut) {
        servers.shift_remove(SERVER_NAME);
        if servers.is_empty() {
            obj.shift_remove(key);
        }
    }
    Ok(finish(&before, root, bom))
}

pub fn has_mcp(text: &str, key: &str) -> bool {
    parse(Some(text))
        .ok()
        .and_then(|p| p.root.get(key)?.get(SERVER_NAME).cloned())
        .is_some()
}

// ---- Claude Code hooks --------------------------------------------------

fn is_tool_event(event: &str) -> bool {
    matches!(event, "PreToolUse" | "PostToolUse")
}

/// `"<exe>" hook <Event>`
pub fn hook_command(exe: &Path, event: &str) -> String {
    format!("\"{}\" hook {event}", exe.to_string_lossy())
}

/// `true` for a command line that runs `agentdiet hook ...`.
pub fn is_agentdiet_hook_command(command: &str) -> bool {
    let command = command.trim();
    let (program, rest) = if let Some(quoted) = command.strip_prefix('"') {
        match quoted.find('"') {
            Some(end) => (&quoted[..end], quoted[end + 1..].trim_start()),
            None => return false,
        }
    } else {
        match command.split_once(char::is_whitespace) {
            Some((program, rest)) => (program, rest.trim_start()),
            None => (command, ""),
        }
    };
    matches!(basename(program), "agentdiet" | "agentdiet.exe")
        && (rest == "hook" || rest.starts_with("hook "))
}

fn is_ours(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(is_agentdiet_hook_command)
}

fn new_group(event: &str, command: String) -> Value {
    let hook = json!({ "type": "command", "command": command });
    if is_tool_event(event) {
        json!({ "matcher": "*", "hooks": [hook] })
    } else {
        json!({ "hooks": [hook] })
    }
}

/// Ensure each event in [`CLAUDE_EVENTS`] has exactly one agentdiet hook with
/// the canonical command. A matching hook is left untouched, a stale one is
/// updated in place, duplicates are removed, and foreign hooks are preserved.
pub fn patch_claude_hooks(current: Option<&str>, exe: &Path) -> Result<Option<String>, PatchError> {
    let Parsed { root: before, bom } = parse(current)?;
    let mut root = before.clone();
    let hooks = object_mut(root_object(&mut root)?, "hooks")?;

    for event in CLAUDE_EVENTS {
        let command = hook_command(exe, event);
        let groups = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| PatchError::BadShape(format!("hooks.{event}")))?;

        let mut found = false;
        let mut emptied = Vec::new();
        for (index, group) in groups.iter_mut().enumerate() {
            let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            let mut removed = false;
            list.retain_mut(|hook| {
                if !is_ours(hook) {
                    return true;
                }
                if found {
                    removed = true;
                    return false;
                }
                found = true;
                if let Some(obj) = hook.as_object_mut() {
                    obj.insert("type".into(), json!("command"));
                    obj.insert("command".into(), json!(command));
                }
                true
            });
            if removed && list.is_empty() {
                emptied.push(index);
            }
        }
        for index in emptied.into_iter().rev() {
            groups.remove(index);
        }
        if !found {
            groups.push(new_group(event, command));
        }
    }
    Ok(finish(&before, root, bom))
}

/// Remove every agentdiet hook, pruning groups, events and `hooks` itself when
/// they end up empty because of the removal.
pub fn unpatch_claude_hooks(text: &str) -> Result<Option<String>, PatchError> {
    let Parsed { root: before, bom } = parse(Some(text))?;
    let mut root = before.clone();
    let obj = root_object(&mut root)?;
    if let Some(hooks) = obj.get_mut("hooks").and_then(Value::as_object_mut) {
        let events: Vec<String> = hooks.keys().cloned().collect();
        for event in events {
            let Some(groups) = hooks.get_mut(&event).and_then(Value::as_array_mut) else {
                continue;
            };
            let mut touched = false;
            groups.retain_mut(|group| {
                let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                    return true;
                };
                let len = list.len();
                list.retain(|hook| !is_ours(hook));
                let removed = list.len() != len;
                touched |= removed;
                !(removed && list.is_empty())
            });
            if touched && groups.is_empty() {
                hooks.shift_remove(&event);
            }
        }
        if hooks.is_empty() {
            obj.shift_remove("hooks");
        }
    }
    Ok(finish(&before, root, bom))
}

pub fn has_claude_hooks(text: &str) -> bool {
    let Ok(parsed) = parse(Some(text)) else {
        return false;
    };
    let Some(hooks) = parsed.root.get("hooks").and_then(Value::as_object) else {
        return false;
    };
    hooks.values().filter_map(Value::as_array).any(|groups| {
        groups
            .iter()
            .filter_map(|g| g.get("hooks").and_then(Value::as_array))
            .any(|list| list.iter().any(is_ours))
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn exe() -> PathBuf {
        PathBuf::from("/opt/bin/agentdiet")
    }

    fn apply(text: &str) -> String {
        patch_claude_hooks(Some(text), &exe()).unwrap().unwrap()
    }

    fn count_ours(v: &Value, event: &str) -> usize {
        v["hooks"][event]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap())
            .filter(|h| is_ours(h))
            .count()
    }

    #[test]
    fn recognises_our_commands() {
        assert!(is_agentdiet_hook_command(
            "\"/opt/bin/agentdiet\" hook PreToolUse"
        ));
        assert!(is_agentdiet_hook_command("agentdiet hook SessionStart"));
        assert!(is_agentdiet_hook_command(
            "\"C:\\bin\\agentdiet.exe\" hook PreCompact"
        ));
        assert!(!is_agentdiet_hook_command("agentdiet show #4f9a"));
        assert!(!is_agentdiet_hook_command("rtk hook claude"));
        assert!(!is_agentdiet_hook_command("\"unterminated hook"));
    }

    #[test]
    fn hooks_are_added_once_and_second_pass_is_noop() {
        let first = apply("{\"model\":  \"x\"}");
        let v: Value = serde_json::from_str(&first).unwrap();
        for event in CLAUDE_EVENTS {
            assert_eq!(count_ours(&v, event), 1, "{event}");
        }
        assert_eq!(v["model"], "x");
        assert!(patch_claude_hooks(Some(&first), &exe()).unwrap().is_none());
    }

    #[test]
    fn stale_hook_is_updated_in_place_and_duplicates_removed() {
        let text = r#"{"hooks":{"PreToolUse":[
            {"matcher":"Bash","hooks":[
                {"type":"command","command":"echo foreign"},
                {"type":"command","command":"\"/old/agentdiet\" hook PreToolUse","timeout":5}]},
            {"matcher":"*","hooks":[{"type":"command","command":"/other/agentdiet hook PreToolUse"}]}
        ]}}"#;
        let v: Value = serde_json::from_str(&apply(text)).unwrap();
        assert_eq!(count_ours(&v, "PreToolUse"), 1);
        let groups = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "the emptied duplicate group is dropped");
        let list = groups[0]["hooks"].as_array().unwrap();
        assert_eq!(list[0]["command"], "echo foreign");
        assert_eq!(list[1]["command"], "\"/opt/bin/agentdiet\" hook PreToolUse");
        assert_eq!(list[1]["timeout"], 5, "extra user fields survive");
    }

    #[test]
    fn unpatch_removes_only_ours() {
        let patched = apply(
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo keep"}]}]}}"#,
        );
        let restored = unpatch_claude_hooks(&patched).unwrap().unwrap();
        let v: Value = serde_json::from_str(&restored).unwrap();
        assert_eq!(v["hooks"].as_object().unwrap().len(), 1);
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "echo keep"
        );
        assert!(!has_claude_hooks(&restored));
    }

    #[test]
    fn unpatch_of_pure_agentdiet_file_leaves_empty_object() {
        let patched = apply("{}");
        let restored = unpatch_claude_hooks(&patched).unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&restored).unwrap(), json!({}));
        assert!(unpatch_claude_hooks(&restored).unwrap().is_none());
    }

    #[test]
    fn mcp_entry_is_idempotent_and_keeps_other_servers() {
        let text = r#"{ "mcpServers": { "other": {"command":"x"} }, "n": 1 }"#;
        let first = patch_mcp(Some(text), "mcpServers", false, &exe())
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["mcpServers"]["agentdiet"]["args"], json!(["mcp"]));
        assert!(
            patch_mcp(Some(&first), "mcpServers", false, &exe())
                .unwrap()
                .is_none()
        );
        assert!(has_mcp(&first, "mcpServers"));

        let removed = unpatch_mcp(&first, "mcpServers").unwrap().unwrap();
        let v: Value = serde_json::from_str(&removed).unwrap();
        assert!(v["mcpServers"].get("agentdiet").is_none());
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
    }

    #[test]
    fn typed_entry_for_copilot_uses_servers_key() {
        let out = patch_mcp(None, "servers", true, &exe()).unwrap().unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["servers"]["agentdiet"]["type"], "stdio");
    }

    #[test]
    fn jsonc_is_rejected_not_rewritten() {
        let jsonc = "{\n  // comment\n  \"a\": 1,\n}\n";
        assert!(matches!(
            patch_mcp(Some(jsonc), "mcpServers", false, &exe()),
            Err(PatchError::Parse(_))
        ));
        assert!(patch_claude_hooks(Some(jsonc), &exe()).is_err());
    }

    #[test]
    fn bom_is_tolerated_and_preserved() {
        let text = "\u{feff}{\"a\":1}";
        let out = patch_mcp(Some(text), "mcpServers", false, &exe())
            .unwrap()
            .unwrap();
        assert!(out.starts_with('\u{feff}'));
        assert!(
            patch_mcp(Some(&out), "mcpServers", false, &exe())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn wrong_shapes_are_errors() {
        assert!(matches!(
            patch_mcp(Some("[]"), "mcpServers", false, &exe()),
            Err(PatchError::NotObject)
        ));
        assert!(matches!(
            patch_mcp(Some(r#"{"mcpServers": 3}"#), "mcpServers", false, &exe()),
            Err(PatchError::BadShape(_))
        ));
    }

    #[test]
    fn empty_file_is_treated_as_empty_object() {
        assert!(
            patch_mcp(Some("  \n"), "mcpServers", false, &exe())
                .unwrap()
                .is_some()
        );
    }
}
