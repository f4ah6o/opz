use std::fs;
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

const PLUGIN_SHA: &str = "570d1aae0d54b0ace5bd33cba9a128f6157b854b99b8296260f7901f98bb7c48";

#[cfg(unix)]
fn make_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, body).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[test]
fn plugin_list_and_show_include_bundled_manifest() {
    let list = Command::new(env!("CARGO_BIN_EXE_opz"))
        .arg("plugin")
        .arg("list")
        .output()
        .unwrap();
    assert!(list.status.success());
    let stdout = String::from_utf8(list.stdout).unwrap();
    assert!(stdout.contains("opencode-go-codex\t0.1.0\tbundled"));

    let show = Command::new(env!("CARGO_BIN_EXE_opz"))
        .arg("plugin")
        .arg("show")
        .arg("opencode-go-codex")
        .output()
        .unwrap();
    assert!(show.status.success());
    let stdout = String::from_utf8(show.stdout).unwrap();
    assert!(stdout.contains("name = \"opencode-go-codex\""));
    assert!(stdout.contains("target_commands = [\"codex\"]"));
}

#[test]
fn plugin_list_uses_local_registry_override() {
    let temp = tempfile::tempdir().unwrap();
    let plugin_dir = temp.path().join("plugins").join("opencode-go-codex");
    fs::create_dir_all(&plugin_dir).unwrap();
    let manifest = bundled_manifest_text().replace(
        "description = \"Run Codex with OpenCode Go models\"",
        "description = \"Local registry override\"",
    );
    fs::write(plugin_dir.join("plugin.toml"), &manifest).unwrap();
    let sha = sha256_hex(manifest.as_bytes());
    fs::write(
        temp.path().join("registry.toml"),
        format!(
            r#"schema_version = 1

[[plugins]]
name = "opencode-go-codex"
version = "0.1.0"
source = "github:opz-rs/opz-plugin/plugins/opencode-go-codex"
path = "plugins/opencode-go-codex/plugin.toml"
sha256 = "{sha}"
description = "Local registry override"
target_commands = ["codex"]
"#
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_opz"))
        .arg("plugin")
        .arg("list")
        .env("OPZ_PLUGIN_REGISTRY_DIR", temp.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("opencode-go-codex\t0.1.0\tlocal\tLocal registry override"),
        "{stdout}"
    );
}

#[test]
#[cfg(unix)]
fn plugin_run_generates_codex_home_and_only_allowlisted_secret_env() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let log = temp.path().join("codex.log");
    let copied_config = temp.path().join("config.toml");
    let mode_file = temp.path().join("mode.txt");

    make_fake_op(&bin.join("op"), "example-item");
    make_executable(
        &bin.join("codex"),
        &format!(
            r#"#!/bin/sh
set -eu
test "${{OPENCODE_GO_API_KEY}}" = "secret-api-key"
test "${{UNRELATED_SECRET:-}}" = ""
test "${{OPZ_PLUGIN:-}}" = ""
test -f "${{CODEX_HOME}}/config.toml"
cat "${{CODEX_HOME}}/config.toml" > "{}"
stat -f "%Lp" "${{CODEX_HOME}}/config.toml" > "{}" 2>/dev/null || stat -c "%a" "${{CODEX_HOME}}/config.toml" > "{}"
printf 'ok %s\n' "$CODEX_HOME" > "{}"
"#,
            copied_config.display(),
            mode_file.display(),
            mode_file.display(),
            log.display()
        ),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_opz"))
        .arg("plugin")
        .arg("run")
        .arg("opencode-go-codex")
        .arg("--item")
        .arg("example-item")
        .arg("--")
        .arg("codex")
        .env("PATH", test_path(&bin))
        .env("OPZ_SKIP_CREDENTIAL_SCAN", "1")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let config = fs::read_to_string(copied_config).unwrap();
    assert!(config.contains("model = \"kimi-k2.7\""));
    assert!(config.contains("base_url = \"https://example.invalid/v1\""));
    assert_eq!(fs::read_to_string(mode_file).unwrap().trim(), "600");
    assert!(fs::read_to_string(log).unwrap().contains("ok "));
}

#[test]
#[cfg(unix)]
fn top_level_run_auto_applies_item_plugin() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let log = temp.path().join("codex.log");

    make_fake_op(&bin.join("op"), "owner/repo");
    make_executable(
        &bin.join("codex"),
        &format!(
            r#"#!/bin/sh
set -eu
test "${{OPENCODE_GO_API_KEY}}" = "secret-api-key"
test -f "${{CODEX_HOME}}/config.toml"
printf 'auto\n' > "{}"
"#,
            log.display()
        ),
    );
    run_checked(Command::new("git").arg("init").current_dir(temp.path()));
    run_checked(
        Command::new("git")
            .arg("remote")
            .arg("add")
            .arg("origin")
            .arg("https://github.com/owner/repo.git")
            .current_dir(temp.path()),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_opz"))
        .arg("--")
        .arg("codex")
        .env("PATH", test_path(&bin))
        .env("OPZ_SKIP_CREDENTIAL_SCAN", "1")
        .current_dir(temp.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(log).unwrap(), "auto\n");
}

#[test]
#[cfg(unix)]
fn plugin_rejects_target_command_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    make_fake_op(&bin.join("op"), "example-item");

    let output = Command::new(env!("CARGO_BIN_EXE_opz"))
        .arg("plugin")
        .arg("run")
        .arg("opencode-go-codex")
        .arg("--item")
        .arg("example-item")
        .arg("--")
        .arg("not-codex")
        .env("PATH", test_path(&bin))
        .env("OPZ_SKIP_CREDENTIAL_SCAN", "1")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("OPZ_PLUGIN_TARGET_MISMATCH"), "{stderr}");
}

