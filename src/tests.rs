use super::*;
use clap::CommandFactory;
use proptest::prelude::*;
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
    fn list_tools(&mut self) -> Result<Vec<String>> {
        Ok(vec![
            "append_variables".to_string(),
            "authenticate".to_string(),
            "create_environment".to_string(),
            "create_local_env_file".to_string(),
            "list_environments".to_string(),
            "list_local_env_files".to_string(),
            "list_variables".to_string(),
            "rename_environment".to_string(),
        ])
    }

    fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> Result<serde_json::Value> {
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

#[cfg(unix)]
#[test]
fn test_write_env_file_uses_restrictive_permissions_and_preserves_existing_mode() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let dir = TempDir::new().unwrap();
    let new_path = dir.path().join(".env.new");
    write_env_file(&new_path, &["API_KEY=op://vault/item/API_KEY".to_string()]).unwrap();
    assert_eq!(fs::metadata(&new_path).unwrap().mode() & 0o777, 0o600);

    let existing_path = dir.path().join(".env.existing");
    fs::write(&existing_path, "KEEP=value\n").unwrap();
    fs::set_permissions(&existing_path, fs::Permissions::from_mode(0o640)).unwrap();
    write_env_file(
        &existing_path,
        &["API_KEY=op://vault/item/API_KEY".to_string()],
    )
    .unwrap();
    assert_eq!(fs::metadata(&existing_path).unwrap().mode() & 0o777, 0o640);
    assert!(fs::read_dir(dir.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".opz-")
    }));
}

#[cfg(unix)]
#[test]
fn test_write_env_file_rejects_symlink_targets() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().unwrap();
    let real = dir.path().join("real.env");
    fs::write(&real, "KEEP=value\n").unwrap();
    let link = dir.path().join("linked.env");
    symlink(&real, &link).unwrap();
    let error = write_env_file(&link, &["A=op://vault/item/A".to_string()]).unwrap_err();
    assert!(error.to_string().contains("symlink"));
    assert_eq!(fs::read_to_string(&real).unwrap(), "KEEP=value\n");
}

