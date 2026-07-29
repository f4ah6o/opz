use crate::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvironmentRecord {
    pub(crate) id: String,
    pub(crate) name: String,
}

pub(crate) trait OnePasswordMcp {
    fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> Result<serde_json::Value>;
}

pub(crate) struct OnePasswordMcpStdioClient {
    child: Child,
    stdin: ChildStdin,
    stdout_lines: Receiver<std::result::Result<String, String>>,
    next_id: u64,
}

impl OnePasswordMcpStdioClient {
    fn connect() -> Result<Self> {
        let command = onepassword_mcp_command();
        let path = find_command_path(&command).ok_or_else(|| {
            anyhow!(
                "1Password MCP server command `{command}` was not found. Set OPZ_1PASSWORD_MCP_COMMAND to the executable path, or install `onepassword-mcp` on PATH."
            )
        })?;

        let mut child = Command::new(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
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
        let stdout_lines = spawn_mcp_stdout_reader(stdout);

        let mut client = Self {
            child,
            stdin,
            stdout_lines,
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

    fn request(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
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
            let remaining = timeout.checked_sub(started.elapsed()).ok_or_else(|| {
                anyhow!("timed out waiting for 1Password MCP response to `{method}`")
            })?;
            let line = match self.stdout_lines.recv_timeout(remaining) {
                Ok(Ok(line)) => line,
                Ok(Err(err)) => {
                    return Err(anyhow!(
                        "failed to read 1Password MCP response to `{method}`: {err}"
                    ));
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(anyhow!(
                        "timed out waiting for 1Password MCP response to `{method}`"
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(anyhow!(
                        "1Password MCP server exited before responding to `{method}`"
                    ));
                }
            };
            let value: serde_json::Value = serde_json::from_str(line.trim())
                .with_context(|| format!("1Password MCP response to `{method}` was not JSON"))?;
            if value.get("id").and_then(serde_json::Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(anyhow!(
                    "1Password MCP `{method}` failed: {}",
                    mcp_error_summary(error)
                ));
            }
            return value.get("result").cloned().ok_or_else(|| {
                anyhow!("1Password MCP `{method}` response did not include result")
            });
        }
    }

    fn notify(&mut self, method: &str, params: serde_json::Value) -> Result<()> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&payload)
    }

    fn write_message(&mut self, payload: &serde_json::Value) -> Result<()> {
        serde_json::to_writer(&mut self.stdin, payload).context("failed to encode MCP request")?;
        self.stdin
            .write_all(b"\n")
            .context("failed to write MCP request newline")?;
        self.stdin.flush().context("failed to flush MCP request")
    }
}

pub(crate) fn spawn_mcp_stdout_reader(
    stdout: std::process::ChildStdout,
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

impl Drop for OnePasswordMcpStdioClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl OnePasswordMcp for OnePasswordMcpStdioClient {
    fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> Result<serde_json::Value> {
        self.request(
            "tools/call",
            serde_json::json!({
                "name": name,
                "arguments": arguments,
            }),
        )
        .with_context(|| format!("failed to call 1Password MCP tool `{name}`"))
    }
}

pub(crate) fn run_environment_cli(
    account: Option<&str>,
    command: &EnvironmentCommand,
) -> Result<()> {
    let mut client = OnePasswordMcpStdioClient::connect()?;
    let output = run_environment_command(&mut client, account, command)?;
    instrumentation::with_span("write_outputs", vec![], || {
        print!("{output}");
    });
    Ok(())
}

pub(crate) fn run_environment_command(
    client: &mut dyn OnePasswordMcp,
    account: Option<&str>,
    command: &EnvironmentCommand,
) -> Result<String> {
    let account_id = resolve_mcp_account_id(client, account)?;
    match command {
        EnvironmentCommand::List => {
            let environments = list_mcp_environments(client, &account_id)?;
            Ok(render_environment_records(&environments))
        }
        EnvironmentCommand::Create { name } => {
            let environment = create_mcp_environment(client, &account_id, name)?;
            Ok(format!("{}\t{}\n", environment.id, environment.name))
        }
        EnvironmentCommand::Rename {
            environment,
            new_name,
        } => {
            let existing = resolve_mcp_environment(client, &account_id, environment)?;
            let renamed = rename_mcp_environment(client, &account_id, &existing.id, new_name)?;
            Ok(format!("{}\t{}\n", renamed.id, renamed.name))
        }
        EnvironmentCommand::Variables { environment } => {
            let environment = resolve_mcp_environment(client, &account_id, environment)?;
            let variables = list_mcp_variable_names(client, &account_id, &environment.id)?;
            Ok(render_lines(&variables))
        }
        EnvironmentCommand::Mount { environment, path } => {
            let environment = resolve_mcp_environment(client, &account_id, environment)?;
            let mounts = create_mcp_local_env_file(
                client,
                &account_id,
                &environment.id,
                &environment.name,
                path,
            )?;
            Ok(render_lines(&mounts))
        }
        EnvironmentCommand::Mounts { environment } => {
            let environment = resolve_mcp_environment(client, &account_id, environment)?;
            let mounts = list_mcp_local_env_files(client, &account_id, &environment.id)?;
            Ok(render_lines(&mounts))
        }
    }
}

pub(crate) fn resolve_mcp_account_id(
    client: &mut dyn OnePasswordMcp,
    account: Option<&str>,
) -> Result<String> {
    if let Some(account) = account {
        return Ok(account.to_string());
    }
    let result = client.call_tool("authenticate", serde_json::json!({}))?;
    extract_first_string_for_keys(&mcp_result_values(&result), &["accountId", "account_id"])
        .ok_or_else(|| anyhow!("1Password MCP authenticate response did not include accountId"))
}

pub(crate) fn list_mcp_environments(
    client: &mut dyn OnePasswordMcp,
    account_id: &str,
) -> Result<Vec<EnvironmentRecord>> {
    let result = client.call_tool(
        "list_environments",
        serde_json::json!({ "accountId": account_id }),
    )?;
    let mut environments = extract_environment_records(&mcp_result_values(&result));
    environments.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    Ok(environments)
}

pub(crate) fn create_mcp_environment(
    client: &mut dyn OnePasswordMcp,
    account_id: &str,
    name: &str,
) -> Result<EnvironmentRecord> {
    let result = client.call_tool(
        "create_environment",
        serde_json::json!({
            "accountId": account_id,
            "environmentName": name,
        }),
    )?;
    extract_environment_records(&mcp_result_values(&result))
        .into_iter()
        .next()
        .unwrap_or_else(|| EnvironmentRecord {
            id: extract_first_string_for_keys(
                &mcp_result_values(&result),
                &["environmentId", "id"],
            )
            .unwrap_or_default(),
            name: name.to_string(),
        })
        .validate("create_environment")
}

pub(crate) fn rename_mcp_environment(
    client: &mut dyn OnePasswordMcp,
    account_id: &str,
    environment_id: &str,
    new_name: &str,
) -> Result<EnvironmentRecord> {
    let result = client.call_tool(
        "rename_environment",
        serde_json::json!({
            "accountId": account_id,
            "environmentId": environment_id,
            "environmentName": new_name,
        }),
    )?;
    extract_environment_records(&mcp_result_values(&result))
        .into_iter()
        .next()
        .unwrap_or_else(|| EnvironmentRecord {
            id: environment_id.to_string(),
            name: new_name.to_string(),
        })
        .validate("rename_environment")
}

pub(crate) fn resolve_mcp_environment(
    client: &mut dyn OnePasswordMcp,
    account_id: &str,
    query: &str,
) -> Result<EnvironmentRecord> {
    let environments = list_mcp_environments(client, account_id)?;
    let matches: Vec<_> = environments
        .into_iter()
        .filter(|environment| environment.id == query || environment.name == query)
        .collect();
    match matches.as_slice() {
        [environment] => Ok(environment.clone()),
        [] => Err(anyhow!(
            "No 1Password Environment matched `{query}` by exact ID or name"
        )),
        _ => Err(anyhow!(
            "Multiple 1Password Environments are named `{query}`. Use the Environment ID instead."
        )),
    }
}

pub(crate) fn list_mcp_variable_names(
    client: &mut dyn OnePasswordMcp,
    account_id: &str,
    environment_id: &str,
) -> Result<Vec<String>> {
    let result = client.call_tool(
        "list_variables",
        serde_json::json!({
            "accountId": account_id,
            "environmentId": environment_id,
        }),
    )?;
    Ok(extract_variable_names(&mcp_result_values(&result)))
}

pub(crate) fn create_mcp_local_env_file(
    client: &mut dyn OnePasswordMcp,
    account_id: &str,
    environment_id: &str,
    environment_name: &str,
    path: &Path,
) -> Result<Vec<String>> {
    let result = client.call_tool(
        "create_local_env_file",
        serde_json::json!({
            "accountId": account_id,
            "environmentId": environment_id,
            "environmentName": environment_name,
            "mountPath": path.display().to_string(),
        }),
    )?;
    let mut mounts = extract_mount_paths(&mcp_result_values(&result));
    if mounts.is_empty() {
        mounts.push(path.display().to_string());
    }
    Ok(mounts)
}

pub(crate) fn list_mcp_local_env_files(
    client: &mut dyn OnePasswordMcp,
    account_id: &str,
    environment_id: &str,
) -> Result<Vec<String>> {
    let result = client.call_tool(
        "list_local_env_files",
        serde_json::json!({
            "accountId": account_id,
            "environmentId": environment_id,
        }),
    )?;
    Ok(extract_mount_paths(&mcp_result_values(&result)))
}

impl EnvironmentRecord {
    fn validate(self, tool: &str) -> Result<Self> {
        if self.id.is_empty() {
            return Err(anyhow!(
                "1Password MCP `{tool}` response did not include environment ID"
            ));
        }
        if self.name.is_empty() {
            return Err(anyhow!(
                "1Password MCP `{tool}` response did not include environment name"
            ));
        }
        Ok(self)
    }
}

pub(crate) fn render_environment_records(environments: &[EnvironmentRecord]) -> String {
    let mut out = String::new();
    for environment in environments {
        out.push_str(&format!("{}\t{}\n", environment.id, environment.name));
    }
    out
}

pub(crate) fn render_lines(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

pub(crate) fn onepassword_mcp_command() -> String {
    env::var("OPZ_1PASSWORD_MCP_COMMAND")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "onepassword-mcp".to_string())
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
    let command = onepassword_mcp_command();
    match find_command_path(&command) {
        Some(path) => DoctorCheck::ok(
            "1Password MCP",
            format!(
                "{} (set OPZ_1PASSWORD_MCP_COMMAND to override)",
                path.display()
            ),
            false,
        ),
        None => DoctorCheck::warn(
            "1Password MCP",
            format!("`{command}` not found in PATH; needed by `opz environment` commands"),
        ),
    }
}

pub(crate) fn mcp_result_values(result: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut values = Vec::new();
    if let Some(value) = result.get("structuredContent") {
        values.push(value.clone());
    }
    if let Some(value) = result.get("content") {
        values.push(value.clone());
        if let Some(items) = value.as_array() {
            for item in items {
                if let Some(text) = item.get("text").and_then(serde_json::Value::as_str) {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) {
                        values.push(parsed);
                    } else {
                        values.push(serde_json::Value::String(text.to_string()));
                    }
                }
            }
        }
    }
    values.push(result.clone());
    values
}

pub(crate) fn extract_environment_records(values: &[serde_json::Value]) -> Vec<EnvironmentRecord> {
    let mut records = Vec::new();
    for value in values {
        collect_environment_records(value, &mut records);
    }
    dedupe_environment_records(records)
}

pub(crate) fn collect_environment_records(
    value: &serde_json::Value,
    records: &mut Vec<EnvironmentRecord>,
) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_environment_records(item, records);
            }
        }
        serde_json::Value::Object(map) => {
            let id = map
                .get("environmentId")
                .or_else(|| map.get("environment_id"))
                .or_else(|| map.get("id"))
                .and_then(serde_json::Value::as_str);
            let name = map
                .get("environmentName")
                .or_else(|| map.get("environment_name"))
                .or_else(|| map.get("name"))
                .and_then(serde_json::Value::as_str);
            if let (Some(id), Some(name)) = (id, name) {
                records.push(EnvironmentRecord {
                    id: id.to_string(),
                    name: name.to_string(),
                });
            }
            for child in map.values() {
                collect_environment_records(child, records);
            }
        }
        _ => {}
    }
}

