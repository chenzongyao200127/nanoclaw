use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::types::{AdditionalMount, AllowedRoot, MountAllowlist};

pub const DEFAULT_BLOCKED_PATTERNS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".gpg",
    ".aws",
    ".azure",
    ".gcloud",
    ".kube",
    ".docker",
    "credentials",
    ".env",
    ".netrc",
    ".npmrc",
    ".pypirc",
    "id_rsa",
    "id_ed25519",
    "private_key",
    ".secret",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedMount {
    pub host_path: String,
    pub container_path: String,
    pub readonly: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountValidationResult {
    pub allowed: bool,
    pub reason: String,
    pub real_host_path: Option<String>,
    pub resolved_container_path: Option<String>,
    pub effective_readonly: Option<bool>,
}

pub fn load_mount_allowlist(path: &Path) -> Result<MountAllowlist, String> {
    let content = fs::read_to_string(path)
        .map_err(|err| format!("Failed to read allowlist {}: {err}", path.display()))?;
    let mut allowlist: MountAllowlist =
        serde_json::from_str(&content).map_err(|err| format!("Invalid allowlist JSON: {err}"))?;

    allowlist.blocked_patterns = merged_blocked_patterns(&allowlist);
    Ok(allowlist)
}

pub fn validate_mount(
    mount: &AdditionalMount,
    allowlist: &MountAllowlist,
    is_main: bool,
) -> MountValidationResult {
    let blocked_patterns = merged_blocked_patterns(allowlist);
    let container_path = mount
        .container_path
        .clone()
        .unwrap_or_else(|| default_container_path(&mount.host_path));

    if !is_valid_container_path(&container_path) {
        return MountValidationResult {
            allowed: false,
            reason: format!(
                "Invalid container path: \"{container_path}\" - must be relative, non-empty, and not contain \"..\""
            ),
            real_host_path: None,
            resolved_container_path: None,
            effective_readonly: None,
        };
    }

    let expanded_path = expand_path(&mount.host_path);
    let real_path = match fs::canonicalize(&expanded_path) {
        Ok(path) => path,
        Err(_) => {
            return MountValidationResult {
                allowed: false,
                reason: format!(
                    "Host path does not exist: \"{}\" (expanded: \"{}\")",
                    mount.host_path,
                    expanded_path.display()
                ),
                real_host_path: None,
                resolved_container_path: None,
                effective_readonly: None,
            }
        }
    };

    let real_path_str = real_path.display().to_string();
    if let Some(pattern) = matches_blocked_pattern(&real_path, &blocked_patterns) {
        return MountValidationResult {
            allowed: false,
            reason: format!("Path matches blocked pattern \"{pattern}\": \"{real_path_str}\""),
            real_host_path: None,
            resolved_container_path: None,
            effective_readonly: None,
        };
    }

    let Some(allowed_root) = find_allowed_root(&real_path, &allowlist.allowed_roots) else {
        let roots = allowlist
            .allowed_roots
            .iter()
            .map(|root| expand_path(&root.path).display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return MountValidationResult {
            allowed: false,
            reason: format!(
                "Path \"{real_path_str}\" is not under any allowed root. Allowed roots: {roots}"
            ),
            real_host_path: None,
            resolved_container_path: None,
            effective_readonly: None,
        };
    };

    let requested_read_write = mount.readonly == Some(false);
    let effective_readonly = if requested_read_write {
        (!is_main && allowlist.non_main_read_only) || !allowed_root.allow_read_write
    } else {
        true
    };

    MountValidationResult {
        allowed: true,
        reason: format!(
            "Allowed under root \"{}\"{}",
            allowed_root.path,
            allowed_root
                .description
                .as_ref()
                .map(|desc| format!(" ({desc})"))
                .unwrap_or_default()
        ),
        real_host_path: Some(real_path_str),
        resolved_container_path: Some(container_path),
        effective_readonly: Some(effective_readonly),
    }
}

pub fn validate_additional_mounts(
    mounts: &[AdditionalMount],
    allowlist: &MountAllowlist,
    is_main: bool,
) -> Vec<ValidatedMount> {
    mounts
        .iter()
        .filter_map(|mount| {
            let result = validate_mount(mount, allowlist, is_main);
            result.allowed.then(|| ValidatedMount {
                host_path: result.real_host_path.expect("validated mount path"),
                container_path: format!(
                    "/workspace/extra/{}",
                    result.resolved_container_path.expect("validated container path")
                ),
                readonly: result.effective_readonly.expect("validated readonly"),
            })
        })
        .collect()
}

pub fn generate_allowlist_template() -> String {
    let template = MountAllowlist {
        allowed_roots: vec![
            AllowedRoot {
                path: "~/projects".to_string(),
                allow_read_write: true,
                description: Some("Development projects".to_string()),
            },
            AllowedRoot {
                path: "~/repos".to_string(),
                allow_read_write: true,
                description: Some("Git repositories".to_string()),
            },
            AllowedRoot {
                path: "~/Documents/work".to_string(),
                allow_read_write: false,
                description: Some("Work documents (read-only)".to_string()),
            },
        ],
        blocked_patterns: vec![
            "password".to_string(),
            "secret".to_string(),
            "token".to_string(),
        ],
        non_main_read_only: true,
    };

    serde_json::to_string_pretty(&template).expect("template serialization")
}

fn expand_path(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir();
    }

    if let Some(stripped) = path.strip_prefix("~/") {
        return home_dir().join(stripped);
    }

    PathBuf::from(path)
}

fn home_dir() -> PathBuf {
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn default_container_path(host_path: &str) -> String {
    Path::new(host_path)
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mount".to_string())
}

fn merged_blocked_patterns(allowlist: &MountAllowlist) -> Vec<String> {
    let mut blocked_patterns: Vec<String> = DEFAULT_BLOCKED_PATTERNS
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    for pattern in &allowlist.blocked_patterns {
        if !blocked_patterns.contains(pattern) {
            blocked_patterns.push(pattern.clone());
        }
    }
    blocked_patterns
}

fn matches_blocked_pattern(real_path: &Path, blocked_patterns: &[String]) -> Option<String> {
    let real_path_str = real_path.display().to_string();
    let parts = real_path
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    for pattern in blocked_patterns {
        if parts
            .iter()
            .any(|part| part == pattern || part.contains(pattern))
            || real_path_str.contains(pattern)
        {
            return Some(pattern.clone());
        }
    }

    None
}

fn find_allowed_root<'a>(real_path: &Path, allowed_roots: &'a [AllowedRoot]) -> Option<&'a AllowedRoot> {
    allowed_roots.iter().find(|root| {
        let expanded_root = expand_path(&root.path);
        let Ok(real_root) = fs::canonicalize(expanded_root) else {
            return false;
        };

        real_path.starts_with(real_root)
    })
}

