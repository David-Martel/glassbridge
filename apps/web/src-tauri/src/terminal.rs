// Interactive ADB shell terminal over WebSocket.
//
// Each WebSocket connection at `/ws/terminal/{serial}` owns one
// [`TerminalSession`].  Sessions are independent — no shared state exists
// between connections.
//
// ## Wire protocol
//
// The client sends JSON messages; the server replies with JSON.  All message
// shapes are defined by [`TerminalRequest`] (inbound) and [`TerminalResponse`]
// (outbound).
//
// ### Client → server
//
// | `type`      | Extra fields              | Description                                |
// |-------------|---------------------------|--------------------------------------------|
// | `execute`   | `command: String`         | Run `adb shell <command>` and stream result|
// | `history`   | —                         | Return the command history list            |
// | `output`    | —                         | Return the full terminal output buffer     |
// | `inject`    | `command: String`         | Add a command to the input without running |
//
// ### Server → client
//
// | `type`      | Extra fields                                              |
// |-------------|-----------------------------------------------------------|
// | `result`    | `command`, `stdout`, `stderr`, `exitCode`, `durationMs`   |
// | `history`   | `commands: Vec<String>`                                   |
// | `output`    | `lines: Vec<String>`                                      |
// | `injected`  | `command: String`                                         |
// | `error`     | `message: String`                                         |
//
// ## LLM hook support
//
// Every executed command and its output are retained in the session so an LLM
// agent can call `history` / `output` to get full context, or `inject` to
// pre-fill the client's input field before the user runs the command.
//
// ## Output buffer cap
//
// The output buffer is capped at [`OUTPUT_LINE_CAP`] lines (10 000).  When the
// cap is exceeded the oldest lines are dropped, matching common terminal
// emulator behaviour.

use std::{path::PathBuf, time::Instant};

use axum::extract::WebSocketUpgrade;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path, State,
    },
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::{adb::DeviceOps, server::AppState};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of lines kept in the per-session output buffer.
///
/// When this limit is reached, the oldest lines are evicted (ring-buffer
/// semantics implemented with [`Vec::drain`]).
const OUTPUT_LINE_CAP: usize = 10_000;

// ---------------------------------------------------------------------------
// Wire types — inbound (client → server)
// ---------------------------------------------------------------------------

/// All message shapes the client may send to a terminal session.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::terminal::TerminalRequest;
///
/// let json = r#"{"type":"execute","command":"getprop ro.product.model"}"#;
/// let req: TerminalRequest = serde_json::from_str(json).unwrap();
/// assert!(matches!(req, TerminalRequest::Execute { .. }));
/// ```
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TerminalRequest {
    /// Run `adb shell <command>` on the session's device.
    Execute {
        /// The shell command to execute (passed verbatim to `adb shell`).
        command: String,
    },
    /// Return the list of commands executed in this session.
    History,
    /// Return the full output buffer accumulated in this session.
    Output,
    /// Add `command` to the injected-command buffer without executing it.
    Inject {
        /// The command text to inject into the client's input field.
        command: String,
    },
}

// ---------------------------------------------------------------------------
// Wire types — outbound (server → client)
// ---------------------------------------------------------------------------

/// All message shapes the server may send back to the client.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::terminal::TerminalResponse;
///
/// let resp = TerminalResponse::History {
///     commands: vec!["getprop ro.product.model".into()],
/// };
/// let json = serde_json::to_string(&resp).unwrap();
/// assert!(json.contains("\"type\":\"history\""));
/// ```
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TerminalResponse {
    /// Result of an `execute` request.
    Result {
        /// The command that was run.
        command: String,
        /// Standard output from the shell command (UTF-8).
        stdout: String,
        /// Standard error from the shell command (UTF-8).
        stderr: String,
        /// Process exit code (`-1` if the process was killed).
        #[serde(rename = "exitCode")]
        exit_code: i32,
        /// Wall-clock time for the ADB round-trip in milliseconds.
        #[serde(rename = "durationMs")]
        duration_ms: u64,
    },
    /// Response to a `history` request.
    History {
        /// Ordered list of commands executed in this session, oldest first.
        commands: Vec<String>,
    },
    /// Response to an `output` request.
    Output {
        /// All terminal output lines accumulated in this session.
        lines: Vec<String>,
    },
    /// Acknowledgement of an `inject` request.
    Injected {
        /// The command that was injected.
        command: String,
    },
    /// Returned when a request cannot be processed.
    Error {
        /// Human-readable error description.
        message: String,
    },
}

