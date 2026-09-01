use crate::*;
use std::io::Read;

const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;
const REDACTED: &str = "[REDACTED]";

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub(crate) enum CloudflareCredentialPreset {
    /// Store a Cloudflare API token as a concealed field.
    ApiToken,
    /// Store one Worker secret or a JSON object of Worker secrets as concealed fields.
    WorkerSecret,
    /// Store a JSON API response, redacting credential-like fields unless --raw is set.
    ApiResponse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub(crate) enum CloudflareCredentialMode {
    /// Fail when the item already exists.
    Create,
    /// Fail when the item does not exist.
    Update,
    /// Update an existing exact-title item, otherwise create it.
    Upsert,
}

#[derive(Clone, Debug)]
pub(crate) struct CloudflareCredentialOptions<'a> {
    pub(crate) vault: Option<&'a str>,
    pub(crate) item: &'a str,
    pub(crate) section: Option<&'a str>,
    pub(crate) field: Option<&'a str>,
    pub(crate) preset: CloudflareCredentialPreset,
    pub(crate) mode: CloudflareCredentialMode,
    pub(crate) file: Option<&'a Path>,
    pub(crate) stdin: bool,
    pub(crate) raw: bool,
    pub(crate) dry_run: bool,
    pub(crate) command: &'a [String],
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DestinationField {
    label: String,
    value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedCredential {
    section_label: String,
    fields: Vec<DestinationField>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ItemAction {
    Create,
    Update,
}

pub(crate) fn store_cloudflare_credential(options: CloudflareCredentialOptions<'_>) -> Result<()> {
    validate_cloudflare_options(&options)?;
    let input = load_cloudflare_input(&options)?;
    let prepared = prepare_cloudflare_credential(
        options.preset,
        options.section,
        options.field,
        options.raw,
        &input,
    )?;
    validate_destination_label(&prepared.section_label)?;
    for field in &prepared.fields {
        validate_destination_label(&field.label)?;
    }

    let existing = match find_item_exact(options.vault, options.item) {
        Ok((item_id, vault_id, _, _)) => Some((item_id, vault_id)),
        Err(err) if is_op_lookup_miss(&err) => None,
        Err(err) => return Err(err),
    };
    let action = resolve_item_action(options.mode, existing.is_some())?;

    if options.dry_run {
        println!(
            "Would {} 1Password item {:?} with {} concealed field(s) in section {:?}",
            match action {
                ItemAction::Create => "create",
                ItemAction::Update => "update",
            },
            options.item,
            prepared.fields.len(),
            prepared.section_label
        );
        for field in &prepared.fields {
            println!(
                "op://{}/{}/{}/{}",
                options.vault.unwrap_or("<vault>"),
                options.item,
                secret_reference_component(&prepared.section_label),
                secret_reference_component(&field.label)
            );
        }
        return Ok(());
    }

    let stored = match (action, existing) {
        (ItemAction::Create, _) => create_cloudflare_item(options.vault, options.item, &prepared)?,
        (ItemAction::Update, Some((item_id, vault_id))) => {
            update_cloudflare_item(options.vault, &item_id, &vault_id, &prepared)?
        }
        (ItemAction::Update, None) => unreachable!("update action requires an existing item"),
    };

    invalidate_item_list_cache_best_effort();
    println!(
        "{} 1Password item {:?} with {} concealed field(s)",
        match action {
            ItemAction::Create => "Created",
            ItemAction::Update => "Updated",
        },
        options.item,
        prepared.fields.len()
    );
    for reference in stored.references {
        println!("{reference}");
    }
    Ok(())
}

fn validate_cloudflare_options(options: &CloudflareCredentialOptions<'_>) -> Result<()> {
    let input_count = usize::from(options.file.is_some())
        + usize::from(options.stdin)
        + usize::from(!options.command.is_empty());
    if input_count != 1 {
        return Err(anyhow!(
            "Choose exactly one input source: --stdin, --file <JSON>, or a command after '--'."
        ));
    }
    if options.raw && options.preset != CloudflareCredentialPreset::ApiResponse {
        return Err(anyhow!("--raw is only valid with --preset api-response."));
    }
    if options.item.trim().is_empty() {
        return Err(anyhow!("--item must not be empty."));
    }
    Ok(())
}

fn load_cloudflare_input(options: &CloudflareCredentialOptions<'_>) -> Result<Vec<u8>> {
    if let Some(path) = options.file {
        let metadata = fs::metadata(path).with_context(|| format!("read {}", path.display()))?;
        if metadata.len() > MAX_INPUT_BYTES {
            return Err(anyhow!(
                "input exceeds {} MiB limit: {}",
                MAX_INPUT_BYTES / 1024 / 1024,
                path.display()
            ));
        }
        return fs::read(path).with_context(|| format!("read {}", path.display()));
    }

    if options.stdin {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(MAX_INPUT_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("read Cloudflare credential from stdin")?;
        if bytes.len() as u64 > MAX_INPUT_BYTES {
            return Err(anyhow!(
                "stdin exceeds {} MiB limit",
                MAX_INPUT_BYTES / 1024 / 1024
            ));
        }
        return Ok(bytes);
    }

    let program = options
        .command
        .first()
        .ok_or_else(|| anyhow!("command input requires a program after '--'"))?;
    let output = Command::new(program)
        .args(&options.command[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("run Cloudflare input command {program:?}"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "Cloudflare input command {:?} failed with status {} (stderr suppressed to avoid leaking credentials)",
            program,
            output.status
        ));
    }
    if output.stdout.len() as u64 > MAX_INPUT_BYTES {
        return Err(anyhow!(
            "command output exceeds {} MiB limit",
            MAX_INPUT_BYTES / 1024 / 1024
        ));
    }
    Ok(output.stdout)
}

fn prepare_cloudflare_credential(
    preset: CloudflareCredentialPreset,
    section: Option<&str>,
    field: Option<&str>,
    raw: bool,
    input: &[u8],
) -> Result<PreparedCredential> {
    match preset {
        CloudflareCredentialPreset::ApiToken => {
            let value = parse_single_secret(input, field)?;
            ensure_nonempty_secret(&value)?;
            Ok(PreparedCredential {
                section_label: section.unwrap_or("Cloudflare").to_string(),
                fields: vec![DestinationField {
                    label: field.unwrap_or("CLOUDFLARE_API_TOKEN").to_string(),
                    value,
                }],
            })
        }
        CloudflareCredentialPreset::WorkerSecret => {
            let parsed = serde_json::from_slice::<serde_json::Value>(input).ok();
            let fields = match (parsed, field) {
                (Some(serde_json::Value::Object(values)), None) => values
                    .into_iter()
                    .map(|(label, value)| {
                        let value = json_value_to_secret(value)?;
                        ensure_nonempty_secret(&value)?;
                        Ok(DestinationField { label, value })
                    })
                    .collect::<Result<Vec<_>>>()?,
                (Some(value), Some(label)) => vec![DestinationField {
                    label: label.to_string(),
                    value: json_value_to_secret(value)?,
                }],
                (Some(value), None) => vec![DestinationField {
                    label: "WORKER_SECRET".to_string(),
                    value: json_value_to_secret(value)?,
                }],
                (None, label) => vec![DestinationField {
                    label: label.unwrap_or("WORKER_SECRET").to_string(),
                    value: trim_secret_bytes(input)?,
                }],
            };
            if fields.is_empty() {
                return Err(anyhow!("Worker secret JSON object must not be empty."));
            }
            for destination in &fields {
                validate_destination_label(&destination.label)?;
                ensure_nonempty_secret(&destination.value)?;
            }
            Ok(PreparedCredential {
                section_label: section.unwrap_or("Worker Secrets").to_string(),
                fields,
            })
        }
        CloudflareCredentialPreset::ApiResponse => {
            let mut value: serde_json::Value = serde_json::from_slice(input)
                .context("Cloudflare API response input must be valid JSON")?;
            if !raw {
                redact_cloudflare_json(&mut value);
            }
            Ok(PreparedCredential {
                section_label: section.unwrap_or("API Responses").to_string(),
                fields: vec![DestinationField {
                    label: field.unwrap_or("response").to_string(),
                    value: serde_json::to_string_pretty(&value)
                        .context("serialize Cloudflare API response")?,
                }],
            })
        }
    }
}

fn parse_single_secret(input: &[u8], requested_field: Option<&str>) -> Result<String> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(input) else {
        return trim_secret_bytes(input);
    };
    match value {
        serde_json::Value::Object(values) => {
            if let Some(field) = requested_field {
                if let Some(value) = values.get(field) {
                    return json_value_to_secret(value.clone());
                }
            }
            for key in ["token", "api_token", "apiToken", "value", "secret"] {
                if let Some(value) = values.get(key) {
                    return json_value_to_secret(value.clone());
                }
            }
            if values.len() == 1 {
                return json_value_to_secret(values.into_values().next().unwrap());
            }
            Err(anyhow!(
                "API token JSON must be a scalar or contain token, api_token, apiToken, value, or secret."
            ))
        }
        value => json_value_to_secret(value),
    }
}

fn trim_secret_bytes(input: &[u8]) -> Result<String> {
    let value = std::str::from_utf8(input).context("credential input must be UTF-8")?;
    Ok(value.trim_end_matches(['\r', '\n']).to_string())
}

fn json_value_to_secret(value: serde_json::Value) -> Result<String> {
    match value {
        serde_json::Value::String(value) => Ok(value),
        serde_json::Value::Null => Err(anyhow!("secret value must not be null")),
        value => serde_json::to_string(&value).context("serialize secret JSON value"),
    }
}

fn ensure_nonempty_secret(value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(anyhow!("secret value must not be empty"));
    }
    Ok(())
}

fn validate_destination_label(label: &str) -> Result<()> {
    if label.trim().is_empty() {
        return Err(anyhow!("destination field name must not be empty"));
    }
    Ok(())
}

pub(crate) fn redact_cloudflare_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                if is_sensitive_cloudflare_key(key) {
                    *child = serde_json::Value::String(REDACTED.to_string());
                } else {
                    redact_cloudflare_json(child);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                redact_cloudflare_json(child);
            }
        }
        _ => {}
    }
}