#[test]
fn test_write_env_file_rejects_non_regular_target() {
    let dir = TempDir::new().unwrap();
    let directory = dir.path().join("env-dir");
    fs::create_dir(&directory).unwrap();
    let error = write_env_file(&directory, &["A=op://vault/item/A".to_string()]).unwrap_err();
    assert!(error.to_string().contains("non-regular"));
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

#[cfg(unix)]
#[test]
fn test_temp_env_file_is_mode_0600() {
    use std::os::unix::fs::MetadataExt;

    let temp_env = TempEnvFile::create().unwrap();
    assert_eq!(fs::metadata(temp_env.path()).unwrap().mode() & 0o777, 0o600);
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
    let json = r#"{"id": "abc123", "title": "My Item", "vault": {"id": "v1", "name": "Personal"}}"#;
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
fn test_cache_models_are_metadata_only() {
    const CANARY: &str = "OPZ_CANARY_CACHE_SECRET_28d1";
    let item_cache = vec![ItemListEntry {
        id: "item-id".to_string(),
        title: "service".to_string(),
        vault: Some(ItemVault {
            id: "vault-id".to_string(),
            name: "Private".to_string(),
        }),
    }];
    let repository_cache = vec![ItemGithubRepositories {
        item_title: "service".to_string(),
        repositories: vec!["owner/repo".to_string()],
    }];
    let serialized = format!(
        "{}{}",
        serde_json::to_string(&item_cache).unwrap(),
        serde_json::to_string(&repository_cache).unwrap()
    );
    assert!(!serialized.contains(CANARY));
    assert!(!serialized.contains("fields"));
    assert!(!serialized.contains("value"));
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
fn test_mask_create_stdout_masks_values_everywhere() {
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
        "ID: abc123\nTitle: my-[REDACTED]-item\nAPI_KEY: [REDACTED]\nDB_HOST: [REDACTED]\n"
    );
}

#[test]
fn test_mask_create_stdout_masks_multiline_notes_field() {
    let template = build_secure_note_template("f4ah6o/opz", "```app.conf\nTOKEN=abc\n```");

    let masked = mask_create_stdout(
        "ID: abc123\nnotesPlain: ```app.conf\nTOKEN=abc\n```\nTitle: f4ah6o/opz\n",
        &collect_create_stdout_sensitive_fields(&template),
    );

    assert_eq!(
        masked,
        "ID: abc123\nnotesPlain: [REDACTED]\nTitle: f4ah6o/opz\n"
    );
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

    let err =
        command_output_with_timeout(cmd, "`slow command`", Duration::from_millis(100)).unwrap_err();

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
    let migration = migrate_package_json_scripts(content, ScriptMigrationMode::Collect).unwrap();

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
    let cli = Cli::try_parse_from(["opz", "env", "--account", "A1", "variables", "dev"]).unwrap();
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
        run_environment_command(&mut client, Some("A1"), &McpEnvironmentAction::List).unwrap();
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
        &McpEnvironmentAction::Variables {
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
        &McpEnvironmentAction::Mount {
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
fn test_environment_add_appends_concealed_empty_placeholders() {
    let mut client = FakeMcpClient::new(vec![
        serde_json::json!({"structuredContent": {"environments": [{"id": "env1", "name": "dev"}]}}),
        serde_json::json!({"structuredContent": {"updated": true}}),
    ]);
    let output = run_environment_command(
        &mut client,
        Some("A1"),
        &McpEnvironmentAction::Add {
            environment: "dev".to_string(),
            variables: vec!["API_TOKEN".to_string(), "DB_URL".to_string()],
        },
    )
    .unwrap();
    assert_eq!(output, "API_TOKEN\nDB_URL\n");
    assert_eq!(client.calls[1].0, "append_variables");
    assert_eq!(client.calls[1].1["variables"][0]["name"], "API_TOKEN");
    assert_eq!(client.calls[1].1["variables"][0]["value"], "");
    assert_eq!(client.calls[1].1["variables"][0]["concealed"], true);
}

#[test]
fn test_environment_add_rejects_invalid_variable_names_before_mcp_mutation() {
    let mut client = FakeMcpClient::new(vec![serde_json::json!({
        "structuredContent": {"environments": [{"id": "env1", "name": "dev"}]}
    })]);
    let err = run_environment_command(
        &mut client,
        Some("A1"),
        &McpEnvironmentAction::Add {
            environment: "dev".to_string(),
            variables: vec!["NOT-VALID".to_string()],
        },
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("Invalid Environment variable name"));
    assert_eq!(client.calls.len(), 1);
}

#[test]
fn test_environment_tools_does_not_authenticate() {
    let mut client = FakeMcpClient::new(vec![]);
    let output = run_environment_command(&mut client, None, &McpEnvironmentAction::Tools).unwrap();
    assert!(output.contains("append_variables\n"));
    assert!(client.calls.is_empty());
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
        &McpEnvironmentAction::Mounts {
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
fn test_desktop_sdk_diagnostics_are_actionable_and_redacted() {
    let unavailable = render_doctor_checks(&[desktop_sdk_unavailable_check()]);
    assert_eq!(
        unavailable,
        "warn  1Password Desktop SDK: unavailable; enable: Settings → Developer → Integrate with the 1Password SDKs → Integrate with other apps\n"
    );

    let fallback = desktop_sdk_fallback_warning_message();
    assert_eq!(
        fallback,
        "warn: 1Password Desktop SDK unavailable; using op CLI fallback. enable: Settings → Developer → Integrate with the 1Password SDKs → Integrate with other apps"
    );
    assert!(!fallback.contains("authorization"));
    assert!(!fallback.contains("IPC"));
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
fn test_help_and_skill_cover_visible_commands() {
    let mut command = Cli::command();
    let help = command.render_long_help().to_string();
    for name in [
        "find",
        "doctor",
        "environment",
        "plugin",
        "skills",
        "show",
        "gen",
        "migrate",
        "note",
        "github-repo",
        "run",
        "github-secret",
        "cloudflare-secret",
    ] {
        assert!(help.contains(name), "top-level help is missing `{name}`");
        assert!(
            OPZ_SKILL.contains(&format!("### `{name}`")),
            "bundled skill is missing `{name}`"
        );
    }

    let command = Cli::command();
    let mut environment = command
        .find_subcommand("environment")
        .expect("environment subcommand")
        .clone();
    let environment_help = environment.render_long_help().to_string();
    assert!(!environment_help.contains("--environment <ENV>"));

    let command = Cli::command();
    let mut plugin = command
        .find_subcommand("plugin")
        .expect("plugin subcommand")
        .clone();
    let plugin_help = plugin.render_long_help().to_string();
    for name in ["list", "show", "run"] {
        assert!(
            plugin_help.contains(name),
            "plugin help is missing `{name}`"
        );
    }
    assert!(OPZ_SKILL.contains("### `plugin`"));
    assert!(OPZ_SKILL.contains("opz plugin list"));
    assert!(OPZ_SKILL.contains("opz plugin show <NAME[@VERSION]>"));
    assert!(OPZ_SKILL.contains("opz plugin run <NAME[@VERSION]>"));

    let command = Cli::command();
    let mut run = command
        .find_subcommand("run")
        .expect("run subcommand")
        .clone();
    let run_help = run.render_long_help().to_string();
    assert!(run_help.contains("--environment <ENV>"));
    for (name, skill_marker) in [
        ("list", "] list"),
        ("create", "] create <NAME>"),
        ("rename", "] rename <ENVIRONMENT> <NEW_NAME>"),
        ("variables", "] variables <ENVIRONMENT>"),
        ("add", "] add <ENVIRONMENT> <NAME>..."),
        ("mount", "] mount <ENVIRONMENT> <PATH>"),
        ("mounts", "] mounts <ENVIRONMENT>"),
        ("tools", "opz environment tools"),
    ] {
        assert!(
            environment_help.contains(name),
            "environment help is missing `{name}`"
        );
        assert!(
            OPZ_SKILL.contains(skill_marker),
            "bundled skill is missing environment command `{name}`"
        );
    }
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
    assert!(OPZ_SKILL.contains("opz plugin list"));
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
            ..
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
    assert!(cli.run_environments.is_empty());
    match cli.cmd {
        Some(Cmd::Run {
            environments,
            items,
            command,
            ..
        }) => {
            assert_eq!(environments, vec!["dev".to_string()]);
            assert!(items.is_empty());
            assert_eq!(command, vec!["env".to_string()]);
        }
        _ => panic!("expected run command"),
    }
}

#[test]
fn test_cli_parse_run_with_environments_alias() {
    let cli = Cli::try_parse_from(["opz", "run", "--environments", "dev", "--", "env"]).unwrap();
    assert!(cli.run_environments.is_empty());
    match cli.cmd {
        Some(Cmd::Run { environments, .. }) => {
            assert_eq!(environments, vec!["dev".to_string()]);
        }
        _ => panic!("expected run command"),
    }
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
    assert!(cli.run_environments.is_empty());
    match cli.cmd {
        Some(Cmd::Run { environments, .. }) => {
            assert_eq!(environments, vec!["dev".to_string(), "staging".to_string()]);
        }
        _ => panic!("expected run command"),
    }
}

#[test]
fn test_cli_parse_top_level_with_environment() {
    let cli = Cli::try_parse_from(["opz", "--environment", "dev", "--", "env"]).unwrap();
    assert!(cli.cmd.is_none());
    assert_eq!(cli.run_environments, vec!["dev".to_string()]);
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
fn test_vault_option_rejected_for_environment_commands() {
    let args = vec![
        OsString::from("opz"),
        OsString::from("environment"),
        OsString::from("--vault"),
        OsString::from("Private"),
        OsString::from("tools"),
    ];
    let err = run_cli(&args).unwrap_err();
    assert!(err
        .to_string()
        .contains("`--vault` cannot be combined with `opz environment`"));
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
    assert!(err
        .to_string()
        .contains("unexpected argument '--environment'"));
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
fn test_cli_parse_cloudflare_credential() {
    let cli = Cli::try_parse_from([
        "opz",
        "cloudflare-credential",
        "--vault",
        "Private",
        "--preset",
        "api-response",
        "--mode",
        "update",
        "--item",
        "cloudflare-api",
        "--section",
        "Audit",
        "--field",
        "zones",
        "--raw",
        "--dry-run",
        "--",
        "cloudflare-client",
        "zones",
        "list",
    ])
    .unwrap();
    assert_eq!(cli.vault.as_deref(), Some("Private"));
    match cli.cmd {
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
        }) => {
            assert_eq!(preset, CloudflareCredentialPreset::ApiResponse);
            assert_eq!(mode, CloudflareCredentialMode::Update);
            assert_eq!(item, "cloudflare-api");
            assert_eq!(section.as_deref(), Some("Audit"));
            assert_eq!(field.as_deref(), Some("zones"));
            assert!(!stdin);
            assert!(file.is_none());
            assert!(raw);
            assert!(dry_run);
            assert_eq!(
                command,
                vec![
                    "cloudflare-client".to_string(),
                    "zones".to_string(),
                    "list".to_string()
                ]
            );
        }
        _ => panic!("expected cloudflare-credential command"),
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
    env_vars.insert("API_TOKEN".to_string(), SecretValue::new("secret-token"));
    env_vars.insert("DB_URL".to_string(), SecretValue::new("postgres://example"));
    env_vars.insert("UNUSED".to_string(), SecretValue::new("unused"));

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

proptest! {
    #[test]
    fn pbt_github_remote_formats_normalize_identically(
        owner in "[A-Za-z0-9][A-Za-z0-9._-]{0,20}",
        repository in "[A-Za-z0-9][A-Za-z0-9._-]{0,20}"
    ) {
        let expected = format!("{}/{}", owner.to_ascii_lowercase(), repository.to_ascii_lowercase());
        let https = format!("https://github.com/{owner}/{repository}.git");
        let ssh = format!("git@github.com:{owner}/{repository}.git");
        let plain = format!("{owner}/{repository}");
        prop_assert_eq!(normalize_github_repo_spec(&https), Some(expected.clone()));
        prop_assert_eq!(normalize_github_repo_spec(&ssh), Some(expected.clone()));
        prop_assert_eq!(normalize_github_repo_spec(&plain), Some(expected));
    }

    #[test]
    fn pbt_merge_env_lines_is_unique_and_last_write_wins(
        entries in prop::collection::vec(
            ("[A-Z_][A-Z0-9_]{0,12}", "[a-zA-Z0-9._:/-]{0,24}"),
            0..80
        )
    ) {
        let sections = entries
            .iter()
            .enumerate()
            .map(|(index, (key, value))| {
                (format!("item-{index}"), vec![format!("{key}={value}")])
            })
            .collect::<Vec<_>>();
        let merged = merge_env_lines(&sections);
        let mut expected = HashMap::new();
        for (key, value) in &entries {
            expected.insert(key.clone(), value.clone());
        }
        let actual = merged
            .iter()
            .filter_map(|line| parse_env_line_kv(line))
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<HashMap<_, _>>();
        prop_assert_eq!(merged.len(), actual.len());
        prop_assert_eq!(actual, expected);
    }
}

#[test]
fn test_desktop_sdk_account_from_list_requires_exactly_one_account() {
    let one = serde_json::json!([{
        "account_uuid": "A1",
        "user_uuid": "U1",
        "email": "user@example.test",
        "url": "example.1password.com"
    }]);
    assert_eq!(desktop_sdk_account_from_list(&one).as_deref(), Some("A1"));

    assert_eq!(desktop_sdk_account_from_list(&serde_json::json!([])), None);
    assert_eq!(
        desktop_sdk_account_from_list(&serde_json::json!([
            {"account_uuid": "A1"},
            {"account_uuid": "A2"}
        ])),
        None
    );
    assert_eq!(
        desktop_sdk_account_from_list(&serde_json::json!([{"url": "example.1password.com"}])),
        None
    );
}

#[test]
fn test_set_sdk_item_text_field_updates_existing_without_dropping_other_data() {
    let mut item = serde_json::json!({
        "id": "item-1",
        "title": "service",
        "category": "ApiCredentials",
        "fields": [
            {"id": "token", "title": "TOKEN", "fieldType": "Concealed", "value": "sensitive-canary"},
            {"id": "github_repositories", "title": "github_repositories", "fieldType": "Text", "value": "old/repo", "extra": "keep"}
        ],
        "extraTopLevel": {"keep": true}
    });
    set_sdk_item_text_field(&mut item, "github_repositories", "owner/repo\nother/repo").unwrap();
    assert_eq!(item["fields"][0]["value"], "sensitive-canary");
    assert_eq!(item["fields"][1]["value"], "owner/repo\nother/repo");
    assert_eq!(item["fields"][1]["extra"], "keep");
    assert_eq!(item["extraTopLevel"]["keep"], true);
}

#[test]
fn test_set_sdk_item_text_field_appends_compatible_text_field() {
    let mut item = serde_json::json!({"fields": []});
    set_sdk_item_text_field(&mut item, "github_repositories", "owner/repo").unwrap();
    assert_eq!(
        item["fields"][0],
        serde_json::json!({
            "id": "github_repositories",
            "title": "github_repositories",
            "fieldType": "Text",
            "value": "owner/repo"
        })
    );
}

#[test]
fn test_sdk_item_mutators_fail_closed_without_echoing_payloads() {
    let mut malformed = serde_json::json!({"fields": "sensitive-canary"});
    let error = set_sdk_item_text_field(&mut malformed, "github_repositories", "new").unwrap_err();
    assert!(!error.to_string().contains("sensitive-canary"));

    let mut item = serde_json::json!({"title": "old", "opaque": "keep"});
    set_sdk_item_title(&mut item, "new").unwrap();
    assert_eq!(item["title"], "new");
    assert_eq!(item["opaque"], "keep");
}

#[test]
fn test_sdk_item_get_maps_sdk_field_titles_to_opz_labels() {
    let vault = ItemVault {
        id: "vault-1".to_string(),
        name: "Personal".to_string(),
    };
    let value = serde_json::json!({
        "id": "item-1",
        "title": "service",
        "fields": [
            {"id": "f1", "title": "API_KEY", "fieldType": "Concealed", "value": "canary"},
            {"id": "f2", "title": "EMPTY", "fieldType": "String", "value": ""}
        ]
    });
    let item = sdk_item_get(&value, &vault).unwrap();
    assert_eq!(item.id.as_deref(), Some("item-1"));
    assert_eq!(item.title.as_deref(), Some("service"));
    assert_eq!(
        item.vault.as_ref().map(|vault| vault.id.as_str()),
        Some("vault-1")
    );
    assert_eq!(item.fields.len(), 2);
    assert_eq!(item.fields[0].label.as_deref(), Some("API_KEY"));
    assert_eq!(
        item.fields[0]
            .value
            .as_ref()
            .and_then(serde_json::Value::as_str),
        Some("canary")
    );
}

#[test]
fn test_select_exact_item_entries_preserves_order_and_requires_uniqueness() {
    let vault = ItemVault {
        id: "v1".into(),
        name: "Personal".into(),
    };
    let entries = vec![
        ItemListEntry {
            id: "i1".into(),
            title: "alpha".into(),
            vault: Some(vault.clone()),
        },
        ItemListEntry {
            id: "i2".into(),
            title: "beta".into(),
            vault: Some(vault.clone()),
        },
    ];
    let selected =
        select_exact_item_entries(&entries, &["beta".to_string(), "alpha".to_string()]).unwrap();
    assert_eq!(selected[0].id, "i2");
    assert_eq!(selected[1].id, "i1");
    assert!(select_exact_item_entries(&entries, &["missing".to_string()]).is_err());

    let mut ambiguous = entries;
    ambiguous.push(ItemListEntry {
        id: "i3".into(),
        title: "alpha".into(),
        vault: Some(vault),
    });
    assert!(select_exact_item_entries(&ambiguous, &["alpha".to_string()]).is_err());
}

#[test]
fn test_select_sdk_vaults_accepts_id_or_name_and_rejects_ambiguity() {
    let vaults = vec![
        ItemVault {
            id: "v1".into(),
            name: "Personal".into(),
        },
        ItemVault {
            id: "v2".into(),
            name: "Work".into(),
        },
    ];
    assert_eq!(select_sdk_vaults(&vaults, Some("v1")).unwrap()[0].id, "v1");
    assert_eq!(
        select_sdk_vaults(&vaults, Some("Work")).unwrap()[0].id,
        "v2"
    );
    assert_eq!(select_sdk_vaults(&vaults, None).unwrap().len(), 2);
    assert!(select_sdk_vaults(&vaults, Some("missing")).is_err());

    let ambiguous = vec![
        ItemVault {
            id: "v1".into(),
            name: "Same".into(),
        },
        ItemVault {
            id: "v2".into(),
            name: "Same".into(),
        },
    ];
    assert!(select_sdk_vaults(&ambiguous, Some("Same")).is_err());
}

#[test]
fn test_sdk_item_create_params_maps_api_credentials_without_echoing_shape_errors() {
    let template = ItemCreateTemplate {
        title: "service".into(),
        category: "API_CREDENTIAL".into(),
        fields: vec![ItemCreateField {
            id: "API_KEY".into(),
            field_type: "STRING".into(),
            label: "API_KEY".into(),
            value: "canary-secret".into(),
            purpose: None,
        }],
    };
    let params = sdk_item_create_params(&template, "vault-1").unwrap();
    assert_eq!(params["category"], "ApiCredentials");
    assert_eq!(params["vaultId"], "vault-1");
    assert_eq!(params["title"], "service");
    assert_eq!(params["fields"][0]["id"], "API_KEY");
    assert_eq!(params["fields"][0]["title"], "API_KEY");
    assert_eq!(params["fields"][0]["fieldType"], "Text");
    assert_eq!(params["fields"][0]["value"], "canary-secret");

    let unsupported = ItemCreateTemplate {
        title: "secret-title".into(),
        category: "UNKNOWN".into(),
        fields: vec![],
    };
    let error = sdk_item_create_params(&unsupported, "vault-1").unwrap_err();
    assert!(!error.to_string().contains("secret-title"));
}

#[test]
fn test_sdk_item_create_params_maps_secure_note_to_notes() {
    let template = build_secure_note_template("repo", "```env\nTOKEN=canary\n```");
    let params = sdk_item_create_params(&template, "vault-2").unwrap();
    assert_eq!(params["category"], "SecureNote");
    assert_eq!(params["vaultId"], "vault-2");
    assert_eq!(params["title"], "repo");
    assert_eq!(params["notes"], "```env\nTOKEN=canary\n```");
    assert!(params.get("fields").is_none());
}
