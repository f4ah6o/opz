mod protocol;

use protocol::{args_match, Invocation, Scenario, Step};
use std::{
    collections::BTreeMap,
    env,
    fs::{self, OpenOptions},
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
    process, thread,
    time::Duration,
};

fn main() {
    if let Err(err) = run() {
        eprintln!("opz-test-tool: {err}");
        process::exit(97);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let scenario_path = PathBuf::from(env::var("OPZ_TEST_SCENARIO")?);
    let log_dir = PathBuf::from(env::var("OPZ_TEST_LOG_DIR")?);
    fs::create_dir_all(&log_dir)?;

    let scenario: Scenario = serde_json::from_slice(&fs::read(&scenario_path)?)?;
    let tool = current_tool_name()?;
    let args: Vec<String> = env::args().skip(1).collect();
    let (index, step) = claim_step(&scenario, &log_dir, &tool, &args)?;

    if !step.mcp_results.is_empty() {
        return run_mcp(&log_dir, index, &tool, &args, &step);
    }

    if step.delay_ms > 0 {
        thread::sleep(Duration::from_millis(step.delay_ms));
    }

    let mut stdin = String::new();
    if step.read_stdin {
        io::stdin().read_to_string(&mut stdin)?;
    }
    write_invocation(&log_dir, index, &tool, args, stdin, &step)?;

    io::stdout().write_all(step.stdout.as_bytes())?;
    io::stderr().write_all(step.stderr.as_bytes())?;
    process::exit(step.exit_code);
}

fn current_tool_name() -> Result<String, Box<dyn std::error::Error>> {
    let exe = env::current_exe()?;
    let stem = exe
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or("test tool executable name was not UTF-8")?;
    Ok(stem.to_string())
}

fn claim_step(
    scenario: &Scenario,
    log_dir: &Path,
    tool: &str,
    args: &[String],
) -> Result<(usize, Step), Box<dyn std::error::Error>> {
    for (index, step) in scenario.steps.iter().enumerate() {
        if step.tool != tool || !args_match(&step.args, args) {
            continue;
        }
        let claim = log_dir.join(format!("{index:03}.claim"));
        match OpenOptions::new().write(true).create_new(true).open(claim) {
            Ok(_) => return Ok((index, step.clone())),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.into()),
        }
    }

    Err(format!("no unused scenario step matched {tool} {args:?}").into())
}

fn write_invocation(
    log_dir: &Path,
    index: usize,
    tool: &str,
    args: Vec<String>,
    stdin: String,
    step: &Step,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = step
        .capture_env
        .iter()
        .filter_map(|name| env::var(name).ok().map(|value| (name.clone(), value)))
        .collect::<BTreeMap<_, _>>();
    let invocation = Invocation {
        tool: tool.to_string(),
        args,
        stdin,
        env,
    };
    fs::write(
        log_dir.join(format!("{index:03}.json")),
        serde_json::to_vec_pretty(&invocation)?,
    )?;
    Ok(())
}

fn run_mcp(
    log_dir: &Path,
    index: usize,
    tool: &str,
    args: &[String],
    step: &Step,
) -> Result<(), Box<dyn std::error::Error>> {
    write_invocation(log_dir, index, tool, args.to_vec(), String::new(), step)?;
    let mut result_index = 0usize;
    let mut request_log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join(format!("{index:03}.mcp.jsonl")))?;

    for line in io::stdin().lock().lines() {
        let line = line?;
        writeln!(request_log, "{line}")?;
        request_log.flush()?;
        let request: serde_json::Value = serde_json::from_str(&line)?;
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let result = if method == "initialize" {
            serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "serverInfo": {"name": "opz-test-tool", "version": "1"}
            })
        } else {
            let result = step
                .mcp_results
                .get(result_index)
                .cloned()
                .ok_or("fake MCP server ran out of configured results")?;
            result_index += 1;
            result
        };
        serde_json::to_writer(
            &mut io::stdout(),
            &serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}),
        )?;
        io::stdout().write_all(b"\n")?;
        io::stdout().flush()?;
    }
    Ok(())
}
