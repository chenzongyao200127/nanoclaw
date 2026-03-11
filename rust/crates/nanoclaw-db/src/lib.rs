use std::collections::HashMap;
use std::fs;
use std::path::Path;

use nanoclaw_core::group_folder::is_valid_group_folder;
use nanoclaw_core::types::{
    ContainerConfig, ContextMode, NewMessage, RegisteredGroup, ScheduleType, ScheduledTask,
    TaskRunLog, TaskStatus, TaskUpdate,
};
use rusqlite::{Connection, OptionalExtension, Result, params, params_from_iter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaTable {
    Chats,
    Messages,
    ScheduledTasks,
    TaskRunLogs,
    RouterState,
    Sessions,
    RegisteredGroups,
}

impl SchemaTable {
    pub fn name(self) -> &'static str {
        match self {
            Self::Chats => "chats",
            Self::Messages => "messages",
            Self::ScheduledTasks => "scheduled_tasks",
            Self::TaskRunLogs => "task_run_logs",
            Self::RouterState => "router_state",
            Self::Sessions => "sessions",
            Self::RegisteredGroups => "registered_groups",
        }
    }
}

pub fn planned_tables() -> &'static [SchemaTable] {
    &[
        SchemaTable::Chats,
        SchemaTable::Messages,
        SchemaTable::ScheduledTasks,
        SchemaTable::TaskRunLogs,
        SchemaTable::RouterState,
        SchemaTable::Sessions,
        SchemaTable::RegisteredGroups,
    ]
}

pub struct NanoClawDb {
    conn: Connection,
}