// ---------------------------------------------------------------------------
// TerminalSession
// ---------------------------------------------------------------------------

/// Per-connection state for an interactive ADB shell terminal session.
///
/// Create one per WebSocket connection; drop it when the connection closes.
/// All fields are local to the connection — there is no cross-session sharing.
pub struct TerminalSession {
    /// ADB serial of the target device.
    serial: String,
    /// Ordered list of commands executed in this session, oldest first.
    history: Vec<String>,
    /// Terminal output buffer: prompt lines and command output interleaved.
    ///
    /// Capped at [`OUTPUT_LINE_CAP`] lines; oldest lines are evicted first.
    output: Vec<String>,
    /// Filesystem path to the vendored `adb` binary directory.
    #[allow(dead_code)]
    vendor_root: PathBuf,
}

impl TerminalSession {
    /// Create a new session for `serial`, resolving binaries from `vendor_root`.
    pub fn new(serial: impl Into<String>, vendor_root: PathBuf) -> Self {
        Self {
            serial: serial.into(),
            history: Vec::new(),
            output: Vec::new(),
            vendor_root,
        }
    }

    // -- private helpers ---------------------------------------------------

    /// Append lines to the output buffer, evicting old lines when the cap is hit.
    fn append_output(&mut self, lines: impl IntoIterator<Item = String>) {
        for line in lines {
            if self.output.len() >= OUTPUT_LINE_CAP {
                // Evict oldest 10 % of lines in one drain call to amortise
                // the cost over many appends.
                let evict = OUTPUT_LINE_CAP / 10;
                self.output.drain(..evict);
            }
            self.output.push(line);
        }
    }

    // -- public message handlers -------------------------------------------

    /// Execute `command` via `adb.shell_command` and record the result.
    ///
    /// Appends a prompt line (`$ <command>`) followed by stdout lines to the
    /// output buffer, then records the command in [`history`].
    ///
    /// # Errors
    ///
    /// Returns a [`TerminalResponse::Error`] when the ADB backend is
    /// unreachable; callers should forward it to the client rather than
    /// treating it as fatal.
    pub async fn execute(
        &mut self,
        command: &str,
        adb: &(dyn DeviceOps + Send + Sync),
    ) -> TerminalResponse {
        // Record prompt line immediately so the output buffer always shows
        // commands before their output (consistent with a real shell).
        self.append_output([format!("$ {command}")]);

        let started = Instant::now();
        let result = adb.shell_command(&self.serial, command).await;
        let duration_ms = started.elapsed().as_millis() as u64;

        match result {
            Ok(cr) => {
                // Append stdout to the output buffer line-by-line.
                self.append_output(cr.stdout.lines().map(ToOwned::to_owned));

                // Record in history after a successful (or non-zero exit) run.
                self.history.push(command.to_owned());

                TerminalResponse::Result {
                    command: command.to_owned(),
                    stdout: cr.stdout,
                    stderr: cr.stderr,
                    exit_code: cr.exit_code,
                    duration_ms,
                }
            }
            Err(err) => {
                let msg = err.to_string();
                self.append_output([format!("error: {msg}")]);
                TerminalResponse::Error { message: msg }
            }
        }
    }

    /// Return the command history for this session.
    pub fn get_history(&self) -> TerminalResponse {
        TerminalResponse::History {
            commands: self.history.clone(),
        }
    }

