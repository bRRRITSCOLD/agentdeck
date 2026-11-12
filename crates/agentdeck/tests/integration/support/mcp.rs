//! A minimal MCP client over `agentdeck mcp`'s stdio: newline-delimited JSON-RPC, written by
//! hand so the test speaks the wire protocol rather than trusting rmcp on both ends.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use serde_json::{Value, json};

use super::TestEnv;

/// How long the server gets to answer one request.
const TIMEOUT: Duration = Duration::from_secs(20);

/// The protocol version the test client asks for.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// A running `agentdeck mcp`. Dropping it closes stdin and kills the server.
pub struct McpClient {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
}

impl McpClient {
    /// Starts `agentdeck mcp` in `env` with `vars` set, such as `AGENTDECK_WORKSPACE`.
    pub fn start(env: &TestEnv, vars: &[(&str, &str)]) -> McpClient {
        let mut command = env.command();
        command
            .arg("mcp")
            .envs(vars.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().expect("starting agentdeck mcp");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped stdout");
        let (sender, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        McpClient {
            child,
            stdin,
            lines,
            next_id: 1,
        }
    }

    fn write(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{message}").expect("writing to agentdeck mcp");
        stdin.flush().expect("flushing agentdeck mcp's stdin");
    }

    /// Sends a request and returns the response with the same id, skipping notifications.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let line = match self.lines.recv_timeout(TIMEOUT) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => panic!("no answer to {method} in {TIMEOUT:?}"),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("agentdeck mcp closed stdout before answering {method}")
                }
            };
            let message: Value = serde_json::from_str(&line).unwrap_or_else(|err| {
                panic!("agentdeck mcp wrote a non-JSON line ({err}): {line}")
            });
            if message["id"] == json!(id) {
                return message;
            }
        }
    }

    /// Sends a notification, which gets no answer.
    pub fn notify(&mut self, method: &str) {
        self.write(&json!({ "jsonrpc": "2.0", "method": method }));
    }

    /// `initialize`, then `notifications/initialized`; returns the initialize result.
    pub fn initialize(&mut self) -> Value {
        let response = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "agentdeck-integration-test", "version": "0" },
            }),
        );
        self.notify("notifications/initialized");
        response["result"].clone()
    }

    /// `tools/call`: returns the result, whose `isError` says whether the tool failed.
    pub fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let response = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        );
        assert!(
            response.get("error").is_none(),
            "{tool} was a protocol error: {response}"
        );
        response["result"].clone()
    }

    /// `tools/call` for a tool that must succeed; parses the JSON text it returns.
    pub fn call_ok(&mut self, tool: &str, arguments: Value) -> Value {
        let result = self.call(tool, arguments);
        assert_ne!(result["isError"], json!(true), "{tool} failed: {result}");
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{tool} returned no text: {result}"));
        serde_json::from_str(text)
            .unwrap_or_else(|err| panic!("{tool} returned non-JSON ({err}): {text}"))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
