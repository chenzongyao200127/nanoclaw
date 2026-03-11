use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use nanoclaw_core::config::AppConfig;
use nanoclaw_core::group_folder::{resolve_group_folder_path, resolve_group_ipc_path};
use nanoclaw_core::mount_security::{ValidatedMount, load_mount_allowlist, validate_additional_mounts};
use nanoclaw_core::types::{AvailableGroup, RegisteredGroup, ScheduledTask};
use serde::{Deserialize, Serialize};

use crate::{CONTAINER_HOST_GATEWAY, CONTAINER_RUNTIME_BIN, host_gateway_args, readonly_mount_args, stop_container_command};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    ApiKey,
    OAuth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeMount {
    pub host_path: PathBuf,
    pub container_path: String,
    pub readonly: bool,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunnerStatus {
    Success,
    Error,
}

pub fn detect_auth_mode_from_env(
    anthropic_api_key: Option<&str>,
) -> AuthMode {
    if anthropic_api_key.is_some_and(|value| !value.is_empty()) {
        AuthMode::ApiKey
    } else {
        AuthMode::OAuth
    }
}

pub fn build_volume_mounts(
    config: &AppConfig,
    group: &RegisteredGroup,
    is_main: bool,
) -> Result<Vec<VolumeMount>, String> {
    let mut mounts = Vec::new();
    let project_root = std::env::current_dir().map_err(|err| err.to_string())?;
    let group_dir = resolve_group_folder_path(&config.groups_dir, &group.folder)?;

    if is_main {
        mounts.push(VolumeMount {
            host_path: project_root.clone(),
            container_path: "/workspace/project".to_string(),
            readonly: true,
        });

        let env_file = project_root.join(".env");
        if env_file.exists() {
            mounts.push(VolumeMount {
                host_path: PathBuf::from("/dev/null"),
                container_path: "/workspace/project/.env".to_string(),
                readonly: true,
            });
        }
    }

    mounts.push(VolumeMount {
        host_path: group_dir.clone(),
        container_path: "/workspace/group".to_string(),
        readonly: false,
    });

    if !is_main {
        let global_dir = config.groups_dir.join("global");
        if global_dir.exists() {
            mounts.push(VolumeMount {
                host_path: global_dir,
                container_path: "/workspace/global".to_string(),
                readonly: true,
            });
        }
    }

    let session_dir = config
        .data_dir
        .join("sessions")
        .join(&group.folder)
        .join(".claude");
    fs::create_dir_all(&session_dir).map_err(|err| err.to_string())?;
    ensure_settings_file(&session_dir)?;
    mounts.push(VolumeMount {
        host_path: session_dir,
        container_path: "/home/node/.claude".to_string(),
        readonly: false,
    });

    let ipc_dir = resolve_group_ipc_path(&config.data_dir, &group.folder)?;
    fs::create_dir_all(ipc_dir.join("messages")).map_err(|err| err.to_string())?;
    fs::create_dir_all(ipc_dir.join("tasks")).map_err(|err| err.to_string())?;
    fs::create_dir_all(ipc_dir.join("input")).map_err(|err| err.to_string())?;
    mounts.push(VolumeMount {
        host_path: ipc_dir,
        container_path: "/workspace/ipc".to_string(),
        readonly: false,
    });

    let agent_runner_src = project_root.join("container").join("agent-runner").join("src");
    let group_agent_runner_dir = config
        .data_dir
        .join("sessions")
        .join(&group.folder)
        .join("agent-runner-src");
    if !group_agent_runner_dir.exists() && agent_runner_src.exists() {
        copy_dir_recursive(&agent_runner_src, &group_agent_runner_dir)?;
    }
    if group_agent_runner_dir.exists() {
        mounts.push(VolumeMount {
            host_path: group_agent_runner_dir,
            container_path: "/app/src".to_string(),
            readonly: false,
        });
    }

    if let Some(container_config) = &group.container_config {
        let allowlist = load_mount_allowlist(&config.mount_allowlist_path)?;
        let validated = validate_additional_mounts(
            &container_config.additional_mounts,
            &allowlist,
            is_main,
        );
        mounts.extend(validated.into_iter().map(validated_mount_to_volume_mount));
    }

    Ok(mounts)
}

pub fn build_container_args(
    config: &AppConfig,
    mounts: &[VolumeMount],
    container_name: &str,
    auth_mode: AuthMode,
) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "-i".to_string(),
        "--rm".to_string(),
        "--name".to_string(),
        container_name.to_string(),
        "-e".to_string(),
        format!("TZ={}", config.timezone),
        "-e".to_string(),
        format!(
            "ANTHROPIC_BASE_URL=http://{}:{}",
            CONTAINER_HOST_GATEWAY, config.credential_proxy_port
        ),
    ];

    match auth_mode {
        AuthMode::ApiKey => {
            args.push("-e".to_string());
            args.push("ANTHROPIC_API_KEY=placeholder".to_string());
        }
        AuthMode::OAuth => {
            args.push("-e".to_string());
            args.push("CLAUDE_CODE_OAUTH_TOKEN=placeholder".to_string());
        }
    }

    args.extend(host_gateway_args());

    for mount in mounts {
        if mount.readonly {
            args.extend(readonly_mount_args(&mount.host_path, &mount.container_path));
        } else {
            args.push("-v".to_string());
            args.push(format!(
                "{}:{}",
                mount.host_path.display(),
                mount.container_path
            ));
        }
    }

    args.push(config.container_image.clone());
    args
}

