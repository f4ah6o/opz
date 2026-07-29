use crate::*;

const REDACTED: &str = "[REDACTED]";

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct SecretValue(String);

impl SecretValue {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(REDACTED)
    }
}

impl std::fmt::Display for SecretValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(REDACTED)
    }
}

#[derive(Clone, Default)]
pub(crate) struct Redactor {
    secrets: Vec<String>,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Redactor")
            .field("secret_count", &self.secrets.len())
            .finish()
    }
}

impl Redactor {
    pub(crate) fn new(values: impl IntoIterator<Item = SecretValue>) -> Self {
        Self::from_strings(values.into_iter().map(|value| value.0))
    }

    pub(crate) fn from_strings(values: impl IntoIterator<Item = String>) -> Self {
        let mut secrets: Vec<String> = values
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect();
        let mut seen = HashSet::new();
        secrets.retain(|value| seen.insert(value.clone()));
        secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
        Self { secrets }
    }

    pub(crate) fn redact(&self, text: &str) -> String {
        self.secrets
            .iter()
            .fold(text.to_string(), |redacted, secret| {
                redacted.replace(secret, REDACTED)
            })
    }

    pub(crate) fn write_stdout(&self, bytes: &[u8]) -> Result<()> {
        std::io::stdout()
            .write_all(self.redact(&String::from_utf8_lossy(bytes)).as_bytes())
            .context("failed to write redacted command stdout")
    }

    pub(crate) fn write_stderr(&self, bytes: &[u8]) -> Result<()> {
        std::io::stderr()
            .write_all(self.redact(&String::from_utf8_lossy(bytes)).as_bytes())
            .context("failed to write redacted command stderr")
    }
}

pub(crate) fn collect_create_stdout_sensitive_fields(
    template: &ItemCreateTemplate,
) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    for field in &template.fields {
        if field.value.is_empty() {
            continue;
        }
        fields.push((field.label.clone(), field.value.clone()));
        if field.id != field.label {
            fields.push((field.id.clone(), field.value.clone()));
        }
    }

    fields.sort_by_key(|(_, value)| std::cmp::Reverse(value.len()));
    fields.dedup();
    fields
}

pub(crate) fn mask_create_stdout(stdout: &str, sensitive_fields: &[(String, String)]) -> String {
    let redactor = Redactor::from_strings(sensitive_fields.iter().map(|(_, value)| value.clone()));
    redactor.redact(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_value_never_formats_plaintext() {
        let secret = SecretValue::new("OPZ_CANARY_SECRET_VALUE");
        assert_eq!(format!("{secret}"), REDACTED);
        assert_eq!(format!("{secret:?}"), REDACTED);
    }

    #[test]
    fn redactor_replaces_longest_values_first() {
        let redactor =
            Redactor::from_strings(["token".to_string(), "token-with-suffix".to_string()]);
        assert_eq!(
            redactor.redact("token-with-suffix token"),
            "[REDACTED] [REDACTED]"
        );
        assert!(!format!("{redactor:?}").contains("token"));
    }
}
