use crate::*;

/// Sections of `(item title, env lines)` collected per requested item.
pub(crate) type ItemSections = Vec<(String, Vec<String>)>;
type ResolvedItem = (String, String, String, ItemGet);

pub(crate) fn collect_item_env_sections(
    context: &ItemContext,
    items: &[String],
) -> Result<ItemSections> {
    let resolved = find_items(context.vault.as_deref(), items)?;
    let mut sections = Vec::with_capacity(resolved.len());

    for (item_id, vault_id, resolved_title, item) in resolved {
        let env_lines = item_to_env_lines(&item, &vault_id, &item_id)?;
        sections.push((resolved_title, env_lines));
    }

    Ok(sections)
}

pub(crate) fn collect_item_env_sections_with_github_repos(
    context: &ItemContext,
    items: &[String],
) -> Result<(ItemSections, Vec<ItemGithubRepositories>)> {
    let resolved = find_items(context.vault.as_deref(), items)?;
    let mut sections = Vec::with_capacity(resolved.len());
    let mut repositories = Vec::with_capacity(resolved.len());

    for (item_id, vault_id, resolved_title, item) in resolved {
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

pub(crate) fn collect_item_label_sections(
    context: &ItemContext,
    items: &[String],
) -> Result<ItemSections> {
    let resolved = find_items(context.vault.as_deref(), items)?;
    let mut sections = Vec::with_capacity(resolved.len());

    for (_, _, resolved_title, item) in resolved {
        let labels = item_to_valid_labels(&item)?;
        sections.push((resolved_title, labels));
    }

    Ok(sections)
}

pub(crate) fn resolve_run_items(context: &ItemContext, items: &[String]) -> Result<Vec<String>> {
    if !items.is_empty() {
        return Ok(items.to_vec());
    }

    let repositories = list_remote_repo_names()
        .context("No item specified and failed to auto-detect a repository from git remotes")?;
    let matches =
        match_item_titles_by_github_repository_titles(context.vault.as_deref(), &repositories)?;
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

pub(crate) fn match_item_titles_by_github_repository_titles(
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

pub(crate) fn is_op_lookup_miss(err: &anyhow::Error) -> bool {
    let text = err.to_string();
    text.contains("isn't an item")
        || text.contains("is not an item")
        || text.contains("No item matched")
        || text.contains("No exact item matched")
        || text.contains("not found")
}

pub(crate) fn legacy_autodetect_scan_enabled() -> bool {
    env::var("OPZ_AUTODETECT_LEGACY_SCAN")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

pub(crate) fn match_item_titles_by_github_repositories(
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

pub(crate) fn merge_env_lines(sections: &[(String, Vec<String>)]) -> Vec<String> {
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

pub(crate) fn resolve_env_vars(env_lines: &[String]) -> Result<HashMap<String, SecretValue>> {
    let references: Vec<(String, String)> = env_lines
        .iter()
        .filter_map(|line| {
            parse_env_line_kv(line).map(|(key, reference)| (key.to_string(), reference.to_string()))
        })
        .collect();
    if references.is_empty() {
        return Ok(HashMap::new());
    }

    if let Some(Ok(env_vars)) = try_resolve_env_vars_sdk(&references) {
        return Ok(env_vars);
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
    let mut env_vars: HashMap<String, SecretValue> = HashMap::with_capacity(references.len());
    for line in env_lines {
        if let Some((key, reference)) = parse_env_line_kv(line) {
            let value = op_read(reference)?;
            env_vars.insert(key.to_string(), SecretValue::new(value));
        }
    }

    Ok(env_vars)
}

fn try_resolve_env_vars_sdk(
    references: &[(String, String)],
) -> Option<Result<HashMap<String, SecretValue>>> {
    if !desktop_sdk_enabled() {
        return None;
    }
    let account = desktop_sdk_account()?;
    Some(instrumentation::with_span_result(
        "load_inputs.desktop_sdk_batch_resolve",
        vec![KeyValue::new(
            "env.reference_count",
            references.len() as i64,
        )],
        || {
            let secret_references = references
                .iter()
                .map(|(_, reference)| reference.as_str())
                .collect::<Vec<_>>();
            let response = sdk_bridge_call(
                &account,
                "secrets_resolve_all",
                serde_json::json!({"references": secret_references}),
            )?;
            let values: Vec<String> = serde_json::from_value(response)
                .context("parse isolated Desktop SDK secret response")?;
            if values.len() != references.len() {
                return Err(anyhow!(
                    "desktop SDK resolution was incomplete ({}/{})",
                    values.len(),
                    references.len()
                ));
            }
            Ok(references
                .iter()
                .zip(values)
                .map(|((key, _), value)| (key.clone(), SecretValue::new(value)))
                .collect())
        },
    ))
}

pub(crate) fn desktop_sdk_enabled() -> bool {
    if env::var_os("OPZ_TEST_SCENARIO").is_some() {
        return false;
    }
    !env::var("OPZ_ONEPASSWORD_SDK")
        .map(|value| matches!(value.as_str(), "0" | "false" | "FALSE" | "off" | "OFF"))
        .unwrap_or(false)
}

static DESKTOP_SDK_ACCOUNT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub(crate) fn desktop_sdk_account() -> Option<String> {
    if let Ok(account) = env::var("OP_ACCOUNT") {
        let account = account.trim();
        if !account.is_empty() {
            return Some(account.to_string());
        }
    }
    if let Some(account) = DESKTOP_SDK_ACCOUNT.get() {
        return Some(account.clone());
    }
    let accounts = op_json(&["account", "list", "--format", "json"]).ok()?;
    let account = desktop_sdk_account_from_list(&accounts)?;
    let _ = DESKTOP_SDK_ACCOUNT.set(account.clone());
    Some(account)
}

pub(crate) fn desktop_sdk_account_from_list(accounts: &serde_json::Value) -> Option<String> {
    let accounts = accounts.as_array()?;
    let [account] = accounts.as_slice() else {
        return None;
    };
    account
        .get("account_uuid")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub(crate) fn sdk_vaults(account: &str) -> Result<Vec<ItemVault>> {
    let response = sdk_bridge_call(account, "vaults_list", serde_json::json!({}))?;
    response
        .as_array()
        .ok_or_else(|| anyhow!("isolated Desktop SDK vault response was not an array"))?
        .iter()
        .map(|vault| {
            let id = vault
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("desktop SDK vault response omitted id"))?;
            let name = vault
                .get("title")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("desktop SDK vault response omitted title"))?;
            Ok(ItemVault {
                id: id.to_owned(),
                name: name.to_owned(),
            })
        })
        .collect()
}

pub(crate) fn select_sdk_vaults(
    vaults: &[ItemVault],
    vault: Option<&str>,
) -> Result<Vec<ItemVault>> {
    let Some(spec) = vault else {
        return Ok(vaults.to_vec());
    };
    let matches = vaults
        .iter()
        .filter(|candidate| candidate.id == spec || candidate.name == spec)
        .cloned()
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [selected] => Ok(vec![selected.clone()]),
        [] => Err(anyhow!("desktop SDK did not find vault `{spec}`")),
        _ => Err(anyhow!("desktop SDK vault name `{spec}` is ambiguous")),
    }
}

fn sdk_item_list_entry(value: &serde_json::Value, vault: &ItemVault) -> Result<ItemListEntry> {
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("desktop SDK item overview omitted id"))?;
    let title = value
        .get("title")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("desktop SDK item overview omitted title"))?;
    Ok(ItemListEntry {
        id: id.to_owned(),
        title: title.to_owned(),
        vault: Some(vault.clone()),
    })
}

pub(crate) fn sdk_item_get(value: &serde_json::Value, vault: &ItemVault) -> Result<ItemGet> {
    let fields = value
        .get("fields")
        .and_then(serde_json::Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .map(|field| ItemField {
                    label: field
                        .get("title")
                        .or_else(|| field.get("label"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    value: field.get("value").cloned(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(ItemGet {
        id: value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        title: value
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        fields,
        vault: Some(vault.clone()),
    })
}

fn try_item_list_sdk(vault: Option<&str>) -> Option<Result<Vec<ItemListEntry>>> {
    if !desktop_sdk_enabled() {
        return None;
    }
    let account = desktop_sdk_account()?;
    Some((|| {
        let vaults = sdk_vaults(&account)?;
        let selected = select_sdk_vaults(&vaults, vault)?;
        let mut entries = Vec::new();
        for selected_vault in selected {
            let items = sdk_bridge_call(
                &account,
                "items_list",
                serde_json::json!({"vault_id": selected_vault.id}),
            )?;
            let items = items
                .as_array()
                .ok_or_else(|| anyhow!("isolated Desktop SDK item list was not an array"))?;
            entries.extend(
                items
                    .iter()
                    .map(|item| sdk_item_list_entry(item, &selected_vault))
                    .collect::<Result<Vec<_>>>()?,
            );
        }
        Ok(entries)
    })())
}

fn try_find_item_exact_sdk(
    vault: Option<&str>,
    item_title: &str,
) -> Option<Result<Option<ResolvedItem>>> {
    if !desktop_sdk_enabled() {
        return None;
    }
    let account = desktop_sdk_account()?;
    Some((|| {
        let matches = item_list_cached(vault)?
            .into_iter()
            .filter(|entry| entry.title == item_title)
            .collect::<Vec<_>>();
        let [entry] = matches.as_slice() else {
            return if matches.is_empty() {
                Ok(None)
            } else {
                Err(anyhow!(
                    "desktop SDK found multiple exact items titled `{item_title}`"
                ))
            };
        };
        let selected_vault = entry
            .vault
            .as_ref()
            .ok_or_else(|| anyhow!("desktop SDK item overview omitted vault metadata"))?;
        let value = sdk_bridge_call(
            &account,
            "items_get",
            serde_json::json!({"vault_id": selected_vault.id, "item_id": entry.id}),
        )?;
        let item = sdk_item_get(&value, selected_vault)?;
        Ok(Some((
            entry.id.clone(),
            selected_vault.id.clone(),
            entry.title.clone(),
            item,
        )))
    })())
}

fn try_github_repository_index_sdk(
    vault: Option<&str>,
) -> Option<Result<Vec<ItemGithubRepositories>>> {
    if !desktop_sdk_enabled() {
        return None;
    }
    let account = desktop_sdk_account()?;
    Some((|| {
        let vaults = sdk_vaults(&account)?;
        let selected = select_sdk_vaults(&vaults, vault)?;
        let mut index = Vec::new();
        for selected_vault in selected {
            let overviews = sdk_bridge_call(
                &account,
                "items_list",
                serde_json::json!({"vault_id": selected_vault.id}),
            )?;
            let overviews = overviews
                .as_array()
                .ok_or_else(|| anyhow!("isolated Desktop SDK item list was not an array"))?;
            for chunk in overviews.chunks(100) {
                let ids = chunk
                    .iter()
                    .map(|item| {
                        item.get("id")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| anyhow!("desktop SDK item overview omitted id"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                if ids.is_empty() {
                    continue;
                }
                let response = sdk_bridge_call(
                    &account,
                    "items_get_all",
                    serde_json::json!({"vault_id": selected_vault.id, "item_ids": ids}),
                )?;
                let responses = response
                    .get("individualResponses")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| anyhow!("desktop SDK item batch response was malformed"))?;
                if responses.len() != ids.len() {
                    return Err(anyhow!("desktop SDK item batch response was incomplete"));
                }
                for response in responses {
                    let content = response
                        .get("content")
                        .ok_or_else(|| anyhow!("desktop SDK item batch contained an error"))?;
                    let item = sdk_item_get(content, &selected_vault)?;
                    let repositories = item_github_repositories(&item);
                    if !repositories.is_empty() {
                        index.push(ItemGithubRepositories {
                            item_title: item.title.clone().unwrap_or_default(),
                            repositories,
                        });
                    }
                }
            }
        }
        Ok(index)
    })())
}

pub(crate) fn resolve_env_vars_batch(
    references: &[(String, String)],
) -> Result<HashMap<String, SecretValue>> {
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
                return Err(anyhow!("op run failed with status: {}", out.status));
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
                    env_vars.insert(key.to_string(), SecretValue::new(value));
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

pub(crate) fn should_fallback_to_op_read(err: &anyhow::Error) -> bool {
    !is_op_timeout_error(err)
}

pub(crate) fn is_op_timeout_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.to_string().contains("timed out after"))
}

pub(crate) fn print_sectioned_env_output(sections: &[(String, Vec<String>)]) {
    print!("{}", sectioned_env_output_string(sections));
}

pub(crate) fn sectioned_env_output_string(sections: &[(String, Vec<String>)]) -> String {
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

pub(crate) fn show_item_labels(
    context: &ItemContext,
    items: &[String],
    with_item: bool,
) -> Result<()> {
    let sections = instrumentation::with_span_result(
        "load_inputs",
        vec![KeyValue::new("item.count", items.len() as i64)],
        || collect_item_label_sections(context, items),
    )?;
    let rendered = instrumentation::with_span("main_operation", vec![], || {
        show_output_string(&sections, with_item)
    });
    instrumentation::with_span("write_outputs", vec![], || {
        print!("{rendered}");
    });
    Ok(())
}

pub(crate) fn show_output_string(sections: &[(String, Vec<String>)], with_item: bool) -> String {
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

fn find_items(vault: Option<&str>, item_titles: &[String]) -> Result<Vec<ResolvedItem>> {
    if item_titles.len() > 1 {
        if let Some(Ok(items)) = try_find_items_exact_sdk(vault, item_titles) {
            return Ok(items);
        }
    }
    item_titles
        .iter()
        .map(|item_title| find_item(vault, item_title))
        .collect()
}

fn try_find_items_exact_sdk(
    vault: Option<&str>,
    item_titles: &[String],
) -> Option<Result<Vec<ResolvedItem>>> {
    if !desktop_sdk_enabled() {
        return None;
    }
    let account = desktop_sdk_account()?;
    Some((|| {
        let entries = item_list_cached(vault)?;
        let selected = select_exact_item_entries(&entries, item_titles)?;
        let mut groups: Vec<(ItemVault, Vec<usize>)> = Vec::new();
        for (index, entry) in selected.iter().enumerate() {
            let item_vault = entry
                .vault
                .clone()
                .ok_or_else(|| anyhow!("desktop SDK item overview omitted vault metadata"))?;
            if let Some((_, indices)) = groups
                .iter_mut()
                .find(|(candidate, _)| candidate.id == item_vault.id)
            {
                indices.push(index);
            } else {
                groups.push((item_vault, vec![index]));
            }
        }

        let mut resolved: Vec<Option<ResolvedItem>> = std::iter::repeat_with(|| None)
            .take(selected.len())
            .collect();
        for (item_vault, indices) in groups {
            for chunk in indices.chunks(100) {
                let item_ids = chunk
                    .iter()
                    .map(|index| selected[*index].id.as_str())
                    .collect::<Vec<_>>();
                let response = sdk_bridge_call(
                    &account,
                    "items_get_all",
                    serde_json::json!({"vault_id": item_vault.id, "item_ids": item_ids}),
                )?;
                let responses = response
                    .get("individualResponses")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| anyhow!("desktop SDK item batch response was malformed"))?;
                if responses.len() != chunk.len() {
                    return Err(anyhow!("desktop SDK item batch response was incomplete"));
                }
                for (index, response) in chunk.iter().copied().zip(responses) {
                    let content = response
                        .get("content")
                        .ok_or_else(|| anyhow!("desktop SDK item batch contained an error"))?;
                    let entry = &selected[index];
                    let item = sdk_item_get(content, &item_vault)?;
                    resolved[index] = Some((
                        entry.id.clone(),
                        item_vault.id.clone(),
                        entry.title.clone(),
                        item,
                    ));
                }
            }
        }
        resolved
            .into_iter()
            .map(|item| item.ok_or_else(|| anyhow!("desktop SDK item batch was incomplete")))
            .collect()
    })())
}

pub(crate) fn select_exact_item_entries(
    entries: &[ItemListEntry],
    item_titles: &[String],
) -> Result<Vec<ItemListEntry>> {
    item_titles
        .iter()
        .map(|item_title| {
            let matches = entries
                .iter()
                .filter(|entry| entry.title == *item_title)
                .cloned()
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [entry] => Ok(entry.clone()),
                [] => Err(anyhow!(
                    "desktop SDK batch lookup requires exact item title `{item_title}`"
                )),
                _ => Err(anyhow!(
                    "desktop SDK found multiple exact items titled `{item_title}`"
                )),
            }
        })
        .collect()
}

/// Find and match an item by title.
pub(crate) fn find_item(
    vault: Option<&str>,
    item_title: &str,
) -> Result<(String, String, String, ItemGet)> {
    match find_item_exact(vault, item_title) {
        Ok(item) => return Ok(item),
        Err(err) if is_op_lookup_miss(&err) => {}
        Err(err) => return Err(err),
    }

    find_item_from_cached_list(vault, item_title)
}

pub(crate) fn find_item_from_cached_list(
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
pub(crate) fn find_item_exact(
    vault: Option<&str>,
    item_title: &str,
) -> Result<(String, String, String, ItemGet)> {
    if let Some(Ok(Some(item))) = try_find_item_exact_sdk(vault, item_title) {
        return Ok(item);
    }
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

pub(crate) fn resolve_vault_id(
    list_vault: Option<&ItemVault>,
    item_vault: Option<&ItemVault>,
) -> Option<String> {
    list_vault.or(item_vault).map(|v| v.id.clone())
}

pub(crate) fn generate_env_output(
    context: &ItemContext,
    items: &[String],
    env_file: Option<&Path>,
) -> Result<()> {
    let sections = instrumentation::with_span_result(
        "load_inputs",
        vec![KeyValue::new("item.count", items.len() as i64)],
        || collect_item_env_sections(context, items),
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

/// Cache item-list metadata to speed up repeated runs.
pub(crate) fn item_list_cached(vault: Option<&str>) -> Result<Vec<ItemListEntry>> {
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

            let items = if let Some(Ok(items)) = try_item_list_sdk(vault) {
                items
            } else {
                let mut args = vec!["item", "list", "--format", "json"];
                if let Some(v) = vault {
                    args.push("--vault");
                    args.push(v);
                }
                instrumentation::with_span_result("load_inputs.item_list_fetch", vec![], || {
                    let v = op_json(&args)?;
                    let items: Vec<ItemListEntry> = serde_json::from_value(v)?;
                    Ok(items)
                })?
            };
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

pub(crate) fn item_github_repository_index_cached(
    vault: Option<&str>,
) -> Result<Vec<(String, Vec<String>)>> {
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

            let index = if let Some(Ok(index)) = try_github_repository_index_sdk(vault) {
                index
            } else {
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
                index
            };

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

pub(crate) struct TempEnvFile {
    path: PathBuf,
    file: fs::File,
}

impl TempEnvFile {
    pub(crate) fn create() -> Result<Self> {
        let dir = env::temp_dir();
        for attempt in 0..100 {
            let path = dir.join(format!(
                "opz-env-{}-{}-{attempt}.env",
                std::process::id(),
                stable_hex_hash(&format!("{:?}", SystemTime::now()))
            ));
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => return Ok(Self { path, file }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err).with_context(|| format!("create {}", path.display())),
            }
        }

        Err(anyhow!("failed to create unique temp env file"))
    }

    pub(crate) fn path(&self) -> &Path {
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
pub(crate) enum CachePlatform {
    Windows,
    Macos,
    Other,
}

pub(crate) fn item_list_cache_dir() -> Result<PathBuf> {
    let platform = if cfg!(target_os = "windows") {
        CachePlatform::Windows
    } else if cfg!(target_os = "macos") {
        CachePlatform::Macos
    } else {
        CachePlatform::Other
    };
    item_list_cache_dir_from_env(platform, |key| env::var_os(key))
}

pub(crate) fn item_list_cache_dir_from_env(
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

pub(crate) fn cache_file_path(vault: Option<&str>) -> Result<PathBuf> {
    let base = item_list_cache_dir()?;
    let key = vault.unwrap_or("_all_");
    let name = format!("item_list_{}.json", stable_hex_hash(key));
    Ok(base.join(name))
}

pub(crate) fn github_repository_index_cache_file_path(vault: Option<&str>) -> Result<PathBuf> {
    let base = item_list_cache_dir()?;
    let key = format!("github_repositories:{}", vault.unwrap_or("_all_"));
    let name = format!("github_repository_index_{}.json", stable_hex_hash(&key));
    Ok(base.join(name))
}

pub(crate) fn stable_hex_hash(input: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub(crate) fn invalidate_item_list_cache() -> Result<()> {
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

pub(crate) fn invalidate_item_list_cache_best_effort() {
    if let Err(err) = invalidate_item_list_cache() {
        eprintln!("Warning: failed to invalidate item list cache: {err}");
    }
}

pub(crate) fn item_get(item_id: &str) -> Result<ItemGet> {
    instrumentation::with_span_result("load_inputs.item_get", vec![], || {
        if let Some(Ok(item)) = try_item_get_sdk(item_id) {
            return Ok(item);
        }
        let v = op_json(&["item", "get", item_id, "--format", "json"])?;
        let item: ItemGet = serde_json::from_value(v)?;
        Ok(item)
    })
}

fn try_item_get_sdk(item_id: &str) -> Option<Result<ItemGet>> {
    if !desktop_sdk_enabled() {
        return None;
    }
    let account = desktop_sdk_account()?;
    let vault = item_list_cached(None)
        .ok()?
        .into_iter()
        .find(|entry| entry.id == item_id)?
        .vault?;
    Some((|| {
        let value = sdk_bridge_call(
            &account,
            "items_get",
            serde_json::json!({"vault_id": vault.id, "item_id": item_id}),
        )?;
        sdk_item_get(&value, &vault)
    })())
}

pub(crate) fn item_get_with_vault(vault: Option<&str>, item: &str) -> Result<ItemGet> {
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
