use crate::*;

#[derive(Clone, Deserialize, Serialize, Debug)]
pub(crate) struct ItemListEntry {
    pub(crate) id: String,
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) vault: Option<ItemVault>,
}
#[derive(Clone, Deserialize, Serialize, Debug)]
pub(crate) struct ItemVault {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Deserialize)]
pub(crate) struct ItemGet {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) fields: Vec<ItemField>,
    #[serde(default)]
    pub(crate) vault: Option<ItemVault>,
}
#[derive(Deserialize)]
pub(crate) struct ItemField {
    #[serde(default)]
    pub(crate) label: Option<String>,
    #[serde(default)]
    pub(crate) value: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub(crate) struct ItemCreateTemplate {
    pub(crate) title: String,
    pub(crate) category: String,
    pub(crate) fields: Vec<ItemCreateField>,
}

#[derive(Serialize)]
pub(crate) struct ItemCreateField {
    pub(crate) id: String,
    #[serde(rename = "type")]
    pub(crate) field_type: String,
    pub(crate) label: String,
    pub(crate) value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) purpose: Option<String>,
}

pub(crate) fn create_api_credential_item_from_env(
    context: &ItemContext,
    item_title: &str,
    env_file: &Path,
) -> Result<()> {
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
            build_create_item_args(context.vault.as_deref()),
            build_api_credential_template(item_title, &env_pairs, &github_repositories),
        )
    });
    instrumentation::with_span_result("write_outputs", vec![], || {
        run_op_item_create(&args, &template)?;
        invalidate_item_list_cache_best_effort();
        Ok(())
    })
}

/// Item lookup options shared below the clap layer.
#[derive(Clone, Debug, Default)]
pub(crate) struct ItemContext {
    pub(crate) vault: Option<String>,
}

pub(crate) fn build_create_item_args(vault: Option<&str>) -> Vec<String> {
    let mut args = vec!["item".to_string(), "create".to_string()];

    if let Some(v) = vault {
        args.push("--vault".to_string());
        args.push(v.to_string());
    }

    args.push("-".to_string());
    args
}

pub(crate) fn build_api_credential_template(
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

pub(crate) fn create_secure_notes_from_file(context: &ItemContext, file_path: &Path) -> Result<()> {
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
            let args = build_create_item_args(context.vault.as_deref());
            let template = build_secure_note_template(&item_title, &body);
            run_op_item_create(&args, &template)?;
        }
        invalidate_item_list_cache_best_effort();
        Ok(())
    })
}

