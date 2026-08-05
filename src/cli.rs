use crate::*;

#[derive(Parser, Debug)]
#[command(author, version, about)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct Cli {
    /// Vault name for item-backed commands. Incompatible with `environment`.
    #[arg(long, global = true)]
    pub(crate) vault: Option<String>,

    /// Output env file path (optional, no file generated if omitted)
    #[arg(long, value_name = "ENV")]
    pub(crate) env_file: Option<PathBuf>,

    /// 1Password Environment name or ID for native op run injection
    #[arg(long = "environment", alias = "environments", value_name = "ENV")]
    pub(crate) run_environments: Vec<String>,

    #[command(subcommand)]
    pub(crate) cmd: Option<Cmd>,

    /// Item titles (when not using subcommand)
    #[arg(value_name = "ITEM")]
    pub(crate) items: Vec<String>,

    /// Command to run (after --)
    #[arg(last = true)]
    pub(crate) command: Vec<String>,
}

impl From<&Cli> for ItemContext {
    fn from(cli: &Cli) -> Self {
        Self {
            vault: cli.vault.clone(),
        }
    }
}

#[derive(Subcommand, Debug)]
pub(crate) enum Cmd {
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

    /// List, inspect, and run declarative launch profile plugins
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },

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

        /// 1Password Environment name or ID for native op run injection
        #[arg(long = "environment", alias = "environments", value_name = "ENV")]
        environments: Vec<String>,

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

    /// Import Cloudflare credentials or API responses into a 1Password item
    CloudflareCredential {
        /// Cloudflare storage preset controlling validation, defaults, and redaction
        #[arg(long, value_enum)]
        preset: CloudflareCredentialPreset,

        /// Create, update, or upsert the exact item title
        #[arg(long, value_enum, default_value = "upsert")]
        mode: CloudflareCredentialMode,

        /// Exact 1Password item title
        #[arg(long, value_name = "ITEM")]
        item: String,

        /// Destination section label (preset-specific default when omitted)
        #[arg(long, value_name = "SECTION")]
        section: Option<String>,

        /// Destination field label (preset-specific default when omitted)
        #[arg(long, value_name = "FIELD")]
        field: Option<String>,

        /// Read input from stdin
        #[arg(long)]
        stdin: bool,

        /// Read input from a JSON file
        #[arg(long, value_name = "JSON")]
        file: Option<PathBuf>,

        /// Store an API response without redaction; only valid for api-response
        #[arg(long)]
        raw: bool,

        /// Validate and show target secret references without writing to 1Password
        #[arg(long)]
        dry_run: bool,

        /// Command whose stdout is imported (after --)
        #[arg(last = true)]
        command: Vec<String>,
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
pub(crate) enum EnvironmentCommand {
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

    /// Add concealed placeholder variables without putting secret values in argv
    Add {
        /// Environment ID or exact name
        environment: String,

        /// Shell-compatible variable names to add with empty concealed values
        #[arg(value_name = "NAME", num_args = 1..)]
        variables: Vec<String>,
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

    /// List tool names advertised by the connected 1Password MCP server
    Tools,
}

impl From<&EnvironmentCommand> for McpEnvironmentAction {
    fn from(command: &EnvironmentCommand) -> Self {
        match command {
            EnvironmentCommand::List => Self::List,
            EnvironmentCommand::Create { name } => Self::Create { name: name.clone() },
            EnvironmentCommand::Rename {
                environment,
                new_name,
            } => Self::Rename {
                environment: environment.clone(),
                new_name: new_name.clone(),
            },
            EnvironmentCommand::Variables { environment } => Self::Variables {
                environment: environment.clone(),
            },
            EnvironmentCommand::Add {
                environment,
                variables,
            } => Self::Add {
                environment: environment.clone(),
                variables: variables.clone(),
            },
            EnvironmentCommand::Mount { environment, path } => Self::Mount {
                environment: environment.clone(),
                path: path.clone(),
            },
            EnvironmentCommand::Mounts { environment } => Self::Mounts {
                environment: environment.clone(),
            },
            EnvironmentCommand::Tools => Self::Tools,
        }
    }
}

pub(crate) fn run_cli(args: &[OsString]) -> Result<()> {
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
    let context = ItemContext::from(&cli);
    if cli.vault.is_some() && matches!(cli.cmd, Some(Cmd::Environment { .. })) {
        return Err(anyhow!(
            "`--vault` cannot be combined with `opz environment`; Developer Environments are account-scoped, not vault item lookups."
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
            let action = McpEnvironmentAction::from(command);
            run_environment_cli(account.as_deref(), &action)
        }
        Some(Cmd::Skills) => print_bundled_skill(),
        Some(Cmd::Plugin { command }) => run_plugin_cli(&context, command),
        Some(Cmd::Show { with_item, items }) => show_item_labels(&context, items, *with_item),
        Some(Cmd::Gen { items, env_file }) => {
            print_credential_file_advice_for_secret_command("gen");
            generate_env_output(&context, items, env_file.as_deref())
        }
        Some(Cmd::Migrate {
            dry_run,
            new,
            restore,
        }) => migrate_scripts(&context, *dry_run, *new, *restore),
        Some(Cmd::Note { file }) => create_secure_notes_from_file(&context, file),
        Some(Cmd::Create { .. }) => Err(anyhow!(
            "`opz create` was removed. Use `opz migrate --new` to create an item from .env, or `opz note <FILE>` to store a private config file."
        )),
        Some(Cmd::GithubRepo {
            repo,
            dry_run,
            items,
        }) => update_github_repositories_metadata(&context, repo, *dry_run, items),
        Some(Cmd::Run {
            environments,
            items,
            env_file,
            command,
        }) => {
            if command.is_empty() {
                return Err(anyhow!(
                    "Command required after '--'. Usage: opz run [OPTIONS] [--env-file <ENV>] [--environment <ENV>] [<ITEM>...] -- <COMMAND>..."
                ));
            }
            if !environments.is_empty() {
                return run_with_environments(
                    cli.vault.as_deref(),
                    environments,
                    items,
                    env_file.as_deref(),
                    command,
                );
            }
            print_credential_file_advice_for_secret_command("run");
            let resolved_items = resolve_run_items(&context, items)?;
            run_with_items_plugin_aware(
                &context,
                &resolved_items,
                env_file.as_deref(),
                command,
            )
        }
        Some(Cmd::GithubSecret {
            repo,
            dry_run,
            items,
        }) => {
            print_credential_file_advice_for_secret_command("github-secret");
            set_github_secrets(&context, repo.as_deref(), *dry_run, items)
        }
        Some(Cmd::CloudflareCredential {
            preset,
            mode,
            item,
            section,
            field,
            stdin,
            file,
            raw,
            dry_run,
            command,
        }) => store_cloudflare_credential(CloudflareCredentialOptions {
            vault: cli.vault.as_deref(),
            item,
            section: section.as_deref(),
            field: field.as_deref(),
            preset: *preset,
            mode: *mode,
            file: file.as_deref(),
            stdin: *stdin,
            raw: *raw,
            dry_run: *dry_run,
            command,
        }),
        Some(Cmd::CloudflareSecret {
            name,
            env,
            config,
            dry_run,
            items,
        }) => {
            print_credential_file_advice_for_secret_command("cloudflare-secret");
            set_cloudflare_secrets(
                &context,
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
            if !cli.run_environments.is_empty() {
                return run_with_environments(
                    cli.vault.as_deref(),
                    &cli.run_environments,
                    &cli.items,
                    cli.env_file.as_deref(),
                    &cli.command,
                );
            }
            print_credential_file_advice_for_secret_command("run");
            let resolved_items = resolve_run_items(&context, &cli.items)?;
            run_with_items_plugin_aware(
                &context,
                &resolved_items,
                cli.env_file.as_deref(),
                &cli.command,
            )
        }
    }
}

pub(crate) fn is_clap_display_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<clap::Error>()
        .is_some_and(|clap_err| clap_err.exit_code() == 0)
}

pub(crate) fn detect_command_hint(args: &[OsString]) -> &'static str {
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
            "plugin" => "plugin",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_rejects_run_environment_option_for_find() {
        let args = [
            OsString::from("opz"),
            OsString::from("find"),
            OsString::from("--environment"),
            OsString::from("dev"),
            OsString::from("query"),
        ];
        let error = run_cli(&args).unwrap_err();
        assert!(error
            .to_string()
            .contains("unexpected argument '--environment'"));
    }
}
