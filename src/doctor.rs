use crate::*;

#[derive(Debug)]
pub(crate) struct DoctorFailure;

impl std::fmt::Display for DoctorFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("doctor found required failures")
    }
}

impl std::error::Error for DoctorFailure {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DoctorStatus {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DoctorCheck {
    status: DoctorStatus,
    name: String,
    message: String,
    required: bool,
}

impl DoctorCheck {
    pub(crate) fn ok(name: impl Into<String>, message: impl Into<String>, required: bool) -> Self {
        Self {
            status: DoctorStatus::Ok,
            name: name.into(),
            message: message.into(),
            required,
        }
    }

    pub(crate) fn warn(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: DoctorStatus::Warn,
            name: name.into(),
            message: message.into(),
            required: false,
        }
    }

    pub(crate) fn error(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: DoctorStatus::Error,
            name: name.into(),
            message: message.into(),
            required: true,
        }
    }
}

pub(crate) fn run_doctor() -> Result<()> {
    let checks = instrumentation::with_span("main_operation", vec![], collect_doctor_checks);
    let rendered = render_doctor_checks(&checks);
    instrumentation::with_span("write_outputs", vec![], || {
        print!("{rendered}");
    });

    if doctor_has_required_failure(&checks) {
        return Err(anyhow!(DoctorFailure));
    }
    Ok(())
}

pub(crate) fn collect_doctor_checks() -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    match check_required_cli_version("op", &["--version"]) {
        Ok(check) => checks.push(check),
        Err(check) => {
            checks.push(check);
            checks.push(DoctorCheck::error(
                "op auth",
                "skipped because op is not available",
            ));
            checks.push(optional_cli_check(
                "gh",
                &["--version"],
                "needed by github-secret",
            ));
            checks.push(optional_cli_check(
                "wrangler",
                &["--version"],
                "needed by cloudflare-secret",
            ));
            checks.push(optional_cli_check(
                "git",
                &["--version"],
                "needed by migrate and note",
            ));
            checks.push(optional_cli_check("sh", &["--version"], "needed by run"));
            checks.push(optional_cli_check(
                "secretlint",
                &["--version"],
                "needed by doctor plaintext credential scan",
            ));
            checks.push(check_onepassword_mcp_server());
            checks.push(check_credential_files());
            return checks;
        }
    }

    checks.push(check_op_auth());
    checks.push(check_op_accounts());
    checks.push(check_op_run_environments_support());
    checks.push(optional_cli_check(
        "gh",
        &["--version"],
        "needed by github-secret",
    ));
    checks.push(optional_cli_check(
        "wrangler",
        &["--version"],
        "needed by cloudflare-secret",
    ));
    checks.push(optional_cli_check(
        "git",
        &["--version"],
        "needed by migrate and note",
    ));
    checks.push(optional_cli_check("sh", &["--version"], "needed by run"));
    checks.push(optional_cli_check(
        "secretlint",
        &["--version"],
        "needed by doctor plaintext credential scan",
    ));
    checks.push(check_onepassword_mcp_server());
    checks.push(check_credential_files());

    checks
}

