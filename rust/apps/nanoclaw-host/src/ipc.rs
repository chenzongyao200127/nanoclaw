use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Duration, Utc};
use nanoclaw_core::group_folder::is_valid_group_folder;
use nanoclaw_core::scheduler::compute_next_run_at;
use nanoclaw_core::types::{
    AvailableGroup, ContextMode, RegisteredGroup, ScheduleType, ScheduledTask, TaskStatus,
    TaskUpdate,
};
use nanoclaw_db::NanoClawDb;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcTask {
    ScheduleTask {
        task_id: Option<String>,
        prompt: String,
        schedule_type: ScheduleType,
        schedule_value: String,
        context_mode: ContextMode,
        target_jid: String,
    },
    PauseTask { task_id: String },
    ResumeTask { task_id: String },
    CancelTask { task_id: String },
    UpdateTask {
        task_id: String,
        prompt: Option<String>,
        schedule_type: Option<ScheduleType>,
        schedule_value: Option<String>,
    },
    RefreshGroups,
    RegisterGroup {
        jid: String,
        name: String,
        folder: String,
        trigger: String,
        requires_trigger: Option<bool>,
        container_config: Option<nanoclaw_core::types::ContainerConfig>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcEvent {
    TaskCreated { task_id: String, target_folder: String },
    TaskPaused { task_id: String },
    TaskResumed { task_id: String },
    TaskCancelled { task_id: String },
    TaskUpdated { task_id: String },
    GroupsRefreshRequested,
    GroupRegistered { jid: String, folder: String },
    Ignored,
}

pub struct IpcContext<'a> {
    pub db: &'a NanoClawDb,
    pub source_group: &'a str,
    pub is_main: bool,
    pub timezone: &'a str,
    pub registered_groups: &'a mut HashMap<String, RegisteredGroup>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IpcScanReport {
    pub processed_messages: usize,
    pub processed_tasks: usize,
    pub quarantined_files: usize,
    pub unauthorized_attempts: usize,
}

pub trait IpcWatcherDeps {
    fn send_message(&mut self, jid: &str, text: &str) -> Result<(), String>;
    fn sync_groups(&mut self, force: bool) -> Result<(), String>;
    fn get_available_groups(&self) -> Vec<AvailableGroup>;
    fn write_groups_snapshot(
        &mut self,
        group_folder: &str,
        is_main: bool,
        available_groups: &[AvailableGroup],
        registered_jids: &HashSet<String>,
    ) -> Result<(), String>;
    fn on_group_registered(&mut self, jid: &str, group: &RegisteredGroup) -> Result<(), String>;
}

#[derive(Debug, Deserialize)]
struct RawMessageRequest {
    #[serde(rename = "type")]
    request_type: String,
    #[serde(alias = "chatJid")]
    chat_jid: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawTaskRequest {
    #[serde(rename = "type")]
    request_type: String,
    #[serde(alias = "taskId")]
    task_id: Option<String>,
    prompt: Option<String>,
    #[serde(alias = "schedule_type", alias = "scheduleType")]
    schedule_type: Option<String>,
    #[serde(alias = "schedule_value", alias = "scheduleValue")]
    schedule_value: Option<String>,
    #[serde(alias = "context_mode", alias = "contextMode")]
    context_mode: Option<String>,
    #[serde(alias = "chatJid")]
    chat_jid: Option<String>,
    #[serde(alias = "targetJid")]
    target_jid: Option<String>,
    jid: Option<String>,
    name: Option<String>,
    folder: Option<String>,
    trigger: Option<String>,
    #[serde(alias = "requiresTrigger")]
    requires_trigger: Option<bool>,
    #[serde(alias = "containerConfig")]
    container_config: Option<nanoclaw_core::types::ContainerConfig>,
}

pub fn process_task_ipc(ctx: &mut IpcContext<'_>, task: IpcTask) -> rusqlite::Result<IpcEvent> {
    match task {
        IpcTask::ScheduleTask {
            task_id,
            prompt,
            schedule_type,
            schedule_value,
            context_mode,
            target_jid,
        } => schedule_task(ctx, task_id, prompt, schedule_type, schedule_value, context_mode, target_jid),
        IpcTask::PauseTask { task_id } => {
            let event_task_id = task_id.clone();
            update_task_status(ctx, &task_id, TaskStatus::Paused, IpcEvent::TaskPaused { task_id: event_task_id })
        }
        IpcTask::ResumeTask { task_id } => {
            let event_task_id = task_id.clone();
            update_task_status(ctx, &task_id, TaskStatus::Active, IpcEvent::TaskResumed { task_id: event_task_id })
        }
        IpcTask::CancelTask { task_id } => cancel_task(ctx, &task_id),
        IpcTask::UpdateTask {
            task_id,
            prompt,
            schedule_type,
            schedule_value,
        } => update_task(ctx, &task_id, prompt, schedule_type, schedule_value),
        IpcTask::RefreshGroups => {
            if ctx.is_main {
                Ok(IpcEvent::GroupsRefreshRequested)
            } else {
                Ok(IpcEvent::Ignored)
            }
        }
        IpcTask::RegisterGroup {
            jid,
            name,
            folder,
            trigger,
            requires_trigger,
            container_config,
        } => register_group(
            ctx,
            jid,
            name,
            folder,
            trigger,
            requires_trigger,
            container_config,
        ),
    }
}

pub fn registered_group_jids(
    registered_groups: &HashMap<String, RegisteredGroup>,
) -> HashSet<String> {
    registered_groups.keys().cloned().collect()
}

pub fn write_groups_snapshot_payload(
    is_main: bool,
    available_groups: &[AvailableGroup],
    _registered_jids: &HashSet<String>,
) -> String {
    let groups = if is_main { available_groups.to_vec() } else { Vec::new() };
    serde_json::to_string_pretty(&serde_json::json!({
        "groups": groups,
        "lastSync": Utc::now().to_rfc3339(),
    }))
    .expect("snapshot serialization")
}

pub fn scan_ipc_once<D: IpcWatcherDeps>(
    data_dir: &Path,
    timezone: &str,
    db: &NanoClawDb,
    registered_groups: &mut HashMap<String, RegisteredGroup>,
    deps: &mut D,
) -> Result<IpcScanReport, String> {
    let ipc_base_dir = data_dir.join("ipc");
    fs::create_dir_all(&ipc_base_dir).map_err(|err| err.to_string())?;

    let mut group_folders = fs::read_dir(&ipc_base_dir)
        .map_err(|err| err.to_string())?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            let is_dir = entry.file_type().ok()?.is_dir();
            let name = path.file_name()?.to_str()?.to_string();
            if is_dir && name != "errors" {
                Some(name)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    group_folders.sort();

    let mut report = IpcScanReport::default();
    for source_group in group_folders {
        let is_main = registered_groups
            .values()
            .any(|group| group.folder == source_group && group.is_main == Some(true));
        let group_ipc_dir = ipc_base_dir.join(&source_group);

        process_message_files(
            &group_ipc_dir.join("messages"),
            &source_group,
            is_main,
            registered_groups,
            deps,
            &mut report,
        )?;
        process_task_files(
            &group_ipc_dir.join("tasks"),
            data_dir,
            timezone,
            db,
            &source_group,
            is_main,
            registered_groups,
            deps,
            &mut report,
        )?;
    }

    Ok(report)
}

pub fn run_ipc_watcher_loop<D, F>(
    data_dir: &Path,
    timezone: &str,
    db: &NanoClawDb,
    registered_groups: &mut HashMap<String, RegisteredGroup>,
    poll_interval: StdDuration,
    deps: &mut D,
    mut should_stop: F,
) -> Result<(), String>
where
    D: IpcWatcherDeps,
    F: FnMut() -> bool,
{
    while !should_stop() {
        scan_ipc_once(data_dir, timezone, db, registered_groups, deps)?;
        if should_stop() {
            break;
        }
        thread::sleep(poll_interval);
    }
    Ok(())
}

fn process_message_files<D: IpcWatcherDeps>(
    messages_dir: &Path,
    source_group: &str,
    is_main: bool,
    registered_groups: &HashMap<String, RegisteredGroup>,
    deps: &mut D,
    report: &mut IpcScanReport,
) -> Result<(), String> {
    for file_path in json_files(messages_dir)? {
        let file_name = file_name(&file_path);
        let raw = match fs::read_to_string(&file_path) {
            Ok(raw) => raw,
            Err(err) => {
                quarantine_file(messages_dir, source_group, &file_path)?;
                report.quarantined_files += 1;
                return Err(format!("failed to read {}: {err}", file_name));
            }
        };

        let request = match serde_json::from_str::<RawMessageRequest>(&raw) {
            Ok(request) => request,
            Err(_) => {
                quarantine_file(messages_dir, source_group, &file_path)?;
                report.quarantined_files += 1;
                continue;
            }
        };

        if request.request_type != "message" {
            quarantine_file(messages_dir, source_group, &file_path)?;
            report.quarantined_files += 1;
            continue;
        }

        let Some(chat_jid) = request.chat_jid else {
            quarantine_file(messages_dir, source_group, &file_path)?;
            report.quarantined_files += 1;
            continue;
        };
        let Some(text) = request.text else {
            quarantine_file(messages_dir, source_group, &file_path)?;
            report.quarantined_files += 1;
            continue;
        };

        let authorized = is_main
            || registered_groups
                .get(&chat_jid)
                .is_some_and(|group| group.folder == source_group);
        if authorized {
            if deps.send_message(&chat_jid, &text).is_err() {
                quarantine_file(messages_dir, source_group, &file_path)?;
                report.quarantined_files += 1;
                continue;
            }
            report.processed_messages += 1;
        } else {
            report.unauthorized_attempts += 1;
        }

        let _ = fs::remove_file(&file_path);
    }

    Ok(())
}

fn process_task_files<D: IpcWatcherDeps>(
    tasks_dir: &Path,
    data_dir: &Path,
    timezone: &str,
    db: &NanoClawDb,
    source_group: &str,
    is_main: bool,
    registered_groups: &mut HashMap<String, RegisteredGroup>,
    deps: &mut D,
    report: &mut IpcScanReport,
) -> Result<(), String> {
    for file_path in json_files(tasks_dir)? {
        let raw = match fs::read_to_string(&file_path) {
            Ok(raw) => raw,
            Err(_) => {
                quarantine_file(tasks_dir, source_group, &file_path)?;
                report.quarantined_files += 1;
                continue;
            }
        };

        let request = match serde_json::from_str::<RawTaskRequest>(&raw) {
            Ok(request) => request,
            Err(_) => {
                quarantine_file(tasks_dir, source_group, &file_path)?;
                report.quarantined_files += 1;
                continue;
            }
        };

        let task = match request.into_ipc_task() {
            Ok(task) => task,
            Err(_) => {
                quarantine_file(tasks_dir, source_group, &file_path)?;
                report.quarantined_files += 1;
                continue;
            }
        };

        let event = {
            let mut ctx = IpcContext {
                db,
                source_group,
                is_main,
                timezone,
                registered_groups,
            };
            process_task_ipc(&mut ctx, task).map_err(|err| err.to_string())?
        };

        match event {
            IpcEvent::GroupsRefreshRequested => {
                deps.sync_groups(true)?;
                let available_groups = deps.get_available_groups();
                let registered_jids = registered_group_jids(registered_groups);
                deps.write_groups_snapshot(
                    source_group,
                    true,
                    &available_groups,
                    &registered_jids,
                )?;
            }
            IpcEvent::GroupRegistered { jid, .. } => {
                if let Some(group) = registered_groups.get(&jid).cloned() {
                    deps.on_group_registered(&jid, &group)?;
                    let group_ipc_path = data_dir.join("ipc").join(&group.folder);
                    fs::create_dir_all(group_ipc_path.join("messages"))
                        .map_err(|err| err.to_string())?;
                    fs::create_dir_all(group_ipc_path.join("tasks"))
                        .map_err(|err| err.to_string())?;
                    fs::create_dir_all(group_ipc_path.join("input"))
                        .map_err(|err| err.to_string())?;
                }
            }
            IpcEvent::Ignored => {
                report.unauthorized_attempts += 1;
            }
            _ => {}
        }

        report.processed_tasks += 1;
        let _ = fs::remove_file(&file_path);
    }

    Ok(())
}

fn json_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = fs::read_dir(dir)
        .map_err(|err| err.to_string())?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

fn quarantine_file(source_dir: &Path, source_group: &str, file_path: &Path) -> Result<(), String> {
    let errors_dir = source_dir
        .parent()
        .map(|parent| parent.parent().unwrap_or(parent))
        .unwrap_or(source_dir)
        .join("errors");
    fs::create_dir_all(&errors_dir).map_err(|err| err.to_string())?;
    let file_name = file_name(file_path);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| err.to_string())?
        .as_nanos();
    let target = errors_dir.join(format!("{source_group}-{nanos}-{file_name}"));
    fs::rename(file_path, target).map_err(|err| err.to_string())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown.json")
        .to_string()
}

impl RawTaskRequest {
    fn into_ipc_task(self) -> Result<IpcTask, String> {
        match self.request_type.as_str() {
            "schedule_task" => Ok(IpcTask::ScheduleTask {
                task_id: self.task_id,
                prompt: self.prompt.ok_or_else(|| "missing prompt".to_string())?,
                schedule_type: parse_schedule_type(self.schedule_type.as_deref())?,
                schedule_value: self
                    .schedule_value
                    .ok_or_else(|| "missing schedule_value".to_string())?,
                context_mode: parse_context_mode(self.context_mode.as_deref())?,
                target_jid: self
                    .target_jid
                    .or(self.chat_jid)
                    .ok_or_else(|| "missing target_jid".to_string())?,
            }),
            "pause_task" => Ok(IpcTask::PauseTask {
                task_id: self.task_id.ok_or_else(|| "missing task_id".to_string())?,
            }),
            "resume_task" => Ok(IpcTask::ResumeTask {
                task_id: self.task_id.ok_or_else(|| "missing task_id".to_string())?,
            }),
            "cancel_task" => Ok(IpcTask::CancelTask {
                task_id: self.task_id.ok_or_else(|| "missing task_id".to_string())?,
            }),
            "update_task" => Ok(IpcTask::UpdateTask {
                task_id: self.task_id.ok_or_else(|| "missing task_id".to_string())?,
                prompt: self.prompt,
                schedule_type: match self.schedule_type {
                    Some(value) => Some(parse_schedule_type(Some(&value))?),
                    None => None,
                },
                schedule_value: self.schedule_value,
            }),
            "refresh_groups" => Ok(IpcTask::RefreshGroups),
            "register_group" => Ok(IpcTask::RegisterGroup {
                jid: self.jid.ok_or_else(|| "missing jid".to_string())?,
                name: self.name.ok_or_else(|| "missing name".to_string())?,
                folder: self.folder.ok_or_else(|| "missing folder".to_string())?,
                trigger: self.trigger.ok_or_else(|| "missing trigger".to_string())?,
                requires_trigger: self.requires_trigger,
                container_config: self.container_config,
            }),
            other => Err(format!("unknown task type: {other}")),
        }
    }
}

fn parse_schedule_type(value: Option<&str>) -> Result<ScheduleType, String> {
    match value {
        Some("cron") => Ok(ScheduleType::Cron),
        Some("interval") => Ok(ScheduleType::Interval),
        Some("once") => Ok(ScheduleType::Once),
        Some(other) => Err(format!("invalid schedule type: {other}")),
        None => Err("missing schedule type".to_string()),
    }
}

fn parse_context_mode(value: Option<&str>) -> Result<ContextMode, String> {
    match value.unwrap_or("isolated") {
        "group" => Ok(ContextMode::Group),
        "isolated" => Ok(ContextMode::Isolated),
        other => Err(format!("invalid context mode: {other}")),
    }
}

fn schedule_task(
    ctx: &mut IpcContext<'_>,
    task_id: Option<String>,
    prompt: String,
    schedule_type: ScheduleType,
    schedule_value: String,
    context_mode: ContextMode,
    target_jid: String,
) -> rusqlite::Result<IpcEvent> {
    let Some(target_group) = ctx.registered_groups.get(&target_jid) else {
        return Ok(IpcEvent::Ignored);
    };
    if !ctx.is_main && target_group.folder != ctx.source_group {
        return Ok(IpcEvent::Ignored);
    }

    let next_run = initial_next_run(&schedule_type, &schedule_value, ctx.timezone)?;
    let task_id = task_id.unwrap_or_else(|| format!("task-{}", Utc::now().timestamp_millis()));
    let task = ScheduledTask {
        id: task_id.clone(),
        group_folder: target_group.folder.clone(),
        chat_jid: target_jid,
        prompt,
        schedule_type,
        schedule_value,
        context_mode,
        next_run,
        last_run: None,
        last_result: None,
        status: TaskStatus::Active,
        created_at: Utc::now().to_rfc3339(),
    };
    ctx.db.create_task(&task)?;
    Ok(IpcEvent::TaskCreated {
        task_id,
        target_folder: task.group_folder,
    })
}

fn update_task_status(
    ctx: &mut IpcContext<'_>,
    task_id: &str,
    status: TaskStatus,
    success_event: IpcEvent,
) -> rusqlite::Result<IpcEvent> {
    let Some(task) = ctx.db.get_task_by_id(task_id)? else {
        return Ok(IpcEvent::Ignored);
    };
    if !ctx.is_main && task.group_folder != ctx.source_group {
        return Ok(IpcEvent::Ignored);
    }
    ctx.db.update_task(
        task_id,
        &TaskUpdate {
            prompt: None,
            schedule_type: None,
            schedule_value: None,
            next_run: None,
            status: Some(status),
        },
    )?;
    Ok(success_event)
}

fn cancel_task(ctx: &mut IpcContext<'_>, task_id: &str) -> rusqlite::Result<IpcEvent> {
    let Some(task) = ctx.db.get_task_by_id(task_id)? else {
        return Ok(IpcEvent::Ignored);
    };
    if !ctx.is_main && task.group_folder != ctx.source_group {
        return Ok(IpcEvent::Ignored);
    }
    ctx.db.delete_task(task_id)?;
    Ok(IpcEvent::TaskCancelled {
        task_id: task_id.to_string(),
    })
}

fn update_task(
    ctx: &mut IpcContext<'_>,
    task_id: &str,
    prompt: Option<String>,
    schedule_type: Option<ScheduleType>,
    schedule_value: Option<String>,
) -> rusqlite::Result<IpcEvent> {
    let Some(task) = ctx.db.get_task_by_id(task_id)? else {
        return Ok(IpcEvent::Ignored);
    };
    if !ctx.is_main && task.group_folder != ctx.source_group {
        return Ok(IpcEvent::Ignored);
    }

    let effective_schedule_type = schedule_type.unwrap_or(task.schedule_type);
    let effective_schedule_value = schedule_value
        .clone()
        .unwrap_or_else(|| task.schedule_value.clone());
    let next_run = if schedule_type.is_some() || schedule_value.is_some() {
        initial_next_run(&effective_schedule_type, &effective_schedule_value, ctx.timezone)?
    } else {
        None
    };

    ctx.db.update_task(
        task_id,
        &TaskUpdate {
            prompt,
            schedule_type,
            schedule_value,
            next_run,
            status: None,
        },
    )?;
    Ok(IpcEvent::TaskUpdated {
        task_id: task_id.to_string(),
    })
}

fn register_group(
    ctx: &mut IpcContext<'_>,
    jid: String,
    name: String,
    folder: String,
    trigger: String,
    requires_trigger: Option<bool>,
    container_config: Option<nanoclaw_core::types::ContainerConfig>,
) -> rusqlite::Result<IpcEvent> {
    if !ctx.is_main || !is_valid_group_folder(&folder) {
        return Ok(IpcEvent::Ignored);
    }
    let group = RegisteredGroup {
        name,
        folder: folder.clone(),
        trigger,
        added_at: Utc::now().to_rfc3339(),
        container_config,
        requires_trigger,
        is_main: None,
    };
    ctx.db.set_registered_group(&jid, &group)?;
    ctx.registered_groups.insert(jid.clone(), group);
    Ok(IpcEvent::GroupRegistered { jid, folder })
}

fn initial_next_run(
    schedule_type: &ScheduleType,
    schedule_value: &str,
    timezone: &str,
) -> rusqlite::Result<Option<String>> {
    match schedule_type {
        ScheduleType::Once => chrono::DateTime::parse_from_rfc3339(schedule_value)
            .map(|value: DateTime<chrono::FixedOffset>| Some(value.with_timezone(&Utc).to_rfc3339()))
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into())),
        ScheduleType::Interval => {
            let ms = schedule_value
                .parse::<i64>()
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
            if ms <= 0 {
                return Err(rusqlite::Error::ToSqlConversionFailure(
                    "invalid interval".into(),
                ));
            }
            Ok(Some((Utc::now() + Duration::milliseconds(ms)).to_rfc3339()))
        }
        ScheduleType::Cron => {
            let seed = ScheduledTask {
                id: "seed".to_string(),
                group_folder: "seed".to_string(),
                chat_jid: "seed".to_string(),
                prompt: "seed".to_string(),
                schedule_type: *schedule_type,
                schedule_value: schedule_value.to_string(),
                context_mode: ContextMode::Isolated,
                next_run: None,
                last_run: None,
                last_result: None,
                status: TaskStatus::Active,
                created_at: Utc::now().to_rfc3339(),
            };
            let next = compute_next_run_at(&seed, timezone, Utc::now())
                .ok_or_else(|| rusqlite::Error::ToSqlConversionFailure("invalid cron".into()))?;
            Ok(Some(next))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanoclaw_core::types::ContainerConfig;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Default)]
    struct TestDeps {
        sent_messages: Vec<(String, String)>,
        sync_calls: usize,
        snapshot_calls: Vec<(String, bool, Vec<AvailableGroup>)>,
        registered_calls: Vec<(String, String)>,
    }

    impl IpcWatcherDeps for TestDeps {
        fn send_message(&mut self, jid: &str, text: &str) -> Result<(), String> {
            self.sent_messages.push((jid.to_string(), text.to_string()));
            Ok(())
        }

        fn sync_groups(&mut self, force: bool) -> Result<(), String> {
            if force {
                self.sync_calls += 1;
            }
            Ok(())
        }

        fn get_available_groups(&self) -> Vec<AvailableGroup> {
            vec![AvailableGroup {
                jid: "other@g.us".to_string(),
                name: "Other".to_string(),
                last_activity: "2026-03-11T00:00:00Z".to_string(),
                is_registered: true,
            }]
        }

        fn write_groups_snapshot(
            &mut self,
            group_folder: &str,
            is_main: bool,
            available_groups: &[AvailableGroup],
            _registered_jids: &HashSet<String>,
        ) -> Result<(), String> {
            self.snapshot_calls.push((
                group_folder.to_string(),
                is_main,
                available_groups.to_vec(),
            ));
            Ok(())
        }

        fn on_group_registered(
            &mut self,
            jid: &str,
            group: &RegisteredGroup,
        ) -> Result<(), String> {
            self.registered_calls
                .push((jid.to_string(), group.folder.clone()));
            Ok(())
        }
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "nanoclaw-ipc-{label}-{nanos}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn groups() -> HashMap<String, RegisteredGroup> {
        HashMap::from([
            (
                "main@g.us".to_string(),
                RegisteredGroup {
                    name: "Main".to_string(),
                    folder: "whatsapp_main".to_string(),
                    trigger: "always".to_string(),
                    added_at: "2024-01-01T00:00:00.000Z".to_string(),
                    container_config: None,
                    requires_trigger: None,
                    is_main: Some(true),
                },
            ),
            (
                "other@g.us".to_string(),
                RegisteredGroup {
                    name: "Other".to_string(),
                    folder: "other-group".to_string(),
                    trigger: "@Andy".to_string(),
                    added_at: "2024-01-01T00:00:00.000Z".to_string(),
                    container_config: None,
                    requires_trigger: None,
                    is_main: None,
                },
            ),
            (
                "third@g.us".to_string(),
                RegisteredGroup {
                    name: "Third".to_string(),
                    folder: "third-group".to_string(),
                    trigger: "@Andy".to_string(),
                    added_at: "2024-01-01T00:00:00.000Z".to_string(),
                    container_config: None,
                    requires_trigger: None,
                    is_main: None,
                },
            ),
        ])
    }

    #[test]
    fn main_group_can_schedule_for_other_group() {
        let db = NanoClawDb::open_in_memory().expect("db");
        let mut groups = groups();
        for (jid, group) in &groups {
            db.set_registered_group(jid, group).expect("group");
        }
        let mut ctx = IpcContext {
            db: &db,
            source_group: "whatsapp_main",
            is_main: true,
            timezone: "UTC",
            registered_groups: &mut groups,
        };

        let event = process_task_ipc(
            &mut ctx,
            IpcTask::ScheduleTask {
                task_id: Some("task-1".to_string()),
                prompt: "do something".to_string(),
                schedule_type: ScheduleType::Once,
                schedule_value: "2026-06-01T00:00:00+00:00".to_string(),
                context_mode: ContextMode::Isolated,
                target_jid: "other@g.us".to_string(),
            },
        )
        .expect("schedule");

        assert_eq!(
            event,
            IpcEvent::TaskCreated {
                task_id: "task-1".to_string(),
                target_folder: "other-group".to_string(),
            }
        );
    }

    #[test]
    fn non_main_group_cannot_schedule_for_another_group() {
        let db = NanoClawDb::open_in_memory().expect("db");
        let mut groups = groups();
        for (jid, group) in &groups {
            db.set_registered_group(jid, group).expect("group");
        }
        let mut ctx = IpcContext {
            db: &db,
            source_group: "other-group",
            is_main: false,
            timezone: "UTC",
            registered_groups: &mut groups,
        };

        let event = process_task_ipc(
            &mut ctx,
            IpcTask::ScheduleTask {
                task_id: Some("task-1".to_string()),
                prompt: "nope".to_string(),
                schedule_type: ScheduleType::Once,
                schedule_value: "2026-06-01T00:00:00+00:00".to_string(),
                context_mode: ContextMode::Isolated,
                target_jid: "main@g.us".to_string(),
            },
        )
        .expect("schedule");

        assert_eq!(event, IpcEvent::Ignored);
        assert!(db.get_all_tasks().expect("tasks").is_empty());
    }

    #[test]
    fn pause_resume_cancel_are_authorized_by_group() {
        let db = NanoClawDb::open_in_memory().expect("db");
        let mut groups = groups();
        for (jid, group) in &groups {
            db.set_registered_group(jid, group).expect("group");
        }
        db.create_task(&ScheduledTask {
            id: "task-1".to_string(),
            group_folder: "other-group".to_string(),
            chat_jid: "other@g.us".to_string(),
            prompt: "test".to_string(),
            schedule_type: ScheduleType::Once,
            schedule_value: "2026-06-01T00:00:00+00:00".to_string(),
            context_mode: ContextMode::Isolated,
            next_run: Some("2026-06-01T00:00:00+00:00".to_string()),
            last_run: None,
            last_result: None,
            status: TaskStatus::Active,
            created_at: "2024-01-01T00:00:00.000Z".to_string(),
        })
        .expect("task");

        let mut ctx = IpcContext {
            db: &db,
            source_group: "other-group",
            is_main: false,
            timezone: "UTC",
            registered_groups: &mut groups,
        };

        assert_eq!(
            process_task_ipc(&mut ctx, IpcTask::PauseTask { task_id: "task-1".to_string() })
                .expect("pause"),
            IpcEvent::TaskPaused {
                task_id: "task-1".to_string()
            }
        );
        assert_eq!(db.get_task_by_id("task-1").expect("get").expect("task").status, TaskStatus::Paused);

        assert_eq!(
            process_task_ipc(&mut ctx, IpcTask::ResumeTask { task_id: "task-1".to_string() })
                .expect("resume"),
            IpcEvent::TaskResumed {
                task_id: "task-1".to_string()
            }
        );

        assert_eq!(
            process_task_ipc(&mut ctx, IpcTask::CancelTask { task_id: "task-1".to_string() })
                .expect("cancel"),
            IpcEvent::TaskCancelled {
                task_id: "task-1".to_string()
            }
        );
        assert_eq!(db.get_task_by_id("task-1").expect("get"), None);
    }

    #[test]
    fn only_main_group_can_register_group() {
        let db = NanoClawDb::open_in_memory().expect("db");
        let mut groups = groups();
        for (jid, group) in &groups {
            db.set_registered_group(jid, group).expect("group");
        }

        let mut non_main_ctx = IpcContext {
            db: &db,
            source_group: "other-group",
            is_main: false,
            timezone: "UTC",
            registered_groups: &mut groups,
        };
        assert_eq!(
            process_task_ipc(
                &mut non_main_ctx,
                IpcTask::RegisterGroup {
                    jid: "new@g.us".to_string(),
                    name: "New".to_string(),
                    folder: "new-group".to_string(),
                    trigger: "@Andy".to_string(),
                    requires_trigger: Some(true),
                    container_config: Some(ContainerConfig {
                        additional_mounts: vec![],
                        timeout_ms: Some(1000),
                    }),
                }
            )
            .expect("register"),
            IpcEvent::Ignored
        );

        let mut main_ctx = IpcContext {
            db: &db,
            source_group: "whatsapp_main",
            is_main: true,
            timezone: "UTC",
            registered_groups: &mut groups,
        };
        assert_eq!(
            process_task_ipc(
                &mut main_ctx,
                IpcTask::RegisterGroup {
                    jid: "new@g.us".to_string(),
                    name: "New".to_string(),
                    folder: "new-group".to_string(),
                    trigger: "@Andy".to_string(),
                    requires_trigger: Some(true),
                    container_config: None,
                }
            )
            .expect("register"),
            IpcEvent::GroupRegistered {
                jid: "new@g.us".to_string(),
                folder: "new-group".to_string(),
            }
        );
    }

    #[test]
    fn scan_ipc_once_processes_authorized_message_file() {
        let db = NanoClawDb::open_in_memory().expect("db");
        let mut groups = groups();
        let data_dir = unique_temp_dir("message-ok");
        let messages_dir = data_dir.join("ipc").join("other-group").join("messages");
        fs::create_dir_all(&messages_dir).expect("messages");
        fs::write(
            messages_dir.join("1.json"),
            r#"{"type":"message","chatJid":"other@g.us","text":"hello"}"#,
        )
        .expect("message");

        let mut deps = TestDeps::default();
        let report = scan_ipc_once(&data_dir, "UTC", &db, &mut groups, &mut deps).expect("scan");

        assert_eq!(report.processed_messages, 1);
        assert_eq!(
            deps.sent_messages,
            vec![("other@g.us".to_string(), "hello".to_string())]
        );
        assert!(fs::read_dir(&messages_dir).expect("dir").next().is_none());
    }

    #[test]
    fn scan_ipc_once_quarantines_invalid_files() {
        let db = NanoClawDb::open_in_memory().expect("db");
        let mut groups = groups();
        let data_dir = unique_temp_dir("message-bad");
        let messages_dir = data_dir.join("ipc").join("other-group").join("messages");
        fs::create_dir_all(&messages_dir).expect("messages");
        fs::write(messages_dir.join("bad.json"), "{not-json").expect("message");

        let mut deps = TestDeps::default();
        let report = scan_ipc_once(&data_dir, "UTC", &db, &mut groups, &mut deps).expect("scan");

        assert_eq!(report.quarantined_files, 1);
        assert_eq!(fs::read_dir(data_dir.join("ipc").join("errors")).expect("errors").count(), 1);
    }

    #[test]
    fn scan_ipc_once_refreshes_groups_for_main_group() {
        let db = NanoClawDb::open_in_memory().expect("db");
        let mut groups = groups();
        let data_dir = unique_temp_dir("refresh");
        let tasks_dir = data_dir.join("ipc").join("whatsapp_main").join("tasks");
        fs::create_dir_all(&tasks_dir).expect("tasks");
        fs::write(tasks_dir.join("1.json"), r#"{"type":"refresh_groups"}"#).expect("task");

        let mut deps = TestDeps::default();
        let report = scan_ipc_once(&data_dir, "UTC", &db, &mut groups, &mut deps).expect("scan");

        assert_eq!(report.processed_tasks, 1);
        assert_eq!(deps.sync_calls, 1);
        assert_eq!(deps.snapshot_calls.len(), 1);
        assert_eq!(deps.snapshot_calls[0].0, "whatsapp_main");
        assert!(deps.snapshot_calls[0].1);
    }

    #[test]
    fn scan_ipc_once_registers_group_and_creates_ipc_dirs() {
        let db = NanoClawDb::open_in_memory().expect("db");
        let mut groups = groups();
        let data_dir = unique_temp_dir("register");
        let tasks_dir = data_dir.join("ipc").join("whatsapp_main").join("tasks");
        fs::create_dir_all(&tasks_dir).expect("tasks");
        fs::write(
            tasks_dir.join("1.json"),
            r#"{"type":"register_group","jid":"new@g.us","name":"New","folder":"new-group","trigger":"@Andy"}"#,
        )
        .expect("task");

        let mut deps = TestDeps::default();
        let report = scan_ipc_once(&data_dir, "UTC", &db, &mut groups, &mut deps).expect("scan");

        assert_eq!(report.processed_tasks, 1);
        assert_eq!(
            deps.registered_calls,
            vec![("new@g.us".to_string(), "new-group".to_string())]
        );
        assert!(groups.contains_key("new@g.us"));
        assert!(data_dir.join("ipc").join("new-group").join("messages").exists());
        assert!(db.get_registered_group("new@g.us").expect("db").is_some());
    }
}
