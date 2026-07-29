#[path = "support/protocol.rs"]
mod protocol;

use protocol::{Invocation, Scenario, Step};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Harness {
    _temp: tempfile::TempDir,
    root: PathBuf,
    bin: PathBuf,
    logs: PathBuf,
    scenario: PathBuf,
}

impl Harness {
    fn new(steps: Vec<Step>, tools: &[&str]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let bin = root.join("bin");
        let logs = root.join("logs");
        let scenario = root.join("scenario.json");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&logs).unwrap();
        fs::write(
            &scenario,
            serde_json::to_vec_pretty(&Scenario { steps }).unwrap(),
        )
        .unwrap();

        for tool in tools {
            install_tool(&bin, tool);
        }

        Self {
            _temp: temp,
            root,
            bin,
            logs,
            scenario,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_opz"));
        let mut paths = vec![self.bin.clone()];
        if let Some(existing) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&existing));
        }
        command
            .current_dir(&self.root)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("OPZ_TEST_SCENARIO", &self.scenario)
            .env("OPZ_TEST_LOG_DIR", &self.logs)
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env(
                "OPZ_1PASSWORD_MCP_COMMAND",
                tool_path(&self.bin, "onepassword-mcp"),
            );
        command
    }

    fn output(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn invocation(&self, index: usize) -> Invocation {
        serde_json::from_slice(&fs::read(self.logs.join(format!("{index:03}.json"))).unwrap())
            .unwrap()
    }

    fn mcp_requests(&self, index: usize) -> Vec<serde_json::Value> {
        fs::read_to_string(self.logs.join(format!("{index:03}.mcp.jsonl")))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

fn step(tool: &str, args: &[&str], stdout: &str) -> Step {
    Step {
        tool: tool.to_string(),
        args: args.iter().map(|value| value.to_string()).collect(),
        stdout: stdout.to_string(),
        ..Step::default()
    }
}

fn item_json(title: &str) -> String {
    format!(
        r#"{{"id":"item-id","title":"{title}","vault":{{"id":"vault-id","name":"Private"}},"fields":[{{"label":"API_KEY","value":"concealed"}}]}}"#
    )
}

fn install_tool(bin: &Path, name: &str) {
    let destination = tool_path(bin, name);
    fs::copy(env!("CARGO_BIN_EXE_opz-test-tool"), &destination).unwrap();
    #[cfg(windows)]
    fs::copy(env!("CARGO_BIN_EXE_opz-test-tool"), bin.join(name)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&destination).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&destination, permissions).unwrap();
    }
}

