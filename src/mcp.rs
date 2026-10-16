//! Minimal stdio MCP server (newline-delimited JSON-RPC 2.0).
//!
//! Slice 1 only completes the handshake and advertises zero tools, so
//! assistants that `init` registered us do not report a failed server. The
//! real tools (`diet_inspect`, `diet_search`, `diet_exec`) arrive later.

use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

const DEFAULT_PROTOCOL: &str = "2024-11-05";

/// Serve requests from `reader` until EOF.
pub fn serve(reader: impl BufRead, mut writer: impl Write) -> io::Result<()> {
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = handle_line(&line) {
            writeln!(writer, "{response}")?;
            writer.flush()?;
        }
    }
    Ok(())
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn handle_line(line: &str) -> Option<Value> {
    let Ok(request) = serde_json::from_str::<Value>(line) else {
        return Some(error(Value::Null, -32700, "parse error"));
    };
    let Some(object) = request.as_object() else {
        return Some(error(Value::Null, -32600, "invalid request"));
    };
    let id = object.get("id").cloned();
    let method = object.get("method").and_then(Value::as_str).unwrap_or("");

    // Notifications (no id) never get a response.
    let id = id?;

    let result = match method {
        "initialize" => {
            let version = object
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_PROTOCOL);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "agentdiet", "version": env!("CARGO_PKG_VERSION") },
            })
        }
        "tools/list" => json!({ "tools": [] }),
        "ping" => json!({}),
        _ => return Some(error(id, -32601, "method not found")),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        serve(input.as_bytes(), &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn handshake_and_empty_tool_list() {
        let replies = exchange(concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#, "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, "\n",
            r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#, "\n",
        ));
        assert_eq!(replies.len(), 3, "the notification gets no reply");
        assert_eq!(replies[0]["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "agentdiet");
        assert_eq!(replies[1]["result"]["tools"], json!([]));
        assert_eq!(replies[2]["result"], json!({}));
    }

    #[test]
    fn unknown_methods_and_garbage_are_errors() {
        let replies = exchange("{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"nope\"}\nnot json\n");
        assert_eq!(replies[0]["error"]["code"], -32601);
        assert_eq!(replies[0]["id"], 9);
        assert_eq!(replies[1]["error"]["code"], -32700);
    }
}