impl NanoClawDb {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
        }

        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS chats (
              jid TEXT PRIMARY KEY,
              name TEXT,
              last_message_time TEXT,
              channel TEXT,
              is_group INTEGER DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS messages (
              id TEXT,
              chat_jid TEXT,
              sender TEXT,
              sender_name TEXT,
              content TEXT,
              timestamp TEXT,
              is_from_me INTEGER,
              is_bot_message INTEGER DEFAULT 0,
              PRIMARY KEY (id, chat_jid),
              FOREIGN KEY (chat_jid) REFERENCES chats(jid)
            );
            CREATE INDEX IF NOT EXISTS idx_timestamp ON messages(timestamp);

            CREATE TABLE IF NOT EXISTS scheduled_tasks (
              id TEXT PRIMARY KEY,
              group_folder TEXT NOT NULL,
              chat_jid TEXT NOT NULL,
              prompt TEXT NOT NULL,
              schedule_type TEXT NOT NULL,
              schedule_value TEXT NOT NULL,
              next_run TEXT,
              last_run TEXT,
              last_result TEXT,
              status TEXT DEFAULT 'active',
              created_at TEXT NOT NULL,
              context_mode TEXT DEFAULT 'isolated'
            );
            CREATE INDEX IF NOT EXISTS idx_next_run ON scheduled_tasks(next_run);
            CREATE INDEX IF NOT EXISTS idx_status ON scheduled_tasks(status);

            CREATE TABLE IF NOT EXISTS task_run_logs (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              task_id TEXT NOT NULL,
              run_at TEXT NOT NULL,
              duration_ms INTEGER NOT NULL,
              status TEXT NOT NULL,
              result TEXT,
              error TEXT,
              FOREIGN KEY (task_id) REFERENCES scheduled_tasks(id)
            );
            CREATE INDEX IF NOT EXISTS idx_task_run_logs ON task_run_logs(task_id, run_at);

            CREATE TABLE IF NOT EXISTS router_state (
              key TEXT PRIMARY KEY,
              value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sessions (
              group_folder TEXT PRIMARY KEY,
              session_id TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS registered_groups (
              jid TEXT PRIMARY KEY,
              name TEXT NOT NULL,
              folder TEXT NOT NULL UNIQUE,
              trigger_pattern TEXT NOT NULL,
              added_at TEXT NOT NULL,
              container_config TEXT,
              requires_trigger INTEGER DEFAULT 1,
              is_main INTEGER DEFAULT 0
            );
            "#,
        )?;
        Ok(())
    }

    pub fn store_chat_metadata(
        &self,
        chat_jid: &str,
        timestamp: &str,
        name: Option<&str>,
        channel: Option<&str>,
        is_group: Option<bool>,
    ) -> Result<()> {
        let is_group = is_group.map(bool_to_sql);
        let name = name.unwrap_or(chat_jid);
        self.conn.execute(
            r#"
            INSERT INTO chats (jid, name, last_message_time, channel, is_group)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(jid) DO UPDATE SET
              name = excluded.name,
              last_message_time = MAX(last_message_time, excluded.last_message_time),
              channel = COALESCE(excluded.channel, channel),
              is_group = COALESCE(excluded.is_group, is_group)
            "#,
            params![chat_jid, name, timestamp, channel, is_group],
        )?;
        Ok(())
    }

    pub fn update_chat_name(&self, chat_jid: &str, name: &str) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO chats (jid, name, last_message_time)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(jid) DO UPDATE SET
              name = excluded.name
            "#,
            params![chat_jid, name, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_all_chats(&self) -> Result<Vec<nanoclaw_core::types::ChatInfo>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT jid, name, last_message_time, channel, is_group
            FROM chats
            ORDER BY last_message_time DESC
            "#,
        )?;
        let rows = stmt.query_map([], row_to_chat_info)?;
        rows.collect()
    }

    pub fn get_last_group_sync(&self) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT last_message_time FROM chats WHERE jid = '__group_sync__'",
                [],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn set_last_group_sync(&self) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR REPLACE INTO chats (jid, name, last_message_time) VALUES ('__group_sync__', '__group_sync__', ?1)",
            params![now],
        )?;
        Ok(())
    }

    pub fn store_message(&self, msg: &NewMessage) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT OR REPLACE INTO messages
            (id, chat_jid, sender, sender_name, content, timestamp, is_from_me, is_bot_message)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                msg.id,
                msg.chat_jid,
                msg.sender,
                msg.sender_name,
                msg.content,
                msg.timestamp,
                msg.is_from_me.map(bool_to_sql).unwrap_or(0),
                msg.is_bot_message.map(bool_to_sql).unwrap_or(0)
            ],
        )?;
        Ok(())
    }

    pub fn get_new_messages(
        &self,
        jids: &[String],
        last_timestamp: &str,
        bot_prefix: &str,
        limit: usize,
    ) -> Result<(Vec<NewMessage>, String)> {
        if jids.is_empty() {
            return Ok((Vec::new(), last_timestamp.to_string()));
        }

        let placeholders = std::iter::repeat_n("?", jids.len()).collect::<Vec<_>>().join(",");
        let sql = format!(
            r#"
            SELECT * FROM (
              SELECT id, chat_jid, sender, sender_name, content, timestamp, is_from_me, is_bot_message
              FROM messages
              WHERE timestamp > ? AND chat_jid IN ({placeholders})
                AND is_bot_message = 0 AND content NOT LIKE ?
                AND content != '' AND content IS NOT NULL
              ORDER BY timestamp DESC
              LIMIT ?
            ) ORDER BY timestamp
            "#
        );

        let mut params = Vec::with_capacity(3 + jids.len());
        params.push(rusqlite::types::Value::from(last_timestamp.to_string()));
        params.extend(jids.iter().cloned().map(rusqlite::types::Value::from));
        params.push(rusqlite::types::Value::from(format!("{bot_prefix}:%")));
        params.push(rusqlite::types::Value::from(limit as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params.iter()), row_to_new_message)?;
        let messages = rows.collect::<Result<Vec<_>>>()?;
        let new_timestamp = messages
            .last()
            .map(|message| message.timestamp.clone())
            .unwrap_or_else(|| last_timestamp.to_string());
        Ok((messages, new_timestamp))
    }

    pub fn get_messages_since(
        &self,
        chat_jid: &str,
        since_timestamp: &str,
        bot_prefix: &str,
        limit: usize,
    ) -> Result<Vec<NewMessage>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT * FROM (
              SELECT id, chat_jid, sender, sender_name, content, timestamp, is_from_me, is_bot_message
              FROM messages
              WHERE chat_jid = ?1 AND timestamp > ?2
                AND is_bot_message = 0 AND content NOT LIKE ?3
                AND content != '' AND content IS NOT NULL
              ORDER BY timestamp DESC
              LIMIT ?4
            ) ORDER BY timestamp
            "#,
        )?;
        let rows = stmt.query_map(
            params![chat_jid, since_timestamp, format!("{bot_prefix}:%"), limit as i64],
            row_to_new_message,
        )?;
        rows.collect()
    }

    pub fn create_task(&self, task: &ScheduledTask) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO scheduled_tasks
            (id, group_folder, chat_jid, prompt, schedule_type, schedule_value, context_mode, next_run, last_run, last_result, status, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
            params![
                task.id,
                task.group_folder,
                task.chat_jid,
                task.prompt,
                enum_label_schedule_type(&task.schedule_type),
                task.schedule_value,
                enum_label_context_mode(&task.context_mode),
                task.next_run,
                task.last_run,
                task.last_result,
                enum_label_task_status(&task.status),
                task.created_at
            ],
        )?;
        Ok(())
    }

    pub fn get_task_by_id(&self, id: &str) -> Result<Option<ScheduledTask>> {
        self.conn
            .query_row(
                r#"
                SELECT id, group_folder, chat_jid, prompt, schedule_type, schedule_value, context_mode,
                       next_run, last_run, last_result, status, created_at
                FROM scheduled_tasks
                WHERE id = ?1
                "#,
                params![id],
                |row| row_to_scheduled_task(row),
            )
            .optional()
    }

    pub fn get_all_tasks(&self) -> Result<Vec<ScheduledTask>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, group_folder, chat_jid, prompt, schedule_type, schedule_value, context_mode,
                   next_run, last_run, last_result, status, created_at
            FROM scheduled_tasks
            ORDER BY created_at DESC
            "#,
        )?;
        let rows = stmt.query_map([], row_to_scheduled_task)?;
        rows.collect()
    }

    pub fn get_due_tasks(&self, now_iso: &str) -> Result<Vec<ScheduledTask>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, group_folder, chat_jid, prompt, schedule_type, schedule_value, context_mode,
                   next_run, last_run, last_result, status, created_at
            FROM scheduled_tasks
            WHERE status = 'active' AND next_run IS NOT NULL AND next_run <= ?1
            ORDER BY next_run
            "#,
        )?;
        let rows = stmt.query_map(params![now_iso], row_to_scheduled_task)?;
        rows.collect()
    }

    pub fn update_task(&self, id: &str, updates: &TaskUpdate) -> Result<()> {
        if let Some(prompt) = &updates.prompt {
            self.conn.execute(
                "UPDATE scheduled_tasks SET prompt = ?1 WHERE id = ?2",
                params![prompt, id],
            )?;
        }
        if let Some(schedule_type) = &updates.schedule_type {
            self.conn.execute(
                "UPDATE scheduled_tasks SET schedule_type = ?1 WHERE id = ?2",
                params![enum_label_schedule_type(schedule_type), id],
            )?;
        }
        if let Some(schedule_value) = &updates.schedule_value {
            self.conn.execute(
                "UPDATE scheduled_tasks SET schedule_value = ?1 WHERE id = ?2",
                params![schedule_value, id],
            )?;
        }
        if let Some(next_run) = &updates.next_run {
            self.conn.execute(
                "UPDATE scheduled_tasks SET next_run = ?1 WHERE id = ?2",
                params![next_run, id],
            )?;
        }
        if let Some(status) = &updates.status {
            self.conn.execute(
                "UPDATE scheduled_tasks SET status = ?1 WHERE id = ?2",
                params![enum_label_task_status(status), id],
            )?;
        }
        Ok(())
    }

    pub fn delete_task(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM task_run_logs WHERE task_id = ?1", params![id])?;
        self.conn
            .execute("DELETE FROM scheduled_tasks WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn update_task_after_run(&self, id: &str, next_run: Option<&str>, last_result: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let status = if next_run.is_none() { "completed" } else { "active" };
        self.conn.execute(
            r#"
            UPDATE scheduled_tasks
            SET next_run = ?1, last_run = ?2, last_result = ?3, status = ?4
            WHERE id = ?5
            "#,
            params![next_run, now, last_result, status, id],
        )?;
        Ok(())
    }

    pub fn log_task_run(&self, log: &TaskRunLog) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO task_run_logs (task_id, run_at, duration_ms, status, result, error)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                log.task_id,
                log.run_at,
                log.duration_ms,
                enum_label_run_status(&log.status),
                log.result,
                log.error
            ],
        )?;
        Ok(())
    }

    pub fn get_router_state(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM router_state WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn set_router_state(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO router_state (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_session(&self, group_folder: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT session_id FROM sessions WHERE group_folder = ?1",
                params![group_folder],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn set_session(&self, group_folder: &str, session_id: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO sessions (group_folder, session_id) VALUES (?1, ?2)",
            params![group_folder, session_id],
        )?;
        Ok(())
    }

    pub fn get_all_sessions(&self) -> Result<HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT group_folder, session_id FROM sessions")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut sessions = HashMap::new();
        for row in rows {
            let (group_folder, session_id) = row?;
            sessions.insert(group_folder, session_id);
        }
        Ok(sessions)
    }

    pub fn set_registered_group(&self, jid: &str, group: &RegisteredGroup) -> Result<()> {
        let container_config = group
            .container_config
            .as_ref()
            .map(serialize_container_config)
            .transpose()
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;

        self.conn.execute(
            r#"
            INSERT OR REPLACE INTO registered_groups
            (jid, name, folder, trigger_pattern, added_at, container_config, requires_trigger, is_main)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                jid,
                group.name,
                group.folder,
                group.trigger,
                group.added_at,
                container_config,
                group.requires_trigger.map(bool_to_sql).unwrap_or(1),
                group.is_main.map(bool_to_sql).unwrap_or(0)
            ],
        )?;
        Ok(())
    }

    pub fn get_registered_group(&self, jid: &str) -> Result<Option<RegisteredGroup>> {
        self.conn
            .query_row(
                r#"
                SELECT name, folder, trigger_pattern, added_at, container_config, requires_trigger, is_main
                FROM registered_groups
                WHERE jid = ?1
                "#,
                params![jid],
                row_to_registered_group,
            )
            .optional()
            .map(|group| {
                group.and_then(|group| {
                    if is_valid_group_folder(&group.folder) {
                        Some(group)
                    } else {
                        None
                    }
                })
            })
    }

    pub fn get_all_registered_groups(&self) -> Result<HashMap<String, RegisteredGroup>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT jid, name, folder, trigger_pattern, added_at, container_config, requires_trigger, is_main
            FROM registered_groups
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row_to_registered_group_with_jid_offset(row, 1)?))
        })?;

        let mut groups = HashMap::new();
        for row in rows {
            let (jid, group) = row?;
            if is_valid_group_folder(&group.folder) {
                groups.insert(jid, group);
            }
        }
        Ok(groups)
    }
}

