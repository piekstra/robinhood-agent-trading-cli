//! `rhap mcp` end to end: the real binary over stdio, as Claude Code and Claude Desktop
//! launch it. Offline and keychain-free: every call here is answered before a request
//! would leave, and nothing in this path touches a credential.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

const TOOLS: [&str; 5] = [
    "rhap_accounts",
    "rhap_portfolio",
    "rhap_positions",
    "rhap_orders",
    "rhap_quotes",
];

/// A scratch home, so a test never reads the developer's real settings. Returns the
/// config file's path too.
fn rhap(test: &str) -> (Command, PathBuf) {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("mcp-stdio-{test}"));
    let config = root.join("rhap").join("config.json");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&config);
    let mut c = Command::new(env!("CARGO_BIN_EXE_rhap"));
    c.env_remove("RHAP_MCP_TOKEN")
        .env("XDG_CONFIG_HOME", &root)
        .env("HOME", &root)
        .env("USERPROFILE", &root);
    (c, config)
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Server {
    fn start(mut command: Command) -> Self {
        let mut child = command
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("rhap starts");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
        }
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(20))
                .expect("a response within 20 s");
            let msg: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("non-JSON on stdout ({e}): {line}"));
            if msg["id"] == json!(id) {
                return msg;
            }
        }
    }

    fn initialize(&mut self) -> Value {
        self.request(
            0,
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"}
            }),
        )
    }

    fn call(&mut self, id: u64, tool: &str, arguments: Value) -> Value {
        self.request(
            id,
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
        )
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(call: &Value) -> &str {
    call["result"]["content"][0]["text"].as_str().unwrap()
}

fn tool_names(list: &Value) -> Vec<&str> {
    list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect()
}

#[test]
fn a_client_initializes_lists_and_is_told_what_the_tools_read() {
    let (command, _) = rhap("reads");
    let mut s = Server::start(command);
    let init = s.initialize();
    assert_eq!(init["result"]["serverInfo"]["name"], "rhap", "{init}");
    let instructions = init["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("no tool can place, change or cancel"));
    assert!(instructions.contains("not affiliated"));
    assert!(instructions.contains("`rhap auth login`"), "{instructions}");

    let list = s.request(1, "tools/list", json!({}));
    assert_eq!(tool_names(&list["result"]), TOOLS);
    for tool in list["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["annotations"]["readOnlyHint"], true, "{tool}");
        assert_eq!(tool["annotations"]["destructiveHint"], false, "{tool}");
    }

    // Each is answered here, before a session is read or a request would leave.
    for args in [
        json!({"account": "SAMPLE01", "since": "soon"}),
        json!({"account": "SAMPLE01", "limit": 0}),
        json!({"account": " "}),
    ] {
        let call = s.call(2, "rhap_orders", args.clone());
        assert_eq!(call["result"]["isError"], true, "{args}: {call}");
        assert!(text(&call).starts_with("usage:"), "{args}: {call}");
    }
}

/// An order is not a tool, however it is asked for.
#[test]
fn asking_for_an_order_cancel_or_transfer_is_refused() {
    let (command, _) = rhap("orders");
    let mut s = Server::start(command);
    s.initialize();
    for tool in [
        "place_equity_order",
        "cancel_equity_order",
        "exercise_option",
        "rhap_place_order",
        "rhap_cancel_order",
        "rhap_transfer",
    ] {
        let args = json!({"account": "SAMPLE01", "symbol": "SAMPL", "side": "buy"});
        let call = s.call(1, tool, args);
        let refused = call["error"].is_object() || call["result"]["isError"] == true;
        assert!(refused, "{tool}: {call}");
    }
}

/// A config that cannot be parsed fails a call that needs it, not the server, and listing
/// the tools does not read it.
#[test]
fn a_broken_config_is_one_calls_error() {
    let (command, config) = rhap("broken");
    std::fs::write(&config, "{not json").unwrap();
    let mut s = Server::start(command);
    s.initialize();
    let list = s.request(1, "tools/list", json!({}));
    assert_eq!(tool_names(&list["result"]), TOOLS);
    let call = s.call(2, "rhap_accounts", json!({}));
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert!(text(&call).contains("rhap config show"), "{call}");
    assert!(!text(&call).contains("mcp-stdio"), "no path: {call}");
}

#[test]
fn list_tools_prints_the_definitions_without_a_config() {
    let (mut command, _) = rhap("list");
    let out = command.args(["mcp", "--list-tools"]).output().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(tool_names(&v), TOOLS);
}