fn is_valid_container_path(container_path: &str) -> bool {
    !container_path.is_empty()
        && container_path == container_path.trim()
        && !container_path.starts_with('/')
        && !container_path.contains("..")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_millis();
        let dir = env::temp_dir().join(format!("nanoclaw-{label}-{millis}"));
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn validates_main_group_read_write_mount() {
        let root = unique_temp_dir("mount-root");
        let project = root.join("projects");
        fs::create_dir_all(&project).expect("project dir");

        let allowlist = MountAllowlist {
            allowed_roots: vec![AllowedRoot {
                path: project.display().to_string(),
                allow_read_write: true,
                description: None,
            }],
            blocked_patterns: vec![],
            non_main_read_only: true,
        };
        let mount = AdditionalMount {
            host_path: project.display().to_string(),
            container_path: Some("project".to_string()),
            readonly: Some(false),
        };

        let result = validate_mount(&mount, &allowlist, true);
        assert!(result.allowed);
        assert_eq!(result.effective_readonly, Some(false));
    }

    #[test]
    fn forces_non_main_mounts_read_only() {
        let root = unique_temp_dir("non-main");
        let docs = root.join("docs");
        fs::create_dir_all(&docs).expect("docs dir");

        let allowlist = MountAllowlist {
            allowed_roots: vec![AllowedRoot {
                path: docs.display().to_string(),
                allow_read_write: true,
                description: None,
            }],
            blocked_patterns: vec![],
            non_main_read_only: true,
        };
        let mount = AdditionalMount {
            host_path: docs.display().to_string(),
            container_path: None,
            readonly: Some(false),
        };

        let result = validate_mount(&mount, &allowlist, false);
        assert!(result.allowed);
        assert_eq!(result.effective_readonly, Some(true));
    }

    #[test]
    fn blocks_sensitive_paths() {
        let root = unique_temp_dir("blocked");
        let ssh_dir = root.join(".ssh");
        fs::create_dir_all(&ssh_dir).expect("ssh dir");

        let allowlist = MountAllowlist {
            allowed_roots: vec![AllowedRoot {
                path: root.display().to_string(),
                allow_read_write: true,
                description: None,
            }],
            blocked_patterns: vec![],
            non_main_read_only: true,
        };
        let mount = AdditionalMount {
            host_path: ssh_dir.display().to_string(),
            container_path: None,
            readonly: Some(true),
        };

        let result = validate_mount(&mount, &allowlist, true);
        assert!(!result.allowed);
        assert!(result.reason.contains(".ssh"));
    }

    #[test]
    fn generates_template_json() {
        let template = generate_allowlist_template();
        assert!(template.contains("\"allowedRoots\""));
        assert!(template.contains("\"nonMainReadOnly\""));
    }
}