fn bool_to_sql(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

fn serialize_container_config(config: &ContainerConfig) -> serde_json::Result<String> {
    serde_json::to_string(config)
}

fn deserialize_container_config(value: &str) -> serde_json::Result<ContainerConfig> {
    serde_json::from_str(value)
}

fn enum_label_schedule_type(value: &nanoclaw_core::types::ScheduleType) -> &'static str {
    match value {
        nanoclaw_core::types::ScheduleType::Cron => "cron",
        nanoclaw_core::types::ScheduleType::Interval => "interval",
        nanoclaw_core::types::ScheduleType::Once => "once",
    }
}

fn enum_label_context_mode(value: &nanoclaw_core::types::ContextMode) -> &'static str {
    match value {
        nanoclaw_core::types::ContextMode::Group => "group",
        nanoclaw_core::types::ContextMode::Isolated => "isolated",
    }
}

fn enum_label_task_status(value: &nanoclaw_core::types::TaskStatus) -> &'static str {
    match value {
        nanoclaw_core::types::TaskStatus::Active => "active",
        nanoclaw_core::types::TaskStatus::Paused => "paused",
        nanoclaw_core::types::TaskStatus::Completed => "completed",
    }
}

fn enum_label_run_status(value: &nanoclaw_core::types::TaskRunStatus) -> &'static str {
    match value {
        nanoclaw_core::types::TaskRunStatus::Success => "success",
        nanoclaw_core::types::TaskRunStatus::Error => "error",
    }
}