pub(crate) fn is_sensitive_cloudflare_key(key: &str) -> bool {
    let words = identifier_words(key);
    if words.iter().any(|word| {
        matches!(
            word.as_str(),
            "authorization"
                | "cookie"
                | "cookies"
                | "token"
                | "tokens"
                | "secret"
                | "secrets"
                | "key"
                | "keys"
        )
    }) {
        return true;
    }

    let collapsed = words.concat();
    collapsed.contains("authorization")
        || collapsed.contains("cookie")
        || collapsed.contains("token")
        || collapsed.contains("secret")
        || matches!(
            collapsed.as_str(),
            "apikey"
                | "accesskey"
                | "authkey"
                | "clientkey"
                | "encryptionkey"
                | "decryptionkey"
                | "masterkey"
                | "privatekey"
                | "publickey"
                | "signingkey"
        )
}

fn identifier_words(value: &str) -> Vec<String> {
    let mut expanded = String::with_capacity(value.len() * 2);
    let mut previous_lower_or_digit = false;
    for ch in value.chars() {
        if ch.is_ascii_uppercase() && previous_lower_or_digit {
            expanded.push('_');
        }
        if ch.is_ascii_alphanumeric() {
            expanded.push(ch.to_ascii_lowercase());
            previous_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        } else {
            expanded.push('_');
            previous_lower_or_digit = false;
        }
    }
    expanded
        .split('_')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

fn resolve_item_action(mode: CloudflareCredentialMode, exists: bool) -> Result<ItemAction> {
    match (mode, exists) {
        (CloudflareCredentialMode::Create, true) => Err(anyhow!(
            "1Password item already exists; use --mode update or --mode upsert."
        )),
        (CloudflareCredentialMode::Update, false) => Err(anyhow!(
            "1Password item does not exist; use --mode create or --mode upsert."
        )),
        (CloudflareCredentialMode::Create, false) => Ok(ItemAction::Create),
        (CloudflareCredentialMode::Update, true) => Ok(ItemAction::Update),
        (CloudflareCredentialMode::Upsert, true) => Ok(ItemAction::Update),
        (CloudflareCredentialMode::Upsert, false) => Ok(ItemAction::Create),
    }
}

#[derive(Debug)]
struct StoredCredential {
    references: Vec<String>,
}

type CloudflareSdkCreate = (String, String, serde_json::Value, String, Vec<String>);
type CloudflareSdkUpdate = (String, serde_json::Value, String, Vec<String>);

fn create_cloudflare_item(
    vault: Option<&str>,
    item_title: &str,
    prepared: &PreparedCredential,
) -> Result<StoredCredential> {
    if let Some(Ok((account, vault_id, params, section_id, field_ids))) =
        prepare_cloudflare_sdk_create(vault, item_title, prepared)
    {
        // Create is not safely retryable after submission: a timeout can mean
        // the item exists but the response was lost. Do not fall back to CLI.
        let item = sdk_bridge_call(
            &account,
            "items_create",
            serde_json::json!({"params": params}),
        )
        .context("create Cloudflare credential through isolated 1Password Desktop SDK")?;
        return build_sdk_stored_credential(item, &vault_id, section_id, field_ids);
    }

    let (template, section_id, field_ids) = build_create_template(item_title, prepared);
    let mut args = vec!["item".to_string(), "create".to_string()];
    if let Some(vault) = vault {
        args.push("--vault".to_string());
        args.push(vault.to_string());
    }
    args.push("--format=json".to_string());
    args.push("-".to_string());
    let item = run_op_item_template(&args, &template, "op item create")?;
    build_stored_credential(item, section_id, field_ids)
}

fn update_cloudflare_item(
    vault: Option<&str>,
    item_id: &str,
    vault_id: &str,
    prepared: &PreparedCredential,
) -> Result<StoredCredential> {
    if let Some(Ok((account, item, section_id, field_ids))) =
        prepare_cloudflare_sdk_update(item_id, vault_id, prepared)
    {
        // As with create, once a mutation is submitted we fail closed rather
        // than switching transports with an uncertain write outcome.
        let item = sdk_bridge_call(&account, "items_put", serde_json::json!({"item": item}))
            .context("update Cloudflare credential through isolated 1Password Desktop SDK")?;
        return build_sdk_stored_credential(item, vault_id, section_id, field_ids);
    }

    let mut get_args = vec!["item", "get", item_id, "--format", "json"];
    if let Some(vault) = vault {
        get_args.push("--vault");
        get_args.push(vault);
    }
    let mut template = op_json(&get_args)?;
    validate_cloudflare_item_category(&template)?;
    let (section_id, field_ids) = merge_prepared_fields(&mut template, prepared)?;
    let mut args = vec![
        "item".to_string(),
        "edit".to_string(),
        item_id.to_string(),
        "--format=json".to_string(),
    ];
    if let Some(vault) = vault {
        args.push("--vault".to_string());
        args.push(vault.to_string());
    }
    let item = run_op_item_template(&args, &template, "op item edit")?;
    let mut stored = build_stored_credential(item, section_id, field_ids)?;
    if stored.references.is_empty() {
        stored.references = prepared
            .fields
            .iter()
            .map(|field| {
                format!(
                    "op://{}/{}/{}/{}",
                    vault_id,
                    item_id,
                    secret_reference_component(&prepared.section_label),
                    secret_reference_component(&field.label)
                )
            })
            .collect();
    }
    Ok(stored)
}

fn prepare_cloudflare_sdk_create(
    vault: Option<&str>,
    item_title: &str,
    prepared: &PreparedCredential,
) -> Option<Result<CloudflareSdkCreate>> {
    if !desktop_sdk_enabled() {
        return None;
    }
    let vault_spec = vault?;
    let account = desktop_sdk_account()?;
    Some((|| {
        let vaults = sdk_vaults(&account)?;
        let selected = select_sdk_vaults(&vaults, Some(vault_spec))?;
        let [selected] = selected.as_slice() else {
            return Err(anyhow!(
                "Desktop SDK Cloudflare create requires exactly one vault"
            ));
        };
        let (params, section_id, field_ids) =
            build_cloudflare_sdk_create_params(item_title, prepared, &selected.id);
        Ok((account, selected.id.clone(), params, section_id, field_ids))
    })())
}

fn build_cloudflare_sdk_create_params(
    item_title: &str,
    prepared: &PreparedCredential,
    vault_id: &str,
) -> (serde_json::Value, String, Vec<String>) {
    let section_id = stable_id(&prepared.section_label, "cloudflare");
    let field_ids = unique_field_ids(&prepared.fields);
    let fields = prepared
        .fields
        .iter()
        .zip(&field_ids)
        .map(|(field, field_id)| {
            serde_json::json!({
                "id": field_id,
                "title": field.label,
                "sectionId": section_id,
                "fieldType": "Concealed",
                "value": field.value,
            })
        })
        .collect::<Vec<_>>();
    (
        serde_json::json!({
            "category": "ApiCredentials",
            "vaultId": vault_id,
            "title": item_title,
            "sections": [{"id": section_id, "title": prepared.section_label}],
            "fields": fields,
        }),
        section_id,
        field_ids,
    )
}

fn prepare_cloudflare_sdk_update(
    item_id: &str,
    vault_id: &str,
    prepared: &PreparedCredential,
) -> Option<Result<CloudflareSdkUpdate>> {
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
        validate_cloudflare_sdk_item_category(&item)?;
        let (section_id, field_ids) = merge_prepared_sdk_fields(&mut item, prepared)?;
        Ok((account, item, section_id, field_ids))
    })())
}

