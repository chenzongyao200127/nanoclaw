use std::fs;
use std::path::Path;
use std::process::Command;

use nanoclaw_core::config::AppConfig;
use nanoclaw_db::NanoClawDb;

use crate::platform::{Platform, command_exists, get_platform, is_headless, is_wsl};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppleContainerState {
    Installed,
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerState {
    Running,
    InstalledNotRunning,
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentReport {
    pub platform: Platform,
    pub is_wsl: bool,
    pub is_headless: bool,
    pub apple_container: AppleContainerState,
    pub docker: DockerState,
    pub has_env: bool,
    pub has_auth: bool,
    pub has_registered_groups: bool,
}

pub fn inspect_environment(config: &AppConfig, project_root: &Path) -> EnvironmentReport {
    let platform = get_platform();
    let is_wsl = is_wsl();
    let is_headless = is_headless();
    let apple_container = if command_exists("container") {
        AppleContainerState::Installed
    } else {
        AppleContainerState::NotFound
    };
    let docker = detect_docker();
    let has_env = project_root.join(".env").exists();
    let auth_dir = project_root.join("store").join("auth");
    let has_auth = auth_dir.exists()
        && fs::read_dir(&auth_dir)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false);
    let has_registered_groups = has_registered_groups(config, project_root);

    EnvironmentReport {
        platform,
        is_wsl,
        is_headless,
        apple_container,
        docker,
        has_env,
        has_auth,
        has_registered_groups,
    }
}

fn detect_docker() -> DockerState {
    if !command_exists("docker") {
        return DockerState::NotFound;
    }
    match Command::new("docker").arg("info").output() {
        Ok(output) if output.status.success() => DockerState::Running,
        Ok(_) | Err(_) => DockerState::InstalledNotRunning,
    }
}

fn has_registered_groups(config: &AppConfig, project_root: &Path) -> bool {
    if project_root.join("data").join("registered_groups.json").exists() {
        return true;
    }

    let db_path = config.store_dir.join("messages.db");
    if !db_path.exists() {
        return false;
    }

    let Ok(db) = NanoClawDb::open(&db_path) else {
        return false;
    };
    db.connection()
        .query_row("SELECT COUNT(*) FROM registered_groups", [], |row| row.get::<_, i64>(0))
        .map(|count| count > 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanoclaw_core::types::RegisteredGroup;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_millis();
        let dir = std::env::temp_dir().join(format!("nanoclaw-setup-{label}-{millis}"));
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn config(root: &Path) -> AppConfig {
        AppConfig {
            assistant_name: "Andy".to_string(),
            assistant_has_own_number: false,
            poll_interval_ms: 2000,
            scheduler_poll_interval_ms: 60000,
            mount_allowlist_path: root.join("mount-allowlist.json"),
            sender_allowlist_path: root.join("sender-allowlist.json"),
            store_dir: root.join("store"),
            groups_dir: root.join("groups"),
            data_dir: root.join("data"),
            container_image: "nanoclaw-agent:latest".to_string(),
            container_timeout_ms: 1000,
            container_max_output_size: 10_485_760,
            credential_proxy_port: 3001,
            ipc_poll_interval_ms: 1000,
            idle_timeout_ms: 1800000,
            max_concurrent_containers: 5,
            timezone: "UTC".to_string(),
        }
    }

    #[test]
    fn detects_empty_registered_groups_table() {
        let root = unique_temp_dir("env-empty");
        let cfg = config(&root);
        fs::create_dir_all(&cfg.store_dir).expect("store");
        let db = NanoClawDb::open(cfg.store_dir.join("messages.db")).expect("db");
        assert_eq!(
            db.connection()
                .query_row("SELECT COUNT(*) FROM registered_groups", [], |row| row.get::<_, i64>(0))
                .expect("count"),
            0
        );
        let report = inspect_environment(&cfg, &root);
        assert!(!report.has_registered_groups);
    }

    #[test]
    fn detects_registered_groups_in_db() {
        let root = unique_temp_dir("env-groups");
        let cfg = config(&root);
        let db = NanoClawDb::open(cfg.store_dir.join("messages.db")).expect("db");
        db.set_registered_group(
            "123@g.us",
            &RegisteredGroup {
                name: "Group".to_string(),
                folder: "group-1".to_string(),
                trigger: "@Andy".to_string(),
                added_at: "2024-01-01T00:00:00.000Z".to_string(),
                container_config: None,
                requires_trigger: Some(true),
                is_main: None,
            },
        )
        .expect("group");
        let report = inspect_environment(&cfg, &root);
        assert!(report.has_registered_groups);
    }

    #[test]
    fn detects_env_and_auth_dir() {
        let root = unique_temp_dir("env-auth");
        let cfg = config(&root);
        fs::write(root.join(".env"), "ANTHROPIC_API_KEY=test\n").expect("env");
        let auth_dir = root.join("store").join("auth");
        fs::create_dir_all(&auth_dir).expect("auth dir");
        fs::write(auth_dir.join("session.json"), "{}").expect("auth file");

        let report = inspect_environment(&cfg, &root);
        assert!(report.has_env);
        assert!(report.has_auth);
    }
}
