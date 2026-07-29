use crate::*;

pub(crate) fn run_with_environments(
    vault: Option<&str>,
    environments: &[String],
    items: &[String],
    env_file: Option<&Path>,
    command: &[String],
) -> Result<()> {
    if let Some(vault) = vault {
        return Err(anyhow!(
            "`--vault {vault}` cannot be combined with `--environment` because 1Password Environments are not vault item lookups. Remove `--vault` or use item-backed `opz run <ITEM> -- <COMMAND>`."
        ));
    }
    if !items.is_empty() {
        return Err(anyhow!(
            "`--environment` cannot be combined with item arguments in v1. Use either `opz run --environment <ENV> -- <COMMAND>` or item-backed `opz run <ITEM> -- <COMMAND>`."
        ));
    }
    if let Some(path) = env_file {
        return Err(anyhow!(
            "`--environment` cannot be combined with `--env-file` in v1 because Environment execution is delegated to `op run`. Remove `--env-file` or use item-backed `opz gen --env-file {}`.",
            path.display()
        ));
    }

    let flag = detect_op_run_environment_flag()?;
    let args = build_op_run_environment_args(flag, environments, command);

    instrumentation::with_span_result("write_outputs.command_exec", vec![], || {
        let status = Command::new("op")
            .args(&args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("failed to run `op run` with 1Password Environment")?;

        if !status.success() {
            return Err(anyhow!("op run failed with status: {}", status));
        }
        Ok(())
    })
}

pub(crate) fn detect_op_run_environment_flag() -> Result<&'static str> {
    let mut cmd = Command::new("op");
    cmd.arg("run").arg("--help");
    cmd.stdin(Stdio::null());
    let out = command_output_with_timeout(cmd, "`op run --help`", op_command_timeout())?;
    if !out.status.success() {
        return Err(anyhow!(
            "failed to inspect `op run --help`: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    let help = String::from_utf8_lossy(&out.stdout);
    match detect_environment_flag_from_help(&help) {
        Some(flag) => Ok(flag),
        None => Err(anyhow!(
            "This 1Password CLI does not expose Environment runtime injection in `op run --help`. Use item-backed `opz run <ITEM> -- <COMMAND>`, or use the 1Password MCP server to mount a local .env file for this project."
        )),
    }
}

pub(crate) fn detect_environment_flag_from_help(help: &str) -> Option<&'static str> {
    if help.contains("--environments") {
        Some("--environments")
    } else if help.contains("--environment") {
        Some("--environment")
    } else {
        None
    }
}

pub(crate) fn build_op_run_environment_args(
    flag: &str,
    environments: &[String],
    command: &[String],
) -> Vec<String> {
    let mut args = Vec::with_capacity(environments.len() * 2 + command.len() + 2);
    args.push("run".to_string());
    for environment in environments {
        args.push(flag.to_string());
        args.push(environment.clone());
    }
    args.push("--".to_string());
    args.extend(command.iter().cloned());
    args
}