fn validate_cloudflare_sdk_item_category(item: &serde_json::Value) -> Result<()> {
    if item.get("category").and_then(serde_json::Value::as_str) != Some("ApiCredentials") {
        return Err(anyhow!(
            "Refusing to edit non-ApiCredentials item through the Desktop SDK"
        ));
    }
    Ok(())
}

fn merge_prepared_sdk_fields(
    item: &mut serde_json::Value,
    prepared: &PreparedCredential,
) -> Result<(String, Vec<String>)> {
    let object = item
        .as_object_mut()
        .ok_or_else(|| anyhow!("Desktop SDK item was not an object"))?;
    let sections = object
        .entry("sections")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| anyhow!("Desktop SDK item sections were not an array"))?;
    let section_id = sections
        .iter()
        .find_map(|section| {
            let title = section.get("title").and_then(serde_json::Value::as_str)?;
            title
                .eq_ignore_ascii_case(&prepared.section_label)
                .then(|| section.get("id").and_then(serde_json::Value::as_str))
                .flatten()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            let base = stable_id(&prepared.section_label, "cloudflare");
            let mut id = base.clone();
            let mut suffix = 2usize;
            while sections.iter().any(|section| {
                section.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())
            }) {
                id = format!("{base}_{suffix}");
                suffix += 1;
            }
            sections.push(serde_json::json!({"id": id, "title": prepared.section_label}));
            id
        });

    let fields = object
        .entry("fields")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| anyhow!("Desktop SDK item fields were not an array"))?;
    let mut field_ids = Vec::with_capacity(prepared.fields.len());
    for destination in &prepared.fields {
        let existing_index = fields.iter().position(|field| {
            field
                .get("title")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|title| title.eq_ignore_ascii_case(&destination.label))
                && field.get("sectionId").and_then(serde_json::Value::as_str)
                    == Some(section_id.as_str())
        });
        if let Some(index) = existing_index {
            let field = fields[index]
                .as_object_mut()
                .ok_or_else(|| anyhow!("Desktop SDK item field was not an object"))?;
            let field_id = field
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| stable_id(&destination.label, "field"));
            field.insert("id".into(), serde_json::Value::String(field_id.clone()));
            field.insert(
                "title".into(),
                serde_json::Value::String(destination.label.clone()),
            );
            field.insert(
                "sectionId".into(),
                serde_json::Value::String(section_id.clone()),
            );
            field.insert(
                "fieldType".into(),
                serde_json::Value::String("Concealed".into()),
            );
            field.insert(
                "value".into(),
                serde_json::Value::String(destination.value.clone()),
            );
            field_ids.push(field_id);
        } else {
            let mut field_id = stable_id(&destination.label, "field");
            let mut suffix = 2usize;
            while fields.iter().any(|field| {
                field.get("id").and_then(serde_json::Value::as_str) == Some(field_id.as_str())
            }) {
                field_id = format!("{}_{}", stable_id(&destination.label, "field"), suffix);
                suffix += 1;
            }
            fields.push(serde_json::json!({
                "id": field_id,
                "title": destination.label,
                "sectionId": section_id,
                "fieldType": "Concealed",
                "value": destination.value,
            }));
            field_ids.push(field_id);
        }
    }
    Ok((section_id, field_ids))
}

