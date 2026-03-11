use std::collections::HashMap;
use std::fs;
use std::path::Path;

use nanoclaw_core::env::read_env_file;
use nanoclaw_db::NanoClawDb;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub credentials: bool,
    pub configured_channels: Vec<String>,
    pub registered_groups: usize,
    pub mount_allowlist: bool,
    pub has_whatsapp_auth: bool,
}

pub fn inspect_installation(
    project_root: &Path,
    home_dir: &Path,
    db: Option<&NanoClawDb>,
) -> VerifyReport {
    let env_file = project_root.join(".env");
    let env_values = read_env_file(
        &env_file,
        &[
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "TELEGRAM_BOT_TOKEN",
            "SLACK_BOT_TOKEN",
            "SLACK_APP_TOKEN",
            "DISCORD_BOT_TOKEN",
        ],
    );

    let credentials = env_values.contains_key("CLAUDE_CODE_OAUTH_TOKEN")
        || env_values.contains_key("ANTHROPIC_API_KEY");

    let auth_dir = project_root.join("store").join("auth");
    let has_whatsapp_auth = auth_dir.exists()
        && fs::read_dir(&auth_dir)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false);

    let configured_channels = configured_channels(&env_values, has_whatsapp_auth);
    let registered_groups = db
        .map(|db| {
            db.connection()
                .query_row("SELECT COUNT(*) FROM registered_groups", [], |row| row.get::<_, i64>(0))
                .map(|count| count as usize)
                .unwrap_or(0)
        })
        .unwrap_or(0);

    VerifyReport {
        credentials,
        configured_channels,
        registered_groups,
        mount_allowlist: home_dir
            .join(".config")
            .join("nanoclaw")
            .join("mount-allowlist.json")
            .exists(),
        has_whatsapp_auth,
    }
}

fn configured_channels(env_values: &HashMap<String, String>, has_whatsapp_auth: bool) -> Vec<String> {
    let mut channels = Vec::new();
    if has_whatsapp_auth {
        channels.push("whatsapp".to_string());
    }
    if env_values.contains_key("TELEGRAM_BOT_TOKEN") {
        channels.push("telegram".to_string());
    }
    if env_values.contains_key("SLACK_BOT_TOKEN") && env_values.contains_key("SLACK_APP_TOKEN") {
        channels.push("slack".to_string());
    }
    if env_values.contains_key("DISCORD_BOT_TOKEN") {
        channels.push("discord".to_string());
    }
    channels
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanoclaw_core::types::RegisteredGroup;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_root(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("nanoclaw-verify-{label}-{nanos}"));
        fs::create_dir_all(&root).expect("mkdir");
        root
    }

    #[test]
    fn inspects_configured_installation() {
        let root = unique_root("configured");
        let home = unique_root("home");
        fs::write(
            root.join(".env"),
            "ANTHROPIC_API_KEY=test\nTELEGRAM_BOT_TOKEN=tg\nDISCORD_BOT_TOKEN=dc\n",
        )
        .expect("env");
        fs::create_dir_all(root.join("store/auth")).expect("auth");
        fs::write(root.join("store/auth/session.json"), "{}").expect("auth file");
        fs::create_dir_all(home.join(".config/nanoclaw")).expect("cfg dir");
        fs::write(home.join(".config/nanoclaw/mount-allowlist.json"), "{}").expect("allowlist");

        let db = NanoClawDb::open(root.join("store/messages.db")).expect("db");
        db.set_registered_group(
            "123@g.us",
            &RegisteredGroup {
                name: "Group".to_string(),
                folder: "group".to_string(),
                trigger: "@Andy".to_string(),
                added_at: "2024-01-01T00:00:00Z".to_string(),
                container_config: None,
                requires_trigger: Some(true),
                is_main: None,
            },
        )
        .expect("group");

        let report = inspect_installation(&root, &home, Some(&db));
        assert!(report.credentials);
        assert!(report.has_whatsapp_auth);
        assert!(report.mount_allowlist);
        assert_eq!(report.registered_groups, 1);
        assert!(report.configured_channels.contains(&"telegram".to_string()));
        assert!(report.configured_channels.contains(&"discord".to_string()));
        assert!(report.configured_channels.contains(&"whatsapp".to_string()));
    }
}