pub fn run_container_agent(
    config: &AppConfig,
    group: &RegisteredGroup,
    input: &ContainerInput,
    on_process: Option<&mut dyn FnMut(u32, &str)>,
    on_output: Option<&mut dyn FnMut(ContainerOutput)>,
) -> Result<ContainerOutput, String> {
    let group_dir = resolve_group_folder_path(&config.groups_dir, &group.folder)?;
    fs::create_dir_all(group_dir.join("logs")).map_err(|err| err.to_string())?;

    let mounts = build_volume_mounts(config, group, input.is_main)?;
    let safe_name = sanitize_container_name(&group.folder);
    let container_name = format!("nanoclaw-{safe_name}-{}", chrono::Utc::now().timestamp_millis());
    let auth_mode = detect_auth_mode_from_env(std::env::var("ANTHROPIC_API_KEY").ok().as_deref());
    let args = build_container_args(config, &mounts, &container_name, auth_mode);
    let stop_command = stop_container_command(&container_name);

    run_process_agent(
        config,
        group,
        input,
        CONTAINER_RUNTIME_BIN,
        &args,
        &stop_command,
        &container_name,
        on_process,
        on_output,
    )
}

pub fn extract_outputs_from_buffer(buffer: &mut String) -> Vec<ContainerOutput> {
    let mut outputs = Vec::new();
    while let Some(start_idx) = buffer.find(OUTPUT_START_MARKER) {
        let Some(end_idx) = buffer[start_idx + OUTPUT_START_MARKER.len()..]
            .find(OUTPUT_END_MARKER)
            .map(|offset| start_idx + OUTPUT_START_MARKER.len() + offset)
        else {
            break;
        };

        let json = buffer[start_idx + OUTPUT_START_MARKER.len()..end_idx].trim();
        if let Ok(output) = serde_json::from_str::<ContainerOutput>(json) {
            outputs.push(output);
        }
        let drain_end = end_idx + OUTPUT_END_MARKER.len();
        buffer.drain(..drain_end);
    }
    outputs
}

