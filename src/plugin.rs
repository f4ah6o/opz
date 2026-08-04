use crate::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::OpenOptions;
use std::path::{Component, Path, PathBuf};

const PLUGIN_REGISTRY_DIR_ENV: &str = "OPZ_PLUGIN_REGISTRY_DIR";
const PLUGIN_SCHEMA_VERSION_LABEL: &str = "OPZ_PLUGIN_SCHEMA_VERSION";
const PLUGIN_LABEL: &str = "OPZ_PLUGIN";
const PLUGIN_SOURCE_LABEL: &str = "OPZ_PLUGIN_SOURCE";
const PLUGIN_VERSION_LABEL: &str = "OPZ_PLUGIN_VERSION";
const PLUGIN_SHA256_LABEL: &str = "OPZ_PLUGIN_SHA256";
const PLUGIN_CONFIG_LABEL: &str = "OPZ_PLUGIN_CONFIG";
const MAX_PLUGIN_CONFIG_BYTES: usize = 16 * 1024;
const PROTECTED_ENV: &[&str] = &[
    "HOME",
    "PATH",
    "SHELL",
    "TMPDIR",
    "PWD",
    "OLDPWD",
    "USER",
    "LOGNAME",
    "LD_PRELOAD",
    "DYLD_INSERT_LIBRARIES",
    "PYTHONPATH",
    "NODE_OPTIONS",
    "RUSTFLAGS",
];

static BUNDLED_REGISTRY: &str = include_str!("../vendor/opz-plugin/registry.toml");
static BUNDLED_CLAUDE_ENV: &str =
    include_str!("../vendor/opz-plugin/plugins/claude-env/plugin.toml");
static BUNDLED_CODEX_OPENAI: &str =
    include_str!("../vendor/opz-plugin/plugins/codex-openai/plugin.toml");
static BUNDLED_OPENCODE_GO_CODEX: &str =
    include_str!("../vendor/opz-plugin/plugins/opencode-go-codex/plugin.toml");

#[derive(Subcommand, Debug)]
pub(crate) enum PluginCommand {
    /// List available declarative plugin releases
    List,

    /// Show a digest-verified plugin manifest
    Show {
        /// Plugin name, optionally pinned as NAME@VERSION
        selector: String,
    },

