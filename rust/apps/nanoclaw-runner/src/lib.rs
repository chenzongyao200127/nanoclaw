use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const OUTPUT_START_MARKER: &str = "---NANOCLAW_OUTPUT_START---";
pub const OUTPUT_END_MARKER: &str = "---NANOCLAW_OUTPUT_END---";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerInput {
    pub prompt: String,
    pub session_id: Option<String>,
    pub group_folder: String,
    pub chat_jid: String,
    pub is_main: bool,
    pub is_scheduled_task: Option<bool>,
    pub assistant_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerOutput {
    pub status: RunnerStatus,
    pub result: Option<String>,
    pub new_session_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunnerStatus {
    Success,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum InputMessage {
    Message { text: String },
}

pub fn read_container_input<R: Read>(reader: &mut R) -> Result<ContainerInput, String> {
    let mut input = String::new();
    reader
        .read_to_string(&mut input)
        .map_err(|err| err.to_string())?;
    serde_json::from_str::<ContainerInput>(&input).map_err(|err| err.to_string())
}

pub fn write_output<W: Write>(writer: &mut W, output: &ContainerOutput) -> Result<(), String> {
    writeln!(writer, "{OUTPUT_START_MARKER}").map_err(|err| err.to_string())?;
    writeln!(
        writer,
        "{}",
        serde_json::to_string(output).map_err(|err| err.to_string())?
    )
    .map_err(|err| err.to_string())?;
    writeln!(writer, "{OUTPUT_END_MARKER}").map_err(|err| err.to_string())?;
    Ok(())
}

pub fn close_sentinel_path(ipc_input_dir: &Path) -> PathBuf {
    ipc_input_dir.join("_close")
}

pub fn consume_close_sentinel(ipc_input_dir: &Path) -> bool {
    let sentinel = close_sentinel_path(ipc_input_dir);
    if sentinel.exists() {
        let _ = fs::remove_file(sentinel);
        true
    } else {
        false
    }
}

pub fn drain_ipc_input(ipc_input_dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(ipc_input_dir) else {
        return Vec::new();
    };

    let mut files = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    files.sort();

    let mut messages = Vec::new();
    for file in files {
        let parsed = fs::read_to_string(&file)
            .ok()
            .and_then(|content| serde_json::from_str::<InputMessage>(&content).ok());
        let _ = fs::remove_file(&file);

        if let Some(InputMessage::Message { text }) = parsed {
            messages.push(text);
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = env::temp_dir().join(format!("nanoclaw-runner-{label}-{nanos}-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn reads_container_input_from_json() {
        let json = r#"{"prompt":"Hello","groupFolder":"group","chatJid":"jid","isMain":true}"#;
        let mut bytes = json.as_bytes();
        let input = read_container_input(&mut bytes).expect("input");
        assert_eq!(input.prompt, "Hello");
        assert_eq!(input.group_folder, "group");
        assert_eq!(input.chat_jid, "jid");
        assert!(input.is_main);
    }

    #[test]
    fn writes_output_with_markers() {
        let mut out = Vec::new();
        write_output(
            &mut out,
            &ContainerOutput {
                status: RunnerStatus::Success,
                result: Some("Done".to_string()),
                new_session_id: Some("session-1".to_string()),
                error: None,
            },
        )
        .expect("write");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains(OUTPUT_START_MARKER));
        assert!(text.contains(OUTPUT_END_MARKER));
        assert!(text.contains("\"newSessionId\":\"session-1\""));
    }

    #[test]
    fn consumes_close_sentinel() {
        let dir = unique_dir("close");
        fs::write(close_sentinel_path(&dir), "").expect("sentinel");
        assert!(consume_close_sentinel(&dir));
        assert!(!consume_close_sentinel(&dir));
    }

    #[test]
    fn drains_ipc_messages_and_ignores_invalid_files() {
        let dir = unique_dir("ipc");
        fs::write(
            dir.join("1.json"),
            r#"{"type":"message","text":"hello"}"#,
        )
        .expect("msg1");
        fs::write(dir.join("2.json"), r#"{"type":"message","text":"world"}"#).expect("msg2");
        fs::write(dir.join("3.json"), r#"{"type":"bad","text":"ignored"}"#).expect("bad");

        let messages = drain_ipc_input(&dir);
        assert_eq!(messages, vec!["hello".to_string(), "world".to_string()]);
        assert!(fs::read_dir(&dir).expect("dir").next().is_none());
    }
}