fn build_sdk_stored_credential(
    item: serde_json::Value,
    vault_id: &str,
    section_id: String,
    field_ids: Vec<String>,
) -> Result<StoredCredential> {
    let item_id = item
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("Desktop SDK item response did not include id"))?;
    Ok(StoredCredential {
        references: field_ids
            .into_iter()
            .map(|field_id| format!("op://{vault_id}/{item_id}/{section_id}/{field_id}"))
            .collect(),
    })
}

fn build_create_template(
    item_title: &str,
    prepared: &PreparedCredential,
) -> (serde_json::Value, String, Vec<String>) {
    let section_id = stable_id(&prepared.section_label, "cloudflare");
    let field_ids = unique_field_ids(&prepared.fields);
    let fields = prepared
        .fields
        .iter()
        .zip(&field_ids)
        .map(|(field, field_id)| {
            serde_json::json!({
                "id": field_id,
                "section": {"id": section_id},
                "type": "CONCEALED",
                "label": field.label,
                "value": field.value,
            })
        })
        .collect::<Vec<_>>();
    (
        serde_json::json!({
            "title": item_title,
            "category": "API_CREDENTIAL",
            "sections": [{"id": section_id, "label": prepared.section_label}],
            "fields": fields,
        }),
        section_id,
        field_ids,
    )
}

