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
fn doctor_succeeds_with_required_op_and_missing_optional_tools() {
    let temp = tempfile::tempdir().unwrap();
    make_executable(
        &temp.path().join("op"),
        r#"#!/bin/sh
case "$*" in
  "--version") echo "2.30.0" ;;
  "whoami --format json") echo '{"email":"user@example.test","account_uuid":"A1"}' ;;
  "account list --format json") echo '[{"url":"example.1password.com"}]' ;;
  *) exit 2 ;;
esac
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_opz"))
        .arg("doctor")
        .env("PATH", temp.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("ok    op: "));
    assert!(stdout.contains("ok    op auth: user@example.test (A1)"));
    assert!(stdout.contains("ok    op accounts: 1 configured account"));
    assert!(stdout.contains("warn  gh: not found in PATH (needed by github-secret)"));
    assert!(stdout.contains("warn  wrangler: not found in PATH (needed by cloudflare-secret)"));
}

#[test]
#[cfg(unix)]
fn doctor_fails_when_op_auth_fails() {
    let temp = tempfile::tempdir().unwrap();
    make_executable(
        &temp.path().join("op"),
        r#"#!/bin/sh
case "$*" in
  "--version") echo "2.30.0" ;;
  "whoami --format json") echo "not signed in" >&2; exit 1 ;;
  "account list --format json") echo '[]' ;;
  *) exit 2 ;;
esac
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_opz"))
        .arg("doctor")
        .env("PATH", temp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("ok    op: "));
    assert!(stdout.contains("error op auth: `op whoami --format json` failed: not signed in"));
}
