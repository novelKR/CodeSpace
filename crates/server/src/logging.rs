use std::collections::HashMap;

use tracing_subscriber::EnvFilter;

const SECRET_KEYS: &[&str] = &[
    "authorization",
    "token",
    "bearer",
    "codespace_http_token",
    "password",
    "secret",
];

pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
}

pub fn sanitize_fields(fields: &HashMap<String, String>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (key, value) in fields {
        if is_secret_key(key) {
            out.insert(key.clone(), "[redacted]".to_string());
        } else {
            out.insert(key.clone(), value.clone());
        }
    }
    out
}

fn is_secret_key(key: &str) -> bool {
    SECRET_KEYS.contains(&key.to_ascii_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_bearer_and_token_keys() {
        let mut fields = HashMap::new();
        fields.insert("token".into(), "super-secret".into());
        fields.insert("Authorization".into(), "Bearer super-secret".into());
        fields.insert("workspace_id".into(), "demo".into());
        let sanitized = sanitize_fields(&fields);
        assert_eq!(
            sanitized.get("token").map(String::as_str),
            Some("[redacted]")
        );
        assert_eq!(
            sanitized.get("Authorization").map(String::as_str),
            Some("[redacted]")
        );
        assert_eq!(
            sanitized.get("workspace_id").map(String::as_str),
            Some("demo")
        );
        let blob = format!("{sanitized:?}");
        assert!(!blob.contains("super-secret"));
    }
}
