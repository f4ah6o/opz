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

            let existing_permissions = match fs::symlink_metadata(path) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        return Err(anyhow!(
                            "refusing to replace symlink env file: {}",
                            path.display()
                        ));
                    }
                    if !metadata.is_file() {
                        return Err(anyhow!(
                            "refusing to replace non-regular env file: {}",
                            path.display()
                        ));
                    }
                    Some(metadata.permissions())
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(error).with_context(|| format!("inspect {}", path.display()));
                }
            };

            // Read existing file and merge.
            if existing_permissions.is_some() {
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

            // Write a same-directory replacement so a partial write never
            // truncates the persistent target.
            let mut replacement = ReplacementFile::create(path)?;
            for line in &result_lines {
                writeln!(replacement.file_mut(), "{line}")?;
            }
            replacement.commit(path, existing_permissions)?;
            Ok(())
        },
    )
}

struct ReplacementFile {
    path: PathBuf,
    file: Option<fs::File>,
}

impl ReplacementFile {
    fn create(target: &Path) -> Result<Self> {
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        let file_name = target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("env");
        for attempt in 0..100 {
            let path = parent.join(format!(
                ".{file_name}.opz-{}-{}-{attempt}.tmp",
                std::process::id(),
                stable_hex_hash(&format!("{:?}", SystemTime::now()))
            ));
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("create replacement for {}", target.display()));
                }
            }
        }
        Err(anyhow!(
            "failed to create unique replacement for {}",
            target.display()
        ))
    }

    fn file_mut(&mut self) -> &mut fs::File {
        self.file
            .as_mut()
            .expect("replacement file is available before commit")
    }

    fn commit(
        mut self,
        target: &Path,
        existing_permissions: Option<fs::Permissions>,
    ) -> Result<()> {
        let file = self
            .file
            .take()
            .expect("replacement file is available before commit");
        file.sync_all()
            .with_context(|| format!("flush replacement for {}", target.display()))?;
        drop(file);

        if let Some(permissions) = existing_permissions {
            fs::set_permissions(&self.path, permissions)
                .with_context(|| format!("preserve permissions for {}", target.display()))?;
        }

        replace_target(&self.path, target)?;
        Ok(())
    }
}

impl Drop for ReplacementFile {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(not(windows))]
fn replace_target(replacement: &Path, target: &Path) -> Result<()> {
    fs::rename(replacement, target).with_context(|| format!("replace {}", target.display()))
}

#[cfg(windows)]
fn replace_target(replacement: &Path, target: &Path) -> Result<()> {
    if !target.exists() {
        return fs::rename(replacement, target)
            .with_context(|| format!("replace {}", target.display()));
    }

    let backup = replacement.with_extension("backup");
    fs::rename(target, &backup)
        .with_context(|| format!("prepare replacement for {}", target.display()))?;
    match fs::rename(replacement, target) {
        Ok(()) => {
            fs::remove_file(&backup)
                .with_context(|| format!("remove replacement backup for {}", target.display()))?;
            Ok(())
        }
        Err(error) => {
            let _ = fs::rename(&backup, target);
            Err(error).with_context(|| format!("replace {}", target.display()))
        }
    }
}
