use std::fs;
use std::path::Path;
use std::process::Command;

#[cfg(unix)]
fn make_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, body).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[test]
#[cfg(unix)]
fn explicit_item_title_uses_direct_lookup_without_item_list() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("op.log");
    make_executable(
        &temp.path().join("op"),
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$OPZ_TEST_LOG"
case "$*" in
  "item get example-item --format json")
    echo '{"id":"item-id","title":"example-item","vault":{"id":"vault-id","name":"Private"},"fields":[{"label":"API_KEY","value":"secret"}]}'
    ;;
  "item list --format json"*)
    echo "item list should not be called for exact item title" >&2
    exit 9
    ;;
  *)
    echo "unexpected op command: $*" >&2
    exit 2
    ;;
esac
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_opz"))
        .arg("gen")
        .arg("example-item")
        .env("PATH", temp.path())
        .env("OPZ_TEST_LOG", &log)
        .current_dir(temp.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("API_KEY=op://vault-id/item-id/API_KEY"));

    let op_calls = fs::read_to_string(log).unwrap();
    assert!(op_calls.contains("item get example-item --format json"));
    assert!(!op_calls.contains("item list"), "{op_calls}");
}
