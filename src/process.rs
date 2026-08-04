use crate::*;

pub(crate) fn run_with_item_sections(
    sections: &ItemSections,
    env_file: Option<&Path>,
    command: &[String],
) -> Result<()> {
    let merged_env_lines =
        instrumentation::with_span("main_operation", vec![], || merge_env_lines(sections));

    instrumentation::with_span_result(
        "write_outputs",
        vec![
            KeyValue::new(
                "cli.output_path",
                env_file
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "-".to_string()),
            ),
            KeyValue::new("cli.command_arg_count", command.len() as i64),
        ],
        || {
            if let Some(path) = env_file {
                write_env_file(path, &merged_env_lines)?;
                eprintln!("Generated: {}", path.display());
            }
            Ok(())
        },
    )?;

    // First pass: collect all environment variable values
    let env_vars = instrumentation::with_span_result("load_inputs", vec![], || {
        resolve_env_vars(&merged_env_lines)
    })?;

    instrumentation::with_span_result("write_outputs.command_exec", vec![], || {
        let mut cmd = build_child_command(command)?;

        // Set environment variables for the child process
        for (key, value) in &env_vars {
            cmd.env(key, value.expose());
        }

        let status = cmd
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("failed to run command")?;

        if !status.success() {
            return Err(anyhow!("command failed with status: {}", status));
        }
        Ok(())
    })
}

pub(crate) fn build_child_command(command: &[String]) -> Result<Command> {
    let program = command
        .first()
        .ok_or_else(|| anyhow!("command must contain a program"))?;

    #[cfg(unix)]
    {
        let mut child = Command::new("sh");
        child.args(["-c", "exec \"$@\"", "sh"]);
        child.arg(program);
        child.args(&command[1..]);
        Ok(child)
    }

    #[cfg(windows)]
    {
        let mut child = Command::new(program);
        child.args(&command[1..]);
        Ok(child)
    }
}

pub(crate) fn op_json(args: &[&str]) -> Result<serde_json::Value> {
    let operation = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
    instrumentation::with_span_result(
        "load_inputs.op_json",
        vec![KeyValue::new("op.operation", operation)],
        || {
            let mut cmd = Command::new("op");
            cmd.args(args);
            cmd.stdin(Stdio::null());
            let display = format!("`op {}`", args.join(" "));
            let out = command_output_with_timeout(cmd, &display, op_command_timeout())?;

            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                if args.starts_with(&["item", "get"])
                    && (stderr.contains("isn't an item")
                        || stderr.contains("is not an item")
                        || stderr.contains("not found"))
                {
                    return Err(anyhow!("op item not found"));
                }
                return Err(anyhow!("op error ({})", out.status));
            }

            let v: serde_json::Value =
                serde_json::from_slice(&out.stdout).context("failed to parse op JSON output")?;
            Ok(v)
        },
    )
}

pub(crate) fn op_command_timeout() -> Duration {
    env::var("OPZ_OP_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(30))
}

pub(crate) fn command_output_with_timeout(
    mut command: Command,
    display: &str,
    timeout: Duration,
) -> Result<Output> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run {display}"))?;
    let started = Instant::now();

    loop {
        if child
            .try_wait()
            .with_context(|| format!("failed to poll {display}"))?
            .is_some()
        {
            return child
                .wait_with_output()
                .with_context(|| format!("failed to wait for {display}"));
        }

        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait_with_output();
            return Err(anyhow!(
                "{display} timed out after {} seconds. Check 1Password CLI authentication with `op whoami`, or set OPZ_OP_TIMEOUT_SECONDS to a larger value.",
                timeout.as_secs()
            ));
        }

        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_command_preserves_argument_boundaries() {
        let command = vec![
            "tool".to_string(),
            "two words".to_string(),
            "$UNCHANGED".to_string(),
        ];
        let child = build_child_command(&command).unwrap();
        let args: Vec<String> = child
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        #[cfg(unix)]
        assert_eq!(
            args,
            ["-c", "exec \"$@\"", "sh", "tool", "two words", "$UNCHANGED"]
        );
        #[cfg(windows)]
        assert_eq!(args, ["two words", "$UNCHANGED"]);
    }
}
