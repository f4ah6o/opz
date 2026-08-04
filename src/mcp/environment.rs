use super::{OnePasswordMcp, OnePasswordMcpStdioClient};
use crate::instrumentation;
use anyhow::{anyhow, Result};
use regex::Regex;
use serde_json::Value;
use std::{collections::HashSet, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvironmentRecord {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum McpEnvironmentAction {
    List,
    Create {
        name: String,
    },
    Rename {
        environment: String,
        new_name: String,
    },
    Variables {
        environment: String,
    },
    Add {
        environment: String,
        variables: Vec<String>,
    },
    Mount {
        environment: String,
        path: PathBuf,
    },
    Mounts {
        environment: String,
    },
    Tools,
}

pub(crate) fn run_environment_cli(
    account: Option<&str>,
    action: &McpEnvironmentAction,
) -> Result<()> {
    let mut client = OnePasswordMcpStdioClient::connect()?;
    let output = run_environment_command(&mut client, account, action)?;
    instrumentation::with_span("write_outputs", vec![], || {
        print!("{output}");
    });
    Ok(())
}

pub(crate) fn run_environment_command(
    client: &mut dyn OnePasswordMcp,
    account: Option<&str>,
    action: &McpEnvironmentAction,
) -> Result<String> {
    if matches!(action, McpEnvironmentAction::Tools) {
        return Ok(render_lines(&client.list_tools()?));
    }

    let account_id = resolve_mcp_account_id(client, account)?;
    match action {
        McpEnvironmentAction::List => {
            let environments = list_mcp_environments(client, &account_id)?;
            Ok(render_environment_records(&environments))
        }
        McpEnvironmentAction::Create { name } => {
            let environment = create_mcp_environment(client, &account_id, name)?;
            Ok(format!("{}\t{}\n", environment.id, environment.name))
        }
        McpEnvironmentAction::Rename {
            environment,
            new_name,
        } => {
            let existing = resolve_mcp_environment(client, &account_id, environment)?;
            let renamed = rename_mcp_environment(client, &account_id, &existing.id, new_name)?;
            Ok(format!("{}\t{}\n", renamed.id, renamed.name))
        }
        McpEnvironmentAction::Variables { environment } => {
            let environment = resolve_mcp_environment(client, &account_id, environment)?;
            let variables = list_mcp_variable_names(client, &account_id, &environment.id)?;
            Ok(render_lines(&variables))
        }
        McpEnvironmentAction::Add {
            environment,
            variables,
        } => {
            let environment = resolve_mcp_environment(client, &account_id, environment)?;
            let variables =
                append_mcp_placeholder_variables(client, &account_id, &environment.id, variables)?;
            Ok(render_lines(&variables))
        }
        McpEnvironmentAction::Mount { environment, path } => {
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
        McpEnvironmentAction::Mounts { environment } => {
            let environment = resolve_mcp_environment(client, &account_id, environment)?;
            let mounts = list_mcp_local_env_files(client, &account_id, &environment.id)?;
            Ok(render_lines(&mounts))
        }
        McpEnvironmentAction::Tools => unreachable!("handled before authentication"),
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

pub(crate) fn append_mcp_placeholder_variables(
    client: &mut dyn OnePasswordMcp,
    account_id: &str,
    environment_id: &str,
    variables: &[String],
) -> Result<Vec<String>> {
    let name_pattern = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$")?;
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    for name in variables {
        if !name_pattern.is_match(name) {
            return Err(anyhow!(
                "Invalid Environment variable name `{name}`. Use shell-compatible names such as API_TOKEN."
            ));
        }
        if seen.insert(name.clone()) {
            names.push(name.clone());
        }
    }
    if names.is_empty() {
        return Err(anyhow!(
            "At least one Environment variable name is required"
        ));
    }

    let payload = names
        .iter()
        .map(|name| {
            serde_json::json!({
                "name": name,
                "value": "",
                "concealed": true,
            })
        })
        .collect::<Vec<_>>();
    let _ = client.call_tool(
        "append_variables",
        serde_json::json!({
            "accountId": account_id,
            "environmentId": environment_id,
            "variables": payload,
        }),
    )?;
    Ok(names)
}

pub(crate) fn create_mcp_local_env_file(
    client: &mut dyn OnePasswordMcp,
    account_id: &str,
    environment_id: &str,
    environment_name: &str,
    path: &std::path::Path,
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

pub(crate) fn mcp_result_values(result: &Value) -> Vec<Value> {
    let mut values = Vec::new();
    if let Some(value) = result.get("structuredContent") {
        values.push(value.clone());
    }
    if let Some(value) = result.get("content") {
        values.push(value.clone());
        if let Some(items) = value.as_array() {
            for item in items {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                        values.push(parsed);
                    } else {
                        values.push(Value::String(text.to_string()));
                    }
                }
            }
        }
    }
    values.push(result.clone());
    values
}

pub(crate) fn extract_environment_records(values: &[Value]) -> Vec<EnvironmentRecord> {
    let mut records = Vec::new();
    for value in values {
        collect_environment_records(value, &mut records);
    }
    dedupe_environment_records(records)
}

pub(crate) fn collect_environment_records(value: &Value, records: &mut Vec<EnvironmentRecord>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_environment_records(item, records);
            }
        }
        Value::Object(map) => {
            let id = map
                .get("environmentId")
                .or_else(|| map.get("environment_id"))
                .or_else(|| map.get("id"))
                .and_then(Value::as_str);
            let name = map
                .get("environmentName")
                .or_else(|| map.get("environment_name"))
                .or_else(|| map.get("name"))
                .and_then(Value::as_str);
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

pub(crate) fn extract_variable_names(values: &[Value]) -> Vec<String> {
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

pub(crate) fn extract_mount_paths(values: &[Value]) -> Vec<String> {
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

pub(crate) fn extract_first_string_for_keys(values: &[Value], keys: &[&str]) -> Option<String> {
    let mut strings = Vec::new();
    for value in values {
        collect_strings_for_keys(value, keys, &mut strings);
    }
    strings.into_iter().next()
}

pub(crate) fn collect_strings_for_keys(value: &Value, keys: &[&str], out: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_strings_for_keys(item, keys, out);
            }
        }
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.get(*key).and_then(Value::as_str) {
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

pub(crate) fn collect_string_array_for_keys(value: &Value, keys: &[&str], out: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_string_array_for_keys(item, keys, out);
            }
        }
        Value::Object(map) => {
            for key in keys {
                if let Some(items) = map.get(*key).and_then(Value::as_array) {
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
