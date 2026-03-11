use std::env;
use std::path::PathBuf;

use crate::env::read_env_file;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub assistant_name: String,
    pub assistant_has_own_number: bool,
    pub poll_interval_ms: u64,
    pub scheduler_poll_interval_ms: u64,
    pub mount_allowlist_path: PathBuf,
    pub sender_allowlist_path: PathBuf,
    pub store_dir: PathBuf,
    pub groups_dir: PathBuf,
    pub data_dir: PathBuf,
    pub container_image: String,
    pub container_timeout_ms: u64,
    pub container_max_output_size: u64,
    pub credential_proxy_port: u16,
    pub ipc_poll_interval_ms: u64,
    pub idle_timeout_ms: u64,
    pub max_concurrent_containers: usize,
    pub timezone: String,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let project_root = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let home_dir = env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        let env_values = read_env_file(
            &project_root.join(".env"),
            &[
                "ASSISTANT_NAME",
                "ASSISTANT_HAS_OWN_NUMBER",
                "CONTAINER_IMAGE",
                "CONTAINER_TIMEOUT",
                "CONTAINER_MAX_OUTPUT_SIZE",
                "CREDENTIAL_PROXY_PORT",
                "IDLE_TIMEOUT",
                "MAX_CONCURRENT_CONTAINERS",
                "TZ",
            ],
        );
        let assistant_name = env::var("ASSISTANT_NAME")
            .ok()
            .or_else(|| env_values.get("ASSISTANT_NAME").cloned())
            .unwrap_or_else(|| "Andy".to_string());
        let timezone = env::var("TZ")
            .ok()
            .or_else(|| env_values.get("TZ").cloned())
            .unwrap_or_else(|| "UTC".to_string());

        Self {
            assistant_name,
            assistant_has_own_number: env_bool(
                "ASSISTANT_HAS_OWN_NUMBER",
                env_values.get("ASSISTANT_HAS_OWN_NUMBER").map(String::as_str),
                false,
            ),
            poll_interval_ms: 2_000,
            scheduler_poll_interval_ms: 60_000,
            mount_allowlist_path: home_dir
                .join(".config")
                .join("nanoclaw")
                .join("mount-allowlist.json"),
            sender_allowlist_path: home_dir
                .join(".config")
                .join("nanoclaw")
                .join("sender-allowlist.json"),
            store_dir: project_root.join("store"),
            groups_dir: project_root.join("groups"),
            data_dir: project_root.join("data"),
            container_image: env::var("CONTAINER_IMAGE")
                .ok()
                .or_else(|| env_values.get("CONTAINER_IMAGE").cloned())
                .unwrap_or_else(|| "nanoclaw-agent:latest".to_string()),
            container_timeout_ms: env_u64(
                "CONTAINER_TIMEOUT",
                env_values.get("CONTAINER_TIMEOUT").map(String::as_str),
                1_800_000,
            ),
            container_max_output_size: env_u64(
                "CONTAINER_MAX_OUTPUT_SIZE",
                env_values.get("CONTAINER_MAX_OUTPUT_SIZE").map(String::as_str),
                10_485_760,
            ),
            credential_proxy_port: env_u16(
                "CREDENTIAL_PROXY_PORT",
                env_values.get("CREDENTIAL_PROXY_PORT").map(String::as_str),
                3001,
            ),
            ipc_poll_interval_ms: 1_000,
            idle_timeout_ms: env_u64(
                "IDLE_TIMEOUT",
                env_values.get("IDLE_TIMEOUT").map(String::as_str),
                1_800_000,
            ),
            max_concurrent_containers: env_usize(
                "MAX_CONCURRENT_CONTAINERS",
                env_values
                    .get("MAX_CONCURRENT_CONTAINERS")
                    .map(String::as_str),
                5,
            )
            .max(1),
            timezone,
        }
    }

    pub fn trigger_pattern(&self) -> String {
        format!("^@{}\\b", escape_regex(&self.assistant_name))
    }
}

fn env_bool(key: &str, fallback: Option<&str>, default: bool) -> bool {
    env::var(key)
        .ok()
        .or_else(|| fallback.map(ToString::to_string))
        .map(|value| value == "true")
        .unwrap_or(default)
}

fn env_u16(key: &str, fallback: Option<&str>, default: u16) -> u16 {
    env::var(key)
        .ok()
        .or_else(|| fallback.map(ToString::to_string))
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, fallback: Option<&str>, default: u64) -> u64 {
    env::var(key)
        .ok()
        .or_else(|| fallback.map(ToString::to_string))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, fallback: Option<&str>, default: usize) -> usize {
    env::var(key)
        .ok()
        .or_else(|| fallback.map(ToString::to_string))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn escape_regex(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']'
            | '\\' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn trigger_pattern_escapes_regex_chars() {
        let config = AppConfig {
            assistant_name: "A+B".to_string(),
            assistant_has_own_number: false,
            poll_interval_ms: 0,
            scheduler_poll_interval_ms: 0,
            mount_allowlist_path: PathBuf::from(""),
            sender_allowlist_path: PathBuf::from(""),
            store_dir: PathBuf::from(""),
            groups_dir: PathBuf::from(""),
            data_dir: PathBuf::from(""),
            container_image: "".to_string(),
            container_timeout_ms: 0,
            container_max_output_size: 0,
            credential_proxy_port: 0,
            ipc_poll_interval_ms: 0,
            idle_timeout_ms: 0,
            max_concurrent_containers: 1,
            timezone: "UTC".to_string(),
        };
        assert_eq!(config.trigger_pattern(), "^@A\\+B\\b");
    }

    #[test]
    fn reads_values_from_dot_env() {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_millis();
        let root = std::env::temp_dir().join(format!("nanoclaw-config-{millis}"));
        fs::create_dir_all(&root).expect("mkdir");
        fs::write(
            root.join(".env"),
            "ASSISTANT_NAME=\"Casey\"\nCONTAINER_TIMEOUT=1234\nTZ=Asia/Shanghai\n",
        )
        .expect("env");
        let cwd = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&root).expect("chdir");

        let config = AppConfig::from_env();

        std::env::set_current_dir(cwd).expect("restore");
        assert_eq!(config.assistant_name, "Casey");
        assert_eq!(config.container_timeout_ms, 1234);
        assert_eq!(config.timezone, "Asia/Shanghai");
    }
}