fn validate_cloudflare_item_category(item: &serde_json::Value) -> Result<()> {
    if let Some(category) = item.get("category").and_then(serde_json::Value::as_str) {
        if category != "API_CREDENTIAL" {
            return Err(anyhow!(
                "Refusing to edit 1Password item category {category:?}; cloudflare-credential only updates API_CREDENTIAL items."
            ));
        }
    }
    Ok(())
}

fn merge_prepared_fields(
    item: &mut serde_json::Value,
    prepared: &PreparedCredential,
) -> Result<(String, Vec<String>)> {
    let object = item
        .as_object_mut()
        .ok_or_else(|| anyhow!("op item JSON was not an object"))?;
    let sections = object
        .entry("sections")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| anyhow!("op item sections were not an array"))?;

    let section_id = sections
        .iter()
        .find_map(|section| {
            let label = section.get("label").and_then(serde_json::Value::as_str)?;
            label
                .eq_ignore_ascii_case(&prepared.section_label)
                .then(|| section.get("id").and_then(serde_json::Value::as_str))
                .flatten()
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            let base = stable_id(&prepared.section_label, "cloudflare");
            let mut id = base.clone();
            let mut suffix = 2usize;
            while sections.iter().any(|section| {
                section.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())
            }) {
                id = format!("{base}_{suffix}");
                suffix += 1;
            }
            sections.push(serde_json::json!({
                "id": id,
                "label": prepared.section_label,
            }));
            id
        });

    let fields = object
        .entry("fields")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| anyhow!("op item fields were not an array"))?;
    let mut field_ids = Vec::with_capacity(prepared.fields.len());
    for destination in &prepared.fields {
        let existing_index = fields.iter().position(|field| {
            let label_matches = field
                .get("label")
                .and_then(serde_json::Value::as_str)
                .map(|label| label.eq_ignore_ascii_case(&destination.label))
                .unwrap_or(false);
            let section_matches = field
                .get("section")
                .and_then(|section| section.get("id"))
                .and_then(serde_json::Value::as_str)
                .map(|id| id == section_id)
                .unwrap_or(false);
            label_matches && section_matches
        });
        if let Some(index) = existing_index {
            let field = fields[index]
                .as_object_mut()
                .ok_or_else(|| anyhow!("op item field was not an object"))?;
            let field_id = field
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| stable_id(&destination.label, "field"));
            field.insert(
                "id".to_string(),
                serde_json::Value::String(field_id.clone()),
            );
            field.insert("section".to_string(), serde_json::json!({"id": section_id}));
            field.insert(
                "type".to_string(),
                serde_json::Value::String("CONCEALED".to_string()),
            );
            field.insert(
                "label".to_string(),
                serde_json::Value::String(destination.label.clone()),
            );
            field.insert(
                "value".to_string(),
                serde_json::Value::String(destination.value.clone()),
            );
            field_ids.push(field_id);
        } else {
            let mut field_id = stable_id(&destination.label, "field");
            let mut suffix = 2usize;
            while fields.iter().any(|field| {
                field.get("id").and_then(serde_json::Value::as_str) == Some(field_id.as_str())
            }) {
                field_id = format!("{}_{}", stable_id(&destination.label, "field"), suffix);
                suffix += 1;
            }
            fields.push(serde_json::json!({
                "id": field_id,
                "section": {"id": section_id},
                "type": "CONCEALED",
                "label": destination.label,
                "value": destination.value,
            }));
            field_ids.push(field_id);
        }
    }
    Ok((section_id, field_ids))
}

