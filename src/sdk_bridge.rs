use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::{
    env,
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{mpsc, Mutex, OnceLock},
    thread,
    time::Duration,
};

const SDK_BRIDGE_REQUEST_LIMIT: usize = 1024 * 1024;
const SDK_BRIDGE_RESPONSE_LIMIT: usize = 16 * 1024 * 1024;

struct SdkBridgeProcess {
    child: Child,
    stdin: ChildStdin,
    responses: mpsc::Receiver<String>,
    next_id: u64,
}

impl SdkBridgeProcess {
    fn spawn() -> Result<Self> {
        let executable = env::current_exe().context("locate opz executable for SDK bridge")?;
        let mut child = Command::new(executable)
            .arg("__sdk-bridge")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("start isolated 1Password SDK bridge")?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("SDK bridge stdin was unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("SDK bridge stdout was unavailable"))?;
        let (sender, responses) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            responses,
            next_id: 1,
        })
    }

    fn call(&mut self, account: &str, operation: &str, parameters: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let request = serde_json::to_vec(&json!({
            "id": id,
            "account": account,
            "operation": operation,
            "parameters": parameters,
        }))?;
        if request.len() > SDK_BRIDGE_REQUEST_LIMIT {
            return Err(anyhow!("SDK bridge request exceeded the size limit"));
        }
        self.stdin
            .write_all(&request)
            .context("write SDK bridge request")?;
        self.stdin
            .write_all(b"\n")
            .context("finish SDK bridge request")?;
        self.stdin.flush().context("flush SDK bridge request")?;

        let line =
            self.responses
                .recv_timeout(sdk_bridge_timeout())
                .map_err(|error| match error {
                    mpsc::RecvTimeoutError::Timeout => anyhow!(
                        "1Password Desktop SDK bridge timed out after {} seconds",
                        sdk_bridge_timeout().as_secs()
                    ),
                    mpsc::RecvTimeoutError::Disconnected => {
                        anyhow!("1Password Desktop SDK bridge exited unexpectedly")
                    }
                })?;
        if line.len() > SDK_BRIDGE_RESPONSE_LIMIT {
            return Err(anyhow!("SDK bridge response exceeded the size limit"));
        }
        let response: Value = serde_json::from_str(&line).context("parse SDK bridge response")?;
        if response.get("id").and_then(Value::as_u64) != Some(id) {
            return Err(anyhow!("SDK bridge response ID did not match request"));
        }
        if response.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(anyhow!("1Password Desktop SDK bridge operation failed"));
        }
        response
            .get("value")
            .cloned()
            .ok_or_else(|| anyhow!("SDK bridge response omitted value"))
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for SdkBridgeProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Default)]
struct SdkBridgeState {
    process: Option<SdkBridgeProcess>,
    disabled: bool,
}

static SDK_BRIDGE_STATE: OnceLock<Mutex<SdkBridgeState>> = OnceLock::new();

pub(crate) fn sdk_bridge_call(account: &str, operation: &str, parameters: Value) -> Result<Value> {
    let state = SDK_BRIDGE_STATE.get_or_init(|| Mutex::new(SdkBridgeState::default()));
    let mut state = state
        .lock()
        .map_err(|_| anyhow!("1Password Desktop SDK bridge state was poisoned"))?;
    if state.disabled {
        return Err(anyhow!(
            "1Password Desktop SDK bridge is disabled for this process"
        ));
    }
    if state.process.is_none() {
        state.process = Some(SdkBridgeProcess::spawn()?);
    }
    let result = state
        .process
        .as_mut()
        .ok_or_else(|| anyhow!("1Password Desktop SDK bridge was unavailable"))?
        .call(account, operation, parameters);
    if result.is_err() {
        if let Some(mut process) = state.process.take() {
            process.stop();
        }
        // One failed or timed-out SDK operation is enough to use the stable CLI
        // fallback for the rest of this short-lived opz invocation.
        state.disabled = true;
    }
    result
}