    /// Run a target command using a plugin pinned by one 1Password item
    Run {
        /// Plugin name, optionally pinned as NAME@VERSION
        selector: String,

        /// 1Password item title. Defaults to repository auto-detection.
        #[arg(long, value_name = "ITEM")]
        item: Option<String>,

        /// Explicitly permit a deprecated release. Revoked releases remain blocked.
        #[arg(long)]
        allow_deprecated: bool,

        /// Target command to run (after --)
        #[arg(last = true)]
        command: Vec<String>,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginRegistry {
    schema_version: u32,
    #[serde(default)]
    plugins: Vec<PluginRegistryEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginRegistryEntry {
    name: String,
    version: String,
    source: String,
    path: String,
    sha256: String,
    plugin_schema_version: u32,
    lifecycle: PluginLifecycle,
    description: String,
    target_commands: Vec<String>,
    min_opz_version: String,
    #[serde(default)]
    replacement: Option<String>,
    #[serde(default)]
    revocation_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum PluginLifecycle {
    Active,
    Deprecated,
    Revoked,
}

impl PluginLifecycle {
    fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Deprecated => "deprecated",
            Self::Revoked => "revoked",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PluginManifest {
    schema_version: u32,
    name: String,
    version: String,
    description: String,
    target_commands: Vec<String>,
    required_env: Vec<String>,
    secret_env_allowlist: Vec<String>,
    #[serde(default)]
    config: BTreeMap<String, PluginConfigField>,
    #[serde(default)]
    defaults: BTreeMap<String, toml::Value>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    files: BTreeMap<String, PluginFile>,
    #[serde(default)]
    arguments: Vec<String>,
    #[serde(default)]
    doctor: Option<PluginDoctor>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PluginConfigField {
    #[serde(rename = "type")]
    kind: PluginConfigType,
    required: bool,
    #[serde(default)]
    default: Option<toml::Value>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(rename = "enum", default)]
    allowed_values: Option<Vec<toml::Value>>,
    #[serde(default)]
    minimum: Option<f64>,
    #[serde(default)]
    maximum: Option<f64>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum PluginConfigType {
    String,
    Integer,
    Number,
    Boolean,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PluginFile {
    mode: String,
    content: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PluginDoctor {
    #[serde(default)]
    required_commands: Vec<String>,
    #[serde(default)]
    required_env: Vec<String>,
    #[serde(default)]
    version_constraints: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PluginOrigin {
    Bundled,
    LocalRegistry,
}

impl PluginOrigin {
    fn label(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::LocalRegistry => "local",
        }
    }
}

#[derive(Debug, Clone)]
struct ResolvedPlugin {
    entry: PluginRegistryEntry,
    manifest: PluginManifest,
    manifest_text: String,
    origin: PluginOrigin,
}

#[derive(Debug, Clone)]
struct ItemPluginPin {
    schema_version: u32,
    name: String,
    source: String,
    version: String,
    sha256: String,
    config: BTreeMap<String, toml::Value>,
}

pub(crate) fn run_plugin_cli(context: &ItemContext, command: &PluginCommand) -> Result<()> {
    match command {
        PluginCommand::List => {
            for plugin in load_available_plugins()? {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    plugin.entry.name,
                    plugin.entry.version,
                    plugin.entry.lifecycle.label(),
                    plugin.origin.label(),
                    plugin.entry.description
                );
            }
            Ok(())
        }
        PluginCommand::Show { selector } => {
            let plugin = resolve_plugin_for_inspection(selector)?;
            println!("name = {}", plugin.entry.name);
            println!("version = {}", plugin.entry.version);
            println!("lifecycle = {}", plugin.entry.lifecycle.label());
            println!("source = {}", plugin.entry.source);
            println!("sha256 = {}", plugin.entry.sha256);
            println!("origin = {}", plugin.origin.label());
            println!();
            print!("{}", plugin.manifest_text);
            if !plugin.manifest_text.ends_with('\n') {
                println!();
            }
            Ok(())
        }
        PluginCommand::Run {
            selector,
            item,
            allow_deprecated,
            command,
        } => {
            if command.is_empty() {
                return Err(anyhow!(
                    "Command required after '--'. Usage: opz plugin run <NAME[@VERSION]> [--item <ITEM>] -- <COMMAND>..."
                ));
            }
            let requested_items = item.iter().cloned().collect::<Vec<_>>();
            let resolved_items = resolve_run_items(context, &requested_items)?;
            run_plugin_with_item_titles(
                context,
                selector,
                &resolved_items,
                command,
                *allow_deprecated,
            )
        }
    }
}

struct InspectedItem {
    item_id: String,
    vault_id: String,
    title: String,
    item: ItemGet,
    env_lines: Vec<String>,
}

pub(crate) fn run_with_items_plugin_aware(
    context: &ItemContext,
    items: &[String],
    env_file: Option<&Path>,
    command: &[String],
) -> Result<()> {
    let inspected = instrumentation::with_span_result(
        "load_inputs",
        vec![KeyValue::new("item.count", items.len() as i64)],
        || inspect_items(context, items),
    )?;
    let selections = inspected
        .iter()
        .filter_map(|item| item_field_value(&item.item, PLUGIN_LABEL))
        .collect::<Vec<_>>();
    if selections.is_empty() {
        let sections = inspected
            .into_iter()
            .map(|item| (item.title, item.env_lines))
            .collect::<ItemSections>();
        return run_with_item_sections(&sections, env_file, command);
    }
    if env_file.is_some() {
        return Err(anyhow!(
            "`--env-file` cannot be combined with an item-selected plugin; generated plugin files are private and temporary"
        ));
    }
    if inspected.len() != 1 || selections.len() != 1 {
        return Err(anyhow!(
            "plugin execution requires exactly one 1Password item"
        ));
    }
    run_plugin_with_resolved_item(&selections[0], &inspected[0], command, false)
}

fn inspect_items(context: &ItemContext, items: &[String]) -> Result<Vec<InspectedItem>> {
    let mut inspected = Vec::with_capacity(items.len());
    for title in items {
        let (item_id, vault_id, resolved_title, item) = find_item(context.vault.as_deref(), title)?;
        let env_lines = item_to_env_lines(&item, &vault_id, &item_id)?;
        inspected.push(InspectedItem {
            item_id,
            vault_id,
            title: resolved_title,
            item,
            env_lines,
        });
    }
    Ok(inspected)
}

fn run_plugin_with_item_titles(
    context: &ItemContext,
    selector: &str,
    item_titles: &[String],
    command: &[String],
    allow_deprecated: bool,
) -> Result<()> {
    if item_titles.len() != 1 {
        return Err(anyhow!(
            "plugin execution requires exactly one 1Password item"
        ));
    }
    let mut inspected = inspect_items(context, item_titles)?;
    run_plugin_with_resolved_item(
        selector,
        inspected
            .first_mut()
            .expect("one plugin item was inspected"),
        command,
        allow_deprecated,
    )
}

fn run_plugin_with_resolved_item(
    selector: &str,
    inspected: &InspectedItem,
    command: &[String],
    allow_deprecated: bool,
) -> Result<()> {
    let pin = parse_item_plugin_pin(&inspected.item)?.ok_or_else(|| {
        anyhow!("item is missing {PLUGIN_LABEL}; plugin runs require an integrity-pinned item")
    })?;
    let requested = parse_selector(selector)?;
    if requested.0 != pin.name {
        return Err(anyhow!(
            "item pins plugin `{}`, but `{}` was requested",
            pin.name,
            requested.0
        ));
    }
    if let Some(version) = requested.1.as_deref() {
        if version != pin.version {
            return Err(anyhow!(
                "item pins plugin version {}, but {} was requested",
                pin.version,
                version
            ));
        }
    }

    let plugin = resolve_plugin(&format!("{}@{}", pin.name, pin.version), allow_deprecated)?;
    validate_item_pin(&plugin, &pin)?;
    validate_target_command(&plugin.manifest, command)?;
    let config = build_runtime_config(&plugin.manifest, &pin.config)?;
    let secret_lines = plugin_secret_lines(
        &plugin.manifest,
        &inspected.item,
        &inspected.vault_id,
        &inspected.item_id,
    )?;
    let secrets = resolve_env_vars(&secret_lines)?;

    let workspace = tempfile::Builder::new()
        .prefix("opz-plugin-")
        .tempdir()
        .context("failed to create plugin workspace")?;
    set_directory_mode(workspace.path(), 0o700)?;
    let rendered_env = render_environment(&plugin.manifest, &config, workspace.path())?;
    render_files(&plugin.manifest, &config, &rendered_env, workspace.path())?;
    let rendered_arguments =
        render_arguments(&plugin.manifest, &config, &rendered_env, workspace.path())?;
    run_plugin_target(command, &rendered_arguments, &rendered_env, &secrets)
}

fn run_plugin_target(
    command: &[String],
    plugin_arguments: &[String],
    generated_env: &BTreeMap<String, String>,
    secrets: &HashMap<String, SecretValue>,
) -> Result<()> {
    let mut effective = Vec::with_capacity(command.len() + plugin_arguments.len());
    effective.push(
        command
            .first()
            .ok_or_else(|| anyhow!("plugin target command is empty"))?
            .clone(),
    );
    effective.extend(plugin_arguments.iter().cloned());
    effective.extend(command.iter().skip(1).cloned());

    let mut child = build_child_command(&effective)?;
    apply_plugin_baseline_environment(&mut child);
    for (key, value) in generated_env {
        child.env(key, value);
    }
    for (key, value) in secrets {
        child.env(key, value.expose());
    }

    let status = child
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("failed to run plugin target")?;
    if !status.success() {
        return Err(anyhow!("plugin target failed with status: {status}"));
    }
    Ok(())
}

fn apply_plugin_baseline_environment(command: &mut Command) {
    command.env_clear();
    for key in [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "TMPDIR",
        "TEMP",
        "TMP",
        "SystemRoot",
        "WINDIR",
        "ComSpec",
        "PATHEXT",
        "TERM",
        "COLORTERM",
        "LANG",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
    for (key, value) in env::vars_os() {
        if key.to_string_lossy().starts_with("LC_") {
            command.env(key, value);
        }
    }
    #[cfg(feature = "test-support")]
    for key in ["OPZ_TEST_SCENARIO", "OPZ_TEST_LOG_DIR"] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
}

fn load_available_plugins() -> Result<Vec<ResolvedPlugin>> {
    let mut plugins = load_registry_plugins(BUNDLED_REGISTRY, None, PluginOrigin::Bundled)?;
    if let Some(root) = env::var_os(PLUGIN_REGISTRY_DIR_ENV) {
        let root = PathBuf::from(root);
        let registry_path = root.join("registry.toml");
        let text = fs::read_to_string(&registry_path)
            .with_context(|| format!("read {}", registry_path.display()))?;
        for local in load_registry_plugins(&text, Some(&root), PluginOrigin::LocalRegistry)? {
            if let Some(index) = plugins.iter().position(|plugin| {
                plugin.entry.name == local.entry.name && plugin.entry.version == local.entry.version
            }) {
                plugins[index] = local;
            } else {
                plugins.push(local);
            }
        }
    }
    plugins.sort_by(|left, right| {
        left.entry
            .name
            .cmp(&right.entry.name)
            .then_with(|| compare_versions(&left.entry.version, &right.entry.version))
    });
    Ok(plugins)
}

fn load_registry_plugins(
    registry_text: &str,
    root: Option<&Path>,
    origin: PluginOrigin,
) -> Result<Vec<ResolvedPlugin>> {
    let registry: PluginRegistry =
        toml::from_str(registry_text).context("parse plugin registry")?;
    if registry.schema_version != 1 {
        return Err(anyhow!(
            "unsupported plugin registry schema_version {}",
            registry.schema_version
        ));
    }

    let mut identities = HashSet::new();
    let mut plugins = Vec::with_capacity(registry.plugins.len());
    for entry in registry.plugins {
        if !identities.insert((entry.name.clone(), entry.version.clone())) {
            return Err(anyhow!(
                "duplicate plugin registry entry {}@{}",
                entry.name,
                entry.version
            ));
        }
        let manifest_text = match root {
            Some(root) => {
                let path = contained_registry_path(root, &entry.path)?;
                fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?
            }
            None => normalize_bundled_newlines(
                bundled_manifest(&entry.path)
                    .ok_or_else(|| anyhow!("bundled plugin manifest is missing: {}", entry.path))?,
            ),
        };
        let digest = sha256_hex(manifest_text.as_bytes());
        if digest != entry.sha256 {
            return Err(anyhow!(
                "plugin digest mismatch for {}@{}",
                entry.name,
                entry.version
            ));
        }
        let manifest: PluginManifest = toml::from_str(&manifest_text)
            .with_context(|| format!("parse plugin manifest {}@{}", entry.name, entry.version))?;
        validate_manifest(&manifest)?;
        validate_registry_manifest_pair(&entry, &manifest)?;
        plugins.push(ResolvedPlugin {
            entry,
            manifest,
            manifest_text,
            origin,
        });
    }
    Ok(plugins)
}

fn normalize_bundled_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn bundled_manifest(path: &str) -> Option<&'static str> {
    match path {
        "plugins/claude-env/plugin.toml" => Some(BUNDLED_CLAUDE_ENV),
        "plugins/codex-openai/plugin.toml" => Some(BUNDLED_CODEX_OPENAI),
        "plugins/opencode-go-codex/plugin.toml" => Some(BUNDLED_OPENCODE_GO_CODEX),
        _ => None,
    }
}

fn contained_registry_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(anyhow!(
            "plugin registry path escapes its root: {relative:?}"
        ));
    }
    let root = root
        .canonicalize()
        .with_context(|| format!("canonicalize {}", root.display()))?;
    let path = root.join(relative);
    let canonical = path
        .canonicalize()
        .with_context(|| format!("canonicalize {}", path.display()))?;
    if !canonical.starts_with(&root) {
        return Err(anyhow!("plugin registry path escapes its root"));
    }
    Ok(canonical)
}

fn resolve_plugin_for_inspection(selector: &str) -> Result<ResolvedPlugin> {
    let (name, requested_version) = parse_selector(selector)?;
    let mut matches = load_available_plugins()?
        .into_iter()
        .filter(|plugin| {
            plugin.entry.name == name
                && requested_version
                    .as_deref()
                    .is_none_or(|version| plugin.entry.version == version)
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| compare_versions(&left.entry.version, &right.entry.version));
    matches
        .pop()
        .ok_or_else(|| anyhow!("plugin not found: {selector}"))
}

fn resolve_plugin(selector: &str, allow_deprecated: bool) -> Result<ResolvedPlugin> {
    let plugin = resolve_plugin_for_inspection(selector)?;
    if version_tuple(&plugin.entry.min_opz_version)? > version_tuple(env!("CARGO_PKG_VERSION"))? {
        return Err(anyhow!(
            "plugin {}@{} requires opz >= {}",
            plugin.entry.name,
            plugin.entry.version,
            plugin.entry.min_opz_version
        ));
    }
    match plugin.entry.lifecycle {
        PluginLifecycle::Active => {}
        PluginLifecycle::Deprecated if allow_deprecated => {}
        PluginLifecycle::Deprecated => {
            let replacement = plugin
                .entry
                .replacement
                .as_deref()
                .map(|value| format!("; replacement: {value}"))
                .unwrap_or_default();
            return Err(anyhow!(
                "plugin {}@{} is deprecated and requires --allow-deprecated{}",
                plugin.entry.name,
                plugin.entry.version,
                replacement
            ));
        }
        PluginLifecycle::Revoked => {
            return Err(anyhow!(
                "plugin {}@{} is revoked: {}",
                plugin.entry.name,
                plugin.entry.version,
                plugin
                    .entry
                    .revocation_reason
                    .as_deref()
                    .unwrap_or("no reason provided")
            ));
        }
    }
    Ok(plugin)
}

fn parse_selector(selector: &str) -> Result<(String, Option<String>)> {
    let (name, version) = match selector.split_once('@') {
        Some((name, version)) if !version.is_empty() => (name, Some(version.to_string())),
        Some(_) => return Err(anyhow!("plugin selector has an empty version")),
        None => (selector, None),
    };
    validate_plugin_name(name)?;
    if let Some(version) = version.as_deref() {
        version_tuple(version)?;
    }
    Ok((name.to_string(), version))
}

fn validate_registry_manifest_pair(
    entry: &PluginRegistryEntry,
    manifest: &PluginManifest,
) -> Result<()> {
    if entry.plugin_schema_version != manifest.schema_version
        || entry.name != manifest.name
        || entry.version != manifest.version
        || entry.description != manifest.description
        || entry.target_commands != manifest.target_commands
    {
        return Err(anyhow!(
            "plugin registry metadata disagrees with manifest for {}@{}",
            entry.name,
            entry.version
        ));
    }
    let expected_source_suffix = format!("/plugins/{}", entry.name);
    if !entry.source.starts_with("github:") || !entry.source.ends_with(&expected_source_suffix) {
        return Err(anyhow!(
            "invalid plugin source for {}@{}",
            entry.name,
            entry.version
        ));
    }
    Ok(())
}

fn validate_manifest(manifest: &PluginManifest) -> Result<()> {
    if manifest.schema_version != 1 {
        return Err(anyhow!(
            "unsupported plugin schema_version {}",
            manifest.schema_version
        ));
    }
    validate_plugin_name(&manifest.name)?;
    version_tuple(&manifest.version)?;
    if manifest.description.is_empty() || manifest.description.len() > 240 {
        return Err(anyhow!(
            "plugin description must contain 1..=240 characters"
        ));
    }
    if manifest.target_commands.is_empty() || manifest.target_commands.len() > 8 {
        return Err(anyhow!("plugin target_commands must contain 1..=8 entries"));
    }
    ensure_unique("target command", &manifest.target_commands)?;
    ensure_unique("required env", &manifest.required_env)?;
    ensure_unique("secret env", &manifest.secret_env_allowlist)?;
    for command in &manifest.target_commands {
        validate_command_name(command)?;
    }
    for name in manifest
        .required_env
        .iter()
        .chain(manifest.secret_env_allowlist.iter())
        .chain(manifest.env.keys())
    {
        validate_env_name(name)?;
    }
    let required = manifest.required_env.iter().collect::<HashSet<_>>();
    let secret_allowlist = manifest.secret_env_allowlist.iter().collect::<HashSet<_>>();
    for secret in &manifest.secret_env_allowlist {
        if !required.contains(secret) {
            return Err(anyhow!(
                "secret_env_allowlist entry `{secret}` is not required"
            ));
        }
    }
    for required_name in &manifest.required_env {
        if !secret_allowlist.contains(required_name) && !manifest.env.contains_key(required_name) {
            return Err(anyhow!(
                "required_env entry `{required_name}` must be allowlisted as a secret or generated by the plugin"
            ));
        }
    }
    for key in manifest.env.keys() {
        if PROTECTED_ENV.contains(&key.as_str()) {
            return Err(anyhow!(
                "plugin may not replace protected environment `{key}`"
            ));
        }
    }
    validate_config_contract(manifest)?;
    for template in manifest
        .env
        .values()
        .chain(manifest.arguments.iter())
        .chain(manifest.files.keys())
        .chain(manifest.files.values().map(|file| &file.content))
    {
        validate_template(template, manifest)?;
    }
    for file in manifest.files.values() {
        if !matches!(file.mode.as_str(), "0600" | "0640" | "0644") {
            return Err(anyhow!("unsupported plugin file mode {}", file.mode));
        }
    }
    if let Some(doctor) = &manifest.doctor {
        for command in doctor
            .required_commands
            .iter()
            .chain(doctor.version_constraints.keys())
        {
            validate_command_name(command)?;
        }
        for name in &doctor.required_env {
            validate_env_name(name)?;
        }
    }
    Ok(())
}

fn validate_config_contract(manifest: &PluginManifest) -> Result<()> {
    for (key, field) in &manifest.config {
        validate_config_key(key)?;
        if field.required && field.default.is_some() {
            return Err(anyhow!("required config.{key} may not declare a default"));
        }
        if !field.required && field.default.is_none() {
            return Err(anyhow!("optional config.{key} must declare a default"));
        }
        if let Some(default) = field.default.as_ref() {
            validate_config_value(key, field, default)?;
        }
        if manifest.defaults.get(key) != field.default.as_ref() {
            return Err(anyhow!(
                "defaults.{key} must exactly match config.{key}.default"
            ));
        }
    }
    for key in manifest.defaults.keys() {
        if !manifest.config.contains_key(key) {
            return Err(anyhow!("defaults.{key} is not declared in config"));
        }
    }
    Ok(())
}

fn validate_config_value(key: &str, field: &PluginConfigField, value: &toml::Value) -> Result<()> {
    let type_matches = match field.kind {
        PluginConfigType::String => value.is_str(),
        PluginConfigType::Integer => value.is_integer(),
        PluginConfigType::Number => value.is_integer() || value.is_float(),
        PluginConfigType::Boolean => value.is_bool(),
    };
    if !type_matches {
        return Err(anyhow!("config.{key} has the wrong scalar type"));
    }
    if let Some(allowed) = &field.allowed_values {
        if !allowed.contains(value) {
            return Err(anyhow!("config.{key} is not an allowed value"));
        }
    }
    if let Some(pattern) = &field.pattern {
        let string = value
            .as_str()
            .ok_or_else(|| anyhow!("config.{key} pattern requires a string value"))?;
        if !Regex::new(pattern)
            .with_context(|| format!("invalid regex for config.{key}"))?
            .is_match(string)
        {
            return Err(anyhow!("config.{key} does not match its pattern"));
        }
    }
    let number = value
        .as_float()
        .or_else(|| value.as_integer().map(|value| value as f64));
    if let (Some(minimum), Some(number)) = (field.minimum, number) {
        if number < minimum {
            return Err(anyhow!("config.{key} is below its minimum"));
        }
    }
    if let (Some(maximum), Some(number)) = (field.maximum, number) {
        if number > maximum {
            return Err(anyhow!("config.{key} is above its maximum"));
        }
    }
    Ok(())
}

fn validate_template(template: &str, manifest: &PluginManifest) -> Result<()> {
    if template.contains("$(")
        || template.contains('`')
        || Regex::new(r"\$\{[^}]+\}")?.is_match(template)
        || template.contains("{{")
        || template.contains("}}")
    {
        return Err(anyhow!("plugin template contains an executable expression"));
    }
    for placeholder in template_placeholders(template)? {
        if placeholder == "tmp" {
            continue;
        }
        if let Some(name) = placeholder.strip_prefix("env.") {
            if !manifest.env.contains_key(name) {
                return Err(anyhow!("template references undeclared env.{name}"));
            }
            continue;
        }
        if let Some(key) = placeholder.strip_prefix("config.") {
            if !manifest.config.contains_key(key) {
                return Err(anyhow!("template references undeclared config.{key}"));
            }
            continue;
        }
        return Err(anyhow!("unsupported plugin placeholder {{{placeholder}}}"));
    }
    Ok(())
}

fn template_placeholders(template: &str) -> Result<Vec<String>> {
    let mut placeholders = Vec::new();
    let mut cursor = 0;
    while let Some(open_offset) = template[cursor..].find('{') {
        let open = cursor + open_offset;
        let close = template[open + 1..]
            .find('}')
            .map(|offset| open + 1 + offset)
            .ok_or_else(|| anyhow!("plugin template contains an unmatched opening brace"))?;
        let placeholder = &template[open + 1..close];
        if placeholder.is_empty() || placeholder.contains('{') {
            return Err(anyhow!("plugin template contains an invalid placeholder"));
        }
        placeholders.push(placeholder.to_string());
        cursor = close + 1;
    }
    if template[cursor..].contains('}') {
        return Err(anyhow!(
            "plugin template contains an unmatched closing brace"
        ));
    }
    Ok(placeholders)
}

fn validate_target_command(manifest: &PluginManifest, command: &[String]) -> Result<()> {
    let program = command
        .first()
        .ok_or_else(|| anyhow!("plugin target command is empty"))?;
    let basename = Path::new(program)
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("plugin target command is not valid UTF-8"))?;
    if !manifest
        .target_commands
        .iter()
        .any(|candidate| candidate == basename)
    {
        return Err(anyhow!(
            "plugin {} supports [{}], not `{basename}`",
            manifest.name,
            manifest.target_commands.join(", ")
        ));
    }
    Ok(())
}

fn parse_item_plugin_pin(item: &ItemGet) -> Result<Option<ItemPluginPin>> {
    let Some(name) = item_field_value(item, PLUGIN_LABEL) else {
        return Ok(None);
    };
    let schema_version = required_item_field(item, PLUGIN_SCHEMA_VERSION_LABEL)?
        .parse::<u32>()
        .context("OPZ_PLUGIN_SCHEMA_VERSION must be an integer")?;
    if schema_version != 1 {
        return Err(anyhow!(
            "unsupported OPZ_PLUGIN_SCHEMA_VERSION {schema_version}"
        ));
    }
    let source = required_item_field(item, PLUGIN_SOURCE_LABEL)?;
    let version = required_item_field(item, PLUGIN_VERSION_LABEL)?;
    let sha256 = required_item_field(item, PLUGIN_SHA256_LABEL)?;
    validate_plugin_name(&name)?;
    version_tuple(&version)?;
    if !Regex::new(r"^[a-f0-9]{64}$")?.is_match(&sha256) {
        return Err(anyhow!("OPZ_PLUGIN_SHA256 must be lowercase SHA-256 hex"));
    }
    let raw_config = item_field_value(item, PLUGIN_CONFIG_LABEL).unwrap_or_default();
    if raw_config.len() > MAX_PLUGIN_CONFIG_BYTES {
        return Err(anyhow!("OPZ_PLUGIN_CONFIG exceeds 16 KiB"));
    }
    let config = if raw_config.trim().is_empty() {
        BTreeMap::new()
    } else {
        toml::from_str::<BTreeMap<String, toml::Value>>(&raw_config)
            .context("parse OPZ_PLUGIN_CONFIG")?
    };
    if config.len() > 64 || config.values().any(|value| !is_scalar(value)) {
        return Err(anyhow!(
            "OPZ_PLUGIN_CONFIG must be a flat TOML table with at most 64 scalar keys"
        ));
    }
    Ok(Some(ItemPluginPin {
        schema_version,
        name,
        source,
        version,
        sha256,
        config,
    }))
}

fn validate_item_pin(plugin: &ResolvedPlugin, pin: &ItemPluginPin) -> Result<()> {
    if pin.schema_version != 1
        || pin.name != plugin.entry.name
        || pin.source != plugin.entry.source
        || pin.version != plugin.entry.version
        || pin.sha256 != plugin.entry.sha256
    {
        return Err(anyhow!(
            "1Password item plugin pin does not match the selected registry release"
        ));
    }
    Ok(())
}

fn required_item_field(item: &ItemGet, label: &str) -> Result<String> {
    item_field_value(item, label)
        .ok_or_else(|| anyhow!("1Password item is missing required field {label}"))
}

fn item_field_value(item: &ItemGet, wanted: &str) -> Option<String> {
    item.fields.iter().find_map(|field| {
        (field.label.as_deref() == Some(wanted))
            .then(|| item_field_string_value(field))
            .flatten()
    })
}

fn build_runtime_config(
    manifest: &PluginManifest,
    overrides: &BTreeMap<String, toml::Value>,
) -> Result<BTreeMap<String, toml::Value>> {
    for key in overrides.keys() {
        if !manifest.config.contains_key(key) {
            return Err(anyhow!("unknown plugin config key `{key}`"));
        }
    }
    let mut config = manifest.defaults.clone();
    config.extend(overrides.clone());
    for (key, field) in &manifest.config {
        let value = config.get(key);
        if field.required && value.is_none() {
            return Err(anyhow!("missing required plugin config key `{key}`"));
        }
        if let Some(value) = value {
            validate_config_value(key, field, value)?;
        }
    }
    Ok(config)
}

fn plugin_secret_lines(
    manifest: &PluginManifest,
    item: &ItemGet,
    vault_id: &str,
    item_id: &str,
) -> Result<Vec<String>> {
    let available = item
        .fields
        .iter()
        .filter_map(|field| field.label.as_deref())
        .collect::<HashSet<_>>();
    for required in &manifest.secret_env_allowlist {
        if !available.contains(required.as_str()) {
            return Err(anyhow!("plugin requires missing item field `{required}`"));
        }
    }
    Ok(manifest
        .secret_env_allowlist
        .iter()
        .map(|name| format!("{name}=op://{vault_id}/{item_id}/{name}"))
        .collect())
}

fn render_environment(
    manifest: &PluginManifest,
    config: &BTreeMap<String, toml::Value>,
    workspace: &Path,
) -> Result<BTreeMap<String, String>> {
    let mut pending = manifest.env.clone();
    let mut rendered = BTreeMap::new();
    while !pending.is_empty() {
        let mut progressed = false;
        for key in pending.keys().cloned().collect::<Vec<_>>() {
            let template = pending
                .get(&key)
                .expect("pending environment key remains present");
            let dependencies = template_placeholders(template)?
                .into_iter()
                .filter_map(|placeholder| placeholder.strip_prefix("env.").map(str::to_string))
                .collect::<Vec<_>>();
            if dependencies
                .iter()
                .any(|dependency| !rendered.contains_key(dependency))
            {
                continue;
            }
            let value = render_template(template, config, &rendered, workspace)?;
            rendered.insert(key.clone(), value);
            pending.remove(&key);
            progressed = true;
        }
        if !progressed {
            return Err(anyhow!(
                "plugin environment templates contain a dependency cycle"
            ));
        }
    }
    Ok(rendered)
}

fn render_arguments(
    manifest: &PluginManifest,
    config: &BTreeMap<String, toml::Value>,
    environment: &BTreeMap<String, String>,
    workspace: &Path,
) -> Result<Vec<String>> {
    manifest
        .arguments
        .iter()
        .map(|template| render_template(template, config, environment, workspace))
        .collect()
}

fn render_files(
    manifest: &PluginManifest,
    config: &BTreeMap<String, toml::Value>,
    environment: &BTreeMap<String, String>,
    workspace: &Path,
) -> Result<()> {
    for (path_template, file) in &manifest.files {
        let rendered_path = render_template(path_template, config, environment, workspace)?;
        let path = normalize_generated_path(workspace, &rendered_path)?;
        let content = render_template(&file.content, config, environment, workspace)?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("generated plugin file has no parent"))?;
        create_private_directories(workspace, parent)?;
        #[cfg(unix)]
        let mode = u32::from_str_radix(file.mode.trim_start_matches('0'), 8)
            .context("parse plugin file mode")?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        let mut output = options
            .open(&path)
            .with_context(|| format!("create plugin file {}", path.display()))?;
        output
            .write_all(content.as_bytes())
            .with_context(|| format!("write plugin file {}", path.display()))?;
        output
            .sync_all()
            .with_context(|| format!("flush plugin file {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        }
    }
    Ok(())
}

fn create_private_directories(workspace: &Path, target: &Path) -> Result<()> {
    let relative = target
        .strip_prefix(workspace)
        .context("plugin directory escapes workspace")?;
    let mut current = workspace.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(anyhow!("invalid plugin directory component"));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(anyhow!(
                        "plugin workspace contains a non-directory component"
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)
                    .with_context(|| format!("create plugin directory {}", current.display()))?;
                set_directory_mode(&current, 0o700)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn normalize_generated_path(workspace: &Path, rendered: &str) -> Result<PathBuf> {
    let path = Path::new(rendered);
    if !path.is_absolute() {
        return Err(anyhow!("generated plugin path must be absolute"));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str())
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(anyhow!("generated plugin path contains parent traversal"));
            }
        }
    }
    if !normalized.starts_with(workspace) || normalized == workspace {
        return Err(anyhow!("generated plugin path escapes workspace"));
    }
    Ok(normalized)
}

fn render_template(
    template: &str,
    config: &BTreeMap<String, toml::Value>,
    environment: &BTreeMap<String, String>,
    workspace: &Path,
) -> Result<String> {
    let mut output = String::with_capacity(template.len());
    let mut cursor = 0;
    while let Some(open_offset) = template[cursor..].find('{') {
        let open = cursor + open_offset;
        output.push_str(&template[cursor..open]);
        let close = template[open + 1..]
            .find('}')
            .map(|offset| open + 1 + offset)
            .ok_or_else(|| anyhow!("plugin template contains an unmatched opening brace"))?;
        let key = &template[open + 1..close];
        let replacement = if key == "tmp" {
            workspace.to_string_lossy().to_string()
        } else if let Some(name) = key.strip_prefix("env.") {
            environment
                .get(name)
                .cloned()
                .ok_or_else(|| anyhow!("missing rendered env.{name}"))?
        } else if let Some(name) = key.strip_prefix("config.") {
            scalar_to_string(
                config
                    .get(name)
                    .ok_or_else(|| anyhow!("missing plugin config.{name}"))?,
            )?
        } else {
            return Err(anyhow!("unsupported plugin placeholder {{{key}}}"));
        };
        output.push_str(&replacement);
        cursor = close + 1;
    }
    if template[cursor..].contains('}') {
        return Err(anyhow!(
            "plugin template contains an unmatched closing brace"
        ));
    }
    output.push_str(&template[cursor..]);
    Ok(output)
}

fn scalar_to_string(value: &toml::Value) -> Result<String> {
    match value {
        toml::Value::String(value) => Ok(value.clone()),
        toml::Value::Integer(value) => Ok(value.to_string()),
        toml::Value::Float(value) => Ok(value.to_string()),
        toml::Value::Boolean(value) => Ok(value.to_string()),
        _ => Err(anyhow!("plugin config value is not scalar")),
    }
}

fn is_scalar(value: &toml::Value) -> bool {
    matches!(
        value,
        toml::Value::String(_)
            | toml::Value::Integer(_)
            | toml::Value::Float(_)
            | toml::Value::Boolean(_)
    )
}

fn validate_plugin_name(name: &str) -> Result<()> {
    if !Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")?.is_match(name) {
        return Err(anyhow!("invalid plugin name `{name}`"));
    }
    Ok(())
}

fn validate_command_name(name: &str) -> Result<()> {
    if !Regex::new(r"^[a-z0-9][a-z0-9._-]*$")?.is_match(name) {
        return Err(anyhow!("invalid plugin target command `{name}`"));
    }
    Ok(())
}

fn validate_env_name(name: &str) -> Result<()> {
    if name.len() > 128 || !Regex::new(r"^[A-Z_][A-Z0-9_]*$")?.is_match(name) {
        return Err(anyhow!("invalid plugin environment name `{name}`"));
    }
    Ok(())
}

fn validate_config_key(name: &str) -> Result<()> {
    if !Regex::new(r"^[a-z][a-z0-9_]*$")?.is_match(name) {
        return Err(anyhow!("invalid plugin config key `{name}`"));
    }
    Ok(())
}

fn ensure_unique(label: &str, values: &[String]) -> Result<()> {
    let unique = values.iter().collect::<HashSet<_>>();
    if unique.len() != values.len() {
        return Err(anyhow!("duplicate {label}"));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

fn version_tuple(version: &str) -> Result<(u64, u64, u64)> {
    let core = version
        .split(['-', '+'])
        .next()
        .ok_or_else(|| anyhow!("invalid semantic version `{version}`"))?;
    let parts = core
        .split('.')
        .map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("invalid semantic version `{version}`"))?;
    match parts.as_slice() {
        [major, minor, patch] => Ok((*major, *minor, *patch)),
        _ => Err(anyhow!("invalid semantic version `{version}`")),
    }
}

fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let left = version_tuple(left).unwrap_or((0, 0, 0));
    let right = version_tuple(right).unwrap_or((0, 0, 0));
    left.cmp(&right)
}

fn set_directory_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .with_context(|| format!("set permissions on {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

pub(crate) fn is_plugin_metadata_label(label: &str) -> bool {
    label.to_ascii_uppercase().starts_with("OPZ_PLUGIN")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn bundled_registry_is_digest_valid() {
        let plugins = load_registry_plugins(BUNDLED_REGISTRY, None, PluginOrigin::Bundled).unwrap();
        assert_eq!(plugins.len(), 3);
        assert_eq!(
            plugins
                .iter()
                .filter(|plugin| plugin.entry.lifecycle == PluginLifecycle::Active)
                .count(),
            2
        );
    }

    #[test]
    fn revoked_plugin_is_discoverable_but_not_runnable() {
        let inspected = resolve_plugin_for_inspection("opencode-go-codex@0.1.0").unwrap();
        assert_eq!(inspected.entry.lifecycle, PluginLifecycle::Revoked);
        let error = resolve_plugin("opencode-go-codex@0.1.0", true).unwrap_err();
        assert!(error.to_string().contains("revoked"));
    }

    #[test]
    fn plugin_metadata_namespace_is_never_projected() {
        assert!(is_plugin_metadata_label("OPZ_PLUGIN"));
        assert!(is_plugin_metadata_label("OPZ_PLUGIN_SHA256"));
        assert!(is_plugin_metadata_label("opz_plugin_future_field"));
        assert!(!is_plugin_metadata_label("OPENAI_API_KEY"));
    }

    #[test]
    fn generated_path_rejects_parent_traversal() {
        let workspace = env::temp_dir().join("opz-plugin-test");
        let path = workspace.join("a").join("..").join("escape");
        let error = normalize_generated_path(&workspace, &path.to_string_lossy()).unwrap_err();
        assert!(error.to_string().contains("parent traversal"));
    }

    #[test]
    fn bundled_digest_input_normalizes_checkout_line_endings() {
        assert_eq!(normalize_bundled_newlines("a\r\nb\r\n"), "a\nb\n");
    }

    proptest! {
        #[test]
        fn plugin_names_accept_exact_canonical_form(
            first in "[a-z0-9]{1,8}",
            rest in prop::collection::vec("[a-z0-9]{1,8}", 0..5)
        ) {
            let mut name = first;
            for part in rest {
                name.push('-');
                name.push_str(&part);
            }
            prop_assert!(validate_plugin_name(&name).is_ok());
            let invalid_prefix = format!("_{}", name);
            let invalid_suffix = format!("{}-", name);
            prop_assert!(validate_plugin_name(&invalid_prefix).is_err());
            prop_assert!(validate_plugin_name(&invalid_suffix).is_err());
        }

        #[test]
        fn env_names_accept_shell_compatible_uppercase(
            first in "[A-Z]",
            rest in "[A-Z0-9_]{0,40}"
        ) {
            let name = format!("{first}{rest}");
            prop_assert!(validate_env_name(&name).is_ok());
            prop_assert!(validate_env_name(&name.to_ascii_lowercase()).is_err());
        }

        #[test]
        fn literal_templates_round_trip(text in "[a-zA-Z0-9 /._:-]{0,200}") {
            let rendered = render_template(
                &text,
                &BTreeMap::new(),
                &BTreeMap::new(),
                Path::new("/tmp/opz-plugin-pbt")
            ).unwrap();
            prop_assert_eq!(rendered, text);
        }

        #[test]
        fn generated_paths_remain_below_workspace(
            segments in prop::collection::vec(
                "[a-zA-Z0-9._-]{1,12}".prop_filter("path segment", |value| value != "." && value != ".."),
                1..8
            )
        ) {
            let workspace = env::temp_dir().join("opz-plugin-pbt");
            let mut path = workspace.clone();
            for segment in &segments {
                path.push(segment);
            }
            let normalized = normalize_generated_path(&workspace, &path.to_string_lossy()).unwrap();
            prop_assert!(normalized.starts_with(&workspace));
            prop_assert_ne!(normalized, workspace);
        }
    }
}
