use crate::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScriptMigration {
    pub(crate) items: Vec<String>,
    pub(crate) uses_dotenv: bool,
    pub(crate) detected_opz: bool,
    pub(crate) rewritten: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ScriptMigrationMode<'a> {
    Collect,
    RenameItems { title: &'a str },
    Restore { title: &'a str },
}

pub(crate) fn migrate_scripts(
    context: &ItemContext,
    dry_run: bool,
    create_new: bool,
    restore: bool,
) -> Result<()> {
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
            create_api_credential_item_from_env(context, item_title, Path::new(".env"))?;
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
            let (item_id, vault_id, resolved_title, item) =
                find_item(context.vault.as_deref(), item_title)?;
            let merged_repos =
                merge_github_repository_lists(&item_github_repositories(&item), &repositories);
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
            if rename_item {
                run_item_edit_title(
                    context.vault.as_deref(),
                    &vault_id,
                    &item_id,
                    &canonical_item_title,
                )?;
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

pub(crate) fn migration_script_paths() -> Result<Vec<PathBuf>> {
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

pub(crate) fn migrate_package_json_scripts(
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

pub(crate) fn replace_json_string_literal(content: &str, old: &str, new: &str) -> Result<String> {
    let old_literal = serde_json::to_string(old)?;
    let new_literal = serde_json::to_string(new)?;
    Ok(content.replacen(&old_literal, &new_literal, 1))
}

pub(crate) fn migrate_script_text(
    content: &str,
    mode: ScriptMigrationMode<'_>,
) -> Result<ScriptMigration> {
    migrate_script_text_with_items(content, &HashMap::new(), mode)
}

pub(crate) fn migrate_justfile_scripts(
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

pub(crate) fn migrate_script_text_with_items(
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

pub(crate) fn is_just_top_level_line(line: &str) -> bool {
    !line.trim().is_empty()
        && !line.trim_start().starts_with('#')
        && line
            .chars()
            .next()
            .is_some_and(|ch| !ch.is_ascii_whitespace())
}

pub(crate) fn parse_just_assignment(
    line: &str,
    values: &HashMap<String, String>,
) -> Option<(String, String)> {
    let (name, value) = line.split_once(":=")?;
    let name = name.trim();
    if !is_just_identifier(name) {
        return None;
    }
    resolve_just_value(value.trim(), values).map(|value| (name.to_string(), value))
}

pub(crate) fn parse_just_recipe_values(
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

pub(crate) fn resolve_just_value(value: &str, values: &HashMap<String, String>) -> Option<String> {
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

pub(crate) fn collect_just_opz_metadata_items(
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

pub(crate) fn first_opz_gen_item_token(args: &str) -> Option<&str> {
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

pub(crate) fn resolve_migration_item_token(
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

pub(crate) fn preferred_restore_item_token(
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

pub(crate) fn rewrite_just_item_assignments(
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

pub(crate) fn is_just_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

pub(crate) fn is_static_item_token(value: &str) -> bool {
    !value.contains('{')
        && !value.contains('}')
        && !value.contains('$')
        && !value.contains('*')
        && !value.contains('?')
        && !value.contains('`')
        && !value.contains('(')
        && !value.contains(')')
}

pub(crate) fn dedupe_preserve_order(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}
