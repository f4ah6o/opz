use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Scenario {
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Step {
    pub tool: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub exit_code: i32,
    #[serde(default)]
    pub delay_ms: u64,
    #[serde(default)]
    pub read_stdin: bool,
    #[serde(default)]
    pub capture_env: Vec<String>,
    #[serde(default)]
    pub mcp_results: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Invocation {
    pub tool: String,
    pub args: Vec<String>,
    pub stdin: String,
    pub env: BTreeMap<String, String>,
}

#[allow(dead_code)]
pub fn args_match(expected: &[String], actual: &[String]) -> bool {
    expected.len() == actual.len()
        && expected
            .iter()
            .zip(actual)
            .all(|(expected, actual)| expected == "*" || expected == actual)
}