pub(crate) fn dedupe_environment_records(
    records: Vec<EnvironmentRecord>,
) -> Vec<EnvironmentRecord> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for record in records {
        if seen.insert((record.id.clone(), record.name.clone())) {
            deduped.push(record);
        }
    }
    deduped
}

pub(crate) fn extract_variable_names(values: &[serde_json::Value]) -> Vec<String> {
    let mut names = Vec::new();
    for value in values {
        collect_strings_for_keys(
            value,
            &["variableName", "variable_name", "name"],
            &mut names,
        );
        collect_string_array_for_keys(
            value,
            &["variables", "variableNames", "variable_names"],
            &mut names,
        );
        if let Some(items) = value.as_array() {
            for item in items {
                if let Some(name) = item.as_str() {
                    names.push(name.to_string());
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

pub(crate) fn extract_mount_paths(values: &[serde_json::Value]) -> Vec<String> {
    let mut mounts = Vec::new();
    for value in values {
        collect_strings_for_keys(
            value,
            &["mountPath", "mount_path", "path", "filePath", "file_path"],
            &mut mounts,
        );
        collect_string_array_for_keys(value, &["mounts", "localEnvFiles", "files"], &mut mounts);
    }
    mounts.sort();
    mounts.dedup();
    mounts
}

pub(crate) fn extract_first_string_for_keys(
    values: &[serde_json::Value],
    keys: &[&str],
) -> Option<String> {
    let mut strings = Vec::new();
    for value in values {
        collect_strings_for_keys(value, keys, &mut strings);
    }
    strings.into_iter().next()
}

pub(crate) fn collect_strings_for_keys(
    value: &serde_json::Value,
    keys: &[&str],
    out: &mut Vec<String>,
) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_strings_for_keys(item, keys, out);
            }
        }
        serde_json::Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.get(*key).and_then(serde_json::Value::as_str) {
                    out.push(value.to_string());
                }
            }
            for child in map.values() {
                collect_strings_for_keys(child, keys, out);
            }
        }
        _ => {}
    }
}

pub(crate) fn collect_string_array_for_keys(
    value: &serde_json::Value,
    keys: &[&str],
    out: &mut Vec<String>,
) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_string_array_for_keys(item, keys, out);
            }
        }
        serde_json::Value::Object(map) => {
            for key in keys {
                if let Some(items) = map.get(*key).and_then(serde_json::Value::as_array) {
                    for item in items {
                        if let Some(text) = item.as_str() {
                            out.push(text.to_string());
                        }
                    }
                }
            }
            for child in map.values() {
                collect_string_array_for_keys(child, keys, out);
            }
        }
        _ => {}
    }
}

pub(crate) fn mcp_error_summary(error: &serde_json::Value) -> String {
    match error.get("code").and_then(serde_json::Value::as_i64) {
        Some(code) => format!("server returned error code {code}"),
        None => "server returned an error".to_string(),
    }
}