fn tool_path(bin: &Path, name: &str) -> PathBuf {
    let mut file = OsString::from(name);
    file.push(std::env::consts::EXE_SUFFIX);
    bin.join(file)
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn item_run_and_top_level_shorthand_use_env_without_secret_argv() {
    let batch_args = [
        "run",
        "--no-masking",
        "--env-file",
        "*",
        "--",
        "sh",
        "-c",
        "env -0",
    ];
    let mut child = step("opz-child", &["hello", "$API_KEY", "${API_KEY}"], "");
    child.capture_env = vec!["API_KEY".to_string()];
    let harness = Harness::new(
        vec![
            step(
                "op",
                &["item", "get", "example", "--format", "json"],
                &item_json("example"),
            ),
            step("op", &batch_args, "API_KEY=canary-secret\0"),
            child.clone(),
            step(
                "op",
                &["item", "get", "example", "--format", "json"],
                &item_json("example"),
            ),
            step("op", &batch_args, "API_KEY=canary-secret\0"),
            child,
        ],
        &["op", "opz-child"],
    );

    let run = harness.output(&[
        "run",
        "example",
        "--",
        "opz-child",
        "hello",
        "$API_KEY",
        "${API_KEY}",
    ]);
    assert_success(&run);
    let shorthand = harness.output(&[
        "example",
        "--",
        "opz-child",
        "hello",
        "$API_KEY",
        "${API_KEY}",
    ]);
    assert_success(&shorthand);

    for index in [2, 5] {
        let invocation = harness.invocation(index);
        assert_eq!(invocation.args, ["hello", "$API_KEY", "${API_KEY}"]);
        assert_eq!(
            invocation.env.get("API_KEY").map(String::as_str),
            Some("canary-secret")
        );
        assert!(!format!("{:?}", invocation.args).contains("canary-secret"));
    }
}

#[test]
fn environment_delegation_preserves_argv_and_reports_failure() {
    let harness = Harness::new(
        vec![
            step("op", &["run", "--help"], "Usage: op run --environments"),
            Step {
                exit_code: 23,
                stderr: "delegated failure".to_string(),
                ..step(
                    "op",
                    &["run", "--environments", "dev", "--", "opz-child", "hello"],
                    "",
                )
            },
        ],
        &["op"],
    );
    let output = harness.output(&["run", "--environment", "dev", "--", "opz-child", "hello"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("status: exit status: 23") || stderr.contains("status: exit code: 23"),
        "{stderr}"
    );
    assert_eq!(
        harness.invocation(1).args,
        ["run", "--environments", "dev", "--", "opz-child", "hello"]
    );
}

#[test]
fn gen_merges_existing_file_without_resolving_values() {
    let harness = Harness::new(
        vec![step(
            "op",
            &["item", "get", "example", "--format", "json"],
            &item_json("example"),
        )],
        &["op"],
    );
    let env_file = harness.root.join(".env.generated");
    fs::write(&env_file, "# keep\nAPI_KEY=old\nOTHER=value\n").unwrap();
    let output = harness
        .command()
        .args(["gen", "--env-file"])
        .arg(&env_file)
        .arg("example")
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(
        fs::read_to_string(env_file).unwrap(),
        "# keep\nAPI_KEY=op://vault-id/item-id/API_KEY\nOTHER=value\n"
    );
}

#[test]
fn repository_auto_detection_uses_fake_git_and_op() {
    let item = r#"{"id":"item-id","title":"owner/repo","vault":{"id":"vault-id","name":"Private"},"fields":[{"label":"github_repositories","value":"owner/repo"},{"label":"API_KEY","value":"concealed"}]}"#
        .to_string();
    let batch_args = [
        "run",
        "--no-masking",
        "--env-file",
        "*",
        "--",
        "sh",
        "-c",
        "env -0",
    ];
    let mut child = step("opz-child", &[], "");
    child.capture_env = vec!["API_KEY".to_string()];
    let harness = Harness::new(
        vec![
            step(
                "git",
                &["config", "--get-regexp", r"^remote\..*\.url$"],
                "remote.origin.url https://github.com/owner/repo.git\n",
            ),
            step(
                "op",
                &["item", "get", "owner/repo", "--format", "json"],
                &item,
            ),
            step(
                "op",
                &["item", "get", "owner/repo", "--format", "json"],
                &item,
            ),
            step("op", &batch_args, "API_KEY=auto-secret\0"),
            child,
        ],
        &["git", "op", "opz-child"],
    );
    let output = harness.output(&["run", "--", "opz-child"]);
    assert_success(&output);
    assert_eq!(
        harness.invocation(4).env.get("API_KEY").map(String::as_str),
        Some("auto-secret")
    );
}

#[test]
fn item_cache_serialization_drops_secret_bearing_fields() {
    const CANARY: &str = "OPZ_CANARY_CACHE_51f7";
    let harness = Harness::new(
        vec![
            Step {
                exit_code: 1,
                stderr: "serv isn't an item".to_string(),
                ..step("op", &["item", "get", "serv", "--format", "json"], "")
            },
            step(
                "op",
                &["item", "list", "--format", "json"],
                &format!(
                    r#"[{{"id":"item-id","title":"service","vault":{{"id":"vault-id","name":"Private"}},"value":"{CANARY}","fields":[{{"value":"{CANARY}"}}]}}]"#
                ),
            ),
            step(
                "op",
                &["item", "get", "item-id", "--format", "json"],
                &item_json("service"),
            ),
        ],
        &["op"],
    );

    let output = harness.output(&["gen", "serv"]);
    assert_success(&output);
    let cache_dir = harness.root.join("cache").join("opz");
    let mut cache_count = 0;
    for entry in fs::read_dir(cache_dir).unwrap() {
        cache_count += 1;
        let content = fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(
            !content.contains(CANARY),
            "cache contained canary: {content}"
        );
        assert!(
            !content.contains("\"fields\""),
            "cache contained fields: {content}"
        );
        assert!(
            !content.contains("\"value\""),
            "cache contained value: {content}"
        );
    }
    assert!(cache_count > 0, "expected a metadata cache file");
}

#[test]
fn github_and_cloudflare_exporters_use_stdin_and_dry_run_skips_resolution() {
    const DRY_RUN_CANARY: &str = "OPZ_CANARY_DRY_RUN_7249";
    let mut gh = step(
        "gh",
        &["secret", "set", "API_KEY", "--repo", "owner/repo"],
        "tool echoed canary-secret\n",
    );
    gh.read_stdin = true;
    gh.stderr = "tool error included canary-secret\n".to_string();
    let mut wrangler = step(
        "wrangler",
        &["secret", "bulk"],
        "bulk echoed canary-secret\n",
    );
    wrangler.read_stdin = true;
    wrangler.stderr = "bulk error included canary-secret\n".to_string();
    let batch_args = [
        "run",
        "--no-masking",
        "--env-file",
        "*",
        "--",
        "sh",
        "-c",
        "env -0",
    ];
    let item = r#"{"id":"item-id","title":"example","vault":{"id":"vault-id","name":"Private"},"fields":[{"label":"github_repositories","value":"owner/repo"},{"label":"API_KEY","value":"concealed"}]}"#
        .to_string();
    let harness = Harness::new(
        vec![
            step("op", &["item", "get", "example", "--format", "json"], &item),
            step("op", &batch_args, "API_KEY=canary-secret\0"),
            gh,
            step("op", &["item", "get", "example", "--format", "json"], &item),
            step("op", &batch_args, "API_KEY=canary-secret\0"),
            wrangler,
            step(
                "op",
                &["item", "get", "example", "--format", "json"],
                &item.replace(
                    r#""value":"concealed""#,
                    &format!(r#""value":"{DRY_RUN_CANARY}""#),
                ),
            ),
        ],
        &["op", "gh", "wrangler"],
    );

    let github = harness.output(&["github-secret", "--repo", "owner/repo", "example"]);
    assert_success(&github);
    assert!(!String::from_utf8_lossy(&github.stdout).contains("canary-secret"));
    assert!(!String::from_utf8_lossy(&github.stderr).contains("canary-secret"));
    assert!(String::from_utf8_lossy(&github.stdout).contains("[REDACTED]"));
    assert_eq!(harness.invocation(2).stdin, "canary-secret");
    assert!(!format!("{:?}", harness.invocation(2).args).contains("canary-secret"));

    let cloudflare = harness.output(&["cloudflare-secret", "example"]);
    assert_success(&cloudflare);
    assert!(!String::from_utf8_lossy(&cloudflare.stdout).contains("canary-secret"));
    assert!(!String::from_utf8_lossy(&cloudflare.stderr).contains("canary-secret"));
    assert!(String::from_utf8_lossy(&cloudflare.stdout).contains("[REDACTED]"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&harness.invocation(5).stdin).unwrap(),
        serde_json::json!({"API_KEY": "canary-secret"})
    );

    let dry_run = harness.output(&[
        "github-secret",
        "--repo",
        "owner/repo",
        "--dry-run",
        "example",
    ]);
    assert_success(&dry_run);
    assert!(String::from_utf8_lossy(&dry_run.stdout).contains("Would set GitHub secret API_KEY"));
    assert_canary_absent(&dry_run, DRY_RUN_CANARY, "dry run");
}

#[test]
fn doctor_uses_fake_tools_and_keeps_stdout_stderr_separate() {
    let harness = Harness::new(
        vec![
            step("op", &["--version"], "2.30.0\n"),
            step(
                "op",
                &["whoami", "--format", "json"],
                r#"{"email":"user@example.test","account_uuid":"A1"}"#,
            ),
            step(
                "op",
                &["account", "list", "--format", "json"],
                r#"[{"url":"example.1password.com"}]"#,
            ),
            step("op", &["run", "--help"], "Usage: op run --environments"),
        ],
        &["op"],
    );
    let mut command = harness.command();
    command.env("PATH", &harness.bin);
    let output = command.arg("doctor").output().unwrap();
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ok    op auth: user@example.test (A1)"));
    assert!(stdout.contains("warn  gh: not found in PATH"));
    assert!(output.stderr.is_empty());
}

#[test]
fn doctor_reports_plaintext_files_and_secretlint_findings() {
    let mut secretlint = step(
        "secretlint",
        &["--format", "json", "--no-color", ".env"],
        r#"[{"filePath":".env","messages":[{"message":"found secret"}]}]"#,
    );
    secretlint.exit_code = 1;
    let harness = Harness::new(
        vec![
            step("op", &["--version"], "2.30.0\n"),
            step(
                "op",
                &["whoami", "--format", "json"],
                r#"{"email":"user@example.test","account_uuid":"A1"}"#,
            ),
            step(
                "op",
                &["account", "list", "--format", "json"],
                r#"[{"url":"example.1password.com"}]"#,
            ),
            step("op", &["run", "--help"], "Usage: op run --environments"),
            step("secretlint", &["--version"], "secretlint/10.0.0\n"),
            secretlint,
        ],
        &["op", "secretlint"],
    );
    fs::write(harness.root.join(".env"), "API_KEY=plaintext\n").unwrap();
    let mut command = harness.command();
    command.env("PATH", &harness.bin);
    let output = command.arg("doctor").output().unwrap();
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("found plaintext credential env file(s): .env"),
        "{stdout}"
    );
    assert!(
        stdout.contains("secretlint reported possible plaintext secrets"),
        "{stdout}"
    );
}

#[test]
fn doctor_required_auth_failure_exits_one_with_diagnostic_on_stdout() {
    let harness = Harness::new(
        vec![
            step("op", &["--version"], "2.30.0\n"),
            Step {
                exit_code: 1,
                stderr: "not signed in".to_string(),
                ..step("op", &["whoami", "--format", "json"], "")
            },
            step("op", &["account", "list", "--format", "json"], "[]"),
            step("op", &["run", "--help"], "Usage: op run"),
        ],
        &["op"],
    );
    let mut command = harness.command();
    command.env("PATH", &harness.bin);
    let output = command.arg("doctor").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("error op auth: `op whoami --format json` failed: not signed in"),
        "{stdout}"
    );
}

#[test]
fn mcp_environment_list_uses_json_rpc_without_secret_values() {
    let mcp = Step {
        tool: "onepassword-mcp".to_string(),
        mcp_results: vec![
            serde_json::json!({"structuredContent": {"accountId": "A1"}}),
            serde_json::json!({"structuredContent": {"environments": [
                {"id": "E2", "name": "staging"},
                {"id": "E1", "name": "dev"}
            ]}}),
        ],
        ..Step::default()
    };
    let harness = Harness::new(vec![mcp], &["onepassword-mcp"]);
    let output = harness.output(&["environment", "list"]);
    assert_success(&output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "E1\tdev\nE2\tstaging\n"
    );
    let requests = harness.mcp_requests(0);
    assert!(requests.iter().any(|request| {
        request.pointer("/params/name").and_then(|v| v.as_str()) == Some("authenticate")
    }));
    assert!(requests.iter().any(|request| {
        request.pointer("/params/name").and_then(|v| v.as_str()) == Some("list_environments")
    }));
    assert!(!format!("{requests:?}").contains("secret-value"));
}

#[test]
fn secret_bearing_tool_failures_do_not_echo_canaries() {
    const CANARY: &str = "OPZ_CANARY_FAILURE_8d90f43c";

    let op_harness = Harness::new(
        vec![Step {
            exit_code: 9,
            stderr: format!("op leaked {CANARY}"),
            ..step("op", &["item", "get", "example", "--format", "json"], "")
        }],
        &["op"],
    );
    assert_canary_absent(
        &op_harness.output(&["gen", "example"]),
        CANARY,
        "op failure",
    );

    let item = item_json("example");
    let batch_args = [
        "run",
        "--no-masking",
        "--env-file",
        "*",
        "--",
        "sh",
        "-c",
        "env -0",
    ];
    let mut gh = step(
        "gh",
        &["secret", "set", "API_KEY", "--repo", "owner/repo"],
        &format!("gh stdout {CANARY}"),
    );
    gh.stderr = format!("gh stderr {CANARY}");
    gh.exit_code = 17;
    gh.read_stdin = true;
    let github_harness = Harness::new(
        vec![
            step("op", &["item", "get", "example", "--format", "json"], &item),
            step("op", &batch_args, &format!("API_KEY={CANARY}\0")),
            gh,
        ],
        &["op", "gh"],
    );
    assert_canary_absent(
        &github_harness.output(&["github-secret", "--repo", "owner/repo", "example"]),
        CANARY,
        "gh failure",
    );

    let mut wrangler = step(
        "wrangler",
        &["secret", "bulk"],
        &format!("wrangler stdout {CANARY}"),
    );
    wrangler.stderr = format!("wrangler stderr {CANARY}");
    wrangler.exit_code = 18;
    wrangler.read_stdin = true;
    let wrangler_harness = Harness::new(
        vec![
            step("op", &["item", "get", "example", "--format", "json"], &item),
            step("op", &batch_args, &format!("API_KEY={CANARY}\0")),
            wrangler,
        ],
        &["op", "wrangler"],
    );
    assert_canary_absent(
        &wrangler_harness.output(&["cloudflare-secret", "example"]),
        CANARY,
        "wrangler failure",
    );

    let mcp_harness = Harness::new(
        vec![Step {
            tool: "onepassword-mcp".to_string(),
            mcp_results: vec![
                serde_json::json!({"structuredContent": {"accountId": "A1"}}),
                serde_json::json!({"__error": {
                    "code": -32000,
                    "message": format!("MCP leaked {CANARY}"),
                    "data": {"secret": CANARY}
                }}),
            ],
            ..Step::default()
        }],
        &["onepassword-mcp"],
    );
    assert_canary_absent(
        &mcp_harness.output(&["environment", "list"]),
        CANARY,
        "MCP failure",
    );
}

fn assert_canary_absent(output: &Output, canary: &str, label: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stdout.contains(canary), "{label} stdout: {stdout}");
    assert!(!stderr.contains(canary), "{label} stderr: {stderr}");
}

#[test]
fn op_timeout_is_deterministic() {
    let harness = Harness::new(
        vec![Step {
            delay_ms: 1_500,
            ..step(
                "op",
                &["item", "get", "example", "--format", "json"],
                &item_json("example"),
            )
        }],
        &["op"],
    );
    let output = harness
        .command()
        .env("OPZ_OP_TIMEOUT_SECONDS", "1")
        .args(["gen", "example"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("timed out after 1 seconds"));
}