fn run_process_agent(
    config: &AppConfig,
    group: &RegisteredGroup,
    input: &ContainerInput,
    executable: &str,
    args: &[String],
    stop_command: &[String],
    container_name: &str,
    mut on_process: Option<&mut dyn FnMut(u32, &str)>,
    mut on_output: Option<&mut dyn FnMut(ContainerOutput)>,
) -> Result<ContainerOutput, String> {
    let logs_dir = resolve_group_folder_path(&config.groups_dir, &group.folder)?.join("logs");
    fs::create_dir_all(&logs_dir).map_err(|err| err.to_string())?;

    let mut child = Command::new(executable)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to spawn container runtime: {err}"))?;

    if let Some(callback) = on_process.as_mut() {
        callback(child.id(), container_name);
    }

    let payload = serde_json::to_vec(input).map_err(|err| err.to_string())?;
    child
        .stdin
        .take()
        .ok_or_else(|| "missing child stdin".to_string())?
        .write_all(&payload)
        .map_err(|err| err.to_string())?;

    let (tx, rx) = mpsc::channel();
    let stdout = child.stdout.take().ok_or_else(|| "missing stdout".to_string())?;
    let stderr = child.stderr.take().ok_or_else(|| "missing stderr".to_string())?;
    spawn_reader(stdout, true, tx.clone());
    spawn_reader(stderr, false, tx);

    let start = Instant::now();
    let config_timeout = group
        .container_config
        .as_ref()
        .and_then(|cfg| cfg.timeout_ms)
        .unwrap_or(config.container_timeout_ms);
    let timeout_ms = config_timeout.max(config.idle_timeout_ms + 30_000);
    let mut deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut timed_out = false;
    let mut had_streaming_output = false;
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    let mut exit_code = None;
    let mut stdout_text = String::new();
    let mut stderr_text = String::new();
    let mut parse_buffer = String::new();
    let mut final_session_id = input.session_id.clone();

    while !stdout_closed || !stderr_closed || exit_code.is_none() {
        if !timed_out && Instant::now() > deadline {
            timed_out = true;
            let _ = stop_process(stop_command);
            let _ = child.kill();
        }

        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(StreamEvent::Stdout(bytes)) => {
                let chunk = String::from_utf8_lossy(&bytes).to_string();
                append_capped(&mut stdout_text, &chunk, config.container_max_output_size as usize);
                parse_buffer.push_str(&chunk);
                let outputs = extract_outputs_from_buffer(&mut parse_buffer);
                if !outputs.is_empty() {
                    had_streaming_output = true;
                    deadline = Instant::now() + Duration::from_millis(timeout_ms);
                    for output in outputs {
                        if output.new_session_id.is_some() {
                            final_session_id = output.new_session_id.clone();
                        }
                        if let Some(callback) = on_output.as_mut() {
                            callback(output);
                        }
                    }
                }
            }
            Ok(StreamEvent::Stderr(bytes)) => {
                let chunk = String::from_utf8_lossy(&bytes).to_string();
                append_capped(&mut stderr_text, &chunk, config.container_max_output_size as usize);
            }
            Ok(StreamEvent::StdoutClosed) => stdout_closed = true,
            Ok(StreamEvent::StderrClosed) => stderr_closed = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                stdout_closed = true;
                stderr_closed = true;
            }
        }

        if exit_code.is_none() {
            if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
                exit_code = Some(status.code().unwrap_or(1));
            }
        }
    }

    write_run_log(
        &logs_dir,
        group,
        input,
        args,
        &stdout_text,
        &stderr_text,
        exit_code.unwrap_or(1),
        start.elapsed(),
        timed_out,
    )?;

    if timed_out {
        return if had_streaming_output {
            Ok(ContainerOutput {
                status: RunnerStatus::Success,
                result: None,
                new_session_id: final_session_id,
                error: None,
            })
        } else {
            Ok(ContainerOutput {
                status: RunnerStatus::Error,
                result: None,
                new_session_id: final_session_id,
                error: Some(format!("Container timed out after {config_timeout}ms")),
            })
        };
    }

    if exit_code != Some(0) {
        return Ok(ContainerOutput {
            status: RunnerStatus::Error,
            result: None,
            new_session_id: final_session_id,
            error: Some(format!(
                "Container exited with code {}: {}",
                exit_code.unwrap_or(1),
                stderr_text.chars().rev().take(200).collect::<String>().chars().rev().collect::<String>()
            )),
        });
    }

    if on_output.is_some() {
        return Ok(ContainerOutput {
            status: RunnerStatus::Success,
            result: None,
            new_session_id: final_session_id,
            error: None,
        });
    }

    if let Some(output) = extract_legacy_output(&stdout_text) {
        Ok(output)
    } else {
        Ok(ContainerOutput {
            status: RunnerStatus::Error,
            result: None,
            new_session_id: final_session_id,
            error: Some("Failed to parse container output".to_string()),
        })
    }
}