pub(crate) fn check_op_run_environments_support() -> DoctorCheck {
    let mut cmd = Command::new("op");
    cmd.arg("run").arg("--help");
    cmd.stdin(Stdio::null());
    match command_output_with_timeout(cmd, "`op run --help`", op_command_timeout()) {
        Ok(out) if out.status.success() => {
            let help = String::from_utf8_lossy(&out.stdout);
            match detect_environment_flag_from_help(&help) {
                Some(flag) => DoctorCheck::ok(
                    "op run environments",
                    format!("supported via {flag}"),
                    false,
                ),
                None => DoctorCheck::warn(
                    "op run environments",
                    "not exposed by this op CLI; item-backed opz run remains available",
                ),
            }
        }
        Ok(out) => DoctorCheck::warn(
            "op run environments",
            format!(
                "could not inspect `op run --help`: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        ),
        Err(err) => DoctorCheck::warn("op run environments", err.to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CredentialFileFinding {
    pub(crate) path: PathBuf,
    plaintext_entries: usize,
}

pub(crate) fn print_credential_file_advice_for_secret_command(command: &str) {
    if env::var_os("OPZ_SKIP_CREDENTIAL_SCAN").is_some() {
        return;
    }
    let Ok(findings) = find_plaintext_credential_files_in_project() else {
        return;
    };
    if findings.is_empty() {
        return;
    }

    eprintln!(
        "Advice: found plaintext credential env file(s) while running `{command}`: {}. Prefer `opz run -- <COMMAND>` without an env file after `opz migrate --new`; use `opz gen --env-file <FILE> <ITEM>` only when a tool requires op:// references.",
        credential_finding_path_list(&findings)
    );
}

pub(crate) fn check_credential_files() -> DoctorCheck {
    let findings = match find_plaintext_credential_files_in_project() {
        Ok(findings) => findings,
        Err(err) => return DoctorCheck::warn("credential files", err.to_string()),
    };

    if findings.is_empty() {
        return DoctorCheck::ok(
            "credential files",
            "no plaintext env credential files found",
            false,
        );
    }

    let advice = credential_file_advice(&findings);
    if find_command_path("secretlint").is_none() {
        return DoctorCheck::warn(
            "credential files",
            format!("{advice}; secretlint not found in PATH"),
        );
    }

    match run_secretlint_on_files(&findings) {
        Ok(SecretlintOutcome::Clean) => DoctorCheck::warn(
            "credential files",
            format!("{advice}; secretlint found no configured rule violations"),
        ),
        Ok(SecretlintOutcome::Findings) => DoctorCheck::warn(
            "credential files",
            format!("{advice}; secretlint reported possible plaintext secrets"),
        ),
        Err(err) => DoctorCheck::warn("credential files", format!("{advice}; {err}")),
    }
}

pub(crate) fn credential_file_advice(findings: &[CredentialFileFinding]) -> String {
    format!(
        "found plaintext credential env file(s): {}; prefer `opz run -- <COMMAND>` without an env file after `opz migrate --new`, and generate files only with `opz gen --env-file <FILE> <ITEM>` when required",
        credential_finding_path_list(findings)
    )
}

pub(crate) fn credential_finding_path_list(findings: &[CredentialFileFinding]) -> String {
    findings
        .iter()
        .take(5)
        .map(|finding| {
            format!(
                "{} ({} entr{})",
                finding.path.display(),
                finding.plaintext_entries,
                if finding.plaintext_entries == 1 {
                    "y"
                } else {
                    "ies"
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SecretlintOutcome {
    Clean,
    Findings,
}

pub(crate) fn run_secretlint_on_files(
    findings: &[CredentialFileFinding],
) -> std::result::Result<SecretlintOutcome, String> {
    let mut cmd = Command::new("secretlint");
    cmd.arg("--format").arg("json").arg("--no-color");
    for finding in findings {
        cmd.arg(&finding.path);
    }

    let out = cmd
        .output()
        .map_err(|err| format!("failed to run `secretlint`: {err}"))?;

    match out.status.code() {
        Some(0) => Ok(SecretlintOutcome::Clean),
        Some(1) => Ok(SecretlintOutcome::Findings),
        _ => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if stderr.is_empty() {
                format!("secretlint failed with status {}", out.status)
            } else {
                format!("secretlint failed: {stderr}")
            })
        }
    }
}

pub(crate) fn find_plaintext_credential_files_in_project() -> Result<Vec<CredentialFileFinding>> {
    let root = project_scan_root()?;
    let mut findings = Vec::new();
    collect_plaintext_credential_files(&root, &root, &mut findings)?;
    findings.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(findings)
}

pub(crate) fn project_scan_root() -> Result<PathBuf> {
    if let Ok(out) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
    {
        if out.status.success() {
            let root = String::from_utf8(out.stdout)
                .context("git rev-parse output was not valid UTF-8")?
                .trim()
                .to_string();
            if !root.is_empty() {
                return Ok(PathBuf::from(root));
            }
        }
    }
    env::current_dir().context("failed to read current directory")
}

pub(crate) fn collect_plaintext_credential_files(
    root: &Path,
    dir: &Path,
    findings: &mut Vec<CredentialFileFinding>,
) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if file_type.is_dir() {
            if should_skip_scan_dir(&name) {
                continue;
            }
            collect_plaintext_credential_files(root, &path, findings)?;
            continue;
        }

        if !file_type.is_file() || !is_credential_env_file_name(&name) {
            continue;
        }

        let plaintext_entries = count_plaintext_env_entries(&path)?;
        if plaintext_entries == 0 {
            continue;
        }

        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        findings.push(CredentialFileFinding {
            path: relative,
            plaintext_entries,
        });
    }

    Ok(())
}

pub(crate) fn should_skip_scan_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | ".cache" | "dist" | "build"
    )
}

pub(crate) fn is_credential_env_file_name(name: &str) -> bool {
    if is_example_credential_env_file_name(name) {
        return false;
    }
    name == ".env" || name.starts_with(".env.") || name.ends_with(".env") || name.contains(".env.")
}

pub(crate) fn is_example_credential_env_file_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".example")
        || lower.ends_with(".sample")
        || lower.ends_with(".template")
        || lower.ends_with(".bak")
        || lower.ends_with(".old")
}

pub(crate) fn count_plaintext_env_entries(path: &Path) -> Result<usize> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let label_re = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$")?;
    let mut count = 0;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let normalized = match line.strip_prefix("export") {
            Some(rest) if rest.chars().next().is_some_and(char::is_whitespace) => rest.trim_start(),
            _ => line,
        };
        let Some((raw_key, raw_value)) = normalized.split_once('=') else {
            continue;
        };
        if !label_re.is_match(raw_key.trim()) {
            continue;
        }
        let value = normalize_env_value(raw_value);
        if !value.is_empty() && !is_op_reference(&value) {
            count += 1;
        }
    }

    Ok(count)
}

