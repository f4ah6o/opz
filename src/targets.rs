use crate::*;

pub(crate) fn set_github_secrets(
    context: &ItemContext,
    repo: Option<&str>,
    dry_run: bool,
    items: &[String],
) -> Result<()> {
    let (sections, item_repositories) = instrumentation::with_span_result(
        "load_inputs",
        vec![KeyValue::new("item.count", items.len() as i64)],
        || collect_item_env_sections_with_github_repos(context, items),
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
pub(crate) struct ItemGithubRepositories {
    pub(crate) item_title: String,
    pub(crate) repositories: Vec<String>,
}

pub(crate) fn guard_github_secret_repo(
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
pub(crate) struct CloudflareSecretTarget<'a> {
    pub(crate) name: Option<&'a str>,
    pub(crate) env: Option<&'a str>,
    pub(crate) config: Option<&'a Path>,
}

pub(crate) fn set_cloudflare_secrets(
    context: &ItemContext,
    target: CloudflareSecretTarget<'_>,
    dry_run: bool,
    items: &[String],
) -> Result<()> {
    let sections = instrumentation::with_span_result(
        "load_inputs",
        vec![KeyValue::new("item.count", items.len() as i64)],
        || collect_item_env_sections(context, items),
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

pub(crate) fn cloudflare_target_label(target: CloudflareSecretTarget<'_>) -> String {
    let worker = target.name.unwrap_or("wrangler config default worker");
    match target.env {
        Some(env) => format!("{worker} ({env})"),
        None => worker.to_string(),
    }
}

pub(crate) fn validate_cloudflare_secret_lines(env_lines: &[String]) -> Result<Vec<String>> {
    validate_secret_lines(env_lines, "Cloudflare")
}

pub(crate) fn validate_github_secret_lines(env_lines: &[String]) -> Result<Vec<String>> {
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

pub(crate) fn validate_secret_lines(
    env_lines: &[String],
    target_name: &str,
) -> Result<Vec<String>> {
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
pub(crate) fn validate_github_secret_name(name: &str) -> Result<()> {
    validate_secret_name(name, "GitHub")?;
    if name.to_ascii_uppercase().starts_with("GITHUB_") {
        return Err(anyhow!(
            "GitHub secret name cannot start with reserved prefix GITHUB_: {name}"
        ));
    }
    Ok(())
}

pub(crate) fn validate_secret_name(name: &str, target_name: &str) -> Result<()> {
    let re = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$")?;
    if !re.is_match(name) {
        return Err(anyhow!("Invalid {target_name} secret name: {name}"));
    }
    Ok(())
}

pub(crate) fn build_gh_secret_set_args(repo: &str, name: &str) -> Vec<String> {
    vec![
        "secret".to_string(),
        "set".to_string(),
        name.to_string(),
        "--repo".to_string(),
        repo.to_string(),
    ]
}

pub(crate) fn build_wrangler_secret_bulk_args(target: CloudflareSecretTarget<'_>) -> Vec<String> {
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

pub(crate) fn build_secret_json_payload(
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

pub(crate) fn resolve_current_github_repo() -> Result<String> {
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

pub(crate) fn run_gh_secret_set(repo: &str, name: &str, value: &str) -> Result<()> {
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

pub(crate) fn run_wrangler_secret_bulk(
    target: CloudflareSecretTarget<'_>,
    payload: &[u8],
) -> Result<()> {
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
