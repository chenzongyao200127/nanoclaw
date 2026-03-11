use std::path::{Path, PathBuf};

const MAX_FOLDER_LEN: usize = 64;
const RESERVED_FOLDERS: &[&str] = &["global"];

pub fn is_valid_group_folder(folder: &str) -> bool {
    if folder.is_empty() || folder != folder.trim() || folder.len() > MAX_FOLDER_LEN {
        return false;
    }
    if folder.contains('/') || folder.contains('\\') || folder.contains("..") {
        return false;
    }
    if RESERVED_FOLDERS
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(folder))
    {
        return false;
    }

    let mut chars = folder.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    if !first.is_ascii_alphanumeric() {
        return false;
    }

    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

pub fn assert_valid_group_folder(folder: &str) -> Result<(), String> {
    if is_valid_group_folder(folder) {
        Ok(())
    } else {
        Err(format!("Invalid group folder \"{folder}\""))
    }
}

pub fn resolve_group_folder_path(groups_dir: &Path, folder: &str) -> Result<PathBuf, String> {
    assert_valid_group_folder(folder)?;
    let group_path = groups_dir.join(folder);
    ensure_within_base(groups_dir, &group_path)?;
    Ok(group_path)
}

pub fn resolve_group_ipc_path(data_dir: &Path, folder: &str) -> Result<PathBuf, String> {
    assert_valid_group_folder(folder)?;
    let ipc_base = data_dir.join("ipc");
    let ipc_path = ipc_base.join(folder);
    ensure_within_base(&ipc_base, &ipc_path)?;
    Ok(ipc_path)
}

fn ensure_within_base(base_dir: &Path, resolved_path: &Path) -> Result<(), String> {
    let relative = resolved_path
        .strip_prefix(base_dir)
        .map_err(|_| format!("Path escapes base directory: {}", resolved_path.display()))?;

    if relative.is_absolute() {
        return Err(format!(
            "Path escapes base directory: {}",
            resolved_path.display()
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn validates_folder_names() {
        assert!(is_valid_group_folder("main"));
        assert!(is_valid_group_folder("family-chat"));
        assert!(is_valid_group_folder("Team_42"));
        assert!(!is_valid_group_folder("../../etc"));
        assert!(!is_valid_group_folder("/tmp"));
        assert!(!is_valid_group_folder("global"));
        assert!(!is_valid_group_folder(""));
    }

    #[test]
    fn resolves_paths_under_base_dirs() {
        let groups = PathBuf::from("/tmp/project/groups");
        let data = PathBuf::from("/tmp/project/data");

        let group = resolve_group_folder_path(&groups, "family-chat").expect("group");
        let ipc = resolve_group_ipc_path(&data, "family-chat").expect("ipc");

        assert_eq!(group, PathBuf::from("/tmp/project/groups/family-chat"));
        assert_eq!(ipc, PathBuf::from("/tmp/project/data/ipc/family-chat"));
    }

    #[test]
    fn rejects_unsafe_folder_names() {
        let groups = PathBuf::from("/tmp/project/groups");
        let data = PathBuf::from("/tmp/project/data");

        assert!(resolve_group_folder_path(&groups, "../../etc").is_err());
        assert!(resolve_group_ipc_path(&data, "/tmp").is_err());
    }
}