fn unique_field_ids(fields: &[DestinationField]) -> Vec<String> {
    let mut used = HashSet::new();
    fields
        .iter()
        .map(|field| {
            let base = stable_id(&field.label, "field");
            let mut candidate = base.clone();
            let mut suffix = 2usize;
            while !used.insert(candidate.clone()) {
                candidate = format!("{base}_{suffix}");
                suffix += 1;
            }
            candidate
        })
        .collect()
}

fn stable_id(value: &str, fallback: &str) -> String {
    let mut id = String::new();
    let mut previous_separator = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch.to_ascii_lowercase());
            previous_separator = false;
        } else if !previous_separator && !id.is_empty() {
            id.push('_');
            previous_separator = true;
        }
    }
    while id.ends_with('_') {
        id.pop();
    }
    if id.is_empty() {
        format!("{}_{}", fallback, &stable_hex_hash(value)[..8])
    } else {
        id
    }
}

fn secret_reference_component(value: &str) -> String {
    stable_id(value, "field")
}

fn run_op_item_template(
    args: &[String],
    template: &serde_json::Value,
    operation: &str,
) -> Result<serde_json::Value> {
    let mut child = Command::new("op")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run `{operation}`"))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("failed to open stdin for `{operation}`"))?;
        serde_json::to_writer(stdin, template)
            .with_context(|| format!("failed to write `{operation}` template"))?;
    }
    let output = child
        .wait_with_output()
        .with_context(|| format!("failed to wait for `{operation}`"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "{operation} failed with status {} (output suppressed to avoid leaking credentials)",
            output.status
        ));
    }
    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("failed to parse `{operation}` JSON output"))
}

