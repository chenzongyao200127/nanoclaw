use std::fs;
use std::path::{Path, PathBuf};

use nanoclaw_core::types::MountAllowlist;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountConfigResult {
    pub path: PathBuf,
    pub allowed_roots: usize,
    pub non_main_read_only: bool,
}

pub fn write_mount_allowlist(
    config_file: &Path,
    config: &MountAllowlist,
) -> Result<MountConfigResult, String> {
    if let Some(parent) = config_file.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let body = serde_json::to_string_pretty(config).map_err(|err| err.to_string())?;
    fs::write(config_file, format!("{body}\n")).map_err(|err| err.to_string())?;
    Ok(MountConfigResult {
        path: config_file.to_path_buf(),
        allowed_roots: config.allowed_roots.len(),
        non_main_read_only: config.non_main_read_only,
    })
}

pub fn empty_mount_allowlist() -> MountAllowlist {
    MountAllowlist {
        allowed_roots: vec![],
        blocked_patterns: vec![],
        non_main_read_only: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanoclaw_core::types::AllowedRoot;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_path() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir()
            .join(format!("nanoclaw-mounts-{nanos}-{}", std::process::id()))
            .join("mount-allowlist.json")
    }

    #[test]
    fn writes_empty_allowlist() {
        let path = unique_path();
        let result = write_mount_allowlist(&path, &empty_mount_allowlist()).expect("write");
        let body = fs::read_to_string(&path).expect("read");
        assert_eq!(result.allowed_roots, 0);
        assert!(result.non_main_read_only);
        assert!(body.contains("\"allowedRoots\""));
        assert!(body.contains("\"nonMainReadOnly\": true"));
    }

    #[test]
    fn writes_custom_allowlist() {
        let path = unique_path();
        let cfg = MountAllowlist {
            allowed_roots: vec![AllowedRoot {
                path: "~/projects".to_string(),
                allow_read_write: true,
                description: None,
            }],
            blocked_patterns: vec!["token".to_string()],
            non_main_read_only: false,
        };
        let result = write_mount_allowlist(&path, &cfg).expect("write");
        let body = fs::read_to_string(&path).expect("read");
        assert_eq!(result.allowed_roots, 1);
        assert!(!result.non_main_read_only);
        assert!(body.contains("\"nonMainReadOnly\": false"));
    }
}
