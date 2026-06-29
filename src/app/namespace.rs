use super::*;

const ITEM_TITLE_NAMESPACE: &str = "item-title";

pub(super) fn run_if_requested() -> Result<bool> {
    let args: Vec<OsString> = std::env::args_os().collect();
    let Some(stripped_args) = strip_namespace_args(&args)? else {
        return Ok(false);
    };

    let cli = Cli::try_parse_from(&stripped_args)?;
    if !cli.environment.is_empty() {
        return Err(anyhow!(
            "`--namespace` cannot be combined with `--environment` because item titles are not used for Environment-backed execution."
        ));
    }

    match &cli.cmd {
        Some(Cmd::Show { with_item, items }) => {
            let sections = collect_item_label_sections(&cli, items)?;
            let sections = namespace_label_sections(sections)?;
            print!("{}", show_output_string(&sections, *with_item));
        }
        Some(Cmd::Gen { items, env_file }) => {
            print_credential_file_advice_for_secret_command("gen");
            generate_namespaced_env_output(&cli, items, env_file.as_deref())?;
        }
        Some(Cmd::Run {
            items,
            env_file,
            command,
        }) => {
            if command.is_empty() {
                return Err(anyhow!(
                    "Command required after '--'. Usage: opz run --namespace item-title [OPTIONS] [--env-file <ENV>] [<ITEM>...] -- <COMMAND>..."
                ));
            }
            print_credential_file_advice_for_secret_command("run");
            let resolved_items = resolve_run_items(&cli, items)?;
            run_with_namespaced_items(&cli, &resolved_items, env_file.as_deref(), command)?;
        }
        None => {
            if cli.command.is_empty() {
                return Err(anyhow!(
                    "Command required after '--'. Usage: opz --namespace item-title [OPTIONS] [--env-file <ENV>] [<ITEM>...] -- <COMMAND>..."
                ));
            }
            print_credential_file_advice_for_secret_command("run");
            let resolved_items = resolve_run_items(&cli, &cli.items)?;
            run_with_namespaced_items(
                &cli,
                &resolved_items,
                cli.env_file.as_deref(),
                &cli.command,
            )?;
        }
        _ => {
            return Err(anyhow!(
                "`--namespace item-title` is only supported with `opz run`, top-level command execution, `opz gen`, or `opz show`."
            ));
        }
    }

    Ok(true)
}

fn strip_namespace_args(args: &[OsString]) -> Result<Option<Vec<OsString>>> {
    let mut stripped = Vec::with_capacity(args.len());
    let mut namespace_seen = false;
    let mut idx = 0;

    while idx < args.len() {
        let arg = args[idx].to_string_lossy();
        if arg == "--" {
            stripped.extend(args[idx..].iter().cloned());
            break;
        }

        if let Some(value) = arg.strip_prefix("--namespace=") {
            validate_namespace_value(value, namespace_seen)?;
            namespace_seen = true;
            idx += 1;
            continue;
        }

        if arg == "--namespace" {
            if namespace_seen {
                return Err(anyhow!("`--namespace` may only be specified once"));
            }
            let value = args
                .get(idx + 1)
                .ok_or_else(|| anyhow!("`--namespace` requires a value"))?
                .to_string_lossy();
            validate_namespace_value(&value, false)?;
            namespace_seen = true;
            idx += 2;
            continue;
        }

        stripped.push(args[idx].clone());
        idx += 1;
    }

    if idx == args.len() && !namespace_seen {
        return Ok(None);
    }
    if !namespace_seen {
        return Ok(None);
    }
    Ok(Some(stripped))
}

fn validate_namespace_value(value: &str, already_seen: bool) -> Result<()> {
    if already_seen {
        return Err(anyhow!("`--namespace` may only be specified once"));
    }
    if value != ITEM_TITLE_NAMESPACE {
        return Err(anyhow!(
            "Unsupported namespace mode `{value}`. Supported mode: `{ITEM_TITLE_NAMESPACE}`"
        ));
    }
    Ok(())
}