#[test]
#[cfg(unix)]
fn plugin_rejects_item_hash_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    make_fake_op_with_options(
        &bin.join("op"),
        "example-item",
        FakeOpOptions {
            plugin_sha: "0000000000000000000000000000000000000000000000000000000000000000",
            ..FakeOpOptions::default()
        },
    );

    let output = run_plugin_with_bin(&bin, "example-item", "codex");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("OPZ_PLUGIN_HASH_MISMATCH"), "{stderr}");
}

#[test]
#[cfg(unix)]
fn plugin_rejects_item_source_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    make_fake_op_with_options(
        &bin.join("op"),
        "example-item",
        FakeOpOptions {
            plugin_source: "github:opz-rs/opz-plugin/plugins/not-this-plugin",
            ..FakeOpOptions::default()
        },
    );

    let output = run_plugin_with_bin(&bin, "example-item", "codex");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("OPZ_PLUGIN_SCHEMA_INVALID"), "{stderr}");
}

#[test]
#[cfg(unix)]
fn plugin_rejects_item_version_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    make_fake_op_with_options(
        &bin.join("op"),
        "example-item",
        FakeOpOptions {
            plugin_version: "9.9.9",
            ..FakeOpOptions::default()
        },
    );

    let output = run_plugin_with_bin(&bin, "example-item", "codex");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("OPZ_PLUGIN_SCHEMA_INVALID"), "{stderr}");
}

#[test]
#[cfg(unix)]
fn plugin_rejects_missing_required_env() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    make_fake_op_with_options(
        &bin.join("op"),
        "example-item",
        FakeOpOptions {
            include_required_secret: false,
            ..FakeOpOptions::default()
        },
    );

    let output = run_plugin_with_bin(&bin, "example-item", "codex");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("OPZ_PLUGIN_REQUIRED_ENV_MISSING"),
        "{stderr}"
    );
}

#[test]
#[cfg(unix)]
fn plugin_rejects_generated_file_path_escape() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let registry = temp.path().join("registry");
    let plugin_dir = registry.join("plugins").join("opencode-go-codex");
    fs::create_dir_all(&plugin_dir).unwrap();
    let manifest = bundled_manifest_text().replace(
        "[files.\"{env.CODEX_HOME}/config.toml\"]",
        "[files.\"{tmp}/../escape.toml\"]",
    );
    fs::write(plugin_dir.join("plugin.toml"), &manifest).unwrap();
    let sha = sha256_hex(manifest.as_bytes());
    write_registry(&registry, &sha);
    make_fake_op_with_options(
        &bin.join("op"),
        "example-item",
        FakeOpOptions {
            plugin_sha: Box::leak(sha.into_boxed_str()),
            ..FakeOpOptions::default()
        },
    );
    make_executable(&bin.join("codex"), "#!/bin/sh\nexit 0\n");

    let output = Command::new(env!("CARGO_BIN_EXE_opz"))
        .arg("plugin")
        .arg("run")
        .arg("opencode-go-codex")
        .arg("--item")
        .arg("example-item")
        .arg("--")
        .arg("codex")
        .env("PATH", test_path(&bin))
        .env("OPZ_SKIP_CREDENTIAL_SCAN", "1")
        .env("OPZ_PLUGIN_REGISTRY_DIR", &registry)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("OPZ_PLUGIN_RENDER_FAILED"), "{stderr}");
}

