use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use nanoclaw_core::group_folder::is_valid_group_folder;
use nanoclaw_core::types::RegisteredGroup;
use nanoclaw_db::NanoClawDb;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterArgs {
    pub jid: String,
    pub name: String,
    pub trigger: String,
    pub folder: String,
    pub channel: String,
    pub requires_trigger: bool,
    pub is_main: bool,
    pub assistant_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterResult {
    pub folder_path: PathBuf,
    pub name_updated: bool,
}

pub fn register_channel(
    project_root: &Path,
    store_dir: &Path,
    args: &RegisterArgs,
) -> Result<RegisterResult, String> {
    if args.jid.is_empty()
        || args.name.is_empty()
        || args.trigger.is_empty()
        || args.folder.is_empty()
    {
        return Err("missing_required_args".to_string());
    }
    if !is_valid_group_folder(&args.folder) {
        return Err("invalid_folder".to_string());
    }

    fs::create_dir_all(project_root.join("data")).map_err(|err| err.to_string())?;
    fs::create_dir_all(store_dir).map_err(|err| err.to_string())?;

    let db = NanoClawDb::open(store_dir.join("messages.db")).map_err(|err| err.to_string())?;
    db.set_registered_group(
        &args.jid,
        &RegisteredGroup {
            name: args.name.clone(),
            folder: args.folder.clone(),
            trigger: args.trigger.clone(),
            added_at: Utc::now().to_rfc3339(),
            container_config: None,
            requires_trigger: Some(args.requires_trigger),
            is_main: Some(args.is_main),
        },
    )
    .map_err(|err| err.to_string())?;

    let folder_path = project_root.join("groups").join(&args.folder).join("logs");
    fs::create_dir_all(&folder_path).map_err(|err| err.to_string())?;

    let name_updated = if args.assistant_name != "Andy" {
        update_assistant_name(project_root, &args.folder, &args.assistant_name)?
    } else {
        false
    };

    Ok(RegisterResult {
        folder_path,
        name_updated,
    })
}

pub fn update_assistant_name(
    project_root: &Path,
    folder: &str,
    assistant_name: &str,
) -> Result<bool, String> {
    let md_files = [
        project_root.join("groups").join("global").join("CLAUDE.md"),
        project_root.join("groups").join(folder).join("CLAUDE.md"),
    ];

    for md_file in &md_files {
        if md_file.exists() {
            let mut content = fs::read_to_string(md_file).map_err(|err| err.to_string())?;
            content = content.replace("# Andy", &format!("# {assistant_name}"));
            content = content.replace("You are Andy", &format!("You are {assistant_name}"));
            fs::write(md_file, content).map_err(|err| err.to_string())?;
        }
    }

    let env_file = project_root.join(".env");
    let env_content = if env_file.exists() {
        let content = fs::read_to_string(&env_file).map_err(|err| err.to_string())?;
        if content.contains("ASSISTANT_NAME=") {
            content
                .lines()
                .map(|line| {
                    if line.starts_with("ASSISTANT_NAME=") {
                        format!("ASSISTANT_NAME=\"{assistant_name}\"")
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            format!("{content}\nASSISTANT_NAME=\"{assistant_name}\"")
        }
    } else {
        format!("ASSISTANT_NAME=\"{assistant_name}\"\n")
    };
    fs::write(env_file, env_content).map_err(|err| err.to_string())?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_root(label: &str) -> PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_millis();
        let root = std::env::temp_dir().join(format!("nanoclaw-register-{label}-{millis}"));
        fs::create_dir_all(root.join("groups/global")).expect("global");
        root
    }

    #[test]
    fn registers_group_in_db() {
        let root = unique_root("db");
        let store = root.join("store");
        let result = register_channel(
            &root,
            &store,
            &RegisterArgs {
                jid: "123@g.us".to_string(),
                name: "Test Group".to_string(),
                trigger: "@Andy".to_string(),
                folder: "test-group".to_string(),
                channel: "whatsapp".to_string(),
                requires_trigger: true,
                is_main: false,
                assistant_name: "Andy".to_string(),
            },
        )
        .expect("register");

        let db = NanoClawDb::open(store.join("messages.db")).expect("db");
        let row = db.get_registered_group("123@g.us").expect("query").expect("group");
        assert_eq!(row.name, "Test Group");
        assert_eq!(row.folder, "test-group");
        assert!(result.folder_path.ends_with("groups/test-group/logs"));
    }

    #[test]
    fn rejects_invalid_folder() {
        let root = unique_root("invalid");
        let err = register_channel(
            &root,
            &root.join("store"),
            &RegisterArgs {
                jid: "123@g.us".to_string(),
                name: "Test Group".to_string(),
                trigger: "@Andy".to_string(),
                folder: "../../etc".to_string(),
                channel: "whatsapp".to_string(),
                requires_trigger: true,
                is_main: false,
                assistant_name: "Andy".to_string(),
            },
        )
        .expect_err("invalid folder");
        assert_eq!(err, "invalid_folder");
    }

    #[test]
    fn updates_assistant_name_files() {
        let root = unique_root("name");
        let folder = "test-group";
        fs::create_dir_all(root.join("groups").join(folder)).expect("group");
        fs::write(
            root.join("groups/global/CLAUDE.md"),
            "# Andy\n\nYou are Andy, a personal assistant.\n",
        )
        .expect("global claude");
        fs::write(
            root.join("groups").join(folder).join("CLAUDE.md"),
            "# Andy\n\nYou are Andy.\n",
        )
        .expect("group claude");

        let result = register_channel(
            &root,
            &root.join("store"),
            &RegisterArgs {
                jid: "123@g.us".to_string(),
                name: "Test Group".to_string(),
                trigger: "@Andy".to_string(),
                folder: folder.to_string(),
                channel: "whatsapp".to_string(),
                requires_trigger: true,
                is_main: false,
                assistant_name: "Nova".to_string(),
            },
        )
        .expect("register");

        assert!(result.name_updated);
        let env = fs::read_to_string(root.join(".env")).expect("env");
        assert!(env.contains("ASSISTANT_NAME=\"Nova\""));
        let claude = fs::read_to_string(root.join("groups/global/CLAUDE.md")).expect("claude");
        assert!(claude.contains("# Nova"));
        assert!(claude.contains("You are Nova"));
    }
}