pub fn write_tasks_snapshot(
    config: &AppConfig,
    group_folder: &str,
    is_main: bool,
    tasks: &[ScheduledTask],
) -> Result<PathBuf, String> {
    let group_ipc_dir = resolve_group_ipc_path(&config.data_dir, group_folder)?;
    fs::create_dir_all(&group_ipc_dir).map_err(|err| err.to_string())?;

    let filtered_tasks = tasks
        .iter()
        .filter(|task| is_main || task.group_folder == group_folder)
        .map(|task| {
            serde_json::json!({
                "id": task.id,
                "groupFolder": task.group_folder,
                "prompt": task.prompt,
                "schedule_type": schedule_type_label(task.schedule_type),
                "schedule_value": task.schedule_value,
                "status": task_status_label(task.status),
                "next_run": task.next_run,
            })
        })
        .collect::<Vec<_>>();

    let path = group_ipc_dir.join("current_tasks.json");
    fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&filtered_tasks).map_err(|err| err.to_string())?
        ),
    )
    .map_err(|err| err.to_string())?;
    Ok(path)
}

pub fn write_groups_snapshot(
    config: &AppConfig,
    group_folder: &str,
    is_main: bool,
    groups: &[AvailableGroup],
) -> Result<PathBuf, String> {
    let group_ipc_dir = resolve_group_ipc_path(&config.data_dir, group_folder)?;
    fs::create_dir_all(&group_ipc_dir).map_err(|err| err.to_string())?;

    let visible_groups = if is_main { groups.to_vec() } else { Vec::new() };
    let path = group_ipc_dir.join("available_groups.json");
    fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({
                "groups": visible_groups,
                "lastSync": chrono::Utc::now().to_rfc3339(),
            }))
            .map_err(|err| err.to_string())?
        ),
    )
    .map_err(|err| err.to_string())?;
    Ok(path)
}

fn validated_mount_to_volume_mount(mount: ValidatedMount) -> VolumeMount {
    VolumeMount {
        host_path: PathBuf::from(mount.host_path),
        container_path: mount.container_path,
        readonly: mount.readonly,
    }
}

fn schedule_type_label(value: nanoclaw_core::types::ScheduleType) -> &'static str {
    match value {
        nanoclaw_core::types::ScheduleType::Cron => "cron",
        nanoclaw_core::types::ScheduleType::Interval => "interval",
        nanoclaw_core::types::ScheduleType::Once => "once",
    }
}

fn task_status_label(value: nanoclaw_core::types::TaskStatus) -> &'static str {
    match value {
        nanoclaw_core::types::TaskStatus::Active => "active",
        nanoclaw_core::types::TaskStatus::Paused => "paused",
        nanoclaw_core::types::TaskStatus::Completed => "completed",
    }
}

fn sanitize_container_name(folder: &str) -> String {
    folder
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn spawn_reader<R: Read + Send + 'static>(mut reader: R, is_stdout: bool, tx: mpsc::Sender<StreamEvent>) {
    thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let event = if is_stdout {
                        StreamEvent::Stdout(buffer[..read].to_vec())
                    } else {
                        StreamEvent::Stderr(buffer[..read].to_vec())
                    };
                    if tx.send(event).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = if is_stdout {
            tx.send(StreamEvent::StdoutClosed)
        } else {
            tx.send(StreamEvent::StderrClosed)
        };
    });
}

enum StreamEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    StdoutClosed,
    StderrClosed,
}

fn append_capped(target: &mut String, chunk: &str, max_len: usize) {
    if target.len() >= max_len {
        return;
    }
    let remaining = max_len - target.len();
    if chunk.len() > remaining {
        target.push_str(&chunk[..remaining]);
    } else {
        target.push_str(chunk);
    }
}

fn stop_process(command: &[String]) -> Result<(), String> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| "missing stop command".to_string())?;
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|err| err.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("stop command exited with {status}"))
    }
}

