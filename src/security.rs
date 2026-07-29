use crate::*;

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
    let mut masked = stdout.to_string();
    for (field_name, value) in sensitive_fields {
        let pattern = format!(
            "{}(^\\s*{}(?:\\s*\\[[^\\]]+\\])?\\s*[:=]\\s*){}(\\s*$)",
            if value.contains('\n') {
                "(?ms)"
            } else {
                "(?m)"
            },
            regex::escape(field_name),
            regex::escape(value)
        );
        let Ok(regex) = Regex::new(&pattern) else {
            continue;
        };
        masked = regex.replace_all(&masked, "$1***$2").into_owned();
    }
    masked
}