pub(crate) fn sdk_bridge_timeout() -> Duration {
    env::var("OPZ_SDK_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(10))
}

pub(crate) fn run_sdk_bridge_child() -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let mut client: Option<(String, onepassword_sdk_unofficial::Client)> = None;
    for request in stdin.lock().lines() {
        let request = match request {
            Ok(request) if request.len() <= SDK_BRIDGE_REQUEST_LIMIT => request,
            _ => break,
        };
        let value: Value = match serde_json::from_str(&request) {
            Ok(value) => value,
            Err(_) => break,
        };
        let id = value.get("id").and_then(Value::as_u64).unwrap_or(0);
        let account = match value.get("account").and_then(Value::as_str) {
            Some(account) if !account.is_empty() => account,
            _ => {
                write_bridge_failure(&mut stdout, id)?;
                continue;
            }
        };
        let operation = match value.get("operation").and_then(Value::as_str) {
            Some(operation) => operation,
            None => {
                write_bridge_failure(&mut stdout, id)?;
                continue;
            }
        };
        let parameters = value.get("parameters").cloned().unwrap_or(Value::Null);

        let same_account = client
            .as_ref()
            .map(|(configured, _)| configured == account)
            .unwrap_or(false);
        if !same_account {
            client = build_bridge_client(account)
                .ok()
                .map(|client| (account.to_owned(), client));
        }
        let Some((_, sdk)) = client.as_mut() else {
            write_bridge_failure(&mut stdout, id)?;
            continue;
        };

        let result = run_bridge_operation(sdk, operation, &parameters);
        match result {
            Ok(value) => write_bridge_success(&mut stdout, id, value)?,
            Err(_) => write_bridge_failure(&mut stdout, id)?,
        }
    }
    Ok(())
}

fn build_bridge_client(account: &str) -> Result<onepassword_sdk_unofficial::Client> {
    let auth = onepassword_sdk_unofficial::DesktopAuth::new(account)?;
    onepassword_sdk_unofficial::Client::builder(auth)
        .integration_name("opz")
        .integration_version(env!("CARGO_PKG_VERSION"))
        .build()
        .map_err(Into::into)
}

fn run_bridge_operation(
    client: &mut onepassword_sdk_unofficial::Client,
    operation: &str,
    parameters: &Value,
) -> Result<Value> {
    match operation {
        "vaults_list" => Ok(Value::Array(client.vaults().list()?)),
        "items_list" => {
            let vault_id = required_string(parameters, "vault_id")?;
            Ok(Value::Array(client.items().list(vault_id)?))
        }
        "items_get" => {
            let vault_id = required_string(parameters, "vault_id")?;
            let item_id = required_string(parameters, "item_id")?;
            Ok(client.items().get(vault_id, item_id)?)
        }
        "items_get_all" => {
            let vault_id = required_string(parameters, "vault_id")?;
            let item_ids = parameters
                .get("item_ids")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("item_ids were missing"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| anyhow!("item ID was invalid"))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(client.items().get_all(vault_id, &item_ids)?)
        }
        "items_put" => {
            let item = parameters
                .get("item")
                .cloned()
                .ok_or_else(|| anyhow!("item was missing"))?;
            Ok(client.items().put(item)?)
        }
        "secrets_resolve_all" => {
            let references = parameters
                .get("references")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("references were missing"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| anyhow!("secret reference was invalid"))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(serde_json::to_value(
                client.secrets().resolve_all(&references)?,
            )?)
        }
        _ => Err(anyhow!("unknown SDK bridge operation")),
    }
}

fn required_string<'a>(parameters: &'a Value, key: &str) -> Result<&'a str> {
    parameters
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("SDK bridge parameter was missing"))
}

fn write_bridge_success(stdout: &mut impl Write, id: u64, value: Value) -> Result<()> {
    let response = serde_json::to_vec(&json!({"id": id, "ok": true, "value": value}))?;
    if response.len() > SDK_BRIDGE_RESPONSE_LIMIT {
        return write_bridge_failure(stdout, id);
    }
    stdout.write_all(&response)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

fn write_bridge_failure(stdout: &mut impl Write, id: u64) -> Result<()> {
    let response = serde_json::to_vec(&json!({"id": id, "ok": false}))?;
    stdout.write_all(&response)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_bridge_timeout_has_safe_default() {
        if env::var_os("OPZ_SDK_TIMEOUT_SECONDS").is_none() {
            assert_eq!(sdk_bridge_timeout(), Duration::from_secs(10));
        }
    }

    #[test]
    fn bridge_failure_never_contains_upstream_details() {
        let mut output = Vec::new();
        write_bridge_failure(&mut output, 42).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"id\":42,\"ok\":false}\n"
        );
    }
}