fn build_stored_credential(
    item: serde_json::Value,
    section_id: String,
    field_ids: Vec<String>,
) -> Result<StoredCredential> {
    let vault_id = item
        .get("vault")
        .and_then(|vault| vault.get("id"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("op item output did not include vault.id"))?;
    let item_id = item
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("op item output did not include id"))?;
    Ok(StoredCredential {
        references: field_ids
            .into_iter()
            .map(|field_id| format!("op://{vault_id}/{item_id}/{section_id}/{field_id}"))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_nested_sensitive_fields_without_redacting_unrelated_words() {
        let mut value = serde_json::json!({
            "result": [{
                "Authorization": "Bearer canary",
                "accessToken": "canary",
                "api_key": "canary",
                "apikey": "canary",
                "workerSecret": "canary",
                "setcookie": "canary",
                "Cookie": "canary",
                "monkey": "kept",
                "keyboard": "kept",
                "public": {"name": "kept"}
            }]
        });
        redact_cloudflare_json(&mut value);
        let object = value["result"][0].as_object().unwrap();
        for key in [
            "Authorization",
            "accessToken",
            "api_key",
            "apikey",
            "workerSecret",
            "setcookie",
            "Cookie",
        ] {
            assert_eq!(object[key], REDACTED);
        }
        assert_eq!(object["monkey"], "kept");
        assert_eq!(object["keyboard"], "kept");
        assert_eq!(object["public"]["name"], "kept");
    }

    #[test]
    fn api_response_is_redacted_by_default_and_raw_is_preserved() {
        let input = br#"{"result":{"token":"canary","id":"abc"}}"#;
        let redacted = prepare_cloudflare_credential(
            CloudflareCredentialPreset::ApiResponse,
            None,
            None,
            false,
            input,
        )
        .unwrap();
        assert!(redacted.fields[0].value.contains(REDACTED));
        assert!(!redacted.fields[0].value.contains("canary"));

        let raw = prepare_cloudflare_credential(
            CloudflareCredentialPreset::ApiResponse,
            None,
            None,
            true,
            input,
        )
        .unwrap();
        assert!(raw.fields[0].value.contains("canary"));
    }

    #[test]
    fn worker_secret_object_becomes_multiple_concealed_fields() {
        let prepared = prepare_cloudflare_credential(
            CloudflareCredentialPreset::WorkerSecret,
            Some("Production"),
            None,
            false,
            br#"{"DB_PASSWORD":"db-canary","API_KEY":"key-canary"}"#,
        )
        .unwrap();
        assert_eq!(prepared.section_label, "Production");
        assert_eq!(prepared.fields.len(), 2);
        let (template, section_id, field_ids) = build_create_template("worker", &prepared);
        assert_eq!(section_id, "production");
        assert_eq!(field_ids.len(), 2);
        assert!(template["fields"]
            .as_array()
            .unwrap()
            .iter()
            .all(|field| field["type"] == "CONCEALED"));
    }

    #[test]
    fn non_ascii_labels_receive_stable_nonempty_ids() {
        let first = stable_id("本番", "field");
        let second = stable_id("本番", "field");
        assert_eq!(first, second);
        assert!(first.starts_with("field_"));
        assert_ne!(first, stable_id("監査", "field"));
    }

    #[test]
    fn rejects_update_of_non_api_credential_item() {
        let item = serde_json::json!({"category":"LOGIN"});
        let error = validate_cloudflare_item_category(&item).unwrap_err();
        assert!(error.to_string().contains("only updates API_CREDENTIAL"));
    }

    #[test]
    fn update_preserves_unrelated_fields_and_replaces_matching_field() {
        let mut item = serde_json::json!({
            "id": "item-id",
            "sections": [{"id":"cloudflare","label":"Cloudflare"}],
            "fields": [
                {"id":"api_token","section":{"id":"cloudflare"},"type":"CONCEALED","label":"CLOUDFLARE_API_TOKEN","value":"old"},
                {"id":"other","type":"STRING","label":"other","value":"keep"}
            ]
        });
        let prepared = PreparedCredential {
            section_label: "Cloudflare".to_string(),
            fields: vec![DestinationField {
                label: "CLOUDFLARE_API_TOKEN".to_string(),
                value: "new".to_string(),
            }],
        };
        let (_, field_ids) = merge_prepared_fields(&mut item, &prepared).unwrap();
        assert_eq!(field_ids, ["api_token"]);
        assert_eq!(item["fields"][0]["value"], "new");
        assert_eq!(item["fields"][1]["value"], "keep");
    }
    #[test]
    fn sdk_create_params_use_official_item_shape_and_concealed_fields() {
        let prepared = PreparedCredential {
            section_label: "Cloudflare".to_string(),
            fields: vec![DestinationField {
                label: "CLOUDFLARE_API_TOKEN".to_string(),
                value: "canary-secret".to_string(),
            }],
        };
        let (params, section_id, field_ids) =
            build_cloudflare_sdk_create_params("worker", &prepared, "vault-1");
        assert_eq!(params["category"], "ApiCredentials");
        assert_eq!(params["vaultId"], "vault-1");
        assert_eq!(params["sections"][0]["id"], section_id);
        assert_eq!(params["sections"][0]["title"], "Cloudflare");
        assert_eq!(params["fields"][0]["id"], field_ids[0]);
        assert_eq!(params["fields"][0]["title"], "CLOUDFLARE_API_TOKEN");
        assert_eq!(params["fields"][0]["sectionId"], section_id);
        assert_eq!(params["fields"][0]["fieldType"], "Concealed");
        assert_eq!(params["fields"][0]["value"], "canary-secret");
    }

    #[test]
    fn sdk_update_preserves_unrelated_fields_and_replaces_matching_field() {
        let mut item = serde_json::json!({
            "id": "item-id",
            "category": "ApiCredentials",
            "vaultId": "vault-id",
            "opaque": {"keep": true},
            "sections": [{"id":"cloudflare","title":"Cloudflare"}],
            "fields": [
                {"id":"api_token","title":"CLOUDFLARE_API_TOKEN","sectionId":"cloudflare","fieldType":"Concealed","value":"old"},
                {"id":"other","title":"other","fieldType":"Text","value":"keep","details":{"opaque":true}}
            ]
        });
        let prepared = PreparedCredential {
            section_label: "Cloudflare".to_string(),
            fields: vec![DestinationField {
                label: "CLOUDFLARE_API_TOKEN".to_string(),
                value: "new".to_string(),
            }],
        };
        validate_cloudflare_sdk_item_category(&item).unwrap();
        let (_, field_ids) = merge_prepared_sdk_fields(&mut item, &prepared).unwrap();
        assert_eq!(field_ids, ["api_token"]);
        assert_eq!(item["fields"][0]["value"], "new");
        assert_eq!(item["fields"][1]["value"], "keep");
        assert_eq!(item["fields"][1]["details"]["opaque"], true);
        assert_eq!(item["opaque"]["keep"], true);
    }

    #[test]
    fn sdk_cloudflare_category_guard_does_not_echo_item_content() {
        let item = serde_json::json!({
            "category": "Login",
            "fields": [{"value":"canary-secret"}]
        });
        let error = validate_cloudflare_sdk_item_category(&item).unwrap_err();
        assert!(!error.to_string().contains("canary-secret"));
    }
}
