mod instrumentation;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use instrumentation::KeyValue;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::OsString,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Output, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant, SystemTime},
};

#[derive(Parser, Debug)]
#[command(author, version, about)]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    /// Vault name (optional). If omitted, search all items and pick best match.
    #[arg(long, global = true)]
    vault: Option<String>,

    /// Output env file path (optional, no file generated if omitted)
    #[arg(long, value_name = "ENV")]
    env_file: Option<PathBuf>,

    /// 1Password Environment name or ID for native op run injection
    #[arg(long, alias = "environments", value_name = "ENV", global = true)]
    environment: Vec<String>,

    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Item titles (when not using subcommand)
    #[arg(value_name = "ITEM")]
    items: Vec<String>,

    /// Command to run (after --)
    #[arg(last = true)]
    command: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Find items by keyword (title contains)
    Find { query: String },

    /// Check 1Password CLI status and external command dependencies
    Doctor,

    /// Manage 1Password Developer Environments through the 1Password MCP server
    #[command(alias = "env")]
    Environment {
        /// 1Password account ID. If omitted, authenticate with the 1Password app.
        #[arg(long, value_name = "ACCOUNT_ID")]
        account: Option<String>,

        #[command(subcommand)]
        command: EnvironmentCommand,
    },

    /// Print bundled Agent Skills SKILL.md for opz
    Skills,

    /// Show valid env labels from 1Password items
    Show {
        /// Show item title header for each section
        #[arg(long)]
        with_item: bool,

        /// Item titles
        #[arg(value_name = "ITEM", num_args = 1..)]
        items: Vec<String>,
    },

    /// Generate env file only (do not run command). Appends to existing file, overwrites duplicate keys.
    Gen {
        /// Output env file path (optional, no file generated if omitted)
        #[arg(long, value_name = "ENV")]
        env_file: Option<PathBuf>,

        /// Item titles
        #[arg(value_name = "ITEM", num_args = 1..)]
        items: Vec<String>,
    },

    /// Migrate scripts to repository item titles and metadata
    Migrate {
        /// Print changes without editing 1Password items or files
        #[arg(long)]
        dry_run: bool,

        /// Create a new item from .env before rewriting .env-based scripts
        #[arg(long)]
        new: bool,

        /// Restore explicit item arguments for scripts that rely on auto-detection
        #[arg(long)]
        restore: bool,
    },

    /// Store a private config file as Secure Note(s) named from git remotes
    Note {
        /// Source file path
        #[arg(value_name = "FILE")]
        file: PathBuf,
    },

    #[command(hide = true)]
    Create {
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },

    /// Add or update GitHub repository metadata on existing 1Password items
    GithubRepo {
        /// Repository in OWNER/REPO form. Repeat for multiple repositories. Defaults to current git remotes.
        #[arg(long, value_name = "OWNER/REPO")]
        repo: Vec<String>,

        /// Print updates without editing 1Password items
        #[arg(long)]
        dry_run: bool,

        /// Item titles
        #[arg(value_name = "ITEM", num_args = 0..)]
        items: Vec<String>,
    },

    /// Run command with secrets from 1Password item(s), auto-detecting by git remote when omitted
    Run {
        /// Output env file path (optional, no file generated if omitted)
        #[arg(long, value_name = "ENV")]
        env_file: Option<PathBuf>,

        /// Item titles
        #[arg(value_name = "ITEM", num_args = 1..)]
        items: Vec<String>,

        /// Command to run (after --)
        #[arg(last = true)]
        command: Vec<String>,
    },

    /// Store valid fields from 1Password items as GitHub repository secrets
    GithubSecret {
        /// Repository in OWNER/REPO form (defaults to current gh repository)
        #[arg(long, value_name = "OWNER/REPO")]
        repo: Option<String>,

        /// Print target secret names without storing values
        #[arg(long)]
        dry_run: bool,

        /// Item titles
        #[arg(value_name = "ITEM", num_args = 1..)]
        items: Vec<String>,
    },

    /// Store valid fields from 1Password items as Cloudflare Worker secrets
    CloudflareSecret {
        /// Worker name passed to wrangler --name
        #[arg(long, value_name = "WORKER")]
        name: Option<String>,

        /// Wrangler environment passed to wrangler --env
        #[arg(long, value_name = "ENV")]
        env: Option<String>,

        /// Wrangler config path passed to wrangler --config
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,

        /// Print target secret names without storing values
        #[arg(long)]
        dry_run: bool,

        /// Item titles
        #[arg(value_name = "ITEM", num_args = 1..)]
        items: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
enum EnvironmentCommand {
    /// List 1Password Environments
    List,

    /// Create a new 1Password Environment
    Create {
        /// Environment name
        name: String,
    },

    /// Rename a 1Password Environment
    Rename {
        /// Environment ID or exact name
        environment: String,

        /// New Environment name
        new_name: String,
    },

    /// List variable names in a 1Password Environment
    Variables {
        /// Environment ID or exact name
        environment: String,
    },

    /// Create a locally mounted .env file for a 1Password Environment
    Mount {
        /// Environment ID or exact name
        environment: String,

        /// Local .env file mount path
        path: PathBuf,
    },

    /// List local .env file mounts for a 1Password Environment
    Mounts {
        /// Environment ID or exact name
        environment: String,
    },
}

#[derive(Deserialize, Serialize, Debug)]
struct ItemListEntry {
    id: String,
    title: String,
    #[serde(default)]
    vault: Option<ItemVault>,
}
#[derive(Deserialize, Serialize, Debug)]
struct ItemVault {
    id: String,
    name: String,
}

#[derive(Deserialize, Debug)]
struct ItemGet {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    fields: Vec<ItemField>,
    #[serde(default)]
    vault: Option<ItemVault>,
}
#[derive(Deserialize, Debug)]
struct ItemField {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    value: Option<serde_json::Value>,
}

#[derive(Serialize, Debug)]
struct ItemCreateTemplate {
    title: String,
    category: String,
    fields: Vec<ItemCreateField>,
}

#[derive(Serialize, Debug)]
struct ItemCreateField {
    id: String,
    #[serde(rename = "type")]
    field_type: String,
    label: String,
    value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    purpose: Option<String>,
}

static OPZ_SKILL: &str = include_str!("../.agents/skills/opz/SKILL.md");
const GITHUB_REPOSITORIES_LABEL: &str = "github_repositories";

#[derive(Debug)]
struct DoctorFailure;

impl std::fmt::Display for DoctorFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("doctor found required failures")
    }
}

impl std::error::Error for DoctorFailure {}

fn main() -> Result<()> {
    run_main()
}

fn run_main() -> Result<()> {
    let args: Vec<OsString> = std::env::args_os().collect();
    let command_hint = detect_command_hint(&args).to_string();

    let result = instrumentation::with_span(
        &format!("cli.{command_hint}"),
        vec![KeyValue::new("cli.command", command_hint)],
        || {
            let result = run_cli(&args);
            if let Err(err) = &result {
                if !is_clap_display_error(err) {
                    instrumentation::record_error_message(&err.to_string());
                }
            }
            result
        },
    );

    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            if let Some(clap_err) = err.downcast_ref::<clap::Error>() {
                let _ = clap_err.print();
                std::process::exit(clap_err.exit_code());
            }
            if err.downcast_ref::<DoctorFailure>().is_some() {
                std::process::exit(1);
            }
            Err(err)
        }
    }
}

fn run_cli(args: &[OsString]) -> Result<()> {
    let cli = instrumentation::with_span("parse_args", vec![], || {
        let parse_result = Cli::try_parse_from(args);
        if let Err(err) = &parse_result {
            if err.exit_code() != 0 {
                instrumentation::record_error_message(&err.to_string());
            }
        }
        parse_result
    })?;
    instrumentation::with_span("load_config", vec![], || {
        let _ = std::env::current_dir();
    });
    if !cli.environment.is_empty() && !matches!(cli.cmd, Some(Cmd::Run { .. }) | None) {
        return Err(anyhow!(
            "`--environment` is only supported with `opz run` or top-level command execution."
        ));
    }

    match &cli.cmd {
        Some(Cmd::Find { query }) => {
            let items = instrumentation::with_span_result("load_inputs", vec![], || {
                item_list_cached(cli.vault.as_deref())
            })?;
            let q = query.to_lowercase();
            let rows = instrumentation::with_span("main_operation", vec![], || {
                items
                    .into_iter()
                    .filter(|x| x.title.to_lowercase().contains(&q))
                    .map(|it| {
                        let vault = it.vault.as_ref().map(|v| v.name.as_str()).unwrap_or("-");
                        format!("{}\t{}\t{}", it.id, vault, it.title)
                    })
                    .collect::<Vec<_>>()
            });

            instrumentation::with_span("write_outputs", vec![], || {
                for row in &rows {
                    println!("{row}");
                }
            });
            Ok(())
        }
        Some(Cmd::Doctor) => run_doctor(),
        Some(Cmd::Environment { account, command }) => {
            run_environment_cli(account.as_deref(), command)
        }
        Some(Cmd::Skills) => print_bundled_skill(),
        Some(Cmd::Show { with_item, items }) => show_item_labels(&cli, items, *with_item),
        Some(Cmd::Gen { items, env_file }) => {
            print_credential_file_advice_for_secret_command("gen");
            generate_env_output(&cli, items, env_file.as_deref())
        }
        Some(Cmd::Migrate {
            dry_run,
            new,
            restore,
        }) => migrate_scripts(&cli, *dry_run, *new, *restore),
        Some(Cmd::Note { file }) => create_secure_notes_from_file(&cli, file),
        Some(Cmd::Create { .. }) => Err(anyhow!(
            "`opz create` was removed. Use `opz migrate --new` to create an item from .env, or `opz note <FILE>` to store a private config file."
        )),
        Some(Cmd::GithubRepo {
            repo,
            dry_run,
            items,
        }) => update_github_repositories_metadata(&cli, repo, *dry_run, items),
        Some(Cmd::Run {
            items,
            env_file,
            command,
        }) => {
            if command.is_empty() {
                return Err(anyhow!(
                    "Command required after '--'. Usage: opz run [OPTIONS] [--env-file <ENV>] [--environment <ENV>] [<ITEM>...] -- <COMMAND>..."
                ));
            }
            if !cli.environment.is_empty() {
                return run_with_environments(
                    cli.vault.as_deref(),
                    &cli.environment,
                    items,
                    env_file.as_deref(),
                    command,
                );
            }
            print_credential_file_advice_for_secret_command("run");
            let resolved_items = resolve_run_items(&cli, items)?;
            run_with_items(&cli, &resolved_items, env_file.as_deref(), command)
        }
        Some(Cmd::GithubSecret {
            repo,
            dry_run,
            items,
        }) => {
            print_credential_file_advice_for_secret_command("github-secret");
            set_github_secrets(&cli, repo.as_deref(), *dry_run, items)
        }
        Some(Cmd::CloudflareSecret {
            name,
            env,
            config,
            dry_run,
            items,
        }) => {
            print_credential_file_advice_for_secret_command("cloudflare-secret");
            set_cloudflare_secrets(
                &cli,
                CloudflareSecretTarget {
                    name: name.as_deref(),
                    env: env.as_deref(),
                    config: config.as_deref(),
                },
                *dry_run,
                items,
            )
        }
        None => {
            if cli.command.is_empty() {
                return Err(anyhow!(
                    "Command required after '--'. Usage: opz [OPTIONS] [--env-file <ENV>] [--environment <ENV>] [<ITEM>...] -- <COMMAND>..."
                ));
            }
            if !cli.environment.is_empty() {
                return run_with_environments(
                    cli.vault.as_deref(),
                    &cli.environment,
                    &cli.items,
                    cli.env_file.as_deref(),
                    &cli.command,
                );
            }
            print_credential_file_advice_for_secret_command("run");
            let resolved_items = resolve_run_items(&cli, &cli.items)?;
            run_with_items(&cli, &resolved_items, cli.env_file.as_deref(), &cli.command)
        }
    }
}

fn is_clap_display_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<clap::Error>()
        .is_some_and(|clap_err| clap_err.exit_code() == 0)
}

fn detect_command_hint(args: &[OsString]) -> &'static str {
    let mut idx = 1;
    while idx < args.len() {
        let arg = args[idx].to_string_lossy();

        if arg == "--" {
            return "run";
        }
        if arg == "--help" || arg == "-h" {
            return "help";
        }
        if arg == "--version" || arg == "-V" {
            return "version";
        }

        if arg == "--vault" || arg == "--env-file" {
            idx += 2;
            continue;
        }
        if arg == "--environment" || arg == "--environments" {
            idx += 2;
            continue;
        }
        if arg.starts_with("--vault=")
            || arg.starts_with("--env-file=")
            || arg.starts_with("--environment=")
            || arg.starts_with("--environments=")
        {
            idx += 1;
            continue;
        }
        if arg.starts_with("--") {
            idx += 1;
            continue;
        }

        return match arg.as_ref() {
            "find" => "find",
            "doctor" => "doctor",
            "environment" | "env" => "environment",
            "skills" => "skills",
            "show" => "show",
            "gen" => "gen",
            "create" => "create",
            "migrate" => "migrate",
            "note" => "note",
            "github-repo" => "github-repo",
            "run" => "run",
            "github-secret" => "github-secret",
            "cloudflare-secret" => "cloudflare-secret",
            _ => "run",
        };
    }

    "run"
}