fn normalize_item_title_namespace(item_title: &str) -> Result<String> {
    let mut normalized = String::with_capacity(item_title.len());
    let mut replacing = false;

    for ch in item_title.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            normalized.push(ch.to_ascii_uppercase());
            replacing = false;
        } else if !replacing {
            normalized.push('_');
            replacing = true;
        }
    }

    let normalized = normalized.trim_matches('_').to_string();
    if normalized.is_empty() {
        return Err(anyhow!(
            "Item title `{item_title}` produces an empty environment namespace"
        ));
    }
    Ok(normalized)
}

fn register_namespaced_key(
    generated_by: &mut HashMap<String, String>,
    item_title: &str,
    generated_key: &str,
) -> Result<()> {
    if let Some(existing_title) = generated_by.get(generated_key) {
        return Err(anyhow!(
            "Generated environment variable `{generated_key}` collides between items `{existing_title}` and `{item_title}` after item-title normalization"
        ));
    }
    generated_by.insert(generated_key.to_string(), item_title.to_string());
    Ok(())
}

fn namespace_env_sections(
    sections: Vec<(String, Vec<String>)>,
) -> Result<Vec<(String, Vec<String>)>> {
    let mut generated_by = HashMap::new();
    let mut namespaced_sections = Vec::with_capacity(sections.len());

    for (item_title, lines) in sections {
        let item_namespace = normalize_item_title_namespace(&item_title)?;
        let mut namespaced_lines = Vec::with_capacity(lines.len());

        for line in lines {
            let (label, reference) = parse_env_line_kv(&line).ok_or_else(|| {
                anyhow!("Invalid generated environment entry for item `{item_title}`")
            })?;
            let generated_key = format!("{item_namespace}__{label}");
            register_namespaced_key(&mut generated_by, &item_title, &generated_key)?;
            namespaced_lines.push(format!("{generated_key}={reference}"));
        }

        namespaced_sections.push((item_title, namespaced_lines));
    }

    Ok(namespaced_sections)
}

fn namespace_label_sections(
    sections: Vec<(String, Vec<String>)>,
) -> Result<Vec<(String, Vec<String>)>> {
    let mut generated_by = HashMap::new();
    let mut namespaced_sections = Vec::with_capacity(sections.len());

    for (item_title, labels) in sections {
        let item_namespace = normalize_item_title_namespace(&item_title)?;
        let mut namespaced_labels = Vec::with_capacity(labels.len());

        for label in labels {
            let generated_key = format!("{item_namespace}__{label}");
            register_namespaced_key(&mut generated_by, &item_title, &generated_key)?;
            namespaced_labels.push(generated_key);
        }

        namespaced_sections.push((item_title, namespaced_labels));
    }

    Ok(namespaced_sections)
}

fn generate_namespaced_env_output(
    cli: &Cli,
    items: &[String],
    env_file: Option<&Path>,
) -> Result<()> {
    let sections = collect_item_env_sections(cli, items)?;
    let sections = namespace_env_sections(sections)?;
    let merged_env_lines = merge_env_lines(&sections);

    if let Some(path) = env_file {
        write_env_file(path, &merged_env_lines)?;
        eprintln!("Generated: {}", path.display());
    } else {
        print_sectioned_env_output(&sections);
    }
    Ok(())
}