fn parse_schedule_type(value: &str) -> Result<ScheduleType> {
    match value {
        "cron" => Ok(ScheduleType::Cron),
        "interval" => Ok(ScheduleType::Interval),
        "once" => Ok(ScheduleType::Once),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            format!("invalid schedule_type: {other}").into(),
        )),
    }
}

fn parse_context_mode(value: &str) -> Result<ContextMode> {
    match value {
        "group" => Ok(ContextMode::Group),
        "isolated" => Ok(ContextMode::Isolated),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            format!("invalid context_mode: {other}").into(),
        )),
    }
}

fn parse_task_status(value: &str) -> Result<TaskStatus> {
    match value {
        "active" => Ok(TaskStatus::Active),
        "paused" => Ok(TaskStatus::Paused),
        "completed" => Ok(TaskStatus::Completed),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            10,
            rusqlite::types::Type::Text,
            format!("invalid task status: {other}").into(),
        )),
    }
}

fn row_to_scheduled_task(row: &rusqlite::Row<'_>) -> Result<ScheduledTask> {
    let schedule_type: String = row.get(4)?;
    let context_mode: String = row.get(6)?;
    let status: String = row.get(10)?;
    Ok(ScheduledTask {
        id: row.get(0)?,
        group_folder: row.get(1)?,
        chat_jid: row.get(2)?,
        prompt: row.get(3)?,
        schedule_type: parse_schedule_type(&schedule_type)?,
        schedule_value: row.get(5)?,
        context_mode: parse_context_mode(&context_mode)?,
        next_run: row.get(7)?,
        last_run: row.get(8)?,
        last_result: row.get(9)?,
        status: parse_task_status(&status)?,
        created_at: row.get(11)?,
    })
}

