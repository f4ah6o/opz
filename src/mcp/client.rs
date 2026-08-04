use crate::{find_command_path, DoctorCheck};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::{
    env,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

const OFFICIAL_MCP_COMMAND: &str = "1password-mcp";
const LEGACY_MCP_COMMAND: &str = "onepassword-mcp";
const MCP_COMMAND_ENV: &str = "OPZ_1PASSWORD_MCP_COMMAND";

pub(crate) trait OnePasswordMcp {
    fn list_tools(&mut self) -> Result<Vec<String>>;
    fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value>;
}

pub(crate) struct OnePasswordMcpStdioClient {
    child: Child,
    stdin: ChildStdin,
    stdout_lines: Receiver<std::result::Result<String, String>>,
    stderr_hints: Arc<Mutex<McpStderrHints>>,
    next_id: u64,
}

impl OnePasswordMcpStdioClient {
    pub(crate) fn connect() -> Result<Self> {
        let requested_command = onepassword_mcp_command();
        let (_, path, _) = resolve_onepassword_mcp_command().ok_or_else(|| {
            anyhow!(
                "1Password MCP server command `{requested_command}` was not found. Enable the local MCP server in the 1Password app, install a build that provides `{OFFICIAL_MCP_COMMAND}`, or set {MCP_COMMAND_ENV} to its executable path."
            )
        })?;

        let mut child = Command::new(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!("failed to start 1Password MCP server `{}`", path.display())
            })?;
        let stdin = child
            .stdin
            .take()
            .context("1Password MCP server stdin was not available")?;
        let stdout = child
            .stdout
            .take()
            .context("1Password MCP server stdout was not available")?;
        let stderr = child
            .stderr
            .take()
            .context("1Password MCP server stderr was not available")?;
        let stdout_lines = spawn_mcp_stdout_reader(stdout);
        let stderr_hints = Arc::new(Mutex::new(McpStderrHints::default()));
        spawn_mcp_stderr_reader(stderr, Arc::clone(&stderr_hints));

        let mut client = Self {
            child,
            stdin,
            stdout_lines,
            stderr_hints,
            next_id: 1,
        };
        client.initialize()?;
        Ok(client)
    }

    fn initialize(&mut self) -> Result<()> {
        let _ = self.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {
                    "name": "opz",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )?;
        self.notify("notifications/initialized", serde_json::json!({}))?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&payload)?;

        let timeout = mcp_command_timeout();
        let started = Instant::now();
        loop {
            let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
                return Err(self.protocol_error(format!(
                    "timed out waiting for 1Password MCP response to `{method}`"
                )));
            };
            let line = match self.stdout_lines.recv_timeout(remaining) {
                Ok(Ok(line)) => line,
                Ok(Err(_)) => {
                    return Err(self.protocol_error(format!(
                        "failed to read 1Password MCP response to `{method}`"
                    )));
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(self.protocol_error(format!(
                        "timed out waiting for 1Password MCP response to `{method}`"
                    )));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(self.protocol_error(format!(
                        "1Password MCP server exited before responding to `{method}`"
                    )));
                }
            };
            let value: Value = serde_json::from_str(line.trim()).map_err(|_| {
                self.protocol_error(format!(
                    "1Password MCP response to `{method}` was not valid JSON-RPC"
                ))
            })?;
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(self.protocol_error(format!(
                    "1Password MCP `{method}` failed: {}",
                    mcp_error_summary(error)
                )));
            }
            return value.get("result").cloned().ok_or_else(|| {
                self.protocol_error(format!(
                    "1Password MCP `{method}` response did not include result"
                ))
            });
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&payload)
    }

    fn write_message(&mut self, payload: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.stdin, payload).context("failed to encode MCP request")?;
        self.stdin
            .write_all(b"\n")
            .context("failed to write MCP request newline")?;
        self.stdin.flush().context("failed to flush MCP request")
    }

    fn protocol_error(&self, message: String) -> anyhow::Error {
        let hint = self
            .stderr_hints
            .lock()
            .ok()
            .and_then(|hints| hints.user_hint());
        match hint {
            Some(hint) => anyhow!("{message}. {hint}"),
            None => anyhow!(message),
        }
    }
}

impl Drop for OnePasswordMcpStdioClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl OnePasswordMcp for OnePasswordMcpStdioClient {
    fn list_tools(&mut self) -> Result<Vec<String>> {
        let result = self.request("tools/list", serde_json::json!({}))?;
        let mut names = result
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        if names.is_empty() {
            return Err(anyhow!(
                "1Password MCP `tools/list` response did not advertise any tools"
            ));
        }
        Ok(names)
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let result = self
            .request(
                "tools/call",
                serde_json::json!({
                    "name": name,
                    "arguments": arguments,
                }),
            )
            .with_context(|| format!("failed to call 1Password MCP tool `{name}`"))?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(anyhow!(
                "1Password MCP tool `{name}` reported an error. Check the 1Password approval prompt and Developer settings."
            ));
        }
        Ok(result)
    }
}

pub(crate) fn spawn_mcp_stdout_reader(
    stdout: ChildStdout,
) -> Receiver<std::result::Result<String, String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if tx.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    let _ = tx.send(Err(err.to_string()));
                    break;
                }
            }
        }
    });
    rx
}

fn spawn_mcp_stderr_reader(stderr: ChildStderr, hints: Arc<Mutex<McpStderrHints>>) {
    thread::spawn(move || {
        for line in BufReader::new(stderr)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if let Ok(mut hints) = hints.lock() {
                hints.observe(&line);
            }
        }
    });
}

#[derive(Default)]
struct McpStderrHints {
    parent_verification_failed: bool,
    permission_denied: bool,
    developer_feature_disabled: bool,
}

impl McpStderrHints {
    fn observe(&mut self, line: &str) {
        let line = line.to_ascii_lowercase();
        self.parent_verification_failed |= line.contains("parent process verification failed");
        self.permission_denied |= line.contains("permissiondenied")
            || line.contains("permission denied")
            || line.contains("binarypermissions");
        self.developer_feature_disabled |= line.contains("mcp server")
            && (line.contains("disabled") || line.contains("not enabled"));
    }

    fn user_hint(&self) -> Option<&'static str> {
        if self.parent_verification_failed || self.permission_denied {
            Some(
                "1Password rejected the MCP client process. Use the bundled `1password-mcp` directly and verify the client executable permissions and ownership.",
            )
        } else if self.developer_feature_disabled {
            Some(
                "Enable Settings > Labs > MCP Server and Settings > Developer > Integrate with MCP clients in the 1Password app.",
            )
        } else {
            None
        }
    }
}

pub(crate) fn onepassword_mcp_command() -> String {
    configured_mcp_command().unwrap_or_else(|| OFFICIAL_MCP_COMMAND.to_string())
}

fn configured_mcp_command() -> Option<String> {
    env::var(MCP_COMMAND_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolve_onepassword_mcp_command() -> Option<(String, PathBuf, bool)> {
    if let Some(command) = configured_mcp_command() {
        return find_command_path(&command).map(|path| (command, path, false));
    }

    find_command_path(OFFICIAL_MCP_COMMAND)
        .map(|path| (OFFICIAL_MCP_COMMAND.to_string(), path, false))
        .or_else(|| {
            find_command_path(LEGACY_MCP_COMMAND)
                .map(|path| (LEGACY_MCP_COMMAND.to_string(), path, true))
        })
}

pub(crate) fn mcp_command_timeout() -> Duration {
    env::var("OPZ_MCP_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(30))
}

pub(crate) fn check_onepassword_mcp_server() -> DoctorCheck {
    match resolve_onepassword_mcp_command() {
        Some((command, path, legacy)) => {
            let detail = if legacy {
                format!(
                    "{} (legacy `{command}` fallback; prefer `{OFFICIAL_MCP_COMMAND}`)",
                    path.display()
                )
            } else if configured_mcp_command().is_some() {
                format!("{} (configured by {MCP_COMMAND_ENV})", path.display())
            } else {
                path.display().to_string()
            };
            DoctorCheck::ok("1Password MCP", detail, false)
        }
        None => DoctorCheck::warn(
            "1Password MCP",
            format!(
                "`{}` not found in PATH; enable the local MCP server in 1Password or set {MCP_COMMAND_ENV}",
                onepassword_mcp_command()
            ),
        ),
    }
}

fn mcp_error_summary(error: &Value) -> String {
    match error.get("code").and_then(Value::as_i64) {
        Some(code) => format!("server returned error code {code}"),
        None => "server returned an error".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_hints_classify_parent_verification_without_echoing_server_output() {
        let mut hints = McpStderrHints::default();
        hints.observe(
            "Rejecting MCP connection: parent process verification failed: BinaryPermissions",
        );
        assert!(hints
            .user_hint()
            .unwrap()
            .contains("client executable permissions"));
    }
}