pub(crate) fn check_required_cli_version(
    command: &str,
    args: &[&str],
) -> std::result::Result<DoctorCheck, DoctorCheck> {
    let Some(path) = find_command_path(command) else {
        return Err(DoctorCheck::error(command, "not found in PATH"));
    };

    match command_stdout(command, args) {
        Ok(stdout) => Ok(DoctorCheck::ok(
            command,
            format!("{} ({})", path.display(), first_output_line(&stdout)),
            true,
        )),
        Err(err) => Err(DoctorCheck::error(command, err)),
    }
}

pub(crate) fn check_op_auth() -> DoctorCheck {
    match command_stdout("op", &["whoami", "--format", "json"]) {
        Ok(stdout) => {
            let summary = summarize_op_whoami(&stdout).unwrap_or_else(|| "signed in".to_string());
            DoctorCheck::ok("op auth", summary, true)
        }
        Err(err) => DoctorCheck::error("op auth", err),
    }
}

pub(crate) fn check_op_accounts() -> DoctorCheck {
    match command_stdout("op", &["account", "list", "--format", "json"]) {
        Ok(stdout) => {
            let count = serde_json::from_str::<serde_json::Value>(&stdout)
                .ok()
                .and_then(|value| value.as_array().map(Vec::len));
            let message = match count {
                Some(1) => "1 configured account".to_string(),
                Some(n) => format!("{n} configured accounts"),
                None => "account list available".to_string(),
            };
            DoctorCheck::ok("op accounts", message, false)
        }
        Err(err) => DoctorCheck::warn("op accounts", err),
    }
}

pub(crate) fn optional_cli_check(command: &str, args: &[&str], note: &str) -> DoctorCheck {
    let Some(path) = find_command_path(command) else {
        return DoctorCheck::warn(command, format!("not found in PATH ({note})"));
    };

    match command_stdout(command, args) {
        Ok(stdout) => DoctorCheck::ok(
            command,
            format!("{} ({})", path.display(), first_output_line(&stdout)),
            false,
        ),
        Err(err) => DoctorCheck::warn(command, format!("{err} ({note})")),
    }
}

pub(crate) fn command_stdout(command: &str, args: &[&str]) -> std::result::Result<String, String> {
    let out = Command::new(command).args(args).output().map_err(|err| {
        format!(
            "failed to run `{}`: {err}",
            command_with_args(command, args)
        )
    })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("exit status {}", out.status)
        } else {
            stderr
        };
        return Err(format!(
            "`{}` failed: {detail}",
            command_with_args(command, args)
        ));
    }

    String::from_utf8(out.stdout).map_err(|err| {
        format!(
            "`{}` output was not valid UTF-8: {err}",
            command_with_args(command, args)
        )
    })
}

pub(crate) fn command_with_args(command: &str, args: &[&str]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(command.to_string());
    parts.extend(args.iter().map(|arg| arg.to_string()));
    parts.join(" ")
}

pub(crate) fn first_output_line(output: &str) -> String {
    output
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
        .unwrap_or_else(|| "no version output".to_string())
}

pub(crate) fn summarize_op_whoami(stdout: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let email = value
        .get("email")
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.get("user_email").and_then(serde_json::Value::as_str));
    let account = value
        .get("account_uuid")
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.get("account").and_then(serde_json::Value::as_str))
        .or_else(|| value.get("url").and_then(serde_json::Value::as_str));

    match (email, account) {
        (Some(email), Some(account)) => Some(format!("{email} ({account})")),
        (Some(email), None) => Some(email.to_string()),
        (None, Some(account)) => Some(account.to_string()),
        (None, None) => Some("signed in".to_string()),
    }
}

pub(crate) fn find_command_path(command: &str) -> Option<PathBuf> {
    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        return is_executable_file(command_path).then(|| command_path.to_path_buf());
    }

    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(command);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

pub(crate) fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

pub(crate) fn render_doctor_checks(checks: &[DoctorCheck]) -> String {
    let mut out = String::new();
    for check in checks {
        let status = match check.status {
            DoctorStatus::Ok => "ok",
            DoctorStatus::Warn => "warn",
            DoctorStatus::Error => "error",
        };
        out.push_str(&format!("{status:<5} {}: {}\n", check.name, check.message));
    }
    out
}

pub(crate) fn doctor_has_required_failure(checks: &[DoctorCheck]) -> bool {
    checks
        .iter()
        .any(|check| check.required && check.status == DoctorStatus::Error)
}
