use crate::*;

/// Sections of `(item title, env lines)` collected per requested item.
pub(crate) type ItemSections = Vec<(String, Vec<String>)>;

pub(crate) fn collect_item_env_sections(
    context: &ItemContext,
    items: &[String],
) -> Result<ItemSections> {
    let mut sections = Vec::with_capacity(items.len());

    for item_title in items {
        let (item_id, vault_id, resolved_title, item) =
            find_item(context.vault.as_deref(), item_title)?;
        let env_lines = item_to_env_lines(&item, &vault_id, &item_id)?;
        sections.push((resolved_title, env_lines));
    }

    Ok(sections)
}

pub(crate) fn collect_item_env_sections_with_github_repos(
    context: &ItemContext,
    items: &[String],
) -> Result<(ItemSections, Vec<ItemGithubRepositories>)> {
    let mut sections = Vec::with_capacity(items.len());
    let mut repositories = Vec::with_capacity(items.len());

    for item_title in items {
        let (item_id, vault_id, resolved_title, item) =
            find_item(context.vault.as_deref(), item_title)?;
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
    let mut sections = Vec::with_capacity(items.len());

    for item_title in items {
        let (_, _, resolved_title, item) = find_item(context.vault.as_deref(), item_title)?;
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
        let v = op_json(&["item", "get", item_id, "--format", "json"])?;
        let item: ItemGet = serde_json::from_value(v)?;
        Ok(item)
    })
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