    /// Return the full output buffer for this session.
    pub fn get_output(&self) -> TerminalResponse {
        TerminalResponse::Output {
            lines: self.output.clone(),
        }
    }

    /// Inject `command` into the session (records it, does not execute).
    ///
    /// The intended use is for LLM agents to pre-fill the client's input
    /// field.  The client receives an `injected` response acknowledging the
    /// injection.
    pub fn inject(&mut self, command: &str) -> TerminalResponse {
        self.append_output([format!("> {command}")]);
        TerminalResponse::Injected {
            command: command.to_owned(),
        }
    }
}

// ---------------------------------------------------------------------------
// Axum handler
// ---------------------------------------------------------------------------

/// Axum handler that upgrades the connection to a WebSocket terminal session.
///
/// Route: `GET /ws/terminal/{serial}`
///
/// Each connection owns an independent [`TerminalSession`]; sessions are not
/// shared between concurrent connections to the same serial.
///
/// # Examples
///
/// ```text
/// // JavaScript
/// const ws = new WebSocket("ws://127.0.0.1:PORT/ws/terminal/19281FDEE000GS");
/// ws.send(JSON.stringify({ type: "execute", command: "getprop ro.product.model" }));
/// ws.onmessage = (e) => console.log(JSON.parse(e.data));
/// ```
pub async fn terminal_ws_handler(
    ws: WebSocketUpgrade,
    Path(serial): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_terminal_session(socket, serial, state))
}

