use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

pub fn read_env_file(path: &Path, keys: &[&str]) -> HashMap<String, String> {
    let Ok(content) = fs::read_to_string(path) else {
        return HashMap::new();
    };

    let wanted = keys.iter().copied().collect::<HashSet<_>>();
    let mut result = HashMap::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some(eq_idx) = trimmed.find('=') else {
            continue;
        };
        let key = trimmed[..eq_idx].trim();
        if !wanted.contains(key) {
            continue;
        }
        let mut value = trimmed[eq_idx + 1..].trim().to_string();
        if (value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''))
        {
            value = value[1..value.len() - 1].to_string();
        }
        if !value.is_empty() {
            result.insert(key.to_string(), value);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_path() -> std::path::PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_millis();
        env::temp_dir().join(format!("nanoclaw-env-{millis}.env"))
    }

    #[test]
    fn reads_only_requested_keys() {
        let path = unique_path();
        fs::write(
            &path,
            "ASSISTANT_NAME=\"Andy\"\nOTHER=value\nCLAUDE_CODE_OAUTH_TOKEN='token'\n# comment\n",
        )
        .expect("write");

        let values = read_env_file(&path, &["ASSISTANT_NAME", "CLAUDE_CODE_OAUTH_TOKEN"]);
        assert_eq!(values.get("ASSISTANT_NAME").map(String::as_str), Some("Andy"));
        assert_eq!(
            values.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str),
            Some("token")
        );
        assert!(!values.contains_key("OTHER"));
    }
}
