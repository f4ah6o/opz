use crate::*;

pub(crate) fn parse_env_file(path: &Path) -> Result<Vec<(String, String)>> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let label_re = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$")?;
    let mut pairs = Vec::new();

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
        let key = raw_key.trim();
        if !label_re.is_match(key) {
            eprintln!("Skipped invalid key in env file: {key}");
            continue;
        }

        let value = normalize_env_value(raw_value);
        if is_op_reference(&value) {
            eprintln!("Skipped already imported op:// value for key: {key}");
            continue;
        }

        // Last occurrence wins for duplicate keys.
        if let Some(pos) = pairs
            .iter()
            .position(|(existing_key, _)| existing_key == key)
        {
            pairs.remove(pos);
        }

        pairs.push((key.to_string(), value));
    }

    Ok(pairs)
}

pub(crate) fn normalize_env_value(raw_value: &str) -> String {
    let mut value = strip_inline_comment(raw_value).trim().to_string();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value = value[1..value.len() - 1].to_string();
    }
    value
}

pub(crate) fn strip_inline_comment(value: &str) -> &str {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped_in_double = false;

    for (idx, ch) in value.char_indices() {
        if in_double_quote {
            if escaped_in_double {
                escaped_in_double = false;
                continue;
            }
            if ch == '\\' {
                escaped_in_double = true;
                continue;
            }
            if ch == '"' {
                in_double_quote = false;
            }
            continue;
        }

        if in_single_quote {
            if ch == '\'' {
                in_single_quote = false;
            }
            continue;
        }

        match ch {
            '"' => in_double_quote = true,
            '\'' => in_single_quote = true,
            '#' if idx == 0 || value[..idx].chars().last().is_some_and(char::is_whitespace) => {
                return value[..idx].trim_end();
            }
            _ => {}
        }
    }

    value
}

pub(crate) fn is_op_reference(value: &str) -> bool {
    value.starts_with("op://")
}

pub(crate) fn write_env_file(path: &Path, new_lines: &[String]) -> Result<()> {
    instrumentation::with_span_result(
        "write_outputs.write_env_file",
        vec![
            KeyValue::new("cli.output_path", path.display().to_string()),
            KeyValue::new("env.line_count", new_lines.len() as i64),
        ],
        || {
            use std::collections::HashMap;

            // Build a map of new keys for quick lookup
            let new_keys: HashMap<String, &str> = new_lines
                .iter()
                .filter_map(|line| parse_env_key(line).map(|key| (key.to_string(), line.as_str())))
                .collect();

            let mut result_lines: Vec<String> = Vec::new();
            let mut written_keys: std::collections::HashSet<String> =
                std::collections::HashSet::new();

            // Read existing file and merge
            if path.exists() {
                let content =
                    fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;

                for line in content.lines() {
                    if let Some(key) = parse_env_key(line) {
                        if let Some(&new_line) = new_keys.get(key) {
                            // Overwrite with new value
                            result_lines.push(new_line.to_string());
                            written_keys.insert(key.to_string());
                        } else {
                            // Keep existing line
                            result_lines.push(line.to_string());
                        }
                    } else {
                        // Comment or empty line - keep as is
                        result_lines.push(line.to_string());
                    }
                }
            }

            // Append new keys that weren't already in the file
            for line in new_lines {
                if let Some(key) = parse_env_key(line) {
                    if !written_keys.contains(key) {
                        result_lines.push(line.clone());
                    }
                }
            }

            // Write result
            let mut f =
                fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
            for line in &result_lines {
                writeln!(f, "{line}")?;
            }
            Ok(())
        },
    )
}