fn extract_legacy_output(stdout: &str) -> Option<ContainerOutput> {
    let mut buffer = stdout.to_string();
    let mut outputs = extract_outputs_from_buffer(&mut buffer);
    if !outputs.is_empty() {
        return outputs.pop();
    }

    stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| serde_json::from_str::<ContainerOutput>(line).ok())
}

fn write_run_log(
    logs_dir: &Path,
    group: &RegisteredGroup,
    input: &ContainerInput,
    args: &[String],
    stdout: &str,
    stderr: &str,
    exit_code: i32,
    duration: Duration,
    timed_out: bool,
) -> Result<(), String> {
    let timestamp = chrono::Utc::now()
        .to_rfc3339()
        .replace([':', '.'], "-");
    let log_path = logs_dir.join(format!("container-{timestamp}.log"));
    let payload = format!(
        "=== Container Run Log{} ===\nTimestamp: {}\nGroup: {}\nIsMain: {}\nDuration: {}ms\nExit Code: {}\n\n=== Input ===\n{}\n\n=== Args ===\n{}\n\n=== Stderr ===\n{}\n\n=== Stdout ===\n{}\n",
        if timed_out { " (TIMEOUT)" } else { "" },
        chrono::Utc::now().to_rfc3339(),
        group.name,
        input.is_main,
        duration.as_millis(),
        exit_code,
        serde_json::to_string_pretty(input).map_err(|err| err.to_string())?,
        args.join(" "),
        stderr,
        stdout,
    );
    fs::write(log_path, payload).map_err(|err| err.to_string())
}