/// Drive a single WebSocket terminal session.
///
/// Reads JSON [`TerminalRequest`] frames from the client, dispatches them to
/// the session, and sends back [`TerminalResponse`] frames.  The loop exits
/// when the client closes the connection or a network error is detected.
async fn handle_terminal_session(mut socket: WebSocket, serial: String, state: AppState) {
    debug!(serial = %serial, "terminal session opened");

    let mut session = TerminalSession::new(serial.clone(), state.vendor_root.clone());

    while let Some(msg_result) = socket.recv().await {
        match msg_result {
            Ok(Message::Text(text)) => {
                let response = dispatch_request(&text, &mut session, &*state.adb).await;
                let payload = match serde_json::to_string(&response) {
                    Ok(s) => s,
                    Err(err) => {
                        // Serialisation should never fail for our types, but
                        // handle it gracefully rather than silently dropping.
                        warn!(serial = %serial, "terminal: failed to serialise response: {err}");
                        serde_json::json!({
                            "type": "error",
                            "message": format!("internal serialisation error: {err}")
                        })
                        .to_string()
                    }
                };
                if socket.send(Message::Text(payload.into())).await.is_err() {
                    break; // client disconnected mid-send
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {} // ignore binary / ping / pong frames
        }
    }

    debug!(serial = %serial, "terminal session closed");
}

/// Parse `text` as a [`TerminalRequest`] and dispatch to the session.
///
/// Returns a [`TerminalResponse::Error`] when the JSON is malformed so the
/// caller always has a sendable response.
async fn dispatch_request(
    text: &str,
    session: &mut TerminalSession,
    adb: &(dyn DeviceOps + Send + Sync),
) -> TerminalResponse {
    let request: TerminalRequest = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(err) => {
            return TerminalResponse::Error {
                message: format!("invalid request: {err}"),
            };
        }
    };

    match request {
        TerminalRequest::Execute { command } => session.execute(&command, adb).await,
        TerminalRequest::History => session.get_history(),
        TerminalRequest::Output => session.get_output(),
        TerminalRequest::Inject { command } => session.inject(&command),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Request deserialisation
    // -----------------------------------------------------------------------

    #[test]
    fn deserialises_execute_request() {
        let json = r#"{"type":"execute","command":"getprop ro.product.model"}"#;
        let req: TerminalRequest = serde_json::from_str(json).unwrap();
        assert!(
            matches!(req, TerminalRequest::Execute { command } if command == "getprop ro.product.model")
        );
    }

    #[test]
    fn deserialises_history_request() {
        let json = r#"{"type":"history"}"#;
        let req: TerminalRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req, TerminalRequest::History));
    }

    #[test]
    fn deserialises_output_request() {
        let json = r#"{"type":"output"}"#;
        let req: TerminalRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req, TerminalRequest::Output));
    }

    #[test]
    fn deserialises_inject_request() {
        let json = r#"{"type":"inject","command":"ls /sdcard"}"#;
        let req: TerminalRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req, TerminalRequest::Inject { command } if command == "ls /sdcard"));
    }

    #[test]
    fn unknown_type_fails_gracefully() {
        let json = r#"{"type":"bogus"}"#;
        let result: Result<TerminalRequest, _> = serde_json::from_str(json);
        assert!(result.is_err(), "unknown type must not parse");
    }

    // -----------------------------------------------------------------------
    // Response serialisation
    // -----------------------------------------------------------------------

    #[test]
    fn serialises_result_response_with_camel_case_fields() {
        let resp = TerminalResponse::Result {
            command: "pwd".into(),
            stdout: "/\n".into(),
            stderr: String::new(),
            exit_code: 0,
            duration_ms: 42,
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["type"], "result");
        assert_eq!(v["command"], "pwd");
        assert_eq!(v["exitCode"], 0);
        assert_eq!(v["durationMs"], 42);
        assert!(v.get("exit_code").is_none(), "must be camelCase");
        assert!(v.get("duration_ms").is_none(), "must be camelCase");
    }

    #[test]
    fn serialises_history_response() {
        let resp = TerminalResponse::History {
            commands: vec!["getprop ro.product.model".into(), "ls /sdcard".into()],
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["type"], "history");
        assert_eq!(v["commands"][0], "getprop ro.product.model");
        assert_eq!(v["commands"][1], "ls /sdcard");
    }

    #[test]
    fn serialises_output_response() {
        let resp = TerminalResponse::Output {
            lines: vec!["$ pwd".into(), "/".into()],
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["type"], "output");
        assert_eq!(v["lines"][0], "$ pwd");
    }

    #[test]
    fn serialises_injected_response() {
        let resp = TerminalResponse::Injected {
            command: "ls /sdcard".into(),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["type"], "injected");
        assert_eq!(v["command"], "ls /sdcard");
    }

    #[test]
    fn serialises_error_response() {
        let resp = TerminalResponse::Error {
            message: "adb not found".into(),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["type"], "error");
        assert_eq!(v["message"], "adb not found");
    }

    // -----------------------------------------------------------------------
    // TerminalSession — history tracking
    // -----------------------------------------------------------------------

    // Minimal stub that records calls for synchronous history / inject / output
    // tests without requiring a real ADB process.
    use crate::adb::{AdbError, CommandResult, DeviceRecord, ScrcpyPreset};
    use async_trait::async_trait;
    use std::sync::Arc;

    struct StubAdb {
        stdout: String,
        exit_code: i32,
    }

    #[async_trait]
    impl DeviceOps for StubAdb {
        async fn list_devices(&self) -> Result<Vec<DeviceRecord>, AdbError> {
            Ok(vec![])
        }
        async fn list_packages(&self, _serial: &str) -> Result<Vec<String>, AdbError> {
            Ok(vec![])
        }
        async fn send_key(&self, _serial: &str, _action: &str) -> Result<CommandResult, AdbError> {
            Ok(CommandResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
            })
        }
        async fn tap(&self, _serial: &str, _x: f64, _y: f64) -> Result<CommandResult, AdbError> {
            Ok(CommandResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
            })
        }
        async fn input_text(&self, _serial: &str, _text: &str) -> Result<CommandResult, AdbError> {
            Ok(CommandResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
            })
        }
        async fn launch_package(
            &self,
            _serial: &str,
            _package: &str,
        ) -> Result<CommandResult, AdbError> {
            Ok(CommandResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
            })
        }
        async fn stop_package(
            &self,
            _serial: &str,
            _package: &str,
        ) -> Result<CommandResult, AdbError> {
            Ok(CommandResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
            })
        }
        async fn shell_command(
            &self,
            _serial: &str,
            _command: &str,
        ) -> Result<CommandResult, AdbError> {
            Ok(CommandResult {
                stdout: self.stdout.clone(),
                stderr: String::new(),
                exit_code: self.exit_code,
            })
        }
        async fn capture_screenshot(&self, _serial: &str) -> Result<Vec<u8>, AdbError> {
            Ok(vec![])
        }
        async fn start_scrcpy(&self, _serial: &str, _preset: ScrcpyPreset) -> Result<(), AdbError> {
            Ok(())
        }
    }

    fn stub_adb(stdout: &str) -> Arc<StubAdb> {
        Arc::new(StubAdb {
            stdout: stdout.to_owned(),
            exit_code: 0,
        })
    }

    #[tokio::test]
    async fn execute_records_command_in_history() {
        let adb = stub_adb("Pixel 10 Pro XL\n");
        let mut session = TerminalSession::new("TEST001", PathBuf::from("/vendor"));
        session.execute("getprop ro.product.model", &*adb).await;

        let resp = session.get_history();
        let TerminalResponse::History { commands } = resp else {
            panic!("expected History response");
        };
        assert_eq!(commands, vec!["getprop ro.product.model"]);
    }

    #[tokio::test]
    async fn execute_appends_prompt_and_stdout_to_output() {
        let adb = stub_adb("Pixel 10 Pro XL\n");
        let mut session = TerminalSession::new("TEST001", PathBuf::from("/vendor"));
        session.execute("getprop ro.product.model", &*adb).await;

        let resp = session.get_output();
        let TerminalResponse::Output { lines } = resp else {
            panic!("expected Output response");
        };
        assert_eq!(lines[0], "$ getprop ro.product.model");
        assert_eq!(lines[1], "Pixel 10 Pro XL");
    }

    #[tokio::test]
    async fn multiple_executes_accumulate_history() {
        let adb = stub_adb("");
        let mut session = TerminalSession::new("TEST001", PathBuf::from("/vendor"));
        session.execute("cmd1", &*adb).await;
        session.execute("cmd2", &*adb).await;
        session.execute("cmd3", &*adb).await;

        let TerminalResponse::History { commands } = session.get_history() else {
            panic!("expected History response");
        };
        assert_eq!(commands, vec!["cmd1", "cmd2", "cmd3"]);
    }

    #[tokio::test]
    async fn result_response_contains_duration_and_exit_code() {
        let adb = Arc::new(StubAdb {
            stdout: "ok\n".into(),
            exit_code: 42,
        });
        let mut session = TerminalSession::new("TEST001", PathBuf::from("/vendor"));
        let resp = session.execute("exit 42", &*adb).await;

        let TerminalResponse::Result {
            exit_code, stdout, ..
        } = resp
        else {
            panic!("expected Result response");
        };
        assert_eq!(exit_code, 42);
        assert_eq!(stdout, "ok\n");
    }

    // -----------------------------------------------------------------------
    // TerminalSession — inject
    // -----------------------------------------------------------------------

    #[test]
    fn inject_returns_injected_response_and_does_not_add_to_history() {
        let mut session = TerminalSession::new("TEST001", PathBuf::from("/vendor"));
        let resp = session.inject("ls /sdcard");

        assert!(
            matches!(&resp, TerminalResponse::Injected { command } if command == "ls /sdcard"),
            "expected Injected response"
        );

        // inject must not pollute history — only execute does that.
        let TerminalResponse::History { commands } = session.get_history() else {
            panic!("expected History response");
        };
        assert!(
            commands.is_empty(),
            "history should be empty after inject-only"
        );
    }

    #[test]
    fn inject_appends_inject_marker_to_output() {
        let mut session = TerminalSession::new("TEST001", PathBuf::from("/vendor"));
        session.inject("ls /sdcard");

        let TerminalResponse::Output { lines } = session.get_output() else {
            panic!("expected Output response");
        };
        assert_eq!(lines[0], "> ls /sdcard");
    }

    // -----------------------------------------------------------------------
    // TerminalSession — output buffer cap
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn output_buffer_is_capped_at_limit() {
        // Build a stub whose stdout produces exactly 1 line per call.
        // We need more than OUTPUT_LINE_CAP lines total (prompt + stdout).
        // Each execute() appends 2 lines (prompt + 1 stdout line).
        let adb = stub_adb("x");
        let mut session = TerminalSession::new("TEST001", PathBuf::from("/vendor"));

        let calls_needed = OUTPUT_LINE_CAP; // 10 000 calls → 20 000 raw lines
        for _ in 0..calls_needed {
            session.execute("echo x", &*adb).await;
        }

        let TerminalResponse::Output { lines } = session.get_output() else {
            panic!("expected Output response");
        };
        assert!(
            lines.len() <= OUTPUT_LINE_CAP,
            "output buffer exceeded cap: {} lines",
            lines.len()
        );
    }

    // -----------------------------------------------------------------------
    // dispatch_request
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn dispatch_returns_error_on_invalid_json() {
        let adb = stub_adb("");
        let mut session = TerminalSession::new("TEST001", PathBuf::from("/vendor"));
        let resp = dispatch_request("not json", &mut session, &*adb).await;
        assert!(
            matches!(resp, TerminalResponse::Error { .. }),
            "malformed JSON must yield Error response"
        );
    }

    #[tokio::test]
    async fn dispatch_execute_end_to_end() {
        let adb = stub_adb("Pixel 10 Pro XL\n");
        let mut session = TerminalSession::new("TEST001", PathBuf::from("/vendor"));
        let json = r#"{"type":"execute","command":"getprop ro.product.model"}"#;
        let resp = dispatch_request(json, &mut session, &*adb).await;

        let TerminalResponse::Result {
            command,
            stdout,
            exit_code,
            ..
        } = resp
        else {
            panic!("expected Result response, got: {resp:?}");
        };
        assert_eq!(command, "getprop ro.product.model");
        assert_eq!(stdout, "Pixel 10 Pro XL\n");
        assert_eq!(exit_code, 0);
    }

    #[tokio::test]
    async fn dispatch_history_end_to_end() {
        let adb = stub_adb("");
        let mut session = TerminalSession::new("TEST001", PathBuf::from("/vendor"));
        dispatch_request(r#"{"type":"execute","command":"pwd"}"#, &mut session, &*adb).await;

        let resp = dispatch_request(r#"{"type":"history"}"#, &mut session, &*adb).await;
        let TerminalResponse::History { commands } = resp else {
            panic!("expected History response");
        };
        assert_eq!(commands, vec!["pwd"]);
    }

    #[tokio::test]
    async fn dispatch_output_end_to_end() {
        let adb = stub_adb("/\n");
        let mut session = TerminalSession::new("TEST001", PathBuf::from("/vendor"));
        dispatch_request(r#"{"type":"execute","command":"pwd"}"#, &mut session, &*adb).await;

        let resp = dispatch_request(r#"{"type":"output"}"#, &mut session, &*adb).await;
        let TerminalResponse::Output { lines } = resp else {
            panic!("expected Output response");
        };
        assert!(lines.contains(&"$ pwd".to_owned()));
        assert!(lines.contains(&"/".to_owned()));
    }

    #[tokio::test]
    async fn dispatch_inject_end_to_end() {
        let adb = stub_adb("");
        let mut session = TerminalSession::new("TEST001", PathBuf::from("/vendor"));
        let resp = dispatch_request(
            r#"{"type":"inject","command":"ls /sdcard"}"#,
            &mut session,
            &*adb,
        )
        .await;
        assert!(matches!(resp, TerminalResponse::Injected { command } if command == "ls /sdcard"));
    }
}
