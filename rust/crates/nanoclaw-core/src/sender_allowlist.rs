use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatAllowlistEntry {
    pub allow: AllowRule,
    pub mode: AllowlistMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SenderAllowlistConfig {
    pub default: ChatAllowlistEntry,
    pub chats: HashMap<String, ChatAllowlistEntry>,
    pub log_denied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AllowRule {
    All(String),
    List(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AllowlistMode {
    Trigger,
    Drop,
}

pub fn default_config() -> SenderAllowlistConfig {
    SenderAllowlistConfig {
        default: ChatAllowlistEntry {
            allow: AllowRule::All("*".to_string()),
            mode: AllowlistMode::Trigger,
        },
        chats: HashMap::new(),
        log_denied: true,
    }
}

pub fn load_sender_allowlist(path: &Path) -> SenderAllowlistConfig {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => return default_config(),
    };

    let parsed = match serde_json::from_str::<Value>(&raw) {
        Ok(parsed) => parsed,
        Err(_) => return default_config(),
    };

    let Some(default) = parsed.get("default").and_then(parse_entry) else {
        return default_config();
    };

    let chats = parsed
        .get("chats")
        .and_then(Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(jid, entry)| parse_entry(entry).map(|parsed| (jid.clone(), parsed)))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    let log_denied = parsed
        .get("logDenied")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    SenderAllowlistConfig {
        default,
        chats,
        log_denied,
    }
}

pub fn is_sender_allowed(chat_jid: &str, sender: &str, cfg: &SenderAllowlistConfig) -> bool {
    let entry = get_entry(chat_jid, cfg);
    match &entry.allow {
        AllowRule::All(value) if value == "*" => true,
        AllowRule::All(_) => false,
        AllowRule::List(allowed) => allowed.iter().any(|allowed_sender| allowed_sender == sender),
    }
}

pub fn should_drop_message(chat_jid: &str, cfg: &SenderAllowlistConfig) -> bool {
    matches!(get_entry(chat_jid, cfg).mode, AllowlistMode::Drop)
}

pub fn is_trigger_allowed(chat_jid: &str, sender: &str, cfg: &SenderAllowlistConfig) -> bool {
    is_sender_allowed(chat_jid, sender, cfg)
}

fn get_entry<'a>(chat_jid: &str, cfg: &'a SenderAllowlistConfig) -> &'a ChatAllowlistEntry {
    cfg.chats.get(chat_jid).unwrap_or(&cfg.default)
}

fn is_valid_entry(entry: &ChatAllowlistEntry) -> bool {
    match &entry.allow {
        AllowRule::All(value) => value == "*",
        AllowRule::List(_) => true,
    }
}

fn parse_entry(value: &Value) -> Option<ChatAllowlistEntry> {
    let allow_value = value.get("allow")?;
    let mode_value = value.get("mode")?.as_str()?;

    let allow = if allow_value.as_str() == Some("*") {
        AllowRule::All("*".to_string())
    } else {
        let list = allow_value
            .as_array()?
            .iter()
            .map(|value| value.as_str().map(ToString::to_string))
            .collect::<Option<Vec<_>>>()?;
        AllowRule::List(list)
    };

    let mode = match mode_value {
        "trigger" => AllowlistMode::Trigger,
        "drop" => AllowlistMode::Drop,
        _ => return None,
    };

    let entry = ChatAllowlistEntry { allow, mode };
    is_valid_entry(&entry).then_some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_path(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        env::temp_dir().join(format!("nanoclaw-{name}-{nanos}-{}.json", std::process::id()))
    }

    fn write_config(config: &str) -> std::path::PathBuf {
        let path = unique_path("allowlist");
        fs::write(&path, config).expect("write");
        path
    }

    #[test]
    fn returns_default_when_file_missing() {
        let cfg = load_sender_allowlist(&unique_path("missing"));
        assert!(matches!(cfg.default.allow, AllowRule::All(_)));
        assert_eq!(cfg.default.mode, AllowlistMode::Trigger);
        assert!(cfg.log_denied);
    }

    #[test]
    fn loads_valid_config() {
        let path = write_config(
            r#"{"default":{"allow":["alice","bob"],"mode":"drop"},"chats":{},"logDenied":false}"#,
        );
        let cfg = load_sender_allowlist(&path);
        assert_eq!(
            cfg.default.allow,
            AllowRule::List(vec!["alice".to_string(), "bob".to_string()])
        );
        assert_eq!(cfg.default.mode, AllowlistMode::Drop);
        assert!(!cfg.log_denied);
    }

    #[test]
    fn skips_invalid_chat_entries() {
        let path = write_config(
            r#"{"default":{"allow":"*","mode":"trigger"},"chats":{"good":{"allow":["alice"],"mode":"trigger"},"bad":{"allow":"oops","mode":"drop"}},"logDenied":true}"#,
        );
        let cfg = load_sender_allowlist(&path);
        assert!(cfg.chats.contains_key("good"));
        assert!(!cfg.chats.contains_key("bad"));
    }

    #[test]
    fn evaluates_sender_permissions() {
        let cfg = SenderAllowlistConfig {
            default: ChatAllowlistEntry {
                allow: AllowRule::List(vec!["alice".to_string()]),
                mode: AllowlistMode::Trigger,
            },
            chats: HashMap::from([(
                "g2".to_string(),
                ChatAllowlistEntry {
                    allow: AllowRule::All("*".to_string()),
                    mode: AllowlistMode::Drop,
                },
            )]),
            log_denied: true,
        };

        assert!(is_sender_allowed("g1", "alice", &cfg));
        assert!(!is_sender_allowed("g1", "eve", &cfg));
        assert!(is_sender_allowed("g2", "eve", &cfg));
        assert!(!should_drop_message("g1", &cfg));
        assert!(should_drop_message("g2", &cfg));
        assert!(!is_trigger_allowed("g1", "eve", &cfg));
    }
}