pub(crate) fn update_github_repositories_metadata(
    context: &ItemContext,
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
                let (item_id, vault_id, resolved_title, item) =
                    find_item(context.vault.as_deref(), item_title)?;
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

                run_item_edit_github_repositories(
                    context.vault.as_deref(),
                    &vault_id,
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

pub(crate) fn resolve_requested_github_repositories(repos: &[String]) -> Result<Vec<String>> {
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

pub(crate) fn merge_github_repository_lists(
    existing: &[String],
    requested: &[String],
) -> Vec<String> {
    let mut repos = Vec::with_capacity(existing.len() + requested.len());
    repos.extend(existing.iter().cloned());
    repos.extend(requested.iter().cloned());
    dedupe_github_repositories(&repos)
}

pub(crate) fn dedupe_github_repositories(repos: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    repos
        .iter()
        .filter_map(|repo| normalize_github_repo_spec(repo))
        .filter(|repo| seen.insert(repo.clone()))
        .collect()
}

pub(crate) fn build_op_item_edit_github_repositories_args(
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

pub(crate) fn set_sdk_item_text_field(
    item: &mut serde_json::Value,
    field_name: &str,
    value: &str,
) -> Result<()> {
    let fields = item
        .get_mut("fields")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| anyhow!("desktop SDK item fields were malformed"))?;
    if let Some(field) = fields.iter_mut().find(|field| {
        field.get("title").and_then(serde_json::Value::as_str) == Some(field_name)
            || field.get("id").and_then(serde_json::Value::as_str) == Some(field_name)
    }) {
        let object = field
            .as_object_mut()
            .ok_or_else(|| anyhow!("desktop SDK item field was malformed"))?;
        object.insert(
            "value".to_string(),
            serde_json::Value::String(value.to_string()),
        );
        return Ok(());
    }
    fields.push(serde_json::json!({
        "id": field_name,
        "title": field_name,
        "fieldType": "Text",
        "value": value,
    }));
    Ok(())
}

pub(crate) fn set_sdk_item_title(item: &mut serde_json::Value, title: &str) -> Result<()> {
    let object = item
        .as_object_mut()
        .ok_or_else(|| anyhow!("desktop SDK item was malformed"))?;
    object.insert(
        "title".to_string(),
        serde_json::Value::String(title.to_string()),
    );
    Ok(())
}

fn try_sdk_item_edit(
    vault_id: &str,
    item_id: &str,
    mutate: impl FnOnce(&mut serde_json::Value) -> Result<()>,
) -> Option<Result<()>> {
    if !desktop_sdk_enabled() {
        return None;
    }
    let account = desktop_sdk_account()?;
    Some((|| {
        let mut item = sdk_bridge_call(
            &account,
            "items_get",
            serde_json::json!({"vault_id": vault_id, "item_id": item_id}),
        )?;
        mutate(&mut item)?;
        sdk_bridge_call(&account, "items_put", serde_json::json!({"item": item}))?;
        Ok(())
    })())
}

pub(crate) fn run_item_edit_github_repositories(
    vault: Option<&str>,
    vault_id: &str,
    item_id: &str,
    repositories: &[String],
) -> Result<()> {
    let value = repositories.join("\n");
    if let Some(Ok(())) = try_sdk_item_edit(vault_id, item_id, |item| {
        set_sdk_item_text_field(item, GITHUB_REPOSITORIES_LABEL, &value)
    }) {
        return Ok(());
    }
    run_op_item_edit_github_repositories(vault, item_id, repositories)
}

pub(crate) fn run_item_edit_title(
    vault: Option<&str>,
    vault_id: &str,
    item_id: &str,
    title: &str,
) -> Result<()> {
    if let Some(Ok(())) =
        try_sdk_item_edit(vault_id, item_id, |item| set_sdk_item_title(item, title))
    {
        return Ok(());
    }
    run_op_item_edit_title(vault, item_id, title)
}

pub(crate) fn run_op_item_edit_github_repositories(
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

pub(crate) fn run_op_item_edit_title(
    vault: Option<&str>,
    item_id: &str,
    title: &str,
) -> Result<()> {
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

pub(crate) fn build_secure_note_body(file_name: &str, content: &str) -> String {
    let mut body = format!("```{}\n", file_name);
    body.push_str(content);
    if !content.ends_with('\n') {
        body.push('\n');
    }
    body.push_str("```");
    body
}

pub(crate) fn build_secure_note_template(item_title: &str, body: &str) -> ItemCreateTemplate {
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

pub(crate) fn run_op_item_create(args: &[String], template: &ItemCreateTemplate) -> Result<()> {
    instrumentation::with_span_result(
        "write_outputs.op_item_create",
        vec![KeyValue::new("op.arg_count", args.len() as i64)],
        || {
            let sensitive_fields = collect_create_stdout_sensitive_fields(template);
            let redactor =
                Redactor::from_strings(sensitive_fields.iter().map(|(_, value)| value.clone()));
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

            redactor.write_stderr(&output.stderr)?;

            if !output.status.success() {
                redactor.write_stdout(&output.stdout)?;
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

pub(crate) fn list_remote_repo_names() -> Result<Vec<String>> {
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

pub(crate) fn extract_org_repo_from_remote_url(url: &str) -> Option<String> {
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

pub(crate) fn dedupe_titles_with_sequence(base_titles: &[String]) -> Vec<String> {
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

pub(crate) fn item_to_env_lines(
    item: &ItemGet,
    vault_id: &str,
    item_id: &str,
) -> Result<Vec<String>> {
    let labels = collect_item_labels(item)?;
    let mut out = Vec::new();

    for label in labels {
        let reference = format!("op://{}/{}/{}", vault_id, item_id, label);
        out.push(format!("{k}={v}", k = label, v = reference));
    }

    Ok(out)
}

pub(crate) fn item_github_repositories(item: &ItemGet) -> Vec<String> {
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

pub(crate) fn item_field_string_value(field: &ItemField) -> Option<String> {
    match field.value.as_ref()? {
        serde_json::Value::String(value) => Some(value.clone()),
        value => value.as_str().map(str::to_string),
    }
}

pub(crate) fn parse_github_repositories_value(value: &str) -> Vec<String> {
    value
        .split([',', '\n', '\r', '\t'])
        .filter_map(normalize_github_repo_spec)
        .collect()
}

pub(crate) fn normalize_github_repo_spec(value: &str) -> Option<String> {
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

pub(crate) fn collect_item_labels(item: &ItemGet) -> Result<Vec<String>> {
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

pub(crate) fn item_to_valid_labels(item: &ItemGet) -> Result<Vec<String>> {
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

pub(crate) fn is_metadata_label(label: &str) -> bool {
    label.eq_ignore_ascii_case(GITHUB_REPOSITORIES_LABEL) || is_plugin_metadata_label(label)
}

/// Parse env line to extract key name (e.g., "KEY=value" -> "KEY")
pub(crate) fn parse_env_key(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    trimmed.split('=').next()
}

/// Parse env line to extract key and value (e.g., "KEY=value" -> ("KEY", "value"))
pub(crate) fn parse_env_line_kv(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    trimmed.split_once('=')
}

/// Read a secret from 1Password using op read
pub(crate) fn op_read(reference: &str) -> Result<String> {
    instrumentation::with_span_result("load_inputs.op_read", vec![], || {
        let mut cmd = Command::new("op");
        cmd.arg("read").arg(reference);
        cmd.stdin(Stdio::null());
        let out = command_output_with_timeout(cmd, "`op read`", op_command_timeout())?;

        if !out.status.success() {
            return Err(anyhow!("op read failed with status: {}", out.status));
        }

        Ok(String::from_utf8(out.stdout)?.trim().to_string())
    })
}