fn row_to_chat_info(row: &rusqlite::Row<'_>) -> Result<nanoclaw_core::types::ChatInfo> {
    Ok(nanoclaw_core::types::ChatInfo {
        jid: row.get(0)?,
        name: row.get(1)?,
        last_message_time: row.get(2)?,
        channel: row.get(3)?,
        is_group: row.get::<_, i64>(4)? == 1,
    })
}

fn row_to_new_message(row: &rusqlite::Row<'_>) -> Result<NewMessage> {
    Ok(NewMessage {
        id: row.get(0)?,
        chat_jid: row.get(1)?,
        sender: row.get(2)?,
        sender_name: row.get(3)?,
        content: row.get(4)?,
        timestamp: row.get(5)?,
        is_from_me: row.get::<_, Option<i64>>(6)?.map(|value| value == 1),
        is_bot_message: row.get::<_, Option<i64>>(7)?.map(|value| value == 1),
    })
}

fn row_to_registered_group(row: &rusqlite::Row<'_>) -> Result<RegisteredGroup> {
    row_to_registered_group_with_jid_offset(row, 0)
}

fn row_to_registered_group_with_jid_offset(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> Result<RegisteredGroup> {
    let container_config: Option<String> = row.get(offset + 4)?;
    Ok(RegisteredGroup {
        name: row.get(offset)?,
        folder: row.get(offset + 1)?,
        trigger: row.get(offset + 2)?,
        added_at: row.get(offset + 3)?,
        container_config: container_config
            .map(|json| deserialize_container_config(&json))
            .transpose()
            .map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    offset + 4,
                    rusqlite::types::Type::Text,
                    err.into(),
                )
            })?,
        requires_trigger: row.get::<_, Option<i64>>(offset + 5)?.map(|value| value == 1),
        is_main: row.get::<_, Option<i64>>(offset + 6)?.map(|value| value == 1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanoclaw_core::types::{ContextMode, ScheduleType, TaskRunStatus, TaskStatus};

    #[test]
    fn initializes_schema() {
        let db = NanoClawDb::open_in_memory().expect("db");
        for table in planned_tables() {
            let exists: Option<String> = db
                .connection()
                .query_row(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table.name()],
                    |row| row.get(0),
                )
                .optional()
                .expect("query");
            assert_eq!(exists.as_deref(), Some(table.name()));
        }
    }

    #[test]
    fn round_trips_registered_group() {
        let db = NanoClawDb::open_in_memory().expect("db");
        let group = RegisteredGroup {
            name: "Main".to_string(),
            folder: "main".to_string(),
            trigger: "@Andy".to_string(),
            added_at: "2026-03-11T00:00:00Z".to_string(),
            container_config: Some(ContainerConfig {
                additional_mounts: vec![],
                timeout_ms: Some(1_000),
            }),
            requires_trigger: Some(false),
            is_main: Some(true),
        };

        db.set_registered_group("jid-1", &group).expect("insert");
        let actual = db.get_registered_group("jid-1").expect("get");
        assert_eq!(actual, Some(group));
    }

    #[test]
    fn stores_message_and_task() {
        let db = NanoClawDb::open_in_memory().expect("db");
        db.store_chat_metadata(
            "chat-1",
            "2026-03-11T00:00:00Z",
            Some("Chat"),
            Some("discord"),
            Some(true),
        )
        .expect("chat");
        db.store_message(&NewMessage {
            id: "msg-1".to_string(),
            chat_jid: "chat-1".to_string(),
            sender: "user".to_string(),
            sender_name: "User".to_string(),
            content: "hello".to_string(),
            timestamp: "2026-03-11T00:00:00Z".to_string(),
            is_from_me: Some(false),
            is_bot_message: Some(false),
        })
        .expect("message");
        db.create_task(&ScheduledTask {
            id: "task-1".to_string(),
            group_folder: "main".to_string(),
            chat_jid: "chat-1".to_string(),
            prompt: "say hi".to_string(),
            schedule_type: ScheduleType::Once,
            schedule_value: "2026-03-12T00:00:00Z".to_string(),
            context_mode: ContextMode::Isolated,
            next_run: Some("2026-03-12T00:00:00Z".to_string()),
            last_run: None,
            last_result: None,
            status: TaskStatus::Active,
            created_at: "2026-03-11T00:00:00Z".to_string(),
        })
        .expect("task");
        db.log_task_run(&TaskRunLog {
            task_id: "task-1".to_string(),
            run_at: "2026-03-11T00:01:00Z".to_string(),
            duration_ms: 10,
            status: TaskRunStatus::Success,
            result: Some("ok".to_string()),
            error: None,
        })
        .expect("log");
    }

    #[test]
    fn supports_task_crud() {
        let db = NanoClawDb::open_in_memory().expect("db");
        let task = ScheduledTask {
            id: "task-1".to_string(),
            group_folder: "main".to_string(),
            chat_jid: "chat-1".to_string(),
            prompt: "say hi".to_string(),
            schedule_type: ScheduleType::Once,
            schedule_value: "2026-03-12T00:00:00Z".to_string(),
            context_mode: ContextMode::Isolated,
            next_run: Some("2026-03-12T00:00:00Z".to_string()),
            last_run: None,
            last_result: None,
            status: TaskStatus::Active,
            created_at: "2026-03-11T00:00:00Z".to_string(),
        };
        db.create_task(&task).expect("task");
        assert_eq!(db.get_task_by_id("task-1").expect("get"), Some(task.clone()));
        assert_eq!(db.get_due_tasks("2026-03-12T00:00:00Z").expect("due").len(), 1);
        db.update_task(
            "task-1",
            &TaskUpdate {
                prompt: None,
                schedule_type: None,
                schedule_value: None,
                next_run: None,
                status: Some(TaskStatus::Paused),
            },
        )
        .expect("update");
        assert_eq!(
            db.get_task_by_id("task-1")
                .expect("get")
                .expect("task")
                .status,
            TaskStatus::Paused
        );
        db.delete_task("task-1").expect("delete");
        assert_eq!(db.get_task_by_id("task-1").expect("get"), None);
    }

    fn store_message_fixture(
        db: &NanoClawDb,
        id: &str,
        chat_jid: &str,
        content: &str,
        timestamp: &str,
        is_bot_message: bool,
    ) {
        db.store_message(&NewMessage {
            id: id.to_string(),
            chat_jid: chat_jid.to_string(),
            sender: "user@s.whatsapp.net".to_string(),
            sender_name: "User".to_string(),
            content: content.to_string(),
            timestamp: timestamp.to_string(),
            is_from_me: Some(false),
            is_bot_message: Some(is_bot_message),
        })
        .expect("message");
    }

    #[test]
    fn returns_all_chats_by_recent_activity() {
        let db = NanoClawDb::open_in_memory().expect("db");
        db.store_chat_metadata("old@g.us", "2026-03-11T00:00:01Z", Some("Old"), None, Some(true))
            .expect("chat");
        db.store_chat_metadata("new@g.us", "2026-03-11T00:00:05Z", Some("New"), None, Some(true))
            .expect("chat");

        let chats = db.get_all_chats().expect("chats");
        assert_eq!(chats.len(), 2);
        assert_eq!(chats[0].jid, "new@g.us");
        assert_eq!(chats[1].jid, "old@g.us");
    }

    #[test]
    fn returns_messages_since_and_filters_bot_content() {
        let db = NanoClawDb::open_in_memory().expect("db");
        db.store_chat_metadata("group@g.us", "2026-03-11T00:00:00Z", None, None, Some(true))
            .expect("chat");
        store_message_fixture(&db, "m1", "group@g.us", "first", "2026-03-11T00:00:01Z", false);
        store_message_fixture(&db, "m2", "group@g.us", "Andy: old reply", "2026-03-11T00:00:02Z", false);
        store_message_fixture(&db, "m3", "group@g.us", "third", "2026-03-11T00:00:03Z", false);
        store_message_fixture(&db, "m4", "group@g.us", "bot", "2026-03-11T00:00:04Z", true);

        let messages = db
            .get_messages_since("group@g.us", "2026-03-11T00:00:00Z", "Andy", 10)
            .expect("messages");
        assert_eq!(messages.iter().map(|message| message.id.as_str()).collect::<Vec<_>>(), vec!["m1", "m3"]);
    }

    #[test]
    fn returns_new_messages_across_groups_and_latest_timestamp() {
        let db = NanoClawDb::open_in_memory().expect("db");
        for jid in ["group1@g.us", "group2@g.us"] {
            db.store_chat_metadata(jid, "2026-03-11T00:00:00Z", None, None, Some(true))
                .expect("chat");
        }
        store_message_fixture(&db, "a1", "group1@g.us", "one", "2026-03-11T00:00:01Z", false);
        store_message_fixture(&db, "a2", "group2@g.us", "two", "2026-03-11T00:00:02Z", false);
        store_message_fixture(&db, "a3", "group1@g.us", "Andy: reply", "2026-03-11T00:00:03Z", false);
        store_message_fixture(&db, "a4", "group1@g.us", "four", "2026-03-11T00:00:04Z", false);

        let (messages, newest) = db
            .get_new_messages(
                &["group1@g.us".to_string(), "group2@g.us".to_string()],
                "2026-03-11T00:00:00Z",
                "Andy",
                10,
            )
            .expect("messages");
        assert_eq!(messages.iter().map(|message| message.id.as_str()).collect::<Vec<_>>(), vec!["a1", "a2", "a4"]);
        assert_eq!(newest, "2026-03-11T00:00:04Z");
    }

    #[test]
    fn limits_message_queries_to_most_recent_rows_in_chronological_order() {
        let db = NanoClawDb::open_in_memory().expect("db");
        db.store_chat_metadata("group@g.us", "2026-03-11T00:00:00Z", None, None, Some(true))
            .expect("chat");
        for second in 1..=10 {
            store_message_fixture(
                &db,
                &format!("m{second}"),
                "group@g.us",
                &format!("message {second}"),
                &format!("2026-03-11T00:00:{second:02}Z"),
                false,
            );
        }

        let messages = db
            .get_messages_since("group@g.us", "2026-03-11T00:00:00Z", "Andy", 3)
            .expect("messages");
        assert_eq!(
            messages.iter().map(|message| message.content.as_str()).collect::<Vec<_>>(),
            vec!["message 8", "message 9", "message 10"]
        );
    }

    #[test]
    fn returns_all_sessions() {
        let db = NanoClawDb::open_in_memory().expect("db");
        db.set_session("group-a", "session-a").expect("session");
        db.set_session("group-b", "session-b").expect("session");

        let sessions = db.get_all_sessions().expect("sessions");
        assert_eq!(sessions.get("group-a").map(String::as_str), Some("session-a"));
        assert_eq!(sessions.get("group-b").map(String::as_str), Some("session-b"));
    }

    #[test]
    fn returns_only_registered_groups_with_safe_folders() {
        let db = NanoClawDb::open_in_memory().expect("db");
        db.connection()
            .execute(
                r#"
                INSERT INTO registered_groups
                (jid, name, folder, trigger_pattern, added_at, container_config, requires_trigger, is_main)
                VALUES (?1, ?2, ?3, ?4, ?5, NULL, 1, 0)
                "#,
                params!["good@g.us", "Good", "good-folder", "@Andy", "2026-03-11T00:00:00Z"],
            )
            .expect("good");
        db.connection()
            .execute(
                r#"
                INSERT INTO registered_groups
                (jid, name, folder, trigger_pattern, added_at, container_config, requires_trigger, is_main)
                VALUES (?1, ?2, ?3, ?4, ?5, NULL, 1, 0)
                "#,
                params!["bad@g.us", "Bad", "../bad-folder", "@Andy", "2026-03-11T00:00:00Z"],
            )
            .expect("bad");

        let groups = db.get_all_registered_groups().expect("groups");
        assert!(groups.contains_key("good@g.us"));
        assert!(!groups.contains_key("bad@g.us"));
    }
}