fn run_with_namespaced_items(
    cli: &Cli,
    items: &[String],
    env_file: Option<&Path>,
    command: &[String],
) -> Result<()> {
    let sections = collect_item_env_sections(cli, items)?;
    let sections = namespace_env_sections(sections)?;
    let merged_env_lines = merge_env_lines(&sections);

    if let Some(path) = env_file {
        write_env_file(path, &merged_env_lines)?;
        eprintln!("Generated: {}", path.display());
    }

    let env_vars = resolve_env_vars(&merged_env_lines)?;
    let expanded_args: Vec<String> = command
        .iter()
        .map(|arg| expand_vars(arg, &env_vars))
        .collect();

    #[cfg(unix)]
    let mut cmd = {
        let mut child = Command::new("sh");
        child.arg("-c");
        child.arg("exec \"$@\"");
        child.arg("sh");
        child.args(&expanded_args);
        child
    };

    #[cfg(windows)]
    let mut cmd = {
        let mut child = Command::new(&expanded_args[0]);
        child.args(&expanded_args[1..]);
        child
    };

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
        return Err(anyhow!("command failed with status: {status}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_item_titles_deterministically() {
        assert_eq!(normalize_item_title_namespace("service_12").unwrap(), "SERVICE_12");
        assert_eq!(
            normalize_item_title_namespace("team/service-a").unwrap(),
            "TEAM_SERVICE_A"
        );
        assert_eq!(
            normalize_item_title_namespace("  Production   API  ").unwrap(),
            "PRODUCTION_API"
        );
        assert_eq!(normalize_item_title_namespace("café prod").unwrap(), "CAF_PROD");
    }

    #[test]
    fn rejects_empty_normalized_titles() {
        assert!(normalize_item_title_namespace("日本語").is_err());
        assert!(normalize_item_title_namespace("___").is_err());
        assert!(normalize_item_title_namespace("  /-  ").is_err());
    }

    #[test]
    fn preserves_duplicate_labels_from_distinct_items() {
        let sections = vec![
            (
                "service_12".to_string(),
                vec!["API_TOKEN=op://vault1/item1/API_TOKEN".to_string()],
            ),
            (
                "service_18".to_string(),
                vec!["API_TOKEN=op://vault2/item2/API_TOKEN".to_string()],
            ),
        ];

        let namespaced = namespace_env_sections(sections).unwrap();
        assert_eq!(
            namespaced,
            vec![
                (
                    "service_12".to_string(),
                    vec!["SERVICE_12__API_TOKEN=op://vault1/item1/API_TOKEN".to_string()],
                ),
                (
                    "service_18".to_string(),
                    vec!["SERVICE_18__API_TOKEN=op://vault2/item2/API_TOKEN".to_string()],
                ),
            ]
        );
    }

    #[test]
    fn rejects_normalization_collisions_without_exposing_references() {
        let sections = vec![
            (
                "service-a".to_string(),
                vec!["API_TOKEN=op://vault1/item1/API_TOKEN".to_string()],
            ),
            (
                "service_a".to_string(),
                vec!["API_TOKEN=op://vault2/item2/API_TOKEN".to_string()],
            ),
        ];

        let message = namespace_env_sections(sections).unwrap_err().to_string();
        assert!(message.contains("SERVICE_A__API_TOKEN"));
        assert!(message.contains("service-a"));
        assert!(message.contains("service_a"));
        assert!(!message.contains("op://"));
    }

    #[test]
    fn namespaces_show_labels_with_the_same_rules() {
        let sections = vec![(
            "Production API".to_string(),
            vec!["USERNAME".to_string(), "BASE_URL".to_string()],
        )];

        let namespaced = namespace_label_sections(sections).unwrap();
        assert_eq!(
            namespaced[0].1,
            vec![
                "PRODUCTION_API__USERNAME".to_string(),
                "PRODUCTION_API__BASE_URL".to_string(),
            ]
        );
    }

    #[test]
    fn strips_supported_namespace_before_command_separator_only() {
        let args = vec![
            OsString::from("opz"),
            OsString::from("run"),
            OsString::from("--namespace=item-title"),
            OsString::from("service_12"),
            OsString::from("--"),
            OsString::from("echo"),
            OsString::from("--namespace=child-value"),
        ];

        let stripped = strip_namespace_args(&args).unwrap().unwrap();
        assert_eq!(
            stripped,
            vec![
                OsString::from("opz"),
                OsString::from("run"),
                OsString::from("service_12"),
                OsString::from("--"),
                OsString::from("echo"),
                OsString::from("--namespace=child-value"),
            ]
        );
    }

    #[test]
    fn rejects_unknown_or_duplicate_namespace_modes() {
        let unknown = vec![
            OsString::from("opz"),
            OsString::from("--namespace"),
            OsString::from("other"),
            OsString::from("--"),
            OsString::from("env"),
        ];
        assert!(strip_namespace_args(&unknown).is_err());

        let duplicate = vec![
            OsString::from("opz"),
            OsString::from("--namespace=item-title"),
            OsString::from("--namespace"),
            OsString::from("item-title"),
            OsString::from("--"),
            OsString::from("env"),
        ];
        assert!(strip_namespace_args(&duplicate).is_err());
    }
}