fn ensure_settings_file(session_dir: &Path) -> Result<(), String> {
    let settings_path = session_dir.join("settings.json");
    if settings_path.exists() {
        return Ok(());
    }

    let payload = serde_json::json!({
        "env": {
            "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS": "1",
            "CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD": "1",
            "CLAUDE_CODE_DISABLE_AUTO_MEMORY": "0"
        }
    });
    fs::write(
        settings_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&payload).map_err(|err| err.to_string())?
        ),
    )
    .map_err(|err| err.to_string())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|err| err.to_string())?;
    for entry in fs::read_dir(src).map_err(|err| err.to_string())? {
        let entry = entry.map_err(|err| err.to_string())?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path).map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanoclaw_core::types::{
        AdditionalMount, ContainerConfig, ContextMode, ScheduleType, TaskStatus,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_millis();
        let dir = std::env::temp_dir().join(format!("nanoclaw-runtime-{label}-{millis}"));
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

    fn group(folder: &str, container_config: Option<ContainerConfig>) -> RegisteredGroup {
        RegisteredGroup {
            name: "Group".to_string(),
            folder: folder.to_string(),
            trigger: "@Andy".to_string(),
            added_at: "2024-01-01T00:00:00Z".to_string(),
            container_config,
            requires_trigger: None,
            is_main: None,
        }
    }

    fn task(id: &str, group_folder: &str) -> ScheduledTask {
        ScheduledTask {
            id: id.to_string(),
            group_folder: group_folder.to_string(),
            chat_jid: format!("{group_folder}@g.us"),
            prompt: format!("prompt-{id}"),
            schedule_type: ScheduleType::Once,
            schedule_value: "2026-06-01T00:00:00Z".to_string(),
            context_mode: ContextMode::Isolated,
            next_run: Some("2026-06-01T00:00:00Z".to_string()),
            last_run: None,
            last_result: None,
            status: TaskStatus::Active,
            created_at: "2026-03-11T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn detects_auth_mode() {
        assert_eq!(detect_auth_mode_from_env(Some("key")), AuthMode::ApiKey);
        assert_eq!(detect_auth_mode_from_env(None), AuthMode::OAuth);
    }

    #[test]
    fn builds_main_group_mounts() {
        let root = unique_temp_dir("main");
        let cfg = config(&root);
        fs::create_dir_all(cfg.groups_dir.join("main")).expect("group");
        fs::create_dir_all(root.join("container/agent-runner/src")).expect("runner");
        fs::write(root.join(".env"), "SECRET=1\n").expect("env");
        let cwd = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&root).expect("chdir");

        let mounts = build_volume_mounts(&cfg, &group("main", None), true).expect("mounts");

        std::env::set_current_dir(cwd).expect("restore");

        assert!(mounts.iter().any(|mount| mount.container_path == "/workspace/project" && mount.readonly));
        assert!(mounts.iter().any(|mount| mount.container_path == "/workspace/group" && !mount.readonly));
        assert!(mounts.iter().any(|mount| mount.container_path == "/workspace/project/.env"));
        assert!(mounts.iter().any(|mount| mount.container_path == "/home/node/.claude"));
        assert!(mounts.iter().any(|mount| mount.container_path == "/workspace/ipc"));
    }

    #[test]
    fn builds_non_main_mounts_and_additional_mounts() {
        let root = unique_temp_dir("non-main");
        let cfg = config(&root);
        fs::create_dir_all(cfg.groups_dir.join("group-a")).expect("group");
        fs::create_dir_all(cfg.groups_dir.join("global")).expect("global");
        fs::create_dir_all(root.join("allowed")).expect("allowed");
        fs::write(
            &cfg.mount_allowlist_path,
            serde_json::json!({
                "allowedRoots": [{
                    "path": root.join("allowed").display().to_string(),
                    "allowReadWrite": true
                }],
                "blockedPatterns": [],
                "nonMainReadOnly": true
            })
            .to_string(),
        )
        .expect("allowlist");

        let cwd = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&root).expect("chdir");
        let mounts = build_volume_mounts(
            &cfg,
            &group(
                "group-a",
                Some(ContainerConfig {
                    additional_mounts: vec![AdditionalMount {
                        host_path: root.join("allowed").display().to_string(),
                        container_path: Some("allowed".to_string()),
                        readonly: Some(false),
                    }],
                    timeout_ms: None,
                }),
            ),
            false,
        )
        .expect("mounts");
        std::env::set_current_dir(cwd).expect("restore");

        assert!(mounts.iter().any(|mount| mount.container_path == "/workspace/global" && mount.readonly));
        assert!(mounts.iter().any(|mount| mount.container_path == "/workspace/extra/allowed" && mount.readonly));
    }

    #[test]
    fn builds_container_args() {
        let cfg = config(&unique_temp_dir("args"));
        let args = build_container_args(
            &cfg,
            &[VolumeMount {
                host_path: PathBuf::from("/tmp/source"),
                container_path: "/workspace/group".to_string(),
                readonly: true,
            }],
            "nanoclaw-test",
            AuthMode::ApiKey,
        );

        assert!(args.contains(&"run".to_string()));
        assert!(args.contains(&"nanoclaw-test".to_string()));
        assert!(args.contains(&"ANTHROPIC_API_KEY=placeholder".to_string()));
        assert!(args.contains(&"/tmp/source:/workspace/group:ro".to_string()));
        assert_eq!(args.last(), Some(&"nanoclaw-agent:latest".to_string()));
    }

    #[test]
    fn writes_task_snapshot_filtered_for_non_main_group() {
        let root = unique_temp_dir("task-snapshot");
        let cfg = config(&root);
        let path = write_tasks_snapshot(
            &cfg,
            "group-a",
            false,
            &[task("task-1", "group-a"), task("task-2", "group-b")],
        )
        .expect("snapshot");
        let payload = fs::read_to_string(path).expect("read");

        assert!(payload.contains("\"id\": \"task-1\""));
        assert!(payload.contains("\"groupFolder\": \"group-a\""));
        assert!(!payload.contains("\"id\": \"task-2\""));
    }

    #[test]
    fn writes_groups_snapshot_visible_only_to_main_group() {
        let root = unique_temp_dir("groups-snapshot");
        let cfg = config(&root);
        let groups = vec![
            AvailableGroup {
                jid: "group-a@g.us".to_string(),
                name: "Group A".to_string(),
                last_activity: "2026-03-11T00:00:00Z".to_string(),
                is_registered: true,
            },
            AvailableGroup {
                jid: "group-b@g.us".to_string(),
                name: "Group B".to_string(),
                last_activity: "2026-03-11T00:01:00Z".to_string(),
                is_registered: false,
            },
        ];

        let main_path = write_groups_snapshot(&cfg, "group-a", true, &groups).expect("main");
        let non_main_path =
            write_groups_snapshot(&cfg, "group-b", false, &groups).expect("non-main");

        let main_payload = fs::read_to_string(main_path).expect("read main");
        let non_main_payload = fs::read_to_string(non_main_path).expect("read non-main");

        assert!(main_payload.contains("\"groups\""));
        assert!(main_payload.contains("\"group-a@g.us\""));
        assert!(non_main_payload.contains("\"groups\": []"));
    }

    #[test]
    fn extracts_multiple_outputs_from_buffer() {
        let mut buffer = format!(
            "noise\n{OUTPUT_START_MARKER}\n{{\"status\":\"success\",\"result\":\"one\",\"newSessionId\":\"s1\"}}\n{OUTPUT_END_MARKER}\n{OUTPUT_START_MARKER}\n{{\"status\":\"error\",\"result\":null,\"error\":\"bad\"}}\n{OUTPUT_END_MARKER}\n"
        );

        let outputs = extract_outputs_from_buffer(&mut buffer);
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].result.as_deref(), Some("one"));
        assert_eq!(outputs[0].new_session_id.as_deref(), Some("s1"));
        assert_eq!(outputs[1].status, RunnerStatus::Error);
        assert!(buffer.trim().is_empty());
    }

    #[test]
    fn runs_process_agent_in_streaming_mode() {
        let root = unique_temp_dir("streaming-run");
        let cfg = config(&root);
        fs::create_dir_all(cfg.groups_dir.join("group-a")).expect("group");
        let group = group("group-a", None);
        let input = ContainerInput {
            prompt: "hello".to_string(),
            session_id: None,
            group_folder: "group-a".to_string(),
            chat_jid: "group-a@g.us".to_string(),
            is_main: false,
            is_scheduled_task: None,
            assistant_name: Some("Andy".to_string()),
        };
        let script = format!(
            "cat >/dev/null; printf '%s\\n{{\"status\":\"success\",\"result\":\"done\",\"newSessionId\":\"session-1\"}}\\n%s\\n' '{OUTPUT_START_MARKER}' '{OUTPUT_END_MARKER}'"
        );
        let args = vec!["-c".to_string(), script];
        let stop = vec!["sh".to_string(), "-c".to_string(), "exit 0".to_string()];
        let mut seen = Vec::new();
        let mut callback = |output: ContainerOutput| seen.push(output);

        let result = run_process_agent(
            &cfg,
            &group,
            &input,
            "sh",
            &args,
            &stop,
            "nanoclaw-test",
            None,
            Some(&mut callback),
        )
        .expect("run");

        assert_eq!(result.status, RunnerStatus::Success);
        assert_eq!(result.new_session_id.as_deref(), Some("session-1"));
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].result.as_deref(), Some("done"));
    }

    #[test]
    fn runs_process_agent_in_legacy_mode() {
        let root = unique_temp_dir("legacy-run");
        let cfg = config(&root);
        fs::create_dir_all(cfg.groups_dir.join("group-a")).expect("group");
        let group = group("group-a", None);
        let input = ContainerInput {
            prompt: "hello".to_string(),
            session_id: Some("session-old".to_string()),
            group_folder: "group-a".to_string(),
            chat_jid: "group-a@g.us".to_string(),
            is_main: false,
            is_scheduled_task: None,
            assistant_name: Some("Andy".to_string()),
        };
        let script = "cat >/dev/null; printf '%s\n' '{\"status\":\"success\",\"result\":\"legacy\",\"newSessionId\":\"session-2\"}'".to_string();
        let args = vec!["-c".to_string(), script];
        let stop = vec!["sh".to_string(), "-c".to_string(), "exit 0".to_string()];

        let result = run_process_agent(
            &cfg,
            &group,
            &input,
            "sh",
            &args,
            &stop,
            "nanoclaw-test",
            None,
            None,
        )
        .expect("run");

        assert_eq!(result.status, RunnerStatus::Success);
        assert_eq!(result.result.as_deref(), Some("legacy"));
        assert_eq!(result.new_session_id.as_deref(), Some("session-2"));
    }
}