#[cfg(unix)]
fn make_fake_op(path: &Path, item_title: &str) {
    make_fake_op_with_options(path, item_title, FakeOpOptions::default());
}

#[derive(Clone, Copy)]
struct FakeOpOptions {
    plugin_source: &'static str,
    plugin_version: &'static str,
    plugin_sha: &'static str,
    include_required_secret: bool,
}

impl Default for FakeOpOptions {
    fn default() -> Self {
        Self {
            plugin_source: "github:opz-rs/opz-plugin/plugins/opencode-go-codex",
            plugin_version: "0.1.0",
            plugin_sha: PLUGIN_SHA,
            include_required_secret: true,
        }
    }
}

#[cfg(unix)]
fn make_fake_op_with_options(path: &Path, item_title: &str, options: FakeOpOptions) {
    let secret_field = if options.include_required_secret {
        r#",{"label":"OPENCODE_GO_API_KEY","value":"secret"}"#
    } else {
        ""
    };
    make_executable(
        path,
        &format!(
            r#"#!/bin/sh
case "$*" in
  "item get {item_title} --format json")
    cat <<'JSON'
{{"id":"item-id","title":"{item_title}","vault":{{"id":"vault-id","name":"Private"}},"fields":[{{"label":"OPZ_PLUGIN","value":"opencode-go-codex"}},{{"label":"OPZ_PLUGIN_SOURCE","value":"{plugin_source}"}},{{"label":"OPZ_PLUGIN_VERSION","value":"{plugin_version}"}},{{"label":"OPZ_PLUGIN_SHA256","value":"{plugin_sha}"}},{{"label":"OPZ_PLUGIN_CONFIG","value":"model = \"kimi-k2.7\"\n\n[opencode_go]\nbase_url = \"https://example.invalid/v1\"\n"}}{secret_field},{{"label":"UNRELATED_SECRET","value":"do-not-leak"}}]}}
JSON
    ;;
  "item list --format json")
    cat <<'JSON'
[{{"id":"item-id","title":"{item_title}","vault":{{"id":"vault-id","name":"Private"}}}}]
JSON
    ;;
  "run --no-masking --env-file "*)
    printf 'OPENCODE_GO_API_KEY=secret-api-key\0'
    ;;
  *)
    echo "unexpected op command: $*" >&2
    exit 2
    ;;
esac
"#,
            plugin_source = options.plugin_source,
            plugin_version = options.plugin_version,
            plugin_sha = options.plugin_sha,
            secret_field = secret_field
        ),
    );
}

#[cfg(unix)]
fn run_plugin_with_bin(bin: &Path, item: &str, command: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_opz"))
        .arg("plugin")
        .arg("run")
        .arg("opencode-go-codex")
        .arg("--item")
        .arg(item)
        .arg("--")
        .arg(command)
        .env("PATH", test_path(bin))
        .env("OPZ_SKIP_CREDENTIAL_SCAN", "1")
        .output()
        .unwrap()
}

fn test_path(bin: &Path) -> std::ffi::OsString {
    let mut paths = vec![bin.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(paths).unwrap()
}

fn run_checked(cmd: &mut Command) {
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn bundled_manifest_text() -> &'static str {
    r#"schema_version = 1
name = "opencode-go-codex"
version = "0.1.0"
description = "Run Codex with OpenCode Go models"
target_commands = ["codex"]

required_env = ["OPENCODE_GO_API_KEY"]
secret_env_allowlist = ["OPENCODE_GO_API_KEY"]

[defaults]
model = "kimi-k2.7"

[defaults.opencode_go]
base_url = "https://opencode.ai/zen/go/v1"

[env]
CODEX_HOME = "{tmp}/codex-home"

[files."{env.CODEX_HOME}/config.toml"]
mode = "0600"
content = """
model = "{config.model}"
model_provider = "opencode_go"

[model_providers.opencode_go]
name = "OpenCode Go"
base_url = "{config.opencode_go.base_url}"
env_key = "OPENCODE_GO_API_KEY"
requires_openai_auth = false
"""

[doctor]
required_commands = ["codex"]
required_env = ["OPENCODE_GO_API_KEY"]
"#
}

fn write_registry(root: &Path, sha: &str) {
    fs::write(
        root.join("registry.toml"),
        format!(
            r#"schema_version = 1

[[plugins]]
name = "opencode-go-codex"
version = "0.1.0"
source = "github:opz-rs/opz-plugin/plugins/opencode-go-codex"
path = "plugins/opencode-go-codex/plugin.toml"
sha256 = "{sha}"
description = "Run Codex with OpenCode Go models"
target_commands = ["codex"]
"#
        ),
    )
    .unwrap();
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