fn print_bundled_skill() -> Result<()> {
    instrumentation::with_span("main_operation", vec![], || ());
    instrumentation::with_span("write_outputs", vec![], || {
        print!("{OPZ_SKILL}");
    });
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EnvironmentRecord {
    id: String,
    name: String,
}

trait OnePasswordMcp {
    fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> Result<serde_json::Value>;
}

struct OnePasswordMcpStdioClient {
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
                    concise_json(error)
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

fn spawn_mcp_stdout_reader(
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

fn run_environment_cli(account: Option<&str>, command: &EnvironmentCommand) -> Result<()> {
    let mut client = OnePasswordMcpStdioClient::connect()?;
    let output = run_environment_command(&mut client, account, command)?;
    instrumentation::with_span("write_outputs", vec![], || {
        print!("{output}");
    });
    Ok(())
}

fn run_environment_command(
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

fn resolve_mcp_account_id(
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

fn list_mcp_environments(
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

fn create_mcp_environment(
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

fn rename_mcp_environment(
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

fn resolve_mcp_environment(
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

fn list_mcp_variable_names(
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

fn create_mcp_local_env_file(
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

fn list_mcp_local_env_files(
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

fn render_environment_records(environments: &[EnvironmentRecord]) -> String {
    let mut out = String::new();
    for environment in environments {
        out.push_str(&format!("{}\t{}\n", environment.id, environment.name));
    }
    out
}

fn render_lines(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn onepassword_mcp_command() -> String {
    env::var("OPZ_1PASSWORD_MCP_COMMAND")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "onepassword-mcp".to_string())
}

fn mcp_command_timeout() -> Duration {
    env::var("OPZ_MCP_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(30))
}

fn check_onepassword_mcp_server() -> DoctorCheck {
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

fn mcp_result_values(result: &serde_json::Value) -> Vec<serde_json::Value> {
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

fn extract_environment_records(values: &[serde_json::Value]) -> Vec<EnvironmentRecord> {
    let mut records = Vec::new();
    for value in values {
        collect_environment_records(value, &mut records);
    }
    dedupe_environment_records(records)
}

fn collect_environment_records(value: &serde_json::Value, records: &mut Vec<EnvironmentRecord>) {
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

fn dedupe_environment_records(records: Vec<EnvironmentRecord>) -> Vec<EnvironmentRecord> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for record in records {
        if seen.insert((record.id.clone(), record.name.clone())) {
            deduped.push(record);
        }
    }
    deduped
}

fn extract_variable_names(values: &[serde_json::Value]) -> Vec<String> {
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

fn extract_mount_paths(values: &[serde_json::Value]) -> Vec<String> {
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

fn extract_first_string_for_keys(values: &[serde_json::Value], keys: &[&str]) -> Option<String> {
    let mut strings = Vec::new();
    for value in values {
        collect_strings_for_keys(value, keys, &mut strings);
    }
    strings.into_iter().next()
}

fn collect_strings_for_keys(value: &serde_json::Value, keys: &[&str], out: &mut Vec<String>) {
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

fn collect_string_array_for_keys(value: &serde_json::Value, keys: &[&str], out: &mut Vec<String>) {
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

fn concise_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| "<unprintable JSON>".to_string())
        .chars()
        .take(500)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoctorStatus {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DoctorCheck {
    status: DoctorStatus,
    name: String,
    message: String,
    required: bool,
}

impl DoctorCheck {
    fn ok(name: impl Into<String>, message: impl Into<String>, required: bool) -> Self {
        Self {
            status: DoctorStatus::Ok,
            name: name.into(),
            message: message.into(),
            required,
        }
    }

    fn warn(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: DoctorStatus::Warn,
            name: name.into(),
            message: message.into(),
            required: false,
        }
    }

    fn error(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: DoctorStatus::Error,
            name: name.into(),
            message: message.into(),
            required: true,
        }
    }
}

fn run_doctor() -> Result<()> {
    let checks = instrumentation::with_span("main_operation", vec![], collect_doctor_checks);
    let rendered = render_doctor_checks(&checks);
    instrumentation::with_span("write_outputs", vec![], || {
        print!("{rendered}");
    });

    if doctor_has_required_failure(&checks) {
        return Err(anyhow!(DoctorFailure));
    }
    Ok(())
}

fn collect_doctor_checks() -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    match check_required_cli_version("op", &["--version"]) {
        Ok(check) => checks.push(check),
        Err(check) => {
            checks.push(check);
            checks.push(DoctorCheck::error(
                "op auth",
                "skipped because op is not available",
            ));
            checks.push(optional_cli_check(
                "gh",
                &["--version"],
                "needed by github-secret",
            ));
            checks.push(optional_cli_check(
                "wrangler",
                &["--version"],
                "needed by cloudflare-secret",
            ));
            checks.push(optional_cli_check(
                "git",
                &["--version"],
                "needed by migrate and note",
            ));
            checks.push(optional_cli_check("sh", &["--version"], "needed by run"));
            checks.push(optional_cli_check(
                "secretlint",
                &["--version"],
                "needed by doctor plaintext credential scan",
            ));
            checks.push(check_onepassword_mcp_server());
            checks.push(check_credential_files());
            return checks;
        }
    }

    checks.push(check_op_auth());
    checks.push(check_op_accounts());
    checks.push(check_op_run_environments_support());
    checks.push(optional_cli_check(
        "gh",
        &["--version"],
        "needed by github-secret",
    ));
    checks.push(optional_cli_check(
        "wrangler",
        &["--version"],
        "needed by cloudflare-secret",
    ));
    checks.push(optional_cli_check(
        "git",
        &["--version"],
        "needed by migrate and note",
    ));
    checks.push(optional_cli_check("sh", &["--version"], "needed by run"));
    checks.push(optional_cli_check(
        "secretlint",
        &["--version"],
        "needed by doctor plaintext credential scan",
    ));
    checks.push(check_onepassword_mcp_server());
    checks.push(check_credential_files());

    checks
}

fn check_op_run_environments_support() -> DoctorCheck {
    let mut cmd = Command::new("op");
    cmd.arg("run").arg("--help");
    cmd.stdin(Stdio::null());
    match command_output_with_timeout(cmd, "`op run --help`", op_command_timeout()) {
        Ok(out) if out.status.success() => {
            let help = String::from_utf8_lossy(&out.stdout);
            match detect_environment_flag_from_help(&help) {
                Some(flag) => DoctorCheck::ok(
                    "op run environments",
                    format!("supported via {flag}"),
                    false,
                ),
                None => DoctorCheck::warn(
                    "op run environments",
                    "not exposed by this op CLI; item-backed opz run remains available",
                ),
            }
        }
        Ok(out) => DoctorCheck::warn(
            "op run environments",
            format!(
                "could not inspect `op run --help`: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        ),
        Err(err) => DoctorCheck::warn("op run environments", err.to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CredentialFileFinding {
    path: PathBuf,
    plaintext_entries: usize,
}

fn print_credential_file_advice_for_secret_command(command: &str) {
    if env::var_os("OPZ_SKIP_CREDENTIAL_SCAN").is_some() {
        return;
    }
    let Ok(findings) = find_plaintext_credential_files_in_project() else {
        return;
    };
    if findings.is_empty() {
        return;
    }

    eprintln!(
        "Advice: found plaintext credential env file(s) while running `{command}`: {}. Prefer `opz run -- <COMMAND>` without an env file after `opz migrate --new`; use `opz gen --env-file <FILE> <ITEM>` only when a tool requires op:// references.",
        credential_finding_path_list(&findings)
    );
}

fn check_credential_files() -> DoctorCheck {
    let findings = match find_plaintext_credential_files_in_project() {
        Ok(findings) => findings,
        Err(err) => return DoctorCheck::warn("credential files", err.to_string()),
    };

    if findings.is_empty() {
        return DoctorCheck::ok(
            "credential files",
            "no plaintext env credential files found",
            false,
        );
    }

    let advice = credential_file_advice(&findings);
    if find_command_path("secretlint").is_none() {
        return DoctorCheck::warn(
            "credential files",
            format!("{advice}; secretlint not found in PATH"),
        );
    }

    match run_secretlint_on_files(&findings) {
        Ok(SecretlintOutcome::Clean) => DoctorCheck::warn(
            "credential files",
            format!("{advice}; secretlint found no configured rule violations"),
        ),
        Ok(SecretlintOutcome::Findings) => DoctorCheck::warn(
            "credential files",
            format!("{advice}; secretlint reported possible plaintext secrets"),
        ),
        Err(err) => DoctorCheck::warn("credential files", format!("{advice}; {err}")),
    }
}

fn credential_file_advice(findings: &[CredentialFileFinding]) -> String {
    format!(
        "found plaintext credential env file(s): {}; prefer `opz run -- <COMMAND>` without an env file after `opz migrate --new`, and generate files only with `opz gen --env-file <FILE> <ITEM>` when required",
        credential_finding_path_list(findings)
    )
}

fn credential_finding_path_list(findings: &[CredentialFileFinding]) -> String {
    findings
        .iter()
        .take(5)
        .map(|finding| {
            format!(
                "{} ({} entr{})",
                finding.path.display(),
                finding.plaintext_entries,
                if finding.plaintext_entries == 1 {
                    "y"
                } else {
                    "ies"
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecretlintOutcome {
    Clean,
    Findings,
}

fn run_secretlint_on_files(
    findings: &[CredentialFileFinding],
) -> std::result::Result<SecretlintOutcome, String> {
    let mut cmd = Command::new("secretlint");
    cmd.arg("--format").arg("json").arg("--no-color");
    for finding in findings {
        cmd.arg(&finding.path);
    }

    let out = cmd
        .output()
        .map_err(|err| format!("failed to run `secretlint`: {err}"))?;

    match out.status.code() {
        Some(0) => Ok(SecretlintOutcome::Clean),
        Some(1) => Ok(SecretlintOutcome::Findings),
        _ => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if stderr.is_empty() {
                format!("secretlint failed with status {}", out.status)
            } else {
                format!("secretlint failed: {stderr}")
            })
        }
    }
}

fn find_plaintext_credential_files_in_project() -> Result<Vec<CredentialFileFinding>> {
    let root = project_scan_root()?;
    let mut findings = Vec::new();
    collect_plaintext_credential_files(&root, &root, &mut findings)?;
    findings.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(findings)
}

fn project_scan_root() -> Result<PathBuf> {
    if let Ok(out) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
    {
        if out.status.success() {
            let root = String::from_utf8(out.stdout)
                .context("git rev-parse output was not valid UTF-8")?
                .trim()
                .to_string();
            if !root.is_empty() {
                return Ok(PathBuf::from(root));
            }
        }
    }
    env::current_dir().context("failed to read current directory")
}

fn collect_plaintext_credential_files(
    root: &Path,
    dir: &Path,
    findings: &mut Vec<CredentialFileFinding>,
) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if file_type.is_dir() {
            if should_skip_scan_dir(&name) {
                continue;
            }
            collect_plaintext_credential_files(root, &path, findings)?;
            continue;
        }

        if !file_type.is_file() || !is_credential_env_file_name(&name) {
            continue;
        }

        let plaintext_entries = count_plaintext_env_entries(&path)?;
        if plaintext_entries == 0 {
            continue;
        }

        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        findings.push(CredentialFileFinding {
            path: relative,
            plaintext_entries,
        });
    }

    Ok(())
}

fn should_skip_scan_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | ".cache" | "dist" | "build"
    )
}

fn is_credential_env_file_name(name: &str) -> bool {
    if is_example_credential_env_file_name(name) {
        return false;
    }
    name == ".env" || name.starts_with(".env.") || name.ends_with(".env") || name.contains(".env.")
}

fn is_example_credential_env_file_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".example")
        || lower.ends_with(".sample")
        || lower.ends_with(".template")
        || lower.ends_with(".bak")
        || lower.ends_with(".old")
}

fn count_plaintext_env_entries(path: &Path) -> Result<usize> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let label_re = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$")?;
    let mut count = 0;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let normalized = match line.strip_prefix("export") {
            Some(rest) if rest.chars().next().is_some_and(char::is_whitespace) => rest.trim_start(),
            _ => line,
        };
        let Some((raw_key, raw_value)) = normalized.split_once('=') else {
            continue;
        };
        if !label_re.is_match(raw_key.trim()) {
            continue;
        }
        let value = normalize_env_value(raw_value);
        if !value.is_empty() && !is_op_reference(&value) {
            count += 1;
        }
    }

    Ok(count)
}

fn check_required_cli_version(
    command: &str,
    args: &[&str],
) -> std::result::Result<DoctorCheck, DoctorCheck> {
    let Some(path) = find_command_path(command) else {
        return Err(DoctorCheck::error(command, "not found in PATH"));
    };

    match command_stdout(command, args) {
        Ok(stdout) => Ok(DoctorCheck::ok(
            command,
            format!("{} ({})", path.display(), first_output_line(&stdout)),
            true,
        )),
        Err(err) => Err(DoctorCheck::error(command, err)),
    }
}

fn check_op_auth() -> DoctorCheck {
    match command_stdout("op", &["whoami", "--format", "json"]) {
        Ok(stdout) => {
            let summary = summarize_op_whoami(&stdout).unwrap_or_else(|| "signed in".to_string());
            DoctorCheck::ok("op auth", summary, true)
        }
        Err(err) => DoctorCheck::error("op auth", err),
    }
}

fn check_op_accounts() -> DoctorCheck {
    match command_stdout("op", &["account", "list", "--format", "json"]) {
        Ok(stdout) => {
            let count = serde_json::from_str::<serde_json::Value>(&stdout)
                .ok()
                .and_then(|value| value.as_array().map(Vec::len));
            let message = match count {
                Some(1) => "1 configured account".to_string(),
                Some(n) => format!("{n} configured accounts"),
                None => "account list available".to_string(),
            };
            DoctorCheck::ok("op accounts", message, false)
        }
        Err(err) => DoctorCheck::warn("op accounts", err),
    }
}

fn optional_cli_check(command: &str, args: &[&str], note: &str) -> DoctorCheck {
    let Some(path) = find_command_path(command) else {
        return DoctorCheck::warn(command, format!("not found in PATH ({note})"));
    };

    match command_stdout(command, args) {
        Ok(stdout) => DoctorCheck::ok(
            command,
            format!("{} ({})", path.display(), first_output_line(&stdout)),
            false,
        ),
        Err(err) => DoctorCheck::warn(command, format!("{err} ({note})")),
    }
}

fn command_stdout(command: &str, args: &[&str]) -> std::result::Result<String, String> {
    let out = Command::new(command).args(args).output().map_err(|err| {
        format!(
            "failed to run `{}`: {err}",
            command_with_args(command, args)
        )
    })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("exit status {}", out.status)
        } else {
            stderr
        };
        return Err(format!(
            "`{}` failed: {detail}",
            command_with_args(command, args)
        ));
    }

    String::from_utf8(out.stdout).map_err(|err| {
        format!(
            "`{}` output was not valid UTF-8: {err}",
            command_with_args(command, args)
        )
    })
}

fn command_with_args(command: &str, args: &[&str]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(command.to_string());
    parts.extend(args.iter().map(|arg| arg.to_string()));
    parts.join(" ")
}

fn first_output_line(output: &str) -> String {
    output
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
        .unwrap_or_else(|| "no version output".to_string())
}

fn summarize_op_whoami(stdout: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let email = value
        .get("email")
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.get("user_email").and_then(serde_json::Value::as_str));
    let account = value
        .get("account_uuid")
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.get("account").and_then(serde_json::Value::as_str))
        .or_else(|| value.get("url").and_then(serde_json::Value::as_str));

    match (email, account) {
        (Some(email), Some(account)) => Some(format!("{email} ({account})")),
        (Some(email), None) => Some(email.to_string()),
        (None, Some(account)) => Some(account.to_string()),
        (None, None) => Some("signed in".to_string()),
    }
}

fn find_command_path(command: &str) -> Option<PathBuf> {
    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        return is_executable_file(command_path).then(|| command_path.to_path_buf());
    }

    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(command);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

fn render_doctor_checks(checks: &[DoctorCheck]) -> String {
    let mut out = String::new();
    for check in checks {
        let status = match check.status {
            DoctorStatus::Ok => "ok",
            DoctorStatus::Warn => "warn",
            DoctorStatus::Error => "error",
        };
        out.push_str(&format!("{status:<5} {}: {}\n", check.name, check.message));
    }
    out
}

fn doctor_has_required_failure(checks: &[DoctorCheck]) -> bool {
    checks
        .iter()
        .any(|check| check.required && check.status == DoctorStatus::Error)
}

/// Sections of `(item title, env lines)` collected per requested item.
type ItemSections = Vec<(String, Vec<String>)>;

fn collect_item_env_sections(cli: &Cli, items: &[String]) -> Result<ItemSections> {
    let mut sections = Vec::with_capacity(items.len());

    for item_title in items {
        let (item_id, vault_id, resolved_title, item) =
            find_item(cli.vault.as_deref(), item_title)?;
        let env_lines = item_to_env_lines(&item, &vault_id, &item_id)?;
        sections.push((resolved_title, env_lines));
    }

    Ok(sections)
}

fn collect_item_env_sections_with_github_repos(
    cli: &Cli,
    items: &[String],
) -> Result<(ItemSections, Vec<ItemGithubRepositories>)> {
    let mut sections = Vec::with_capacity(items.len());
    let mut repositories = Vec::with_capacity(items.len());

    for item_title in items {
        let (item_id, vault_id, resolved_title, item) =
            find_item(cli.vault.as_deref(), item_title)?;
        let env_lines = item_to_env_lines(&item, &vault_id, &item_id)?;
        let github_repositories = item_github_repositories(&item);
        sections.push((resolved_title.clone(), env_lines));
        repositories.push(ItemGithubRepositories {
            item_title: resolved_title,
            repositories: github_repositories,
        });
    }

    Ok((sections, repositories))
}

fn collect_item_label_sections(cli: &Cli, items: &[String]) -> Result<ItemSections> {
    let mut sections = Vec::with_capacity(items.len());

    for item_title in items {
        let (_, _, resolved_title, item) = find_item(cli.vault.as_deref(), item_title)?;
        let labels = item_to_valid_labels(&item)?;
        sections.push((resolved_title, labels));
    }

    Ok(sections)
}

fn resolve_run_items(cli: &Cli, items: &[String]) -> Result<Vec<String>> {
    if !items.is_empty() {
        return Ok(items.to_vec());
    }

    let repositories = list_remote_repo_names()
        .context("No item specified and failed to auto-detect a repository from git remotes")?;
    let matches =
        match_item_titles_by_github_repository_titles(cli.vault.as_deref(), &repositories)?;
    match matches.as_slice() {
        [title] => Ok(vec![title.clone()]),
        [] => Err(anyhow!(
            "No 1Password item matched git remote repository title: {}. Run `opz migrate`, `opz migrate --new`, or pass an item title explicitly.",
            repositories.join(", ")
        )),
        _ => Err(anyhow!(
            "Multiple 1Password items matched git remote repository title ({}): {}. Pass an item title explicitly.",
            repositories.join(", "),
            matches.join(", ")
        )),
    }
}

fn match_item_titles_by_github_repository_titles(
    vault: Option<&str>,
    repositories: &[String],
) -> Result<Vec<String>> {
    let mut matches = Vec::new();
    for repo in repositories {
        let Some(repo) = normalize_github_repo_spec(repo) else {
            continue;
        };
        match find_item_exact(vault, &repo) {
            Ok((_, _, title, _)) => matches.push(title),
            Err(err) if is_op_lookup_miss(&err) => {}
            Err(err) => return Err(err),
        }
    }
    if matches.is_empty() && legacy_autodetect_scan_enabled() {
        let candidates = item_github_repository_index_cached(vault)?;
        matches = match_item_titles_by_github_repositories(&candidates, repositories);
    }
    Ok(dedupe_preserve_order(matches))
}

fn is_op_lookup_miss(err: &anyhow::Error) -> bool {
    let text = err.to_string();
    text.contains("isn't an item")
        || text.contains("is not an item")
        || text.contains("No item matched")
        || text.contains("No exact item matched")
        || text.contains("not found")
}

fn legacy_autodetect_scan_enabled() -> bool {
    env::var("OPZ_AUTODETECT_LEGACY_SCAN")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn match_item_titles_by_github_repositories(
    candidates: &[(String, Vec<String>)],
    repositories: &[String],
) -> Vec<String> {
    let wanted: HashSet<String> = repositories
        .iter()
        .filter_map(|repo| normalize_github_repo_spec(repo))
        .collect();
    candidates
        .iter()
        .filter(|(_, item_repos)| {
            item_repos
                .iter()
                .filter_map(|repo| normalize_github_repo_spec(repo))
                .any(|repo| wanted.contains(&repo))
        })
        .map(|(title, _)| title.clone())
        .collect()
}

fn merge_env_lines(sections: &[(String, Vec<String>)]) -> Vec<String> {
    let mut merged_lines: Vec<String> = Vec::new();
    let mut key_positions: HashMap<String, usize> = HashMap::new();

    for (_, lines) in sections {
        for line in lines {
            if let Some(key) = parse_env_key(line) {
                if let Some(&idx) = key_positions.get(key) {
                    merged_lines[idx] = line.clone();
                } else {
                    key_positions.insert(key.to_string(), merged_lines.len());
                    merged_lines.push(line.clone());
                }
            }
        }
    }

    merged_lines
}

fn resolve_env_vars(env_lines: &[String]) -> Result<HashMap<String, String>> {
    let references: Vec<(String, String)> = env_lines
        .iter()
        .filter_map(|line| {
            parse_env_line_kv(line).map(|(key, reference)| (key.to_string(), reference.to_string()))
        })
        .collect();
    if references.is_empty() {
        return Ok(HashMap::new());
    }

    match resolve_env_vars_batch(&references) {
        Ok(env_vars) => return Ok(env_vars),
        Err(err) if !should_fallback_to_op_read(&err) => {
            return Err(err.context(
                "batch secret resolution timed out; skipped per-field fallback to avoid waiting once per secret",
            ));
        }
        Err(_) => {}
    }

    // Fallback path for environments where batch resolution is unavailable.
    let mut env_vars: HashMap<String, String> = HashMap::with_capacity(references.len());
    for line in env_lines {
        if let Some((key, reference)) = parse_env_line_kv(line) {
            let value = op_read(reference)?;
            env_vars.insert(key.to_string(), value);
        }
    }

    Ok(env_vars)
}

fn resolve_env_vars_batch(references: &[(String, String)]) -> Result<HashMap<String, String>> {
    instrumentation::with_span_result(
        "load_inputs.op_run_batch_resolve",
        vec![KeyValue::new(
            "env.reference_count",
            references.len() as i64,
        )],
        || {
            let mut temp_env = TempEnvFile::create().context("create temp env file")?;
            for (key, reference) in references {
                writeln!(temp_env, "{key}={reference}")?;
            }
            temp_env.flush()?;

            let mut cmd = Command::new("op");
            cmd.arg("run")
                .arg("--no-masking")
                .arg("--env-file")
                .arg(temp_env.path())
                .arg("--")
                .arg("sh")
                .arg("-c")
                .arg("env -0");
            cmd.stdin(Stdio::null());
            let out = command_output_with_timeout(
                cmd,
                "`op run` for batch secret resolution",
                op_command_timeout(),
            )?;

            if !out.status.success() {
                return Err(anyhow!(
                    "op run failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                ));
            }

            let wanted_keys: std::collections::HashSet<&str> =
                references.iter().map(|(key, _)| key.as_str()).collect();
            let mut env_vars = HashMap::with_capacity(references.len());
            for record in out.stdout.split(|b| *b == b'\0') {
                if record.is_empty() {
                    continue;
                }
                let kv = String::from_utf8_lossy(record);
                let Some((key, value)) = kv.split_once('=') else {
                    continue;
                };
                if wanted_keys.contains(key) {
                    env_vars.insert(key.to_string(), value.to_string());
                }
            }

            if env_vars.len() != references.len() {
                return Err(anyhow!(
                    "batch resolution was incomplete ({}/{})",
                    env_vars.len(),
                    references.len()
                ));
            }

            Ok(env_vars)
        },
    )
}

fn should_fallback_to_op_read(err: &anyhow::Error) -> bool {
    !is_op_timeout_error(err)
}

fn is_op_timeout_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.to_string().contains("timed out after"))
}

fn print_sectioned_env_output(sections: &[(String, Vec<String>)]) {
    print!("{}", sectioned_env_output_string(sections));
}

fn sectioned_env_output_string(sections: &[(String, Vec<String>)]) -> String {
    let mut out = String::new();
    for (idx, (title, lines)) in sections.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(&format!("# --- item: {} ---\n", title));
        for line in lines {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn show_item_labels(cli: &Cli, items: &[String], with_item: bool) -> Result<()> {
    let sections = instrumentation::with_span_result(
        "load_inputs",
        vec![KeyValue::new("item.count", items.len() as i64)],
        || collect_item_label_sections(cli, items),
    )?;
    let rendered = instrumentation::with_span("main_operation", vec![], || {
        show_output_string(&sections, with_item)
    });
    instrumentation::with_span("write_outputs", vec![], || {
        print!("{rendered}");
    });
    Ok(())
}

fn show_output_string(sections: &[(String, Vec<String>)], with_item: bool) -> String {
    let mut out = String::new();

    if with_item {
        for (idx, (title, labels)) in sections.iter().enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            out.push_str(&format!("# --- item: {} ---\n", title));
            for label in labels {
                out.push_str(label);
                out.push('\n');
            }
        }
        return out;
    }

    for (_, labels) in sections {
        for label in labels {
            out.push_str(label);
            out.push('\n');
        }
    }
    out
}

fn create_api_credential_item_from_env(cli: &Cli, item_title: &str, env_file: &Path) -> Result<()> {
    let env_pairs = instrumentation::with_span_result(
        "load_inputs",
        vec![KeyValue::new(
            "cli.input_path",
            env_file.display().to_string(),
        )],
        || parse_env_file(env_file),
    )?;
    if env_pairs.is_empty() {
        return Err(anyhow!(
            "No valid env entries found in {}",
            env_file.display()
        ));
    }

    let github_repositories = list_remote_repo_names().unwrap_or_default();
    let (args, template) = instrumentation::with_span("main_operation", vec![], || {
        (
            build_create_item_args(cli.vault.as_deref()),
            build_api_credential_template(item_title, &env_pairs, &github_repositories),
        )
    });
    instrumentation::with_span_result("write_outputs", vec![], || {
        run_op_item_create(&args, &template)?;
        invalidate_item_list_cache_best_effort();
        Ok(())
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScriptMigration {
    items: Vec<String>,
    uses_dotenv: bool,
    detected_opz: bool,
    rewritten: String,
}

#[derive(Debug, Clone, Copy)]
enum ScriptMigrationMode<'a> {
    Collect,
    RenameItems { title: &'a str },
    Restore { title: &'a str },
}

fn migrate_scripts(cli: &Cli, dry_run: bool, create_new: bool, restore: bool) -> Result<()> {
    let repositories = resolve_requested_github_repositories(&[])?;
    let canonical_item_title = repositories
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("No GitHub repositories found for migrate"))?;
    if repositories.len() > 1 && restore {
        return Err(anyhow!(
            "`opz migrate --restore` requires exactly one git remote repository. Found: {}",
            repositories.join(", ")
        ));
    }
    let mut migrations = Vec::new();

    for path in migration_script_paths()? {
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let migrated = match path.file_name().and_then(|name| name.to_str()) {
            Some("package.json") => {
                migrate_package_json_scripts(&content, ScriptMigrationMode::Collect)?
            }
            Some("justfile" | "Justfile") => {
                migrate_justfile_scripts(&content, ScriptMigrationMode::Collect)?
            }
            _ => migrate_script_text(&content, ScriptMigrationMode::Collect)?,
        };
        if !migrated.items.is_empty()
            || migrated.uses_dotenv
            || migrated.rewritten != content
            || (restore && migrated.detected_opz)
        {
            migrations.push((path, content, migrated));
        } else if migrated.detected_opz {
            println!(
                "{} contains opz commands, but no supported migration pattern matched or the usage is already up to date.",
                path.display()
            );
        }
    }

    if migrations.is_empty() && !create_new {
        println!("No migratable scripts found.");
        return Ok(());
    }
    if migrations.is_empty() && create_new && !Path::new(".env").exists() {
        println!("No migratable scripts or .env file found.");
        return Ok(());
    }

    let mut item_titles = Vec::new();
    for (_, _, migration) in &migrations {
        item_titles.extend(migration.items.iter().cloned());
    }
    item_titles = dedupe_preserve_order(item_titles);

    let dotenv_item = if create_new
        && (Path::new(".env").exists()
            || migrations
                .iter()
                .any(|(_, _, migration)| migration.uses_dotenv))
    {
        Some(
            repositories
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("No GitHub repositories found for --new"))?,
        )
    } else if migrations
        .iter()
        .any(|(_, _, migration)| migration.uses_dotenv)
    {
        eprintln!("Skipped .env-based script migration; pass `--new` to create an item from .env.");
        None
    } else {
        None
    };

    if let Some(item_title) = &dotenv_item {
        item_titles.push(item_title.clone());
        if dry_run {
            println!("Would create item {item_title} from .env");
        } else {
            create_api_credential_item_from_env(cli, item_title, Path::new(".env"))?;
            println!("Created item {item_title} from .env");
        }
    }

    item_titles = dedupe_preserve_order(item_titles);
    if item_titles.len() > 1 && !create_new {
        return Err(anyhow!(
            "`opz migrate` found multiple item titles ({}). Pass explicit items manually or split the migration.",
            item_titles.join(", ")
        ));
    }

    let rename_item = !restore
        && !create_new
        && repositories.len() == 1
        && item_titles.len() == 1
        && item_titles[0] != canonical_item_title;

    for item_title in &item_titles {
        if dry_run {
            println!(
                "Would ensure {} on {} includes {}",
                GITHUB_REPOSITORIES_LABEL,
                item_title,
                repositories.join(", ")
            );
            if rename_item {
                println!("Would rename item {item_title} to {canonical_item_title}");
            }
        } else {
            let (item_id, _, resolved_title, item) = find_item(cli.vault.as_deref(), item_title)?;
            let merged_repos =
                merge_github_repository_lists(&item_github_repositories(&item), &repositories);
            run_op_item_edit_github_repositories(cli.vault.as_deref(), &item_id, &merged_repos)?;
            println!(
                "Set {} on {} to {}",
                GITHUB_REPOSITORIES_LABEL,
                resolved_title,
                merged_repos.join(", ")
            );
            if rename_item {
                run_op_item_edit_title(cli.vault.as_deref(), &item_id, &canonical_item_title)?;
                println!("Renamed item {resolved_title} to {canonical_item_title}");
            }
        }
    }

    for (path, original, migration) in migrations {
        let mode = if restore {
            ScriptMigrationMode::Restore {
                title: &canonical_item_title,
            }
        } else if rename_item {
            ScriptMigrationMode::RenameItems {
                title: &canonical_item_title,
            }
        } else {
            ScriptMigrationMode::Collect
        };
        let rewritten = match path.file_name().and_then(|name| name.to_str()) {
            Some("package.json") => migrate_package_json_scripts(&original, mode)?.rewritten,
            Some("justfile" | "Justfile") => migrate_justfile_scripts(&original, mode)?.rewritten,
            _ => migrate_script_text(&original, mode)?.rewritten,
        };
        let should_write = rewritten != original
            && (!migration.uses_dotenv || create_new)
            && (!migration.items.is_empty() || create_new || restore || rename_item);
        if !should_write {
            continue;
        }
        if dry_run {
            println!("Would rewrite {}", path.display());
        } else {
            fs::write(&path, rewritten).with_context(|| format!("write {}", path.display()))?;
            println!("Rewrote {}", path.display());
        }
    }

    if !dry_run {
        invalidate_item_list_cache_best_effort();
    }
    Ok(())
}

fn migration_script_paths() -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for name in ["justfile", "Justfile", "package.json"] {
        let path = PathBuf::from(name);
        if path.exists() {
            let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if !seen.insert(canonical) {
                continue;
            }
            paths.push(path);
        }
    }
    Ok(paths)
}

fn migrate_package_json_scripts(
    content: &str,
    mode: ScriptMigrationMode<'_>,
) -> Result<ScriptMigration> {
    let value: serde_json::Value =
        serde_json::from_str(content).context("failed to parse package.json")?;
    let Some(scripts) = value.get("scripts").and_then(|value| value.as_object()) else {
        return Ok(ScriptMigration {
            items: Vec::new(),
            uses_dotenv: false,
            detected_opz: content.contains("opz"),
            rewritten: content.to_string(),
        });
    };

    let mut all_items = Vec::new();
    let mut uses_dotenv = false;
    let mut rewritten = content.to_string();
    for script in scripts.values() {
        let Some(text) = script.as_str() else {
            continue;
        };
        let migration = migrate_script_text(text, mode)?;
        all_items.extend(migration.items);
        uses_dotenv |= migration.uses_dotenv;
        if migration.rewritten != text {
            rewritten = replace_json_string_literal(&rewritten, text, &migration.rewritten)?;
        }
    }

    Ok(ScriptMigration {
        items: dedupe_preserve_order(all_items),
        uses_dotenv,
        detected_opz: content.contains("opz"),
        rewritten,
    })
}

fn replace_json_string_literal(content: &str, old: &str, new: &str) -> Result<String> {
    let old_literal = serde_json::to_string(old)?;
    let new_literal = serde_json::to_string(new)?;
    Ok(content.replacen(&old_literal, &new_literal, 1))
}

fn migrate_script_text(content: &str, mode: ScriptMigrationMode<'_>) -> Result<ScriptMigration> {
    migrate_script_text_with_items(content, &HashMap::new(), mode)
}

fn migrate_justfile_scripts(
    content: &str,
    mode: ScriptMigrationMode<'_>,
) -> Result<ScriptMigration> {
    let mut global_values = HashMap::new();
    let mut recipe_values = HashMap::new();
    let mut all_items = Vec::new();
    let mut uses_dotenv = false;
    let mut detected_opz = content.contains("opz");
    let mut rewritten = String::with_capacity(content.len());

    for line in content.split_inclusive('\n') {
        let (body, newline) = line
            .strip_suffix('\n')
            .map(|body| (body, "\n"))
            .unwrap_or((line, ""));

        if is_just_top_level_line(body) {
            if let Some((name, value)) = parse_just_assignment(body, &global_values) {
                global_values.insert(name, value);
            }
            recipe_values = parse_just_recipe_values(body, &global_values).unwrap_or_default();
        }

        let active_values = if recipe_values.is_empty() {
            &global_values
        } else {
            &recipe_values
        };
        let migration = migrate_script_text_with_items(body, active_values, mode)?;
        collect_just_opz_metadata_items(body, active_values, &mut all_items)?;
        all_items.extend(migration.items);
        uses_dotenv |= migration.uses_dotenv;
        detected_opz |= migration.detected_opz;
        rewritten.push_str(&migration.rewritten);
        rewritten.push_str(newline);
    }

    Ok(ScriptMigration {
        items: dedupe_preserve_order(all_items),
        uses_dotenv,
        detected_opz,
        rewritten,
    })
}

fn migrate_script_text_with_items(
    content: &str,
    item_values: &HashMap<String, String>,
    mode: ScriptMigrationMode<'_>,
) -> Result<ScriptMigration> {
    let opz_run_re =
        Regex::new(r"\bopz\s+run(?P<opts>(?:\s+--env-file\s+\S+)?)\s+(?P<item>[^\s-][^\s]*)\s+--")?;
    let shorthand_re = Regex::new(r"\bopz\s+(?P<item>[^\s-][^\s]*)\s+--")?;
    let itemless_run_re = Regex::new(r"\bopz\s+run(?P<opts>(?:\s+--env-file\s+\S+)?)\s+--")?;
    let itemless_shorthand_re = Regex::new(r"\bopz\s+--")?;
    let op_item_get_re = Regex::new(r"\bop\s+item\s+get\s+(?P<item>[^\s-][^\s]*)")?;
    let op_run_env_re = Regex::new(r"\bop\s+run\s+--env-file\s+\.env\s+--")?;

    let mut items = Vec::new();
    let rewritten = opz_run_re
        .replace_all(content, |caps: &regex::Captures| {
            let item = caps["item"].to_string();
            if let Some(resolved) = resolve_migration_item_token(&item, item_values) {
                items.push(resolved);
                match mode {
                    ScriptMigrationMode::RenameItems { title } if is_static_item_token(&item) => {
                        format!("opz run{} {} --", &caps["opts"], title)
                    }
                    _ => caps[0].to_string(),
                }
            } else {
                caps[0].to_string()
            }
        })
        .to_string();
    let rewritten = shorthand_re
        .replace_all(&rewritten, |caps: &regex::Captures| {
            let item = caps["item"].to_string();
            if matches!(
                item.as_str(),
                "run"
                    | "find"
                    | "doctor"
                    | "skills"
                    | "show"
                    | "gen"
                    | "create"
                    | "migrate"
                    | "note"
                    | "github-repo"
                    | "github-secret"
                    | "cloudflare-secret"
            ) {
                caps[0].to_string()
            } else if let Some(resolved) = resolve_migration_item_token(&item, item_values) {
                items.push(resolved);
                match mode {
                    ScriptMigrationMode::RenameItems { title } if is_static_item_token(&item) => {
                        format!("opz {title} --")
                    }
                    _ => caps[0].to_string(),
                }
            } else {
                caps[0].to_string()
            }
        })
        .to_string();
    let rewritten = match mode {
        ScriptMigrationMode::Restore { title } => itemless_run_re
            .replace_all(&rewritten, |caps: &regex::Captures| {
                let item = preferred_restore_item_token(item_values, title);
                format!("opz run{} {} --", &caps["opts"], item)
            })
            .to_string(),
        _ => rewritten,
    };
    let rewritten = match mode {
        ScriptMigrationMode::Restore { title } => itemless_shorthand_re
            .replace_all(&rewritten, |_: &regex::Captures| {
                let item = preferred_restore_item_token(item_values, title);
                format!("opz {item} --")
            })
            .to_string(),
        _ => rewritten,
    };
    for caps in op_item_get_re.captures_iter(&rewritten) {
        let item = caps["item"].to_string();
        if let Some(resolved) = resolve_migration_item_token(&item, item_values) {
            items.push(resolved);
        }
    }
    let uses_dotenv = op_run_env_re.is_match(&rewritten);
    let rewritten = match mode {
        ScriptMigrationMode::RenameItems { title } | ScriptMigrationMode::Restore { title } => {
            op_run_env_re
                .replace_all(&rewritten, format!("opz run {title} --"))
                .to_string()
        }
        ScriptMigrationMode::Collect => rewritten,
    };
    let rewritten = rewrite_just_item_assignments(&rewritten, item_values, mode);

    Ok(ScriptMigration {
        items: dedupe_preserve_order(items),
        uses_dotenv,
        detected_opz: content.contains("opz"),
        rewritten,
    })
}

fn is_just_top_level_line(line: &str) -> bool {
    !line.trim().is_empty()
        && !line.trim_start().starts_with('#')
        && line
            .chars()
            .next()
            .is_some_and(|ch| !ch.is_ascii_whitespace())
}

fn parse_just_assignment(line: &str, values: &HashMap<String, String>) -> Option<(String, String)> {
    let (name, value) = line.split_once(":=")?;
    let name = name.trim();
    if !is_just_identifier(name) {
        return None;
    }
    resolve_just_value(value.trim(), values).map(|value| (name.to_string(), value))
}

fn parse_just_recipe_values(
    line: &str,
    global_values: &HashMap<String, String>,
) -> Option<HashMap<String, String>> {
    if line.contains(":=") {
        return None;
    }
    let (header, _) = line.split_once(':')?;
    let mut parts = header.split_whitespace();
    parts.next()?;

    let mut values = global_values.clone();
    for part in parts {
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        if !is_just_identifier(name) {
            continue;
        }
        if let Some(value) = resolve_just_value(value, &values) {
            values.insert(name.to_string(), value);
        }
    }

    Some(values)
}

fn resolve_just_value(value: &str, values: &HashMap<String, String>) -> Option<String> {
    let value = value.trim();
    if let Some(stripped) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
    {
        return Some(stripped.to_string());
    }
    values.get(value).cloned()
}

fn collect_just_opz_metadata_items(
    line: &str,
    item_values: &HashMap<String, String>,
    items: &mut Vec<String>,
) -> Result<()> {
    let gen_re = Regex::new(r"\bopz\s+gen\b(?P<args>[^\n]*)")?;
    let create_re = Regex::new(r"\bopz\s+create\s+(?P<item>[^\s-][^\s]*)")?;

    for caps in gen_re.captures_iter(line) {
        if let Some(item) = first_opz_gen_item_token(&caps["args"]) {
            if let Some(resolved) = resolve_migration_item_token(item, item_values) {
                items.push(resolved);
            }
        }
    }
    for caps in create_re.captures_iter(line) {
        let item = caps["item"].to_string();
        if let Some(resolved) = resolve_migration_item_token(&item, item_values) {
            items.push(resolved);
        }
    }

    Ok(())
}

fn first_opz_gen_item_token(args: &str) -> Option<&str> {
    let mut tokens = args.split_whitespace();
    while let Some(token) = tokens.next() {
        if token == "--env-file" {
            tokens.next();
            continue;
        }
        if token.starts_with("--env-file=") || token.starts_with('-') {
            continue;
        }
        return Some(token);
    }
    None
}

fn resolve_migration_item_token(
    value: &str,
    item_values: &HashMap<String, String>,
) -> Option<String> {
    if is_static_item_token(value) {
        return Some(value.to_string());
    }
    let name = value
        .strip_prefix("{{")
        .and_then(|value| value.strip_suffix("}}"))?
        .trim();
    item_values.get(name).cloned()
}

fn preferred_restore_item_token(
    item_values: &HashMap<String, String>,
    fallback_title: &str,
) -> String {
    if item_values
        .get("item")
        .is_some_and(|value| !value.is_empty())
    {
        return "{{item}}".to_string();
    }
    for (name, value) in item_values {
        if name.contains("item") && !value.is_empty() {
            return format!("{{{{{name}}}}}");
        }
    }
    for (name, value) in item_values {
        if !value.is_empty() {
            return format!("{{{{{name}}}}}");
        }
    }
    fallback_title.to_string()
}

fn rewrite_just_item_assignments(
    content: &str,
    item_values: &HashMap<String, String>,
    mode: ScriptMigrationMode<'_>,
) -> String {
    let ScriptMigrationMode::RenameItems { title } = mode else {
        return content.to_string();
    };

    let Some((lhs, rhs)) = content.split_once(":=") else {
        return content.to_string();
    };
    let lhs_name = lhs.trim();
    if !item_values
        .keys()
        .any(|name| name == lhs_name && name.contains("item"))
    {
        return content.to_string();
    }
    let prefix = &content[..content.find(":=").unwrap_or(content.len()) + 2];
    let rhs_trimmed = rhs.trim();
    if !(rhs_trimmed.starts_with('"') || rhs_trimmed.starts_with('\'')) {
        return content.to_string();
    }
    format!("{prefix} \"{title}\"")
}

fn is_just_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn is_static_item_token(value: &str) -> bool {
    !value.contains('{')
        && !value.contains('}')
        && !value.contains('$')
        && !value.contains('*')
        && !value.contains('?')
        && !value.contains('`')
        && !value.contains('(')
        && !value.contains(')')
}

fn dedupe_preserve_order(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn build_create_item_args(vault: Option<&str>) -> Vec<String> {
    let mut args = vec!["item".to_string(), "create".to_string()];

    if let Some(v) = vault {
        args.push("--vault".to_string());
        args.push(v.to_string());
    }

    args.push("-".to_string());
    args
}

fn build_api_credential_template(
    item_title: &str,
    env_pairs: &[(String, String)],
    github_repositories: &[String],
) -> ItemCreateTemplate {
    let mut fields =
        Vec::with_capacity(env_pairs.len() + usize::from(!github_repositories.is_empty()));
    for (key, value) in env_pairs {
        fields.push(ItemCreateField {
            id: key.clone(),
            field_type: "STRING".to_string(),
            label: key.clone(),
            value: value.clone(),
            purpose: None,
        });
    }
    if !github_repositories.is_empty() {
        fields.push(ItemCreateField {
            id: GITHUB_REPOSITORIES_LABEL.to_string(),
            field_type: "STRING".to_string(),
            label: GITHUB_REPOSITORIES_LABEL.to_string(),
            value: github_repositories.join("\n"),
            purpose: None,
        });
    }

    ItemCreateTemplate {
        title: item_title.to_string(),
        category: "API_CREDENTIAL".to_string(),
        fields,
    }
}

fn create_secure_notes_from_file(cli: &Cli, file_path: &Path) -> Result<()> {
    let (file_name, content, remote_repo_names) = instrumentation::with_span_result(
        "load_inputs",
        vec![KeyValue::new(
            "cli.input_path",
            file_path.display().to_string(),
        )],
        || {
            let content = fs::read_to_string(file_path)
                .with_context(|| format!("read {}", file_path.display()))?;
            let file_name = file_path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .ok_or_else(|| anyhow!("invalid file path: {}", file_path.display()))?;
            let remote_repo_names = list_remote_repo_names()?;
            Ok((file_name, content, remote_repo_names))
        },
    )?;
    let (body, item_titles) = instrumentation::with_span("main_operation", vec![], || {
        let body = build_secure_note_body(&file_name, &content);
        let item_titles = dedupe_titles_with_sequence(&remote_repo_names);
        (body, item_titles)
    });

    instrumentation::with_span_result("write_outputs", vec![], || {
        for item_title in item_titles {
            let args = build_create_item_args(cli.vault.as_deref());
            let template = build_secure_note_template(&item_title, &body);
            run_op_item_create(&args, &template)?;
        }
        invalidate_item_list_cache_best_effort();
        Ok(())
    })
}

fn update_github_repositories_metadata(
    cli: &Cli,
    repos: &[String],
    dry_run: bool,
    items: &[String],
) -> Result<()> {
    let requested_repos = resolve_requested_github_repositories(repos)?;
    if requested_repos.is_empty() {
        return Err(anyhow!(
            "No GitHub repositories found. Run inside a git repository with a parseable remote, or pass --repo owner/repo."
        ));
    }

    instrumentation::with_span_result(
        "write_outputs.github_repo_metadata",
        vec![
            KeyValue::new("item.count", items.len() as i64),
            KeyValue::new("github.repo_count", requested_repos.len() as i64),
        ],
        || {
            for item_title in items {
                let (item_id, _, resolved_title, item) =
                    find_item(cli.vault.as_deref(), item_title)?;
                let merged_repos = merge_github_repository_lists(
                    &item_github_repositories(&item),
                    &requested_repos,
                );
                if dry_run {
                    println!(
                        "Would set {} on {} to {}",
                        GITHUB_REPOSITORIES_LABEL,
                        resolved_title,
                        merged_repos.join(", ")
                    );
                    continue;
                }

                run_op_item_edit_github_repositories(
                    cli.vault.as_deref(),
                    &item_id,
                    &merged_repos,
                )?;
                println!(
                    "Set {} on {} to {}",
                    GITHUB_REPOSITORIES_LABEL,
                    resolved_title,
                    merged_repos.join(", ")
                );
            }
            if !dry_run {
                invalidate_item_list_cache_best_effort();
            }
            Ok(())
        },
    )
}

fn resolve_requested_github_repositories(repos: &[String]) -> Result<Vec<String>> {
    let raw_repos = if repos.is_empty() {
        list_remote_repo_names()?
    } else {
        repos.to_vec()
    };
    let normalized: Vec<String> = raw_repos
        .iter()
        .filter_map(|repo| normalize_github_repo_spec(repo))
        .collect();
    if normalized.len() != raw_repos.len() {
        return Err(anyhow!(
            "Invalid GitHub repository. Expected owner/repo, https://github.com/owner/repo.git, or git@github.com:owner/repo.git"
        ));
    }
    Ok(dedupe_github_repositories(&normalized))
}

fn merge_github_repository_lists(existing: &[String], requested: &[String]) -> Vec<String> {
    let mut repos = Vec::with_capacity(existing.len() + requested.len());
    repos.extend(existing.iter().cloned());
    repos.extend(requested.iter().cloned());
    dedupe_github_repositories(&repos)
}

fn dedupe_github_repositories(repos: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    repos
        .iter()
        .filter_map(|repo| normalize_github_repo_spec(repo))
        .filter(|repo| seen.insert(repo.clone()))
        .collect()
}

fn build_op_item_edit_github_repositories_args(
    vault: Option<&str>,
    item_id: &str,
    repositories: &[String],
) -> Vec<String> {
    let mut args = vec!["item".to_string(), "edit".to_string(), item_id.to_string()];
    if let Some(vault) = vault {
        args.push("--vault".to_string());
        args.push(vault.to_string());
    }
    args.push(format!(
        "{}={}",
        GITHUB_REPOSITORIES_LABEL,
        repositories.join("\n")
    ));
    args
}

fn run_op_item_edit_github_repositories(
    vault: Option<&str>,
    item_id: &str,
    repositories: &[String],
) -> Result<()> {
    let args = build_op_item_edit_github_repositories_args(vault, item_id, repositories);
    let status = Command::new("op")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("failed to run `op item edit`")?;

    if !status.success() {
        return Err(anyhow!("op item edit failed with status: {}", status));
    }
    Ok(())
}

fn run_op_item_edit_title(vault: Option<&str>, item_id: &str, title: &str) -> Result<()> {
    let mut args = vec![
        "item".to_string(),
        "edit".to_string(),
        item_id.to_string(),
        format!("title={title}"),
    ];
    if let Some(vault) = vault {
        args.push("--vault".to_string());
        args.push(vault.to_string());
    }
    let status = Command::new("op")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("failed to run `op item edit`")?;

    if !status.success() {
        return Err(anyhow!("op item edit failed with status: {}", status));
    }
    Ok(())
}

fn build_secure_note_body(file_name: &str, content: &str) -> String {
    let mut body = format!("```{}\n", file_name);
    body.push_str(content);
    if !content.ends_with('\n') {
        body.push('\n');
    }
    body.push_str("```");
    body
}

fn build_secure_note_template(item_title: &str, body: &str) -> ItemCreateTemplate {
    ItemCreateTemplate {
        title: item_title.to_string(),
        category: "SECURE_NOTE".to_string(),
        fields: vec![ItemCreateField {
            id: "notesPlain".to_string(),
            field_type: "STRING".to_string(),
            label: "notesPlain".to_string(),
            value: body.to_string(),
            purpose: Some("NOTES".to_string()),
        }],
    }
}

fn run_op_item_create(args: &[String], template: &ItemCreateTemplate) -> Result<()> {
    instrumentation::with_span_result(
        "write_outputs.op_item_create",
        vec![KeyValue::new("op.arg_count", args.len() as i64)],
        || {
            let sensitive_fields = collect_create_stdout_sensitive_fields(template);
            let mut cmd = Command::new("op");
            cmd.args(args);

            let mut child = cmd
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("failed to run `op item create`")?;

            {
                let mut stdin = child
                    .stdin
                    .take()
                    .ok_or_else(|| anyhow!("failed to open stdin for `op item create`"))?;
                serde_json::to_writer(&mut stdin, template)
                    .context("failed to write `op item create` template to stdin")?;
            }

            let output = child
                .wait_with_output()
                .context("failed to wait for `op item create`")?;

            std::io::stderr()
                .write_all(&output.stderr)
                .context("failed to write `op item create` stderr")?;

            if !output.status.success() {
                std::io::stdout()
                    .write_all(&output.stdout)
                    .context("failed to write `op item create` stdout")?;
                return Err(anyhow!(
                    "op item create failed with status: {}",
                    output.status
                ));
            }

            let masked_stdout =
                mask_create_stdout(&String::from_utf8_lossy(&output.stdout), &sensitive_fields);
            std::io::stdout()
                .write_all(masked_stdout.as_bytes())
                .context("failed to write masked `op item create` stdout")?;

            Ok(())
        },
    )
}

fn collect_create_stdout_sensitive_fields(template: &ItemCreateTemplate) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    for field in &template.fields {
        if field.value.is_empty() {
            continue;
        }
        fields.push((field.label.clone(), field.value.clone()));
        if field.id != field.label {
            fields.push((field.id.clone(), field.value.clone()));
        }
    }

    fields.sort_by_key(|(_, value)| std::cmp::Reverse(value.len()));
    fields.dedup();
    fields
}

fn mask_create_stdout(stdout: &str, sensitive_fields: &[(String, String)]) -> String {
    let mut masked = stdout.to_string();
    for (field_name, value) in sensitive_fields {
        let pattern = format!(
            "{}(^\\s*{}(?:\\s*\\[[^\\]]+\\])?\\s*[:=]\\s*){}(\\s*$)",
            if value.contains('\n') {
                "(?ms)"
            } else {
                "(?m)"
            },
            regex::escape(field_name),
            regex::escape(value)
        );
        let Ok(regex) = Regex::new(&pattern) else {
            continue;
        };
        masked = regex.replace_all(&masked, "$1***$2").into_owned();
    }
    masked
}

fn list_remote_repo_names() -> Result<Vec<String>> {
    let out = Command::new("git")
        .args(["config", "--get-regexp", r"^remote\..*\.url$"])
        .output()
        .context("failed to run `git config --get-regexp '^remote\\..*\\.url$'`")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(anyhow!(
            "failed to read git remotes: {}",
            if stderr.is_empty() {
                "no remote configured"
            } else {
                &stderr
            }
        ));
    }

    let stdout = String::from_utf8(out.stdout).context("git output was not valid UTF-8")?;
    let mut repo_names = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let _key = parts.next();
        let Some(url) = parts.next() else {
            continue;
        };
        if let Some(repo_name) = extract_org_repo_from_remote_url(url) {
            repo_names.push(repo_name);
        }
    }

    if repo_names.is_empty() {
        return Err(anyhow!(
            "no parseable git remotes found; note requires at least one remote URL like https://host/org/repo.git"
        ));
    }

    Ok(repo_names)
}

fn extract_org_repo_from_remote_url(url: &str) -> Option<String> {
    let stripped = url.split(['?', '#']).next()?;
    let path = if let Some((_, rest)) = stripped.split_once("://") {
        let (host_part, path_part) = rest.split_once('/')?;
        if host_part.is_empty() {
            return None;
        }
        path_part
    } else if stripped.contains('@') && stripped.contains(':') {
        let (_, path_part) = stripped.split_once(':')?;
        path_part
    } else {
        return None;
    };

    let normalized = path.trim_matches('/').trim_end_matches(".git");
    let segments: Vec<&str> = normalized
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() < 2 {
        return None;
    }

    let org = segments[segments.len() - 2];
    let repo = segments[segments.len() - 1];
    Some(format!("{org}/{repo}"))
}

fn dedupe_titles_with_sequence(base_titles: &[String]) -> Vec<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut titles = Vec::with_capacity(base_titles.len());

    for base in base_titles {
        let count = counts.entry(base.clone()).or_insert(0);
        *count += 1;
        if *count == 1 {
            titles.push(base.clone());
        } else {
            titles.push(format!("{}-{}", base, count));
        }
    }

    titles
}

fn parse_env_file(path: &Path) -> Result<Vec<(String, String)>> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let label_re = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$")?;
    let mut pairs = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let normalized = match line.strip_prefix("export") {
            Some(rest) if rest.chars().next().is_some_and(char::is_whitespace) => rest.trim_start(),
            _ => line,
        };
        let Some((raw_key, raw_value)) = normalized.split_once('=') else {
            continue;
        };
        let key = raw_key.trim();
        if !label_re.is_match(key) {
            eprintln!("Skipped invalid key in env file: {key}");
            continue;
        }

        let value = normalize_env_value(raw_value);
        if is_op_reference(&value) {
            eprintln!("Skipped already imported op:// value for key: {key}");
            continue;
        }

        // Last occurrence wins for duplicate keys.
        if let Some(pos) = pairs
            .iter()
            .position(|(existing_key, _)| existing_key == key)
        {
            pairs.remove(pos);
        }

        pairs.push((key.to_string(), value));
    }

    Ok(pairs)
}

fn normalize_env_value(raw_value: &str) -> String {
    let mut value = strip_inline_comment(raw_value).trim().to_string();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value = value[1..value.len() - 1].to_string();
    }
    value
}

fn strip_inline_comment(value: &str) -> &str {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped_in_double = false;

    for (idx, ch) in value.char_indices() {
        if in_double_quote {
            if escaped_in_double {
                escaped_in_double = false;
                continue;
            }
            if ch == '\\' {
                escaped_in_double = true;
                continue;
            }
            if ch == '"' {
                in_double_quote = false;
            }
            continue;
        }

        if in_single_quote {
            if ch == '\'' {
                in_single_quote = false;
            }
            continue;
        }

        match ch {
            '"' => in_double_quote = true,
            '\'' => in_single_quote = true,
            '#' if idx == 0 || value[..idx].chars().last().is_some_and(char::is_whitespace) => {
                return value[..idx].trim_end();
            }
            _ => {}
        }
    }

    value
}

fn is_op_reference(value: &str) -> bool {
    value.starts_with("op://")
}

/// Find and match item by title, returns (item_id, vault_id, item_title)
fn find_item(vault: Option<&str>, item_title: &str) -> Result<(String, String, String, ItemGet)> {
    match find_item_exact(vault, item_title) {
        Ok(item) => return Ok(item),
        Err(err) if is_op_lookup_miss(&err) => {}
        Err(err) => return Err(err),
    }

    find_item_from_cached_list(vault, item_title)
}

fn find_item_from_cached_list(
    vault: Option<&str>,
    item_title: &str,
) -> Result<(String, String, String, ItemGet)> {
    let items = item_list_cached(vault)?;

    let mut matches: Vec<ItemListEntry> = items
        .into_iter()
        .filter(|x| x.title == item_title)
        .collect();

    // If exact match not found, fallback to contains (simple fuzzy)
    if matches.is_empty() {
        let q = item_title.to_lowercase();
        matches = item_list_cached(vault)?
            .into_iter()
            .filter(|x| x.title.to_lowercase().contains(&q))
            .collect();
    }

    if matches.is_empty() {
        return Err(anyhow!("No item matched title: {}", item_title));
    }
    if matches.len() > 1 {
        eprintln!("Ambiguous item title. Candidates:");
        for it in matches.iter().take(20) {
            let vault = it.vault.as_ref().map(|v| v.name.as_str()).unwrap_or("-");
            eprintln!("  {}  [{}]  {}", it.id, vault, it.title);
        }
        return Err(anyhow!(
            "Please be more specific or use `opz find <query>` and pass exact title."
        ));
    }

    let item_id = matches[0].id.clone();
    let item = item_get(&item_id)?;
    let vault_id = resolve_vault_id(
        matches.first().and_then(|m| m.vault.as_ref()),
        item.vault.as_ref(),
    )
    .ok_or_else(|| anyhow!("Vault ID is required. Try specifying --vault."))?;

    Ok((item_id, vault_id, matches[0].title.clone(), item))
}

/// Find an item by exact title without scanning every item. This is the preferred
/// path for repository-title auto-detection.
fn find_item_exact(
    vault: Option<&str>,
    item_title: &str,
) -> Result<(String, String, String, ItemGet)> {
    let item = item_get_with_vault(vault, item_title)?;
    let item_id = item
        .id
        .clone()
        .ok_or_else(|| anyhow!("Item ID is required for `{item_title}`"))?;
    let vault_id = item
        .vault
        .as_ref()
        .map(|vault| vault.id.clone())
        .ok_or_else(|| anyhow!("Vault ID is required. Try specifying --vault."))?;
    let resolved_title = item.title.clone().unwrap_or_else(|| item_title.to_string());
    if resolved_title != item_title {
        return Err(anyhow!("No exact item matched title: {item_title}"));
    }

    Ok((item_id, vault_id, resolved_title, item))
}

fn resolve_vault_id(
    list_vault: Option<&ItemVault>,
    item_vault: Option<&ItemVault>,
) -> Option<String> {
    list_vault.or(item_vault).map(|v| v.id.clone())
}

fn generate_env_output(cli: &Cli, items: &[String], env_file: Option<&Path>) -> Result<()> {
    let sections = instrumentation::with_span_result(
        "load_inputs",
        vec![KeyValue::new("item.count", items.len() as i64)],
        || collect_item_env_sections(cli, items),
    )?;
    let merged_env_lines =
        instrumentation::with_span("main_operation", vec![], || merge_env_lines(&sections));

    instrumentation::with_span_result(
        "write_outputs",
        vec![
            KeyValue::new(
                "cli.output_mode",
                if env_file.is_some() {
                    "file".to_string()
                } else {
                    "stdout".to_string()
                },
            ),
            KeyValue::new(
                "cli.output_path",
                env_file
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "-".to_string()),
            ),
        ],
        || {
            if let Some(path) = env_file {
                write_env_file(path, &merged_env_lines)?;
                eprintln!("Generated: {}", path.display());
            } else {
                print_sectioned_env_output(&sections);
            }
            Ok(())
        },
    )
}

fn set_github_secrets(
    cli: &Cli,
    repo: Option<&str>,
    dry_run: bool,
    items: &[String],
) -> Result<()> {
    let (sections, item_repositories) = instrumentation::with_span_result(
        "load_inputs",
        vec![KeyValue::new("item.count", items.len() as i64)],
        || collect_item_env_sections_with_github_repos(cli, items),
    )?;
    let merged_env_lines =
        instrumentation::with_span("main_operation", vec![], || merge_env_lines(&sections));
    let secret_names = validate_github_secret_lines(&merged_env_lines)?;
    if secret_names.is_empty() {
        return Err(anyhow!("No valid GitHub secret fields found"));
    }

    let resolved_repo =
        instrumentation::with_span_result("load_config.github_repo", vec![], || match repo {
            Some(repo) => Ok(repo.to_string()),
            None => resolve_current_github_repo(),
        })?;

    guard_github_secret_repo(&resolved_repo, &item_repositories)?;

    if dry_run {
        return instrumentation::with_span("write_outputs", vec![], || {
            for name in secret_names {
                println!("Would set GitHub secret {name} in {resolved_repo}");
            }
            Ok(())
        });
    }

    let env_vars = instrumentation::with_span_result("load_inputs", vec![], || {
        resolve_env_vars(&merged_env_lines)
    })?;

    instrumentation::with_span_result(
        "write_outputs.github_secret_set",
        vec![
            KeyValue::new("github.repo", resolved_repo.clone()),
            KeyValue::new("github.secret_count", secret_names.len() as i64),
        ],
        || {
            for name in secret_names {
                let value = env_vars
                    .get(&name)
                    .ok_or_else(|| anyhow!("resolved value missing for GitHub secret {name}"))?;
                run_gh_secret_set(&resolved_repo, &name, value)?;
                println!("Set GitHub secret {name} in {resolved_repo}");
            }
            Ok(())
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ItemGithubRepositories {
    item_title: String,
    repositories: Vec<String>,
}

fn guard_github_secret_repo(
    resolved_repo: &str,
    item_repositories: &[ItemGithubRepositories],
) -> Result<()> {
    let normalized_target = normalize_github_repo_spec(resolved_repo)
        .ok_or_else(|| anyhow!("Invalid GitHub repository: {resolved_repo}"))?;
    let mut missing_metadata = Vec::new();

    for item in item_repositories {
        if item.repositories.is_empty() {
            missing_metadata.push(item.item_title.as_str());
            continue;
        }

        let allowed: HashSet<String> = item
            .repositories
            .iter()
            .filter_map(|repo| normalize_github_repo_spec(repo))
            .collect();
        if !allowed.contains(&normalized_target) {
            return Err(anyhow!(
                "GitHub repository mismatch for item `{}`: target `{}` is not listed in `{}`. Add the repository to the item metadata or pass a matching --repo.",
                item.item_title,
                resolved_repo,
                GITHUB_REPOSITORIES_LABEL
            ));
        }
    }

    if !missing_metadata.is_empty() {
        eprintln!(
            "Warning: item(s) missing `{}` metadata: {}. Add one owner/repo per line to prevent GitHub secret misdelivery.",
            GITHUB_REPOSITORIES_LABEL,
            missing_metadata.join(", ")
        );
    }

    Ok(())
}

#[derive(Clone, Copy)]
struct CloudflareSecretTarget<'a> {
    name: Option<&'a str>,
    env: Option<&'a str>,
    config: Option<&'a Path>,
}

fn set_cloudflare_secrets(
    cli: &Cli,
    target: CloudflareSecretTarget<'_>,
    dry_run: bool,
    items: &[String],
) -> Result<()> {
    let sections = instrumentation::with_span_result(
        "load_inputs",
        vec![KeyValue::new("item.count", items.len() as i64)],
        || collect_item_env_sections(cli, items),
    )?;
    let merged_env_lines =
        instrumentation::with_span("main_operation", vec![], || merge_env_lines(&sections));
    let secret_names = validate_cloudflare_secret_lines(&merged_env_lines)?;
    if secret_names.is_empty() {
        return Err(anyhow!("No valid Cloudflare secret fields found"));
    }

    let target_label = cloudflare_target_label(target);
    if dry_run {
        return instrumentation::with_span("write_outputs", vec![], || {
            for name in secret_names {
                println!("Would set Cloudflare Worker secret {name} in {target_label}");
            }
            Ok(())
        });
    }

    let env_vars = instrumentation::with_span_result("load_inputs", vec![], || {
        resolve_env_vars(&merged_env_lines)
    })?;
    let payload = build_secret_json_payload(&secret_names, &env_vars)?;

    instrumentation::with_span_result(
        "write_outputs.cloudflare_secret_bulk",
        vec![
            KeyValue::new("cloudflare.target", target_label),
            KeyValue::new("cloudflare.secret_count", secret_names.len() as i64),
        ],
        || {
            run_wrangler_secret_bulk(target, payload.as_bytes())?;
            for name in secret_names {
                println!("Set Cloudflare Worker secret {name}");
            }
            Ok(())
        },
    )
}

fn cloudflare_target_label(target: CloudflareSecretTarget<'_>) -> String {
    let worker = target.name.unwrap_or("wrangler config default worker");
    match target.env {
        Some(env) => format!("{worker} ({env})"),
        None => worker.to_string(),
    }
}

fn validate_cloudflare_secret_lines(env_lines: &[String]) -> Result<Vec<String>> {
    validate_secret_lines(env_lines, "Cloudflare")
}

fn validate_github_secret_lines(env_lines: &[String]) -> Result<Vec<String>> {
    let names = validate_secret_lines(env_lines, "GitHub")?;
    for name in &names {
        if name.to_ascii_uppercase().starts_with("GITHUB_") {
            return Err(anyhow!(
                "GitHub secret name cannot start with reserved prefix GITHUB_: {name}"
            ));
        }
    }
    Ok(names)
}

fn validate_secret_lines(env_lines: &[String], target_name: &str) -> Result<Vec<String>> {
    env_lines
        .iter()
        .filter_map(|line| parse_env_key(line).map(str::to_string))
        .map(|name| {
            validate_secret_name(&name, target_name)?;
            Ok(name)
        })
        .collect()
}

#[cfg(test)]
fn validate_github_secret_name(name: &str) -> Result<()> {
    validate_secret_name(name, "GitHub")?;
    if name.to_ascii_uppercase().starts_with("GITHUB_") {
        return Err(anyhow!(
            "GitHub secret name cannot start with reserved prefix GITHUB_: {name}"
        ));
    }
    Ok(())
}

fn validate_secret_name(name: &str, target_name: &str) -> Result<()> {
    let re = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$")?;
    if !re.is_match(name) {
        return Err(anyhow!("Invalid {target_name} secret name: {name}"));
    }
    Ok(())
}

fn build_gh_secret_set_args(repo: &str, name: &str) -> Vec<String> {
    vec![
        "secret".to_string(),
        "set".to_string(),
        name.to_string(),
        "--repo".to_string(),
        repo.to_string(),
    ]
}

fn build_wrangler_secret_bulk_args(target: CloudflareSecretTarget<'_>) -> Vec<String> {
    let mut args = vec!["secret".to_string(), "bulk".to_string()];

    if let Some(name) = target.name {
        args.push("--name".to_string());
        args.push(name.to_string());
    }
    if let Some(env) = target.env {
        args.push("--env".to_string());
        args.push(env.to_string());
    }
    if let Some(config) = target.config {
        args.push("--config".to_string());
        args.push(config.display().to_string());
    }

    args
}

fn build_secret_json_payload(
    secret_names: &[String],
    env_vars: &HashMap<String, String>,
) -> Result<String> {
    let mut secrets = serde_json::Map::with_capacity(secret_names.len());
    for name in secret_names {
        let value = env_vars
            .get(name)
            .ok_or_else(|| anyhow!("resolved value missing for secret {name}"))?;
        secrets.insert(name.clone(), serde_json::Value::String(value.clone()));
    }
    serde_json::to_string(&secrets).context("failed to encode Wrangler secret payload")
}

fn resolve_current_github_repo() -> Result<String> {
    let out = Command::new("gh")
        .args([
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "--jq",
            ".nameWithOwner",
        ])
        .output()
        .context("failed to run `gh repo view`")?;

    if !out.status.success() {
        return Err(anyhow!(
            "gh repo view failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    let repo = String::from_utf8(out.stdout)
        .context("gh repo view output was not valid UTF-8")?
        .trim()
        .to_string();
    if repo.is_empty() {
        return Err(anyhow!("gh repo view returned an empty repository name"));
    }
    Ok(repo)
}

fn run_gh_secret_set(repo: &str, name: &str, value: &str) -> Result<()> {
    let args = build_gh_secret_set_args(repo, name);
    let mut child = Command::new("gh")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to run `gh secret set`")?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("failed to open stdin for `gh secret set`"))?;
        stdin
            .write_all(value.as_bytes())
            .context("failed to write GitHub secret value to stdin")?;
    }

    let status = child.wait().context("failed to wait for `gh secret set`")?;
    if !status.success() {
        return Err(anyhow!("gh secret set failed with status: {}", status));
    }
    Ok(())
}

fn run_wrangler_secret_bulk(target: CloudflareSecretTarget<'_>, payload: &[u8]) -> Result<()> {
    let args = build_wrangler_secret_bulk_args(target);
    let mut child = Command::new("wrangler")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to run `wrangler secret bulk`")?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("failed to open stdin for `wrangler secret bulk`"))?;
        stdin
            .write_all(payload)
            .context("failed to write Cloudflare secret payload to stdin")?;
    }

    let status = child
        .wait()
        .context("failed to wait for `wrangler secret bulk`")?;
    if !status.success() {
        return Err(anyhow!(
            "wrangler secret bulk failed with status: {}",
            status
        ));
    }
    Ok(())
}

/// Expand $VAR and ${VAR} references in a string using provided environment variables.
/// Only expands variables that exist in the provided map; others are left as-is
/// (e.g., $HOME, $PATH).
fn expand_vars(s: &str, env_vars: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(s.len() * 2);
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            // Try to parse ${VAR} or $VAR
            let mut var_name = String::new();
            let mut is_braced = false;

            if chars.peek() == Some(&'{') {
                is_braced = true;
                chars.next(); // consume '{'
            }

            // Collect variable name (ASCII alphanumeric + underscore only)
            // This matches shell variable naming rules
            while let Some(&next) = chars.peek() {
                match next {
                    'a'..='z' | 'A'..='Z' | '0'..='9' | '_' => {
                        var_name.push(chars.next().unwrap());
                    }
                    _ => break,
                }
            }

            if is_braced {
                if chars.peek() == Some(&'}') {
                    chars.next(); // consume '}'
                } else {
                    // Invalid ${ syntax, treat as literal
                    result.push_str("${");
                    result.push_str(&var_name);
                    continue;
                }
            }

            // Look up the variable and replace, or keep original literal form
            if let Some(value) = env_vars.get(&var_name) {
                result.push_str(value);
            } else {
                // Variable not found in our env, keep $VAR as-is
                result.push('$');
                result.push_str(&var_name);
            }
        } else {
            result.push(c);
        }
    }

    result
}

fn run_with_items(
    cli: &Cli,
    items: &[String],
    env_file: Option<&Path>,
    command: &[String],
) -> Result<()> {
    let sections = instrumentation::with_span_result(
        "load_inputs",
        vec![KeyValue::new("item.count", items.len() as i64)],
        || collect_item_env_sections(cli, items),
    )?;
    let merged_env_lines =
        instrumentation::with_span("main_operation", vec![], || merge_env_lines(&sections));

    instrumentation::with_span_result(
        "write_outputs",
        vec![
            KeyValue::new(
                "cli.output_path",
                env_file
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "-".to_string()),
            ),
            KeyValue::new("cli.command_arg_count", command.len() as i64),
        ],
        || {
            if let Some(path) = env_file {
                write_env_file(path, &merged_env_lines)?;
                eprintln!("Generated: {}", path.display());
            }
            Ok(())
        },
    )?;

    // First pass: collect all environment variable values
    let env_vars = instrumentation::with_span_result("load_inputs", vec![], || {
        resolve_env_vars(&merged_env_lines)
    })?;

    // Second pass: expand $VAR references in command arguments
    let expanded_args: Vec<String> = instrumentation::with_span("main_operation", vec![], || {
        command
            .iter()
            .map(|arg| expand_vars(arg, &env_vars))
            .collect()
    });

    instrumentation::with_span_result("write_outputs.command_exec", vec![], || {
        #[cfg(unix)]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.arg("-c");
            c.arg("exec \"$@\"");
            c.arg("sh");
            c.args(&expanded_args);
            c
        };

        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new(&expanded_args[0]);
            c.args(&expanded_args[1..]);
            c
        };

        // Set environment variables for the child process
        for (key, value) in &env_vars {
            cmd.env(key, value);
        }

        let status = cmd
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("failed to run command")?;

        if !status.success() {
            return Err(anyhow!("command failed with status: {}", status));
        }
        Ok(())
    })
}

fn run_with_environments(
    vault: Option<&str>,
    environments: &[String],
    items: &[String],
    env_file: Option<&Path>,
    command: &[String],
) -> Result<()> {
    if let Some(vault) = vault {
        return Err(anyhow!(
            "`--vault {vault}` cannot be combined with `--environment` because 1Password Environments are not vault item lookups. Remove `--vault` or use item-backed `opz run <ITEM> -- <COMMAND>`."
        ));
    }
    if !items.is_empty() {
        return Err(anyhow!(
            "`--environment` cannot be combined with item arguments in v1. Use either `opz run --environment <ENV> -- <COMMAND>` or item-backed `opz run <ITEM> -- <COMMAND>`."
        ));
    }
    if let Some(path) = env_file {
        return Err(anyhow!(
            "`--environment` cannot be combined with `--env-file` in v1 because Environment execution is delegated to `op run`. Remove `--env-file` or use item-backed `opz gen --env-file {}`.",
            path.display()
        ));
    }

    let flag = detect_op_run_environment_flag()?;
    let args = build_op_run_environment_args(flag, environments, command);

    instrumentation::with_span_result("write_outputs.command_exec", vec![], || {
        let status = Command::new("op")
            .args(&args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("failed to run `op run` with 1Password Environment")?;

        if !status.success() {
            return Err(anyhow!("op run failed with status: {}", status));
        }
        Ok(())
    })
}

fn detect_op_run_environment_flag() -> Result<&'static str> {
    let mut cmd = Command::new("op");
    cmd.arg("run").arg("--help");
    cmd.stdin(Stdio::null());
    let out = command_output_with_timeout(cmd, "`op run --help`", op_command_timeout())?;
    if !out.status.success() {
        return Err(anyhow!(
            "failed to inspect `op run --help`: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    let help = String::from_utf8_lossy(&out.stdout);
    match detect_environment_flag_from_help(&help) {
        Some(flag) => Ok(flag),
        None => Err(anyhow!(
            "This 1Password CLI does not expose Environment runtime injection in `op run --help`. Use item-backed `opz run <ITEM> -- <COMMAND>`, or use the 1Password MCP server to mount a local .env file for this project."
        )),
    }
}

fn detect_environment_flag_from_help(help: &str) -> Option<&'static str> {
    if help.contains("--environments") {
        Some("--environments")
    } else if help.contains("--environment") {
        Some("--environment")
    } else {
        None
    }
}

fn build_op_run_environment_args(
    flag: &str,
    environments: &[String],
    command: &[String],
) -> Vec<String> {
    let mut args = Vec::with_capacity(environments.len() * 2 + command.len() + 2);
    args.push("run".to_string());
    for environment in environments {
        args.push(flag.to_string());
        args.push(environment.clone());
    }
    args.push("--".to_string());
    args.extend(command.iter().cloned());
    args
}

fn item_to_env_lines(item: &ItemGet, vault_id: &str, item_id: &str) -> Result<Vec<String>> {
    let labels = collect_item_labels(item)?;
    let mut out = Vec::new();

    for label in labels {
        let reference = format!("op://{}/{}/{}", vault_id, item_id, label);
        out.push(format!("{k}={v}", k = label, v = reference));
    }

    Ok(out)
}

fn item_github_repositories(item: &ItemGet) -> Vec<String> {
    let mut repositories = Vec::new();
    for field in &item.fields {
        let Some(label) = field.label.as_deref() else {
            continue;
        };
        if !label.eq_ignore_ascii_case(GITHUB_REPOSITORIES_LABEL) {
            continue;
        }
        let Some(value) = item_field_string_value(field) else {
            continue;
        };
        repositories.extend(parse_github_repositories_value(&value));
    }

    let mut seen = HashSet::new();
    repositories
        .into_iter()
        .filter(|repo| seen.insert(repo.to_ascii_lowercase()))
        .collect()
}

fn item_field_string_value(field: &ItemField) -> Option<String> {
    match field.value.as_ref()? {
        serde_json::Value::String(value) => Some(value.clone()),
        value => value.as_str().map(str::to_string),
    }
}

fn parse_github_repositories_value(value: &str) -> Vec<String> {
    value
        .split([',', '\n', '\r', '\t'])
        .filter_map(normalize_github_repo_spec)
        .collect()
}

fn normalize_github_repo_spec(value: &str) -> Option<String> {
    let mut text = value.trim();
    if text.is_empty() {
        return None;
    }

    text = text.trim_end_matches('/');
    let path = if let Some((_, rest)) = text.split_once("://") {
        let (_, path_part) = rest.split_once('/')?;
        path_part
    } else if text.contains('@') && text.contains(':') {
        let (_, path_part) = text.split_once(':')?;
        path_part
    } else {
        text
    };

    let normalized = path
        .split(['?', '#'])
        .next()?
        .trim_matches('/')
        .trim_end_matches(".git");
    let segments: Vec<&str> = normalized
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() < 2 {
        return None;
    }

    let owner = segments[segments.len() - 2].to_ascii_lowercase();
    let repo = segments[segments.len() - 1].to_ascii_lowercase();
    Some(format!("{owner}/{repo}"))
}

fn collect_item_labels(item: &ItemGet) -> Result<Vec<String>> {
    let re = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$")?;
    let mut labels = Vec::new();

    for f in &item.fields {
        let Some(label) = f.label.as_ref() else {
            continue;
        };
        if is_metadata_label(label) {
            continue;
        }
        if !re.is_match(label) || f.value.is_none() {
            continue;
        }
        labels.push(label.clone());
    }

    Ok(labels)
}

fn item_to_valid_labels(item: &ItemGet) -> Result<Vec<String>> {
    let re = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$")?;
    let mut out = Vec::new();

    for f in &item.fields {
        let Some(label) = f.label.as_ref() else {
            continue;
        };
        if is_metadata_label(label) {
            continue;
        }
        if !re.is_match(label) {
            continue;
        }
        out.push(label.clone());
    }

    Ok(out)
}

fn is_metadata_label(label: &str) -> bool {
    label.eq_ignore_ascii_case(GITHUB_REPOSITORIES_LABEL)
}

/// Parse env line to extract key name (e.g., "KEY=value" -> "KEY")
fn parse_env_key(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    trimmed.split('=').next()
}

/// Parse env line to extract key and value (e.g., "KEY=value" -> ("KEY", "value"))
fn parse_env_line_kv(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    trimmed.split_once('=')
}

/// Read a secret from 1Password using op read
fn op_read(reference: &str) -> Result<String> {
    instrumentation::with_span_result("load_inputs.op_read", vec![], || {
        let mut cmd = Command::new("op");
        cmd.arg("read").arg(reference);
        cmd.stdin(Stdio::null());
        let out = command_output_with_timeout(cmd, "`op read`", op_command_timeout())?;

        if !out.status.success() {
            return Err(anyhow!(
                "op read failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }

        Ok(String::from_utf8(out.stdout)?.trim().to_string())
    })
}

fn write_env_file(path: &Path, new_lines: &[String]) -> Result<()> {
    instrumentation::with_span_result(
        "write_outputs.write_env_file",
        vec![
            KeyValue::new("cli.output_path", path.display().to_string()),
            KeyValue::new("env.line_count", new_lines.len() as i64),
        ],
        || {
            use std::collections::HashMap;

            // Build a map of new keys for quick lookup
            let new_keys: HashMap<String, &str> = new_lines
                .iter()
                .filter_map(|line| parse_env_key(line).map(|key| (key.to_string(), line.as_str())))
                .collect();

            let mut result_lines: Vec<String> = Vec::new();
            let mut written_keys: std::collections::HashSet<String> =
                std::collections::HashSet::new();

            // Read existing file and merge
            if path.exists() {
                let content =
                    fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;

                for line in content.lines() {
                    if let Some(key) = parse_env_key(line) {
                        if let Some(&new_line) = new_keys.get(key) {
                            // Overwrite with new value
                            result_lines.push(new_line.to_string());
                            written_keys.insert(key.to_string());
                        } else {
                            // Keep existing line
                            result_lines.push(line.to_string());
                        }
                    } else {
                        // Comment or empty line - keep as is
                        result_lines.push(line.to_string());
                    }
                }
            }

            // Append new keys that weren't already in the file
            for line in new_lines {
                if let Some(key) = parse_env_key(line) {
                    if !written_keys.contains(key) {
                        result_lines.push(line.clone());
                    }
                }
            }

            // Write result
            let mut f =
                fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
            for line in &result_lines {
                writeln!(f, "{line}")?;
            }
            Ok(())
        },
    )
}

fn op_json(args: &[&str]) -> Result<serde_json::Value> {
    let operation = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
    instrumentation::with_span_result(
        "load_inputs.op_json",
        vec![KeyValue::new("op.operation", operation)],
        || {
            let mut cmd = Command::new("op");
            cmd.args(args);
            cmd.stdin(Stdio::null());
            let display = format!("`op {}`", args.join(" "));
            let out = command_output_with_timeout(cmd, &display, op_command_timeout())?;

            if !out.status.success() {
                return Err(anyhow!(
                    "op error ({}): {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr)
                ));
            }

            let v: serde_json::Value =
                serde_json::from_slice(&out.stdout).context("failed to parse op JSON output")?;
            Ok(v)
        },
    )
}

fn op_command_timeout() -> Duration {
    env::var("OPZ_OP_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(30))
}

fn command_output_with_timeout(
    mut command: Command,
    display: &str,
    timeout: Duration,
) -> Result<Output> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run {display}"))?;
    let started = Instant::now();

    loop {
        if child
            .try_wait()
            .with_context(|| format!("failed to poll {display}"))?
            .is_some()
        {
            return child
                .wait_with_output()
                .with_context(|| format!("failed to wait for {display}"));
        }

        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait_with_output();
            return Err(anyhow!(
                "{display} timed out after {} seconds. Check 1Password CLI authentication with `op whoami`, or set OPZ_OP_TIMEOUT_SECONDS to a larger value.",
                timeout.as_secs()
            ));
        }

        thread::sleep(Duration::from_millis(50));
    }
}

/// Cache `op item list --format json` to speed up repeated runs.
fn item_list_cached(vault: Option<&str>) -> Result<Vec<ItemListEntry>> {
    instrumentation::with_span_result(
        "load_inputs.item_list_cached",
        vec![KeyValue::new("vault.specified", vault.is_some())],
        || {
            let cache_path = cache_file_path(vault)?;
            let ttl = Duration::from_secs(60); // 60秒程度で十分（好みで調整）

            if let Ok(meta) = fs::metadata(&cache_path) {
                if let Ok(mtime) = meta.modified() {
                    if SystemTime::now().duration_since(mtime).unwrap_or_default() < ttl {
                        return instrumentation::with_span_result(
                            "load_inputs.item_list_cache_read",
                            vec![KeyValue::new(
                                "cache.path",
                                cache_path.display().to_string(),
                            )],
                            || {
                                let bytes = fs::read(&cache_path)?;
                                let items: Vec<ItemListEntry> = serde_json::from_slice(&bytes)?;
                                Ok(items)
                            },
                        );
                    }
                }
            }

            let mut args = vec!["item", "list", "--format", "json"];
            if let Some(v) = vault {
                // `op item list --vault <name>` が使える環境想定（未対応なら削る）
                args.push("--vault");
                args.push(v);
            }

            let items =
                instrumentation::with_span_result("load_inputs.item_list_fetch", vec![], || {
                    let v = op_json(&args)?;
                    let items: Vec<ItemListEntry> = serde_json::from_value(v)?;
                    Ok(items)
                })?;
            instrumentation::with_span_result(
                "load_inputs.item_list_cache_write",
                vec![KeyValue::new(
                    "cache.path",
                    cache_path.display().to_string(),
                )],
                || {
                    let cache_parent = cache_path.parent().ok_or_else(|| {
                        anyhow!(
                            "cache path has no parent directory: {}",
                            cache_path.display()
                        )
                    })?;
                    fs::create_dir_all(cache_parent)?;
                    fs::write(&cache_path, serde_json::to_vec(&items)?)?;
                    Ok(())
                },
            )?;
            Ok(items)
        },
    )
}

fn item_github_repository_index_cached(vault: Option<&str>) -> Result<Vec<(String, Vec<String>)>> {
    instrumentation::with_span_result(
        "load_inputs.item_github_repository_index_cached",
        vec![KeyValue::new("vault.specified", vault.is_some())],
        || {
            let cache_path = github_repository_index_cache_file_path(vault)?;
            let ttl = Duration::from_secs(60);

            if let Ok(meta) = fs::metadata(&cache_path) {
                if let Ok(mtime) = meta.modified() {
                    if SystemTime::now().duration_since(mtime).unwrap_or_default() < ttl {
                        let bytes = fs::read(&cache_path)?;
                        let items: Vec<ItemGithubRepositories> = serde_json::from_slice(&bytes)?;
                        return Ok(items
                            .into_iter()
                            .map(|item| (item.item_title, item.repositories))
                            .collect());
                    }
                }
            }

            let item_entries = item_list_cached(vault)?;
            let mut index = Vec::new();
            for entry in item_entries {
                let item = item_get(&entry.id).with_context(|| {
                    format!("failed to inspect item `{}` for auto-detect", entry.title)
                })?;
                let repositories = item_github_repositories(&item);
                if !repositories.is_empty() {
                    index.push(ItemGithubRepositories {
                        item_title: entry.title,
                        repositories,
                    });
                }
            }

            let cache_parent = cache_path.parent().ok_or_else(|| {
                anyhow!(
                    "cache path has no parent directory: {}",
                    cache_path.display()
                )
            })?;
            fs::create_dir_all(cache_parent)?;
            fs::write(&cache_path, serde_json::to_vec(&index)?)?;

            Ok(index
                .into_iter()
                .map(|item| (item.item_title, item.repositories))
                .collect())
        },
    )
}

struct TempEnvFile {
    path: PathBuf,
    file: fs::File,
}

impl TempEnvFile {
    fn create() -> Result<Self> {
        let dir = env::temp_dir();
        for attempt in 0..100 {
            let path = dir.join(format!(
                "opz-env-{}-{}-{attempt}.env",
                std::process::id(),
                stable_hex_hash(&format!("{:?}", SystemTime::now()))
            ));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => return Ok(Self { path, file }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err).with_context(|| format!("create {}", path.display())),
            }
        }

        Err(anyhow!("failed to create unique temp env file"))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Write for TempEnvFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl Drop for TempEnvFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Clone, Copy)]
enum CachePlatform {
    Windows,
    Macos,
    Other,
}

fn item_list_cache_dir() -> Result<PathBuf> {
    let platform = if cfg!(target_os = "windows") {
        CachePlatform::Windows
    } else if cfg!(target_os = "macos") {
        CachePlatform::Macos
    } else {
        CachePlatform::Other
    };
    item_list_cache_dir_from_env(platform, |key| env::var_os(key))
}

fn item_list_cache_dir_from_env(
    platform: CachePlatform,
    mut var: impl FnMut(&str) -> Option<OsString>,
) -> Result<PathBuf> {
    if let Some(cache_home) = var("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(cache_home).join("opz"));
    }

    if matches!(platform, CachePlatform::Windows) {
        if let Some(local_app_data) = var("LOCALAPPDATA").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(local_app_data).join("opz"));
        }
        if let Some(app_data) = var("APPDATA").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(app_data).join("opz"));
        }
        if let Some(profile) = var("USERPROFILE").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(profile)
                .join("AppData")
                .join("Local")
                .join("opz"));
        }
    }

    let home = var("HOME").ok_or_else(|| anyhow!("no cache dir"))?;
    let home = PathBuf::from(home);
    if matches!(platform, CachePlatform::Macos) {
        Ok(home.join("Library").join("Caches").join("dev.opz.opz"))
    } else {
        Ok(home.join(".cache").join("opz"))
    }
}

fn cache_file_path(vault: Option<&str>) -> Result<PathBuf> {
    let base = item_list_cache_dir()?;
    let key = vault.unwrap_or("_all_");
    let name = format!("item_list_{}.json", stable_hex_hash(key));
    Ok(base.join(name))
}

fn github_repository_index_cache_file_path(vault: Option<&str>) -> Result<PathBuf> {
    let base = item_list_cache_dir()?;
    let key = format!("github_repositories:{}", vault.unwrap_or("_all_"));
    let name = format!("github_repository_index_{}.json", stable_hex_hash(&key));
    Ok(base.join(name))
}

fn stable_hex_hash(input: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn invalidate_item_list_cache() -> Result<()> {
    let cache_dir = item_list_cache_dir()?;
    if !cache_dir.exists() {
        return Ok(());
    }

    for entry in
        fs::read_dir(&cache_dir).with_context(|| format!("read {}", cache_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if (name.starts_with("item_list_") || name.starts_with("github_repository_index_"))
            && name.ends_with(".json")
        {
            fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        }
    }

    Ok(())
}

fn invalidate_item_list_cache_best_effort() {
    if let Err(err) = invalidate_item_list_cache() {
        eprintln!("Warning: failed to invalidate item list cache: {err}");
    }
}

fn item_get(item_id: &str) -> Result<ItemGet> {
    instrumentation::with_span_result("load_inputs.item_get", vec![], || {
        let v = op_json(&["item", "get", item_id, "--format", "json"])?;
        let item: ItemGet = serde_json::from_value(v)?;
        Ok(item)
    })
}

fn item_get_with_vault(vault: Option<&str>, item: &str) -> Result<ItemGet> {
    instrumentation::with_span_result("load_inputs.item_get_with_vault", vec![], || {
        let mut args = vec!["item", "get", item, "--format", "json"];
        if let Some(vault) = vault {
            args.push("--vault");
            args.push(vault);
        }
        let v = op_json(&args)?;
        let item: ItemGet = serde_json::from_value(v)?;
        Ok(item)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::fs;
    use tempfile::TempDir;

    struct FakeMcpClient {
        responses: VecDeque<serde_json::Value>,
        calls: Vec<(String, serde_json::Value)>,
    }

    impl FakeMcpClient {
        fn new(responses: Vec<serde_json::Value>) -> Self {
            Self {
                responses: VecDeque::from(responses),
                calls: Vec::new(),
            }
        }
    }

    impl OnePasswordMcp for FakeMcpClient {
        fn call_tool(
            &mut self,
            name: &str,
            arguments: serde_json::Value,
        ) -> Result<serde_json::Value> {
            self.calls.push((name.to_string(), arguments));
            self.responses
                .pop_front()
                .ok_or_else(|| anyhow!("unexpected MCP call `{name}`"))
        }
    }

    // ============================================
    // Tests for item_to_env_lines()
    // ============================================

    fn make_field(label: Option<&str>, has_value: bool) -> ItemField {
        ItemField {
            label: label.map(String::from),
            value: if has_value {
                Some(serde_json::Value::String("test".to_string()))
            } else {
                None
            },
        }
    }

    fn make_item(fields: Vec<ItemField>) -> ItemGet {
        ItemGet {
            id: None,
            title: None,
            fields,
            vault: None,
        }
    }

    fn env_lines(item: &ItemGet) -> Vec<String> {
        item_to_env_lines(item, "vault-id", "abc123").unwrap()
    }

    fn valid_labels(item: &ItemGet) -> Vec<String> {
        item_to_valid_labels(item).unwrap()
    }

    #[test]
    fn test_collect_item_labels_matches_env_key_rules() {
        let item = make_item(vec![
            make_field(Some("API_KEY"), true),
            make_field(Some("invalid-key"), true),
            make_field(Some("NO_VALUE"), false),
            make_field(None, true),
            make_field(Some("DB_HOST"), true),
            make_field(Some(GITHUB_REPOSITORIES_LABEL), true),
        ]);

        let labels = collect_item_labels(&item).unwrap();
        assert_eq!(labels, vec!["API_KEY".to_string(), "DB_HOST".to_string()]);
    }

    #[test]
    fn test_item_to_env_lines_basic() {
        let item = make_item(vec![
            make_field(Some("API_KEY"), true),
            make_field(Some("DB_HOST"), true),
        ]);
        let lines = env_lines(&item);
        assert_eq!(lines.len(), 2);
        assert!(lines.contains(&"API_KEY=op://vault-id/abc123/API_KEY".to_string()));
        assert!(lines.contains(&"DB_HOST=op://vault-id/abc123/DB_HOST".to_string()));
    }

    #[test]
    fn test_item_to_env_lines_skips_invalid_labels() {
        let item = make_item(vec![
            make_field(Some("VALID_KEY"), true),
            make_field(Some("invalid-key"), true), // dash not allowed
            make_field(Some("123_START"), true),   // can't start with number
            make_field(Some("has space"), true),   // space not allowed
        ]);
        let lines = env_lines(&item);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "VALID_KEY=op://vault-id/abc123/VALID_KEY");
    }

    #[test]
    fn test_item_to_env_lines_valid_label_patterns() {
        let item = make_item(vec![
            make_field(Some("_UNDERSCORE_START"), true),
            make_field(Some("lowercase"), true),
            make_field(Some("MixedCase123"), true),
            make_field(Some("WITH_123_NUMBERS"), true),
        ]);
        let lines = env_lines(&item);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn test_item_to_env_lines_skips_no_label() {
        let item = make_item(vec![
            make_field(None, true),
            make_field(Some("VALID"), true),
        ]);
        let lines = env_lines(&item);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "VALID=op://vault-id/abc123/VALID");
    }

    #[test]
    fn test_item_to_env_lines_empty_fields() {
        let item = make_item(vec![]);
        let lines = env_lines(&item);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_item_to_env_lines_skips_no_value() {
        let item = make_item(vec![
            make_field(Some("NO_VALUE"), false),
            make_field(Some("HAS_VALUE"), true),
        ]);
        let lines = env_lines(&item);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "HAS_VALUE=op://vault-id/abc123/HAS_VALUE");
    }

    #[test]
    fn test_item_to_valid_labels_skips_invalid_and_missing() {
        let item = make_item(vec![
            make_field(Some("VALID_KEY"), false),
            make_field(Some("invalid-key"), true),
            make_field(None, true),
            make_field(Some(GITHUB_REPOSITORIES_LABEL), true),
        ]);
        let labels = valid_labels(&item);
        assert_eq!(labels, vec!["VALID_KEY".to_string()]);
    }

    #[test]
    fn test_item_github_repositories_parses_metadata_field() {
        let item = make_item(vec![ItemField {
            label: Some(GITHUB_REPOSITORIES_LABEL.to_string()),
            value: Some(serde_json::Value::String(
                "Owner/Repo\nhttps://github.com/Other/Service.git, git@github.com:Org/App.git"
                    .to_string(),
            )),
        }]);

        let repos = item_github_repositories(&item);
        assert_eq!(
            repos,
            vec![
                "owner/repo".to_string(),
                "other/service".to_string(),
                "org/app".to_string()
            ]
        );
    }

    #[test]
    fn test_normalize_github_repo_spec_accepts_urls_and_owner_repo() {
        assert_eq!(
            normalize_github_repo_spec("Owner/Repo"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            normalize_github_repo_spec("https://github.com/Owner/Repo.git"),
            Some("owner/repo".to_string())
        );
        assert_eq!(
            normalize_github_repo_spec("git@github.com:Owner/Repo.git"),
            Some("owner/repo".to_string())
        );
        assert_eq!(normalize_github_repo_spec("not-a-repo"), None);
    }

    #[test]
    fn test_match_item_titles_by_github_repositories_matches_one() {
        let candidates = vec![
            ("service".to_string(), vec!["owner/repo".to_string()]),
            ("other".to_string(), vec!["other/repo".to_string()]),
        ];

        let matches =
            match_item_titles_by_github_repositories(&candidates, &["OWNER/REPO".to_string()]);

        assert_eq!(matches, vec!["service".to_string()]);
    }

    #[test]
    fn test_match_item_titles_by_github_repositories_matches_none() {
        let candidates = vec![("service".to_string(), vec!["owner/repo".to_string()])];

        let matches =
            match_item_titles_by_github_repositories(&candidates, &["other/repo".to_string()]);

        assert!(matches.is_empty());
    }

    #[test]
    fn test_match_item_titles_by_github_repositories_preserves_multiple_matches() {
        let candidates = vec![
            ("service".to_string(), vec!["owner/repo".to_string()]),
            ("shared".to_string(), vec!["owner/repo".to_string()]),
        ];

        let matches =
            match_item_titles_by_github_repositories(&candidates, &["owner/repo".to_string()]);

        assert_eq!(matches, vec!["service".to_string(), "shared".to_string()]);
    }

    #[test]
    fn test_resolve_vault_id_prefers_id_even_with_unicode_name() {
        let list_vault = ItemVault {
            id: "vault-123".to_string(),
            name: "情報管理共有".to_string(),
        };
        let item_vault = ItemVault {
            id: "vault-fallback".to_string(),
            name: "別名".to_string(),
        };

        let resolved = resolve_vault_id(Some(&list_vault), Some(&item_vault));
        assert_eq!(resolved.as_deref(), Some("vault-123"));
    }

    // ============================================
    // Tests for parse_env_key()
    // ============================================

    #[test]
    fn test_parse_env_key_basic() {
        assert_eq!(parse_env_key("KEY=value"), Some("KEY"));
        assert_eq!(parse_env_key("FOO_BAR=baz"), Some("FOO_BAR"));
    }

    #[test]
    fn test_parse_env_key_with_quotes() {
        assert_eq!(parse_env_key(r#"KEY="value""#), Some("KEY"));
    }

    #[test]
    fn test_parse_env_key_comments_and_empty() {
        assert_eq!(parse_env_key("# comment"), None);
        assert_eq!(parse_env_key(""), None);
        assert_eq!(parse_env_key("   "), None);
        assert_eq!(parse_env_key("  # indented comment"), None);
    }

    // ============================================
    // Tests for write_env_file()
    // ============================================

    #[test]
    fn test_write_env_file_creates_file() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join(".env");

        let lines = vec![
            r#"KEY1="value1""#.to_string(),
            r#"KEY2="value2""#.to_string(),
        ];

        write_env_file(&file_path, &lines).unwrap();

        assert!(file_path.exists());
        let content = fs::read_to_string(&file_path).unwrap();
        assert!(content.contains(r#"KEY1="value1""#));
        assert!(content.contains(r#"KEY2="value2""#));
    }

    #[test]
    fn test_write_env_file_with_newlines() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join(".env");

        let lines = vec![r#"MULTI="line1\nline2""#.to_string()];

        write_env_file(&file_path, &lines).unwrap();

        let content = fs::read_to_string(&file_path).unwrap();
        assert!(content.contains(r#"MULTI="line1\nline2""#));
    }

    #[test]
    fn test_write_env_file_empty_lines() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join(".env");

        let lines: Vec<String> = vec![];
        write_env_file(&file_path, &lines).unwrap();

        let content = fs::read_to_string(&file_path).unwrap();
        assert!(content.is_empty());
    }

    #[test]
    fn test_write_env_file_appends_new_keys() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join(".env");

        // Write initial content
        fs::write(&file_path, "OLD_KEY=old_value\n").unwrap();

        // Append with new content
        let lines = vec![r#"NEW_KEY="new_value""#.to_string()];
        write_env_file(&file_path, &lines).unwrap();

        let content = fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("OLD_KEY=old_value"));
        assert!(content.contains(r#"NEW_KEY="new_value""#));
    }

    #[test]
    fn test_write_env_file_overwrites_duplicates() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join(".env");

        // Write initial content with a key we'll overwrite
        fs::write(&file_path, "API_KEY=old_secret\nOTHER_KEY=keep_me\n").unwrap();

        // Overwrite API_KEY
        let lines = vec![r#"API_KEY="new_secret""#.to_string()];
        write_env_file(&file_path, &lines).unwrap();

        let content = fs::read_to_string(&file_path).unwrap();
        // Should have new value, not old
        assert!(content.contains(r#"API_KEY="new_secret""#));
        assert!(!content.contains("API_KEY=old_secret"));
        // Other key should be preserved
        assert!(content.contains("OTHER_KEY=keep_me"));
    }

    #[test]
    fn test_write_env_file_preserves_comments() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join(".env");

        // Write initial content with comments
        fs::write(
            &file_path,
            "# This is a comment\nKEY1=value1\n\n# Another comment\n",
        )
        .unwrap();

        // Add new key
        let lines = vec![r#"KEY2="value2""#.to_string()];
        write_env_file(&file_path, &lines).unwrap();

        let content = fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("# This is a comment"));
        assert!(content.contains("# Another comment"));
        assert!(content.contains("KEY1=value1"));
        assert!(content.contains(r#"KEY2="value2""#));
    }

    #[test]
    fn test_write_env_file_mixed_overwrite_and_append() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join(".env");

        // Initial content
        fs::write(&file_path, "KEY1=original1\nKEY2=original2\n").unwrap();

        // Overwrite KEY1 and add KEY3
        let lines = vec![
            r#"KEY1="updated1""#.to_string(),
            r#"KEY3="new3""#.to_string(),
        ];
        write_env_file(&file_path, &lines).unwrap();

        let content = fs::read_to_string(&file_path).unwrap();
        let content_lines: Vec<&str> = content.lines().collect();

        // KEY1 should be updated (in its original position)
        assert!(content_lines[0].contains(r#"KEY1="updated1""#));
        // KEY2 should be preserved
        assert!(content_lines[1].contains("KEY2=original2"));
        // KEY3 should be appended
        assert!(content_lines[2].contains(r#"KEY3="new3""#));
    }

    // ============================================
    // Tests for cache_file_path()
    // ============================================

    #[test]
    fn test_cache_file_path_with_vault() {
        let path1 = cache_file_path(Some("my-vault")).unwrap();
        let path2 = cache_file_path(Some("other-vault")).unwrap();

        // Different vaults should produce different paths
        assert_ne!(path1, path2);

        // Path should end with .json
        assert!(path1.extension().unwrap() == "json");
        assert!(path2.extension().unwrap() == "json");

        // Filename should start with item_list_
        let name1 = path1.file_name().unwrap().to_str().unwrap();
        assert!(name1.starts_with("item_list_"));
    }

    #[test]
    fn test_cache_file_path_without_vault() {
        let path = cache_file_path(None).unwrap();

        // Should produce a valid path
        assert!(path.extension().unwrap() == "json");

        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("item_list_"));
    }

    #[test]
    fn test_cache_file_path_deterministic() {
        // Same input should produce same output
        let path1 = cache_file_path(Some("test-vault")).unwrap();
        let path2 = cache_file_path(Some("test-vault")).unwrap();
        assert_eq!(path1, path2);

        let path3 = cache_file_path(None).unwrap();
        let path4 = cache_file_path(None).unwrap();
        assert_eq!(path3, path4);
    }

    #[test]
    fn test_stable_hex_hash_has_fixed_values() {
        assert_eq!(stable_hex_hash(""), "cbf29ce484222325");
        assert_eq!(stable_hex_hash("_all_"), "3e0d70f177ee4766");
        assert_eq!(stable_hex_hash("test-vault"), "18e1186c195d50bc");
        assert_eq!(
            stable_hex_hash("github_repositories:_all_"),
            "63aa79d753dde420"
        );
    }

    #[test]
    fn test_temp_env_file_writes_and_cleans_up() {
        let path = {
            let mut temp_env = TempEnvFile::create().unwrap();
            let path = temp_env.path().to_path_buf();
            writeln!(temp_env, "API_KEY=op://vault/item/API_KEY").unwrap();
            temp_env.flush().unwrap();

            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                "API_KEY=op://vault/item/API_KEY\n"
            );
            path
        };

        assert!(!path.exists(), "temp env file should be removed on drop");
    }

    #[test]
    fn test_item_list_cache_dir_prefers_xdg_cache_home() {
        let dir = item_list_cache_dir_from_env(CachePlatform::Other, |key| match key {
            "XDG_CACHE_HOME" => Some(OsString::from("/tmp/xdg-cache")),
            "HOME" => Some(OsString::from("/home/user")),
            _ => None,
        })
        .unwrap();

        assert_eq!(dir, PathBuf::from("/tmp/xdg-cache").join("opz"));
    }

    #[test]
    fn test_item_list_cache_dir_uses_macos_cache_path() {
        let dir = item_list_cache_dir_from_env(CachePlatform::Macos, |key| match key {
            "HOME" => Some(OsString::from("/Users/alice")),
            _ => None,
        })
        .unwrap();

        assert_eq!(
            dir,
            PathBuf::from("/Users/alice")
                .join("Library")
                .join("Caches")
                .join("dev.opz.opz")
        );
    }

    #[test]
    fn test_item_list_cache_dir_uses_linux_home_cache_path() {
        let dir = item_list_cache_dir_from_env(CachePlatform::Other, |key| match key {
            "HOME" => Some(OsString::from("/home/alice")),
            _ => None,
        })
        .unwrap();

        assert_eq!(dir, PathBuf::from("/home/alice").join(".cache").join("opz"));
    }

    #[test]
    fn test_item_list_cache_dir_uses_windows_local_app_data() {
        let dir = item_list_cache_dir_from_env(CachePlatform::Windows, |key| match key {
            "LOCALAPPDATA" => Some(OsString::from(r"C:\Users\alice\AppData\Local")),
            "APPDATA" => Some(OsString::from(r"C:\Users\alice\AppData\Roaming")),
            "USERPROFILE" => Some(OsString::from(r"C:\Users\alice")),
            _ => None,
        })
        .unwrap();

        assert_eq!(
            dir,
            PathBuf::from(r"C:\Users\alice\AppData\Local").join("opz")
        );
    }

    #[test]
    fn test_item_list_cache_dir_uses_windows_app_data_fallback() {
        let dir = item_list_cache_dir_from_env(CachePlatform::Windows, |key| match key {
            "APPDATA" => Some(OsString::from(r"C:\Users\alice\AppData\Roaming")),
            "USERPROFILE" => Some(OsString::from(r"C:\Users\alice")),
            _ => None,
        })
        .unwrap();

        assert_eq!(
            dir,
            PathBuf::from(r"C:\Users\alice\AppData\Roaming").join("opz")
        );
    }

    #[test]
    fn test_item_list_cache_dir_uses_windows_userprofile_fallback() {
        let dir = item_list_cache_dir_from_env(CachePlatform::Windows, |key| match key {
            "USERPROFILE" => Some(OsString::from(r"C:\Users\alice")),
            _ => None,
        })
        .unwrap();

        assert_eq!(
            dir,
            PathBuf::from(r"C:\Users\alice")
                .join("AppData")
                .join("Local")
                .join("opz")
        );
    }

    #[test]
    fn test_item_list_cache_dir_errors_without_home_like_env() {
        let err = item_list_cache_dir_from_env(CachePlatform::Other, |_| None).unwrap_err();
        assert_eq!(err.to_string(), "no cache dir");
    }

    // ============================================
    // Tests for ItemListEntry and ItemGet deserialization
    // ============================================

    #[test]
    fn test_item_list_entry_deserialization() {
        let json =
            r#"{"id": "abc123", "title": "My Item", "vault": {"id": "v1", "name": "Personal"}}"#;
        let item: ItemListEntry = serde_json::from_str(json).unwrap();
        assert_eq!(item.id, "abc123");
        assert_eq!(item.title, "My Item");
        assert!(item.vault.is_some());
        assert_eq!(item.vault.as_ref().unwrap().name, "Personal");
    }

    #[test]
    fn test_item_list_entry_without_vault() {
        let json = r#"{"id": "abc123", "title": "My Item"}"#;
        let item: ItemListEntry = serde_json::from_str(json).unwrap();
        assert_eq!(item.id, "abc123");
        assert_eq!(item.title, "My Item");
        assert!(item.vault.is_none());
    }

    #[test]
    fn test_item_get_deserialization() {
        let json = r#"{
            "fields": [
                {"label": "username", "value": "user@example.com"},
                {"label": "password", "value": "secret"}
            ]
        }"#;
        let item: ItemGet = serde_json::from_str(json).unwrap();
        assert_eq!(item.fields.len(), 2);
        assert_eq!(item.fields[0].label, Some("username".to_string()));
    }

    #[test]
    fn test_item_get_empty_fields() {
        let json = r#"{}"#;
        let item: ItemGet = serde_json::from_str(json).unwrap();
        assert!(item.fields.is_empty());
    }

    #[test]
    fn test_item_field_with_null_value() {
        // Unknown fields (like "value") are ignored during deserialization
        let json = r#"{"label": "empty_field", "value": null}"#;
        let field: ItemField = serde_json::from_str(json).unwrap();
        assert_eq!(field.label, Some("empty_field".to_string()));
    }

    #[test]
    fn test_item_field_missing_value() {
        let json = r#"{"label": "no_value_field"}"#;
        let field: ItemField = serde_json::from_str(json).unwrap();
        assert_eq!(field.label, Some("no_value_field".to_string()));
    }

    // ============================================
    // Tests for parse_env_file()
    // ============================================

    #[test]
    fn test_parse_env_file_basic() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join(".env");
        fs::write(&file_path, "API_KEY=secret\nDB_HOST=localhost\n").unwrap();

        let pairs = parse_env_file(&file_path).unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("API_KEY".to_string(), "secret".to_string()));
        assert_eq!(pairs[1], ("DB_HOST".to_string(), "localhost".to_string()));
    }

    #[test]
    fn test_parse_env_file_handles_comments_export_and_quotes() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join(".env");
        fs::write(
            &file_path,
            r#"# comment
export TOKEN=abc
QUOTED="hello"
SINGLE='world'
"#,
        )
        .unwrap();

        let pairs = parse_env_file(&file_path).unwrap();
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0], ("TOKEN".to_string(), "abc".to_string()));
        assert_eq!(pairs[1], ("QUOTED".to_string(), "hello".to_string()));
        assert_eq!(pairs[2], ("SINGLE".to_string(), "world".to_string()));
    }

    #[test]
    fn test_parse_env_file_skips_invalid_keys() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join(".env");
        fs::write(
            &file_path,
            "VALID=value\nINVALID-KEY=value\n1INVALID=value\n",
        )
        .unwrap();

        let pairs = parse_env_file(&file_path).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0], ("VALID".to_string(), "value".to_string()));
    }

    #[test]
    fn test_parse_env_file_supports_inline_comments_and_hash_in_quotes() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join(".env");
        fs::write(
            &file_path,
            r#"PLAIN=value # comment
NO_COMMENT=value#hash
DOUBLE="value # kept"
SINGLE='value # kept'
"#,
        )
        .unwrap();

        let pairs = parse_env_file(&file_path).unwrap();
        assert_eq!(pairs.len(), 4);
        assert_eq!(pairs[0], ("PLAIN".to_string(), "value".to_string()));
        assert_eq!(
            pairs[1],
            ("NO_COMMENT".to_string(), "value#hash".to_string())
        );
        assert_eq!(pairs[2], ("DOUBLE".to_string(), "value # kept".to_string()));
        assert_eq!(pairs[3], ("SINGLE".to_string(), "value # kept".to_string()));
    }

    #[test]
    fn test_parse_env_file_allows_export_with_multiple_spaces() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join(".env");
        fs::write(&file_path, "export   TOKEN=abc\n").unwrap();

        let pairs = parse_env_file(&file_path).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0], ("TOKEN".to_string(), "abc".to_string()));
    }

    #[test]
    fn test_parse_env_file_duplicate_keys_last_wins() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join(".env");
        fs::write(&file_path, "A=first\nB=keep\nA=last\n").unwrap();

        let pairs = parse_env_file(&file_path).unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("B".to_string(), "keep".to_string()));
        assert_eq!(pairs[1], ("A".to_string(), "last".to_string()));
    }

    #[test]
    fn test_parse_env_file_skips_existing_op_references() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join(".env");
        fs::write(
            &file_path,
            "NEW_SECRET=plain\nEXISTING=op://vault/item/EXISTING\n",
        )
        .unwrap();

        let pairs = parse_env_file(&file_path).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0], ("NEW_SECRET".to_string(), "plain".to_string()));
    }

    #[test]
    fn test_is_op_reference() {
        assert!(is_op_reference("op://vault/item/key"));
        assert!(!is_op_reference("value"));
    }

    #[test]
    fn test_build_create_item_uses_stdin_template_without_secret_args() {
        let env_pairs = vec![
            ("API_KEY".to_string(), "secret".to_string()),
            ("DB_HOST".to_string(), "localhost".to_string()),
        ];

        let args = build_create_item_args(Some("Private"));
        let template = build_api_credential_template("my-item", &env_pairs, &[]);

        assert_eq!(args, vec!["item", "create", "--vault", "Private", "-"]);
        assert!(!args.iter().any(|arg| arg.contains("secret")));
        assert_eq!(template.title, "my-item");
        assert_eq!(template.category, "API_CREDENTIAL");
        assert_eq!(template.fields.len(), 2);
        assert_eq!(template.fields[0].id, "API_KEY");
        assert_eq!(template.fields[0].label, "API_KEY");
        assert_eq!(template.fields[0].field_type, "STRING");
        assert_eq!(template.fields[0].value, "secret");
        assert_eq!(template.fields[1].id, "DB_HOST");
        assert_eq!(template.fields[1].value, "localhost");
    }

    #[test]
    fn test_build_create_item_adds_github_repository_metadata() {
        let env_pairs = vec![("API_KEY".to_string(), "secret".to_string())];
        let template = build_api_credential_template(
            "my-item",
            &env_pairs,
            &["owner/repo".to_string(), "other/service".to_string()],
        );

        let metadata = template
            .fields
            .iter()
            .find(|field| field.label == GITHUB_REPOSITORIES_LABEL)
            .unwrap();
        assert_eq!(metadata.value, "owner/repo\nother/service");
        assert_eq!(metadata.field_type, "STRING");
    }

    #[test]
    fn test_collect_create_stdout_sensitive_fields_from_template() {
        let template = ItemCreateTemplate {
            title: "my-item".to_string(),
            category: "API_CREDENTIAL".to_string(),
            fields: vec![
                ItemCreateField {
                    id: "api_key".to_string(),
                    field_type: "STRING".to_string(),
                    label: "API_KEY".to_string(),
                    value: "secret".to_string(),
                    purpose: None,
                },
                ItemCreateField {
                    id: "EMPTY".to_string(),
                    field_type: "STRING".to_string(),
                    label: "EMPTY".to_string(),
                    value: String::new(),
                    purpose: None,
                },
            ],
        };

        assert_eq!(
            collect_create_stdout_sensitive_fields(&template),
            vec![
                ("API_KEY".to_string(), "secret".to_string()),
                ("api_key".to_string(), "secret".to_string()),
            ]
        );
    }

    #[test]
    fn test_mask_create_stdout_masks_only_field_values() {
        let template = build_api_credential_template(
            "my-item",
            &[
                ("API_KEY".to_string(), "secret".to_string()),
                ("DB_HOST".to_string(), "localhost".to_string()),
            ],
            &[],
        );

        let masked = mask_create_stdout(
            "ID: abc123\nTitle: my-secret-item\nAPI_KEY: secret\nDB_HOST: localhost\n",
            &collect_create_stdout_sensitive_fields(&template),
        );

        assert_eq!(
            masked,
            "ID: abc123\nTitle: my-secret-item\nAPI_KEY: ***\nDB_HOST: ***\n"
        );
    }

    #[test]
    fn test_mask_create_stdout_masks_multiline_notes_field() {
        let template = build_secure_note_template("f4ah6o/opz", "```app.conf\nTOKEN=abc\n```");

        let masked = mask_create_stdout(
            "ID: abc123\nnotesPlain: ```app.conf\nTOKEN=abc\n```\nTitle: f4ah6o/opz\n",
            &collect_create_stdout_sensitive_fields(&template),
        );

        assert_eq!(masked, "ID: abc123\nnotesPlain: ***\nTitle: f4ah6o/opz\n");
    }

    #[test]
    fn test_merge_github_repository_lists_dedupes_and_normalizes() {
        let merged = merge_github_repository_lists(
            &["Owner/Repo".to_string(), "old/service".to_string()],
            &[
                "https://github.com/owner/repo.git".to_string(),
                "git@github.com:New/App.git".to_string(),
            ],
        );

        assert_eq!(
            merged,
            vec![
                "owner/repo".to_string(),
                "old/service".to_string(),
                "new/app".to_string()
            ]
        );
    }

    #[test]
    fn test_build_op_item_edit_github_repositories_args() {
        let args = build_op_item_edit_github_repositories_args(
            Some("Private"),
            "item-id",
            &["owner/repo".to_string(), "other/service".to_string()],
        );

        assert_eq!(
            args,
            vec![
                "item".to_string(),
                "edit".to_string(),
                "item-id".to_string(),
                "--vault".to_string(),
                "Private".to_string(),
                "github_repositories=owner/repo\nother/service".to_string(),
            ]
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_command_output_with_timeout_kills_slow_command() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 5");

        let err = command_output_with_timeout(cmd, "`slow command`", Duration::from_millis(100))
            .unwrap_err();

        assert!(err.to_string().contains("timed out"), "{err}");
    }

    #[test]
    fn test_secret_resolution_timeout_does_not_fallback_to_op_read() {
        let err = anyhow!("`op run` for batch secret resolution timed out after 30 seconds");

        assert!(!should_fallback_to_op_read(&err));
    }

    #[test]
    fn test_secret_resolution_non_timeout_can_fallback_to_op_read() {
        let err = anyhow!("op run failed: unsupported option");

        assert!(should_fallback_to_op_read(&err));
    }

    #[test]
    fn test_op_timeout_detection_checks_error_chain() {
        let err = anyhow!("`op read` timed out after 30 seconds").context("resolve secret");

        assert!(is_op_timeout_error(&err));
    }

    #[test]
    fn test_migrate_script_text_rewrites_explicit_opz_run_item() {
        let migration = migrate_script_text(
            "test:\n    opz run service -- env\n",
            ScriptMigrationMode::Collect,
        )
        .unwrap();

        assert_eq!(migration.items, vec!["service".to_string()]);
        assert!(!migration.uses_dotenv);
        assert_eq!(migration.rewritten, "test:\n    opz run service -- env\n");
    }

    #[test]
    fn test_migrate_script_text_rewrites_top_level_shorthand_item() {
        let migration = migrate_script_text(
            "test:\n    opz service -- env\n",
            ScriptMigrationMode::Collect,
        )
        .unwrap();

        assert_eq!(migration.items, vec!["service".to_string()]);
        assert_eq!(migration.rewritten, "test:\n    opz service -- env\n");
    }

    #[test]
    fn test_migrate_script_text_detects_dotenv_op_run() {
        let migration = migrate_script_text(
            "test:\n    op run --env-file .env -- env\n",
            ScriptMigrationMode::Collect,
        )
        .unwrap();

        assert!(migration.items.is_empty());
        assert!(migration.uses_dotenv);
        assert_eq!(
            migration.rewritten,
            "test:\n    op run --env-file .env -- env\n"
        );
    }

    #[test]
    fn test_migrate_script_text_collects_op_item_get_without_rewrite() {
        let migration = migrate_script_text(
            "test:\n    op item get service --format json\n",
            ScriptMigrationMode::Collect,
        )
        .unwrap();

        assert_eq!(migration.items, vec!["service".to_string()]);
        assert_eq!(
            migration.rewritten,
            "test:\n    op item get service --format json\n"
        );
    }

    #[test]
    fn test_migrate_script_text_skips_template_item_tokens() {
        let migration = migrate_script_text(
            "test item:\n    opz run {{item}} -- env\n",
            ScriptMigrationMode::Collect,
        )
        .unwrap();

        assert!(migration.items.is_empty());
        assert_eq!(
            migration.rewritten,
            "test item:\n    opz run {{item}} -- env\n"
        );
    }

    #[test]
    fn test_migrate_script_text_skips_command_substitution_item_tokens() {
        let migration = migrate_script_text(
            "test:\n    opz run $(item) -- env\n",
            ScriptMigrationMode::Collect,
        )
        .unwrap();

        assert!(migration.items.is_empty());
        assert_eq!(migration.rewritten, "test:\n    opz run $(item) -- env\n");
    }

    #[test]
    fn test_migrate_justfile_scripts_resolves_recipe_default_item() {
        let content = r#"docs_item := "papyr-docs-cloudflare-dev"
docs_env_example := "apps/docs/.env.opz.example"
docs_env_file := "apps/docs/.env.opz"

docs-build item=docs_item:
  opz run {{item}} -- just _docs-build

docs-dev item=docs_item:
  opz run {{item}} -- just _docs-dev

docs-op-create item=docs_item:
  tmpdir="$(mktemp -d)"; trap "rm -rf "$tmpdir"" EXIT; cp {{docs_env_example}} "$tmpdir/.env"; cd "$tmpdir"; opz create {{item}} .env

docs-op-env item=docs_item:
  rm -f {{docs_env_file}}
  opz gen {{item}} --env-file {{docs_env_file}}
"#;
        let migration = migrate_justfile_scripts(content, ScriptMigrationMode::Collect).unwrap();

        assert_eq!(
            migration.items,
            vec!["papyr-docs-cloudflare-dev".to_string()]
        );
        assert!(migration.detected_opz);
        assert_eq!(
            migration.rewritten,
            r#"docs_item := "papyr-docs-cloudflare-dev"
docs_env_example := "apps/docs/.env.opz.example"
docs_env_file := "apps/docs/.env.opz"

docs-build item=docs_item:
  opz run {{item}} -- just _docs-build

docs-dev item=docs_item:
  opz run {{item}} -- just _docs-dev

docs-op-create item=docs_item:
  tmpdir="$(mktemp -d)"; trap "rm -rf "$tmpdir"" EXIT; cp {{docs_env_example}} "$tmpdir/.env"; cd "$tmpdir"; opz create {{item}} .env

docs-op-env item=docs_item:
  rm -f {{docs_env_file}}
  opz gen {{item}} --env-file {{docs_env_file}}
"#
        );
    }

    #[test]
    fn test_migrate_justfile_scripts_resolves_after_set_shell_directive() {
        let content = r#"set shell := ["bash", "-euo", "pipefail", "-c"]

docs_item := "papyr-docs-cloudflare-dev"

docs-build item=docs_item:
  opz run {{item}} -- just _docs-build
"#;
        let migration = migrate_justfile_scripts(content, ScriptMigrationMode::Collect).unwrap();

        assert_eq!(
            migration.items,
            vec!["papyr-docs-cloudflare-dev".to_string()]
        );
        assert_eq!(
            migration.rewritten,
            r#"set shell := ["bash", "-euo", "pipefail", "-c"]

docs_item := "papyr-docs-cloudflare-dev"

docs-build item=docs_item:
  opz run {{item}} -- just _docs-build
"#
        );
    }

    #[test]
    fn test_migrate_justfile_scripts_reports_up_to_date_opz_usage() {
        let migration =
            migrate_justfile_scripts("test:\n  opz run -- env\n", ScriptMigrationMode::Collect)
                .unwrap();

        assert!(migration.items.is_empty());
        assert!(migration.detected_opz);
        assert_eq!(migration.rewritten, "test:\n  opz run -- env\n");
    }

    #[test]
    fn test_migrate_justfile_scripts_keeps_unresolved_template_item() {
        let migration = migrate_justfile_scripts(
            "test item:\n  opz run {{item}} -- env\n",
            ScriptMigrationMode::Collect,
        )
        .unwrap();

        assert!(migration.items.is_empty());
        assert!(migration.detected_opz);
        assert_eq!(
            migration.rewritten,
            "test item:\n  opz run {{item}} -- env\n"
        );
    }

    #[test]
    fn test_migrate_justfile_scripts_renames_item_title_without_removing_explicit_item() {
        let content = r#"docs_item := "papyr-docs-cloudflare-dev"
docs_env_file := "apps/docs/.env.opz"

docs-build item=docs_item:
  opz run {{item}} -- just _docs-build

docs-op-env item=docs_item:
  opz gen {{item}} --env-file {{docs_env_file}}
"#;
        let migration = migrate_justfile_scripts(
            content,
            ScriptMigrationMode::RenameItems {
                title: "f4ah6o/papyr.mbt",
            },
        )
        .unwrap();

        assert_eq!(
            migration.rewritten,
            r#"docs_item := "f4ah6o/papyr.mbt"
docs_env_file := "apps/docs/.env.opz"

docs-build item=docs_item:
  opz run {{item}} -- just _docs-build

docs-op-env item=docs_item:
  opz gen {{item}} --env-file {{docs_env_file}}
"#
        );
    }

    #[test]
    fn test_migrate_justfile_scripts_restores_itemless_run_from_recipe_item() {
        let content = r#"docs_item := "f4ah6o/papyr.mbt"

docs-build item=docs_item:
  opz run -- just _docs-build
"#;
        let migration = migrate_justfile_scripts(
            content,
            ScriptMigrationMode::Restore {
                title: "f4ah6o/papyr.mbt",
            },
        )
        .unwrap();

        assert_eq!(
            migration.rewritten,
            r#"docs_item := "f4ah6o/papyr.mbt"

docs-build item=docs_item:
  opz run {{item}} -- just _docs-build
"#
        );
    }

    #[test]
    fn test_migrate_package_json_scripts_rewrites_script_values() {
        let content = r#"{"name":"app","scripts":{"dev":"opz run service -- vite","test":"echo ok"},"dependencies":{"z":"1"}}"#;
        let migration =
            migrate_package_json_scripts(content, ScriptMigrationMode::Collect).unwrap();

        assert_eq!(migration.items, vec!["service".to_string()]);
        assert_eq!(
            migration.rewritten,
            r#"{"name":"app","scripts":{"dev":"opz run service -- vite","test":"echo ok"},"dependencies":{"z":"1"}}"#
        );
    }

    #[test]
    fn test_credential_env_file_name_patterns() {
        assert!(is_credential_env_file_name(".env"));
        assert!(is_credential_env_file_name(".env.local"));
        assert!(is_credential_env_file_name("service.env"));
        assert!(is_credential_env_file_name("service.env.production"));
        assert!(!is_credential_env_file_name(".env.example"));
        assert!(!is_credential_env_file_name(".env.sample"));
        assert!(!is_credential_env_file_name(".env.template"));
        assert!(!is_credential_env_file_name("backup.env.old"));
        assert!(!is_credential_env_file_name("foo.env.bak"));
        assert!(!is_credential_env_file_name(".envrc"));
        assert!(!is_credential_env_file_name("README.md"));
    }

    #[test]
    fn test_count_plaintext_env_entries_ignores_op_references() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join(".env");
        fs::write(
            &file_path,
            "API_KEY=plain\nEXISTING=op://vault/item/EXISTING\nEMPTY=\n# COMMENT=ignored\n",
        )
        .unwrap();

        assert_eq!(count_plaintext_env_entries(&file_path).unwrap(), 1);
    }

    #[test]
    fn test_collect_plaintext_credential_files_skips_generated_dirs() {
        let tmp_dir = TempDir::new().unwrap();
        fs::write(tmp_dir.path().join(".env"), "API_KEY=plain\n").unwrap();
        fs::create_dir(tmp_dir.path().join("target")).unwrap();
        fs::write(tmp_dir.path().join("target").join(".env"), "TOKEN=plain\n").unwrap();

        let mut findings = Vec::new();
        collect_plaintext_credential_files(tmp_dir.path(), tmp_dir.path(), &mut findings).unwrap();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path, PathBuf::from(".env"));
    }

    #[test]
    fn test_extract_org_repo_from_remote_url() {
        assert_eq!(
            extract_org_repo_from_remote_url("https://github.com/f4ah6o/opz.git"),
            Some("f4ah6o/opz".to_string())
        );
        assert_eq!(
            extract_org_repo_from_remote_url("git@github.com:f4ah6o/opz.git"),
            Some("f4ah6o/opz".to_string())
        );
        assert_eq!(
            extract_org_repo_from_remote_url("ssh://git@github.com/f4ah6o/opz.git"),
            Some("f4ah6o/opz".to_string())
        );
        assert_eq!(extract_org_repo_from_remote_url("file:///tmp/opz"), None);
    }

    #[test]
    fn test_dedupe_titles_with_sequence() {
        let base = vec![
            "a/b".to_string(),
            "a/b".to_string(),
            "c/d".to_string(),
            "a/b".to_string(),
        ];
        let deduped = dedupe_titles_with_sequence(&base);
        assert_eq!(
            deduped,
            vec![
                "a/b".to_string(),
                "a/b-2".to_string(),
                "c/d".to_string(),
                "a/b-3".to_string()
            ]
        );
    }

    #[test]
    fn test_build_secure_note_body() {
        let body = build_secure_note_body("app.conf", "line1\nline2");
        assert_eq!(body, "```app.conf\nline1\nline2\n```");
    }

    #[test]
    fn test_build_secure_note_uses_stdin_template_without_body_args() {
        let args = build_create_item_args(Some("Private"));
        let template = build_secure_note_template("f4ah6o/opz", "```a\nb\n```");

        assert_eq!(args, vec!["item", "create", "--vault", "Private", "-"]);
        assert!(!args.iter().any(|arg| arg.contains("```a")));
        assert_eq!(template.title, "f4ah6o/opz");
        assert_eq!(template.category, "SECURE_NOTE");
        assert_eq!(template.fields.len(), 1);
        assert_eq!(template.fields[0].id, "notesPlain");
        assert_eq!(template.fields[0].label, "notesPlain");
        assert_eq!(template.fields[0].purpose.as_deref(), Some("NOTES"));
        assert_eq!(template.fields[0].value, "```a\nb\n```");
    }

    // ============================================
    // Tests for expand_vars()
    // ============================================

    #[test]
    fn test_expand_vars_simple() {
        let mut env = HashMap::new();
        env.insert("API_TOKEN".to_string(), "secret123".to_string());
        assert_eq!(expand_vars("Bearer $API_TOKEN", &env), "Bearer secret123");
    }

    #[test]
    fn test_expand_vars_braced() {
        let mut env = HashMap::new();
        env.insert("HOST".to_string(), "example.com".to_string());
        assert_eq!(
            expand_vars("https://${HOST}/api", &env),
            "https://example.com/api"
        );
    }

    #[test]
    fn test_expand_vars_multiple() {
        let mut env = HashMap::new();
        env.insert("USER".to_string(), "alice".to_string());
        env.insert("HOST".to_string(), "server.com".to_string());
        assert_eq!(expand_vars("$USER@$HOST", &env), "alice@server.com");
    }

    #[test]
    fn test_expand_vars_unknown_var() {
        let env = HashMap::new();
        // Unknown vars should be preserved as-is
        assert_eq!(expand_vars("$HOME/dir", &env), "$HOME/dir");
        assert_eq!(expand_vars("$PATH", &env), "$PATH");
    }

    #[test]
    fn test_expand_vars_mixed_known_unknown() {
        let mut env = HashMap::new();
        env.insert("API_TOKEN".to_string(), "secret".to_string());
        assert_eq!(
            expand_vars("Authorization: $API_TOKEN for $HOME", &env),
            "Authorization: secret for $HOME"
        );
    }

    #[test]
    fn test_expand_vars_with_special_chars() {
        let mut env = HashMap::new();
        env.insert("TOKEN".to_string(), "a$b\"c`d".to_string());
        let result = expand_vars("$TOKEN", &env);
        assert_eq!(result, r#"a$b"c`d"#);
    }

    #[test]
    fn test_expand_vars_empty_value() {
        let mut env = HashMap::new();
        env.insert("EMPTY".to_string(), "".to_string());
        // $EMPTYsuffix looks for "EMPTYsuffix" variable, not "EMPTY"
        // Since EMPTYsuffix doesn't exist, it remains as-is for shell expansion
        assert_eq!(
            expand_vars("prefix$EMPTYsuffix", &env),
            "prefix$EMPTYsuffix"
        );
        // Use ${EMPTY} to explicitly mark variable boundaries
        assert_eq!(expand_vars("prefix${EMPTY}suffix", &env), "prefixsuffix");
        // Direct usage should expand to empty string
        assert_eq!(expand_vars("$EMPTY", &env), "");
    }

    #[test]
    fn test_expand_vars_partial_name() {
        let mut env = HashMap::new();
        env.insert("API".to_string(), "test".to_string());
        // $API_TOKEN looks for "API_TOKEN" variable, not "API"
        // Since API_TOKEN doesn't exist, it remains as-is
        assert_eq!(expand_vars("$API_TOKEN", &env), "$API_TOKEN");
    }

    #[test]
    fn test_expand_vars_no_vars() {
        let env = HashMap::new();
        assert_eq!(expand_vars("hello world", &env), "hello world");
    }

    #[test]
    fn test_expand_vars_consecutive_dollars() {
        let mut env = HashMap::new();
        env.insert("A".to_string(), "1".to_string());
        env.insert("B".to_string(), "2".to_string());
        assert_eq!(expand_vars("$A$B", &env), "12");
    }

    #[test]
    fn test_expand_vars_underscore_in_name() {
        let mut env = HashMap::new();
        env.insert("API_TOKEN".to_string(), "secret".to_string());
        assert_eq!(expand_vars("$API_TOKEN", &env), "secret");
        assert_eq!(expand_vars("${API_TOKEN}", &env), "secret");
    }

    #[test]
    fn test_merge_env_lines_last_item_wins() {
        let sections = vec![
            (
                "foo".to_string(),
                vec![
                    "A=op://vault1/item1/A".to_string(),
                    "B=op://vault1/item1/B".to_string(),
                ],
            ),
            (
                "bar".to_string(),
                vec![
                    "A=op://vault2/item2/A".to_string(),
                    "C=op://vault2/item2/C".to_string(),
                ],
            ),
        ];

        let merged = merge_env_lines(&sections);
        assert_eq!(
            merged,
            vec![
                "A=op://vault2/item2/A".to_string(),
                "B=op://vault1/item1/B".to_string(),
                "C=op://vault2/item2/C".to_string(),
            ]
        );
    }

    #[test]
    fn test_sectioned_env_output_string() {
        let sections = vec![
            (
                "foo".to_string(),
                vec!["A=op://v1/i1/A".to_string(), "B=op://v1/i1/B".to_string()],
            ),
            ("bar".to_string(), vec!["C=op://v2/i2/C".to_string()]),
        ];

        let rendered = sectioned_env_output_string(&sections);
        assert_eq!(
            rendered,
            "# --- item: foo ---\nA=op://v1/i1/A\nB=op://v1/i1/B\n\n# --- item: bar ---\nC=op://v2/i2/C\n"
        );
    }

    #[test]
    fn test_show_output_string_plain() {
        let sections = vec![
            ("foo".to_string(), vec!["A".to_string(), "B".to_string()]),
            ("bar".to_string(), vec!["C".to_string()]),
        ];

        let rendered = show_output_string(&sections, false);
        assert_eq!(rendered, "A\nB\nC\n");
    }

    #[test]
    fn test_show_output_string_with_item() {
        let sections = vec![
            ("foo".to_string(), vec!["A".to_string(), "B".to_string()]),
            ("bar".to_string(), vec!["C".to_string()]),
        ];

        let rendered = show_output_string(&sections, true);
        assert_eq!(
            rendered,
            "# --- item: foo ---\nA\nB\n\n# --- item: bar ---\nC\n"
        );
    }

    #[test]
    fn test_cli_parse_show_multiple_items() {
        let cli = Cli::try_parse_from(["opz", "show", "foo", "bar"]).unwrap();
        match cli.cmd {
            Some(Cmd::Show { with_item, items }) => {
                assert!(!with_item);
                assert_eq!(items, vec!["foo".to_string(), "bar".to_string()]);
            }
            _ => panic!("expected show command"),
        }
    }

    #[test]
    fn test_cli_parse_skills() {
        let cli = Cli::try_parse_from(["opz", "skills"]).unwrap();
        match cli.cmd {
            Some(Cmd::Skills) => {}
            _ => panic!("expected skills command"),
        }
    }

    #[test]
    fn test_cli_parse_doctor() {
        let cli = Cli::try_parse_from(["opz", "doctor"]).unwrap();
        match cli.cmd {
            Some(Cmd::Doctor) => {}
            _ => panic!("expected doctor command"),
        }
    }

    #[test]
    fn test_cli_parse_environment_alias_and_account() {
        let cli =
            Cli::try_parse_from(["opz", "env", "--account", "A1", "variables", "dev"]).unwrap();
        match cli.cmd {
            Some(Cmd::Environment { account, command }) => {
                assert_eq!(account.as_deref(), Some("A1"));
                match command {
                    EnvironmentCommand::Variables { environment } => assert_eq!(environment, "dev"),
                    _ => panic!("expected variables command"),
                }
            }
            _ => panic!("expected environment command"),
        }
    }

    #[test]
    fn test_environment_list_uses_account_without_authentication() {
        let mut client = FakeMcpClient::new(vec![serde_json::json!({
            "structuredContent": {
                "environments": [
                    {"id": "env2", "name": "staging"},
                    {"environmentId": "env1", "environmentName": "dev"}
                ]
            }
        })]);
        let output =
            run_environment_command(&mut client, Some("A1"), &EnvironmentCommand::List).unwrap();
        assert_eq!(output, "env1\tdev\nenv2\tstaging\n");
        assert_eq!(client.calls.len(), 1);
        assert_eq!(client.calls[0].0, "list_environments");
        assert_eq!(client.calls[0].1["accountId"], "A1");
    }

    #[test]
    fn test_environment_variables_authenticates_and_lists_names_only() {
        let mut client = FakeMcpClient::new(vec![
            serde_json::json!({"structuredContent": {"accountId": "A1"}}),
            serde_json::json!({"structuredContent": {"environments": [{"id": "env1", "name": "dev"}]}}),
            serde_json::json!({"structuredContent": {"variables": [
                {"name": "API_TOKEN", "value": "super-secret-value"},
                {"variableName": "DB_URL"}
            ]}}),
        ]);
        let output = run_environment_command(
            &mut client,
            None,
            &EnvironmentCommand::Variables {
                environment: "dev".to_string(),
            },
        )
        .unwrap();
        assert_eq!(output, "API_TOKEN\nDB_URL\n");
        assert!(!output.contains("super-secret-value"));
        assert_eq!(
            client
                .calls
                .iter()
                .map(|call| call.0.as_str())
                .collect::<Vec<_>>(),
            vec!["authenticate", "list_environments", "list_variables"]
        );
    }

    #[test]
    fn test_environment_mount_resolves_by_id_and_prints_mount_path() {
        let mut client = FakeMcpClient::new(vec![
            serde_json::json!({"structuredContent": {"environments": [{"id": "env1", "name": "dev"}]}}),
            serde_json::json!({"structuredContent": {"mountPath": ".env.local"}}),
        ]);
        let output = run_environment_command(
            &mut client,
            Some("A1"),
            &EnvironmentCommand::Mount {
                environment: "env1".to_string(),
                path: PathBuf::from(".env.local"),
            },
        )
        .unwrap();
        assert_eq!(output, ".env.local\n");
        assert_eq!(client.calls[1].0, "create_local_env_file");
        assert_eq!(client.calls[1].1["environmentName"], "dev");
        assert_eq!(client.calls[1].1["mountPath"], ".env.local");
    }

    #[test]
    fn test_environment_resolve_rejects_ambiguous_names() {
        let mut client = FakeMcpClient::new(vec![serde_json::json!({
            "structuredContent": {
                "environments": [
                    {"id": "env1", "name": "dev"},
                    {"id": "env2", "name": "dev"}
                ]
            }
        })]);
        let err = run_environment_command(
            &mut client,
            Some("A1"),
            &EnvironmentCommand::Mounts {
                environment: "dev".to_string(),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("Multiple 1Password Environments"));
    }

    #[test]
    fn test_mcp_text_json_content_is_parsed() {
        let result = serde_json::json!({
            "content": [
                {"type": "text", "text": "{\"accountId\":\"A1\",\"environments\":[{\"id\":\"env1\",\"name\":\"dev\"}]}"}
            ]
        });
        let values = mcp_result_values(&result);
        assert_eq!(
            extract_first_string_for_keys(&values, &["accountId"]).as_deref(),
            Some("A1")
        );
        assert_eq!(
            extract_environment_records(&values),
            vec![EnvironmentRecord {
                id: "env1".to_string(),
                name: "dev".to_string()
            }]
        );
    }

    #[test]
    fn test_detect_command_hint_doctor() {
        let args = vec![OsString::from("opz"), OsString::from("doctor")];
        assert_eq!(detect_command_hint(&args), "doctor");
    }

    #[test]
    fn test_detect_command_hint_github_repo() {
        let args = vec![OsString::from("opz"), OsString::from("github-repo")];
        assert_eq!(detect_command_hint(&args), "github-repo");
    }

    #[test]
    fn test_render_doctor_checks() {
        let checks = vec![
            DoctorCheck::ok("op", "/bin/op (2.0.0)", true),
            DoctorCheck::warn("gh", "not found in PATH (needed by github-secret)"),
            DoctorCheck::error("op auth", "`op whoami --format json` failed"),
        ];

        let rendered = render_doctor_checks(&checks);
        assert!(rendered.contains("ok    op: /bin/op (2.0.0)\n"));
        assert!(rendered.contains("warn  gh: not found in PATH (needed by github-secret)\n"));
        assert!(rendered.contains("error op auth: `op whoami --format json` failed\n"));
    }

    #[test]
    fn test_doctor_has_required_failure_only_for_required_errors() {
        let warnings_only = vec![
            DoctorCheck::ok("op", "/bin/op (2.0.0)", true),
            DoctorCheck::warn("gh", "not found in PATH (needed by github-secret)"),
        ];
        assert!(!doctor_has_required_failure(&warnings_only));

        let required_error = vec![DoctorCheck::error("op", "not found in PATH")];
        assert!(doctor_has_required_failure(&required_error));
    }

    #[test]
    fn test_summarize_op_whoami_uses_non_secret_metadata() {
        let summary = summarize_op_whoami(
            r#"{"email":"user@example.test","account_uuid":"A1","user_uuid":"U1"}"#,
        )
        .unwrap();
        assert_eq!(summary, "user@example.test (A1)");
    }

    #[test]
    fn test_bundled_skill_has_expected_metadata() {
        let skill_lines: Vec<&str> = OPZ_SKILL.lines().collect();
        assert_eq!(skill_lines.first().copied(), Some("---"));
        assert_eq!(skill_lines.get(1).copied(), Some("name: opz"));
        assert!(skill_lines
            .iter()
            .any(|line| line.starts_with("description: ")));
        assert!(OPZ_SKILL.contains("opz find <query>"));
        assert!(OPZ_SKILL.contains("opz doctor"));
        assert!(OPZ_SKILL.contains("opz show [OPTIONS] <ITEM>..."));
        assert!(OPZ_SKILL.contains("opz gen [OPTIONS] <ITEM>..."));
        assert!(OPZ_SKILL.contains("opz migrate [OPTIONS]"));
        assert!(OPZ_SKILL.contains("opz note <FILE>"));
        assert!(OPZ_SKILL.contains("opz github-repo [OPTIONS] <ITEM>..."));
        assert!(OPZ_SKILL.contains("opz run [OPTIONS] [<ITEM>...] -- <COMMAND>..."));
        assert!(OPZ_SKILL.contains("opz github-secret [OPTIONS] <ITEM>..."));
        assert!(OPZ_SKILL.contains("opz cloudflare-secret [OPTIONS] <ITEM>..."));
        assert!(OPZ_SKILL.contains("opz skills"));
    }

    #[test]
    fn test_cli_parse_show_with_item_flag() {
        let cli = Cli::try_parse_from(["opz", "show", "--with-item", "foo"]).unwrap();
        match cli.cmd {
            Some(Cmd::Show { with_item, items }) => {
                assert!(with_item);
                assert_eq!(items, vec!["foo".to_string()]);
            }
            _ => panic!("expected show command"),
        }
    }

    #[test]
    fn test_cli_parse_run_multiple_items() {
        let cli = Cli::try_parse_from(["opz", "run", "foo", "bar", "--", "echo", "ok"]).unwrap();
        match cli.cmd {
            Some(Cmd::Run {
                items,
                command,
                env_file,
            }) => {
                assert_eq!(items, vec!["foo".to_string(), "bar".to_string()]);
                assert_eq!(command, vec!["echo".to_string(), "ok".to_string()]);
                assert!(env_file.is_none());
            }
            _ => panic!("expected run command"),
        }
    }

    #[test]
    fn test_cli_parse_run_with_env_file_option() {
        let cli = Cli::try_parse_from([
            "opz",
            "run",
            "--env-file",
            ".env",
            "foo",
            "bar",
            "--",
            "env",
        ])
        .unwrap();
        match cli.cmd {
            Some(Cmd::Run {
                items, env_file, ..
            }) => {
                assert_eq!(items, vec!["foo".to_string(), "bar".to_string()]);
                assert_eq!(env_file.as_deref(), Some(Path::new(".env")));
            }
            _ => panic!("expected run command"),
        }
    }

    #[test]
    fn test_cli_parse_run_without_items_for_auto_detect() {
        let cli = Cli::try_parse_from(["opz", "run", "--", "echo", "ok"]).unwrap();
        match cli.cmd {
            Some(Cmd::Run { items, command, .. }) => {
                assert!(items.is_empty());
                assert_eq!(command, vec!["echo".to_string(), "ok".to_string()]);
            }
            _ => panic!("expected run command"),
        }
    }

    #[test]
    fn test_cli_parse_top_level_without_items_for_auto_detect() {
        let cli = Cli::try_parse_from(["opz", "--", "echo", "ok"]).unwrap();
        assert!(cli.cmd.is_none());
        assert!(cli.items.is_empty());
        assert_eq!(cli.command, vec!["echo".to_string(), "ok".to_string()]);
    }

    #[test]
    fn test_cli_parse_run_with_environment() {
        let cli = Cli::try_parse_from(["opz", "run", "--environment", "dev", "--", "env"]).unwrap();
        assert_eq!(cli.environment, vec!["dev".to_string()]);
        match cli.cmd {
            Some(Cmd::Run { items, command, .. }) => {
                assert!(items.is_empty());
                assert_eq!(command, vec!["env".to_string()]);
            }
            _ => panic!("expected run command"),
        }
    }

    #[test]
    fn test_cli_parse_run_with_environments_alias() {
        let cli =
            Cli::try_parse_from(["opz", "run", "--environments", "dev", "--", "env"]).unwrap();
        assert_eq!(cli.environment, vec!["dev".to_string()]);
    }

    #[test]
    fn test_cli_parse_run_with_multiple_environments() {
        let cli = Cli::try_parse_from([
            "opz",
            "run",
            "--environment",
            "dev",
            "--environment",
            "staging",
            "--",
            "env",
        ])
        .unwrap();
        assert_eq!(
            cli.environment,
            vec!["dev".to_string(), "staging".to_string()]
        );
    }

    #[test]
    fn test_cli_parse_top_level_with_environment() {
        let cli = Cli::try_parse_from(["opz", "--environment", "dev", "--", "env"]).unwrap();
        assert!(cli.cmd.is_none());
        assert_eq!(cli.environment, vec!["dev".to_string()]);
        assert!(cli.items.is_empty());
        assert_eq!(cli.command, vec!["env".to_string()]);
    }

    #[test]
    fn test_detect_environment_flag_from_help_prefers_plural() {
        assert_eq!(
            detect_environment_flag_from_help(
                "Flags:\n  --environment string\n  --environments stringArray\n"
            ),
            Some("--environments")
        );
    }

    #[test]
    fn test_detect_environment_flag_from_help_accepts_singular() {
        assert_eq!(
            detect_environment_flag_from_help("Flags:\n  --environment string\n"),
            Some("--environment")
        );
    }

    #[test]
    fn test_detect_environment_flag_from_help_rejects_unsupported() {
        assert_eq!(
            detect_environment_flag_from_help("Flags:\n  --env-file stringArray\n"),
            None
        );
    }

    #[test]
    fn test_build_op_run_environment_args_excludes_secret_values() {
        let args = build_op_run_environment_args(
            "--environment",
            &["dev".to_string(), "staging".to_string()],
            &["printenv".to_string(), "API_TOKEN".to_string()],
        );
        assert_eq!(
            args,
            vec![
                "run".to_string(),
                "--environment".to_string(),
                "dev".to_string(),
                "--environment".to_string(),
                "staging".to_string(),
                "--".to_string(),
                "printenv".to_string(),
                "API_TOKEN".to_string(),
            ]
        );
        assert!(!args.contains(&"super-secret-value".to_string()));
    }

    #[test]
    fn test_run_with_environments_rejects_items() {
        let err = run_with_environments(
            None,
            &["dev".to_string()],
            &["item".to_string()],
            None,
            &["env".to_string()],
        )
        .unwrap_err();
        assert!(err.to_string().contains("cannot be combined with item"));
    }

    #[test]
    fn test_run_with_environments_rejects_env_file() {
        let err = run_with_environments(
            None,
            &["dev".to_string()],
            &[],
            Some(Path::new(".env")),
            &["env".to_string()],
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("cannot be combined with `--env-file`"));
    }

    #[test]
    fn test_run_with_environments_rejects_vault() {
        let err = run_with_environments(
            Some("Private"),
            &["dev".to_string()],
            &[],
            None,
            &["env".to_string()],
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("cannot be combined with `--environment`"));
    }

    #[test]
    fn test_detect_command_hint_skips_environment_options() {
        let args = vec![
            OsString::from("opz"),
            OsString::from("--environment"),
            OsString::from("dev"),
            OsString::from("run"),
            OsString::from("--"),
            OsString::from("env"),
        ];
        assert_eq!(detect_command_hint(&args), "run");

        let args = vec![
            OsString::from("opz"),
            OsString::from("--environments=dev"),
            OsString::from("doctor"),
        ];
        assert_eq!(detect_command_hint(&args), "doctor");
    }

    #[test]
    fn test_environment_option_rejected_for_non_run_commands() {
        let args = vec![
            OsString::from("opz"),
            OsString::from("find"),
            OsString::from("--environment"),
            OsString::from("dev"),
            OsString::from("query"),
        ];
        let err = run_cli(&args).unwrap_err();
        assert!(err.to_string().contains("only supported with `opz run`"));
    }

    #[test]
    fn test_cli_parse_migrate_flags() {
        let cli = Cli::try_parse_from(["opz", "migrate", "--dry-run", "--new"]).unwrap();
        match cli.cmd {
            Some(Cmd::Migrate {
                dry_run,
                new,
                restore,
            }) => {
                assert!(dry_run);
                assert!(new);
                assert!(!restore);
            }
            _ => panic!("expected migrate command"),
        }
    }

    #[test]
    fn test_cli_parse_migrate_restore() {
        let cli = Cli::try_parse_from(["opz", "migrate", "--dry-run", "--restore"]).unwrap();
        match cli.cmd {
            Some(Cmd::Migrate {
                dry_run,
                new,
                restore,
            }) => {
                assert!(dry_run);
                assert!(!new);
                assert!(restore);
            }
            _ => panic!("expected migrate command"),
        }
    }

    #[test]
    fn test_cli_parse_note() {
        let cli = Cli::try_parse_from(["opz", "note", "app.conf"]).unwrap();
        match cli.cmd {
            Some(Cmd::Note { file }) => assert_eq!(file, PathBuf::from("app.conf")),
            _ => panic!("expected note command"),
        }
    }

    #[test]
    fn test_cli_parse_removed_create_shim() {
        let cli = Cli::try_parse_from(["opz", "create", "service"]).unwrap();
        match cli.cmd {
            Some(Cmd::Create { args }) => assert_eq!(args, vec!["service".to_string()]),
            _ => panic!("expected hidden create command"),
        }
    }

    #[test]
    fn test_cli_parse_gen_multiple_items() {
        let cli = Cli::try_parse_from(["opz", "gen", "foo", "bar"]).unwrap();
        match cli.cmd {
            Some(Cmd::Gen { items, env_file }) => {
                assert_eq!(items, vec!["foo".to_string(), "bar".to_string()]);
                assert!(env_file.is_none());
            }
            _ => panic!("expected gen command"),
        }
    }

    #[test]
    fn test_cli_parse_github_secret() {
        let cli = Cli::try_parse_from([
            "opz",
            "github-secret",
            "--repo",
            "owner/repo",
            "--dry-run",
            "foo",
            "bar",
        ])
        .unwrap();
        match cli.cmd {
            Some(Cmd::GithubSecret {
                repo,
                dry_run,
                items,
            }) => {
                assert_eq!(repo.as_deref(), Some("owner/repo"));
                assert!(dry_run);
                assert_eq!(items, vec!["foo".to_string(), "bar".to_string()]);
            }
            _ => panic!("expected github-secret command"),
        }
    }

    #[test]
    fn test_cli_parse_github_repo() {
        let cli = Cli::try_parse_from([
            "opz",
            "github-repo",
            "--repo",
            "owner/repo",
            "--repo",
            "other/service",
            "--dry-run",
            "foo",
            "bar",
        ])
        .unwrap();
        match cli.cmd {
            Some(Cmd::GithubRepo {
                repo,
                dry_run,
                items,
            }) => {
                assert_eq!(
                    repo,
                    vec!["owner/repo".to_string(), "other/service".to_string()]
                );
                assert!(dry_run);
                assert_eq!(items, vec!["foo".to_string(), "bar".to_string()]);
            }
            _ => panic!("expected github-repo command"),
        }
    }

    #[test]
    fn test_cli_parse_cloudflare_secret() {
        let cli = Cli::try_parse_from([
            "opz",
            "cloudflare-secret",
            "--name",
            "worker-app",
            "--env",
            "production",
            "--config",
            "wrangler.jsonc",
            "--dry-run",
            "foo",
            "bar",
        ])
        .unwrap();
        match cli.cmd {
            Some(Cmd::CloudflareSecret {
                name,
                env,
                config,
                dry_run,
                items,
            }) => {
                assert_eq!(name.as_deref(), Some("worker-app"));
                assert_eq!(env.as_deref(), Some("production"));
                assert_eq!(config.as_deref(), Some(Path::new("wrangler.jsonc")));
                assert!(dry_run);
                assert_eq!(items, vec!["foo".to_string(), "bar".to_string()]);
            }
            _ => panic!("expected cloudflare-secret command"),
        }
    }

    #[test]
    fn test_validate_github_secret_name_rejects_reserved_prefix() {
        validate_github_secret_name("API_TOKEN").unwrap();
        validate_github_secret_name("_TOKEN").unwrap();
        assert!(validate_github_secret_name("GITHUB_TOKEN").is_err());
        assert!(validate_github_secret_name("github_token").is_err());
    }

    #[test]
    fn test_guard_github_secret_repo_allows_matching_metadata() {
        let items = vec![ItemGithubRepositories {
            item_title: "service".to_string(),
            repositories: vec!["Owner/Repo".to_string(), "other/service".to_string()],
        }];

        guard_github_secret_repo("owner/repo", &items).unwrap();
    }

    #[test]
    fn test_guard_github_secret_repo_rejects_mismatch() {
        let items = vec![ItemGithubRepositories {
            item_title: "service".to_string(),
            repositories: vec!["owner/repo".to_string()],
        }];

        let err = guard_github_secret_repo("other/repo", &items).unwrap_err();
        assert!(err.to_string().contains("GitHub repository mismatch"));
    }

    #[test]
    fn test_guard_github_secret_repo_allows_missing_metadata_with_warning_path() {
        let items = vec![ItemGithubRepositories {
            item_title: "service".to_string(),
            repositories: vec![],
        }];

        guard_github_secret_repo("owner/repo", &items).unwrap();
    }

    #[test]
    fn test_validate_github_secret_lines_uses_merged_last_item_wins() {
        let sections = vec![
            (
                "foo".to_string(),
                vec![
                    "API_TOKEN=op://vault1/item1/API_TOKEN".to_string(),
                    "DB_URL=op://vault1/item1/DB_URL".to_string(),
                ],
            ),
            (
                "bar".to_string(),
                vec!["API_TOKEN=op://vault2/item2/API_TOKEN".to_string()],
            ),
        ];

        let merged = merge_env_lines(&sections);
        let names = validate_github_secret_lines(&merged).unwrap();
        assert_eq!(
            merged,
            vec![
                "API_TOKEN=op://vault2/item2/API_TOKEN".to_string(),
                "DB_URL=op://vault1/item1/DB_URL".to_string(),
            ]
        );
        assert_eq!(names, vec!["API_TOKEN".to_string(), "DB_URL".to_string()]);
    }

    #[test]
    fn test_validate_cloudflare_secret_lines_uses_merged_last_item_wins() {
        let sections = vec![
            (
                "foo".to_string(),
                vec![
                    "API_TOKEN=op://vault1/item1/API_TOKEN".to_string(),
                    "DB_URL=op://vault1/item1/DB_URL".to_string(),
                ],
            ),
            (
                "bar".to_string(),
                vec!["API_TOKEN=op://vault2/item2/API_TOKEN".to_string()],
            ),
        ];

        let merged = merge_env_lines(&sections);
        let names = validate_cloudflare_secret_lines(&merged).unwrap();
        assert_eq!(
            merged,
            vec![
                "API_TOKEN=op://vault2/item2/API_TOKEN".to_string(),
                "DB_URL=op://vault1/item1/DB_URL".to_string(),
            ]
        );
        assert_eq!(names, vec!["API_TOKEN".to_string(), "DB_URL".to_string()]);
    }

    #[test]
    fn test_build_gh_secret_set_args_excludes_secret_value() {
        let args = build_gh_secret_set_args("owner/repo", "API_TOKEN");
        assert_eq!(
            args,
            vec![
                "secret".to_string(),
                "set".to_string(),
                "API_TOKEN".to_string(),
                "--repo".to_string(),
                "owner/repo".to_string(),
            ]
        );
        assert!(!args.contains(&"super-secret-value".to_string()));
    }

    #[test]
    fn test_build_wrangler_secret_bulk_args_excludes_secret_values() {
        let args = build_wrangler_secret_bulk_args(CloudflareSecretTarget {
            name: Some("worker-app"),
            env: Some("production"),
            config: Some(Path::new("wrangler.jsonc")),
        });
        assert_eq!(
            args,
            vec![
                "secret".to_string(),
                "bulk".to_string(),
                "--name".to_string(),
                "worker-app".to_string(),
                "--env".to_string(),
                "production".to_string(),
                "--config".to_string(),
                "wrangler.jsonc".to_string(),
            ]
        );
        assert!(!args.contains(&"super-secret-value".to_string()));
    }

    #[test]
    fn test_build_secret_json_payload_uses_names_and_values() {
        let names = vec!["API_TOKEN".to_string(), "DB_URL".to_string()];
        let mut env_vars = HashMap::new();
        env_vars.insert("API_TOKEN".to_string(), "secret-token".to_string());
        env_vars.insert("DB_URL".to_string(), "postgres://example".to_string());
        env_vars.insert("UNUSED".to_string(), "unused".to_string());

        let payload = build_secret_json_payload(&names, &env_vars).unwrap();
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(value["API_TOKEN"], "secret-token");
        assert_eq!(value["DB_URL"], "postgres://example");
        assert!(value.get("UNUSED").is_none());
    }

    #[test]
    fn test_cli_parse_top_level_multiple_items() {
        let cli = Cli::try_parse_from([
            "opz",
            "--env-file",
            ".env.local",
            "foo",
            "bar",
            "--",
            "printenv",
        ])
        .unwrap();
        assert!(cli.cmd.is_none());
        assert_eq!(cli.items, vec!["foo".to_string(), "bar".to_string()]);
        assert_eq!(cli.command, vec!["printenv".to_string()]);
        assert_eq!(cli.env_file.as_deref(), Some(Path::new(".env.local")));
    }

    #[test]
    fn test_cli_parse_legacy_env_positional_treated_as_item() {
        let cli = Cli::try_parse_from(["opz", "run", "foo", ".env", "--", "env"]).unwrap();
        match cli.cmd {
            Some(Cmd::Run {
                items, env_file, ..
            }) => {
                assert_eq!(items, vec!["foo".to_string(), ".env".to_string()]);
                assert!(env_file.is_none());
            }
            _ => panic!("expected run command"),
        }
    }
}
