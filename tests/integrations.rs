//! Every assistant, end to end, the way its client sees AgentDiet: `init` writes the assistant's own
//! config file; the test reads it in that client's format, starts exactly the command and arguments the
//! client would, and drives an MCP session (initialize, tools/list, a trimmed `diet_exec`, Vault
//! recovery through `diet_inspect`). Then `doctor --json` must report the assistant, `disconnect` must
//! restore the original bytes, and a second `init` must connect it again.
//!
//! This emulates the clients from their documented config formats; it is not a live run of the clients
//! themselves (see docs/RELEASE_READINESS.md for which were checked live).

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use common::Env;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_agentdiet");

fn agentdiet(env: &Env, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", &env.paths.home)
        .env("XDG_CONFIG_HOME", &env.paths.config_dir)
        .env("USERPROFILE", &env.paths.home)
        .env("APPDATA", &env.paths.config_dir)
        .env_remove("AGENTDIET_WORKSPACE")
        .current_dir(&env.paths.project_root)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

struct Client {
    slug: &'static str,
    /// The config file the client reads, relative to HOME (or the platform config dir).
    file: fn(&Env) -> PathBuf,
    /// The original file content before `init` (what `disconnect` must restore).
    original: &'static str,
    /// `(command, args)` exactly as the client reads them from its own format.
    server: fn(&str) -> (String, Vec<String>),
}

fn json_server(key: &'static str) -> impl Fn(&str) -> (String, Vec<String>) {
    move |text: &str| {
        let v: Value = serde_json::from_str(text).unwrap();
        let entry = &v[key]["agentdiet"];
        let args = entry["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect();
        (entry["command"].as_str().unwrap().to_string(), args)
    }
}

fn mcp_servers(text: &str) -> (String, Vec<String>) {
    json_server("mcpServers")(text)
}

fn vscode_servers(text: &str) -> (String, Vec<String>) {
    let v: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        v["servers"]["agentdiet"]["type"], "stdio",
        "VS Code needs the transport type"
    );
    json_server("servers")(text)
}

fn codex_toml(text: &str) -> (String, Vec<String>) {
    let doc: toml::Table = toml::from_str(text).unwrap();
    let entry = &doc["mcp_servers"]["agentdiet"];
    let args = entry["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    (entry["command"].as_str().unwrap().to_string(), args)
}

fn clients() -> Vec<Client> {
    vec![
        Client {
            slug: "claude",
            file: |e| e.paths.home.join(".claude.json"),
            original: "{\"numStartups\": 3}\n",
            server: mcp_servers,
        },
        Client {
            slug: "codex",
            file: |e| e.paths.home.join(".codex/config.toml"),
            original: "model = \"o3\"\n",
            server: codex_toml,
        },
        Client {
            slug: "gemini",
            file: |e| e.paths.home.join(".gemini/settings.json"),
            original: "{\"theme\": \"GitHub\"}\n",
            server: mcp_servers,
        },
        Client {
            slug: "cursor",
            file: |e| e.paths.home.join(".cursor/mcp.json"),
            original: "{\"mcpServers\": {}}\n",
            server: mcp_servers,
        },
        Client {
            slug: "windsurf",
            file: |e| e.paths.home.join(".codeium/windsurf/mcp_config.json"),
            original: "{\"mcpServers\": {}}\n",
            server: mcp_servers,
        },
        Client {
            slug: "copilot",
            file: copilot_file,
            original: "{\"servers\": {}}\n",
            server: vscode_servers,
        },
    ]
}

/// VS Code's user `mcp.json` lives in the platform config dir, as `dirs::config_dir` resolves it.
fn copilot_file(env: &Env) -> PathBuf {
    let base = if cfg!(target_os = "macos") {
        env.paths.home.join("Library/Application Support")
    } else {
        env.paths.config_dir.clone() // XDG_CONFIG_HOME on Linux, APPDATA on Windows
    };
    base.join("Code/User/mcp.json")
}

/// One MCP session against `command args…`, as the client would start it.
fn mcp_session(env: &Env, command: &str, args: &[String], requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(command)
        .args(args)
        .env("HOME", &env.paths.home)
        .env("USERPROFILE", &env.paths.home)
        .env("XDG_CONFIG_HOME", &env.paths.config_dir)
        .env("APPDATA", &env.paths.config_dir)
        .current_dir(&env.paths.project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = String::new();
    for r in requests {
        input.push_str(&r.to_string());
        input.push('\n');
    }
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
}

fn text_of(response: &Value) -> String {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

fn long_output_command() -> Vec<&'static str> {
    if cfg!(windows) {
        vec!["cmd", "/C", "for /L %i in (1,1,4000) do @echo %i"]
    } else {
        vec!["seq", "1", "4000"]
    }
}

#[test]
fn every_assistant_reaches_the_mcp_server_recovers_output_and_disconnects_cleanly() {
    for client in clients() {
        let env = Env::empty();
        let file = (client.file)(&env);
        env.write(file.clone(), client.original);

        let init = agentdiet(&env, &["init", "--only", client.slug]);
        assert!(
            init.status.success(),
            "{}: {}",
            client.slug,
            String::from_utf8_lossy(&init.stderr)
        );
        let text = std::fs::read_to_string(&file).unwrap();
        let (command, args) = (client.server)(&text);
        assert!(
            std::path::Path::new(&command).is_absolute(),
            "{}: the client is given an absolute path: {command}",
            client.slug
        );
        assert_eq!(args, ["mcp"], "{}", client.slug);

        // The client's view: start what the config says, speak MCP.
        let responses = mcp_session(
            &env,
            &command,
            &args,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":client.slug,"version":"test"}}}),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
                call(3, "diet_exec", json!({"command": long_output_command()})),
            ],
        );
        assert_eq!(
            responses[0]["result"]["serverInfo"]["name"], "agentdiet",
            "{}",
            client.slug
        );
        let tools: Vec<&str> = responses[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            tools,
            ["diet_inspect", "diet_search", "diet_exec", "diet_reflect"],
            "{}",
            client.slug
        );
        let trimmed = text_of(&responses[2]);
        assert!(
            trimmed.len() < 4000 * 3,
            "{}: output was trimmed",
            client.slug
        );
        let crumb = trimmed
            .lines()
            .find(|l| l.starts_with("🥗 Diet: "))
            .expect("breadcrumb");
        let id = crumb.rsplit("'#").next().unwrap().trim_end_matches('\'');

        // Vault recovery through the same client connection type.
        let recovered = mcp_session(
            &env,
            &command,
            &args,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                call(
                    2,
                    "diet_inspect",
                    json!({"id": format!("#{id}"), "start_line": 3999, "end_line": 4000}),
                ),
            ],
        );
        assert_eq!(
            text_of(&recovered[1]).replace("\r\n", "\n").trim_end(), // `cmd`'s echo ends lines CRLF
            "3999\n4000",
            "{}",
            client.slug
        );
        let shown = agentdiet(&env, &["show", &format!("#{id}")]);
        assert_eq!(
            shown
                .stdout
                .split(|b| *b == b'\n')
                .filter(|l| !l.is_empty())
                .count(),
            4000
        );

        // Diagnostics see it.
        let doctor: Value =
            serde_json::from_slice(&agentdiet(&env, &["doctor", "--json"]).stdout).unwrap();
        let row = doctor["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["target"] == client.slug && c["scope"] == "user")
            .unwrap_or_else(|| panic!("{}: no doctor row", client.slug));
        assert_eq!(row["level"], "ok", "{}: {row}", client.slug);

        // Disconnect restores the client's file byte for byte; reconnecting works again.
        let out = agentdiet(&env, &["disconnect", client.slug]);
        assert!(out.status.success(), "{}", client.slug);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            client.original,
            "{}",
            client.slug
        );
        let doctor: Value =
            serde_json::from_slice(&agentdiet(&env, &["doctor", "--json"]).stdout).unwrap();
        assert!(
            !doctor["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["target"] == client.slug && c["level"] == "ok"),
            "{}: still reported as connected",
            client.slug
        );
        assert!(
            agentdiet(&env, &["init", "--only", client.slug])
                .status
                .success()
        );
        let again = std::fs::read_to_string(&file).unwrap();
        assert_eq!((client.server)(&again), (command, args), "{}", client.slug);
    }
}
