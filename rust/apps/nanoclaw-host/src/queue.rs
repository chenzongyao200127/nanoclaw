use std::collections::{HashMap, VecDeque};

const MAX_RETRIES: u32 = 5;
const BASE_RETRY_MS: u64 = 5_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueAction {
    StartMessages { group_jid: String, reason: RunReason },
    StartTask { group_jid: String, task_id: String },
    WriteClose { group_jid: String, group_folder: String },
    SendMessage {
        group_jid: String,
        group_folder: String,
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunReason {
    Messages,
    Drain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QueuedTask {
    id: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct GroupState {
    active: bool,
    idle_waiting: bool,
    is_task_container: bool,
    running_task_id: Option<String>,
    pending_messages: bool,
    pending_tasks: VecDeque<QueuedTask>,
    container_name: Option<String>,
    group_folder: Option<String>,
    retry_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScheduledRetry {
    due_at_ms: u64,
    group_jid: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    pub active_count: usize,
    pub detached_containers: Vec<String>,
}

#[derive(Debug, Default)]
pub struct GroupQueue {
    groups: HashMap<String, GroupState>,
    active_count: usize,
    waiting_groups: VecDeque<String>,
    scheduled_retries: Vec<ScheduledRetry>,
    shutting_down: bool,
    max_concurrent_containers: usize,
    now_ms: u64,
}

impl GroupQueue {
    pub fn new(max_concurrent_containers: usize) -> Self {
        Self {
            max_concurrent_containers: max_concurrent_containers.max(1),
            ..Self::default()
        }
    }

    pub fn enqueue_message_check(&mut self, group_jid: &str) -> Vec<QueueAction> {
        if self.shutting_down {
            return vec![];
        }

        if self.groups.get(group_jid).map(|state| state.active).unwrap_or(false) {
            self.get_group_mut(group_jid).pending_messages = true;
            return vec![];
        }

        if self.active_count >= self.max_concurrent_containers {
            self.get_group_mut(group_jid).pending_messages = true;
            self.enqueue_waiting_group(group_jid);
            return vec![];
        }

        self.start_messages(group_jid, RunReason::Messages)
    }

    pub fn enqueue_task(&mut self, group_jid: &str, task_id: &str) -> Vec<QueueAction> {
        if self.shutting_down {
            return vec![];
        }

        let duplicate_or_running = self
            .groups
            .get(group_jid)
            .map(|state| {
                state.running_task_id.as_deref() == Some(task_id)
                    || state.pending_tasks.iter().any(|task| task.id == task_id)
            })
            .unwrap_or(false);
        if duplicate_or_running {
            return vec![];
        }

        if self.groups.get(group_jid).map(|state| state.active).unwrap_or(false) {
            let state = self.get_group_mut(group_jid);
            state.pending_tasks.push_back(QueuedTask {
                id: task_id.to_string(),
            });
            if state.idle_waiting {
                return self.close_stdin(group_jid).into_iter().collect();
            }
            return vec![];
        }

        if self.active_count >= self.max_concurrent_containers {
            let state = self.get_group_mut(group_jid);
            state.pending_tasks.push_back(QueuedTask {
                id: task_id.to_string(),
            });
            self.enqueue_waiting_group(group_jid);
            return vec![];
        }

        self.start_task(group_jid, task_id)
    }

    pub fn register_process(
        &mut self,
        group_jid: &str,
        container_name: &str,
        group_folder: Option<&str>,
    ) {
        let state = self.get_group_mut(group_jid);
        state.container_name = Some(container_name.to_string());
        if let Some(folder) = group_folder {
            state.group_folder = Some(folder.to_string());
        }
    }

    pub fn notify_idle(&mut self, group_jid: &str) -> Vec<QueueAction> {
        let state = self.get_group_mut(group_jid);
        state.idle_waiting = true;
        if !state.pending_tasks.is_empty() {
            return self.close_stdin(group_jid).into_iter().collect();
        }
        vec![]
    }

    pub fn send_message(&mut self, group_jid: &str, text: &str) -> Option<QueueAction> {
        let state = self.get_group_mut(group_jid);
        if !state.active || state.is_task_container {
            return None;
        }
        let group_folder = state.group_folder.clone()?;
        state.idle_waiting = false;
        Some(QueueAction::SendMessage {
            group_jid: group_jid.to_string(),
            group_folder,
            text: text.to_string(),
        })
    }

    pub fn close_stdin(&self, group_jid: &str) -> Option<QueueAction> {
        let state = self.groups.get(group_jid)?;
        if !state.active {
            return None;
        }
        let group_folder = state.group_folder.clone()?;
        Some(QueueAction::WriteClose {
            group_jid: group_jid.to_string(),
            group_folder,
        })
    }

    pub fn complete_message_run(&mut self, group_jid: &str, success: bool) -> Vec<QueueAction> {
        {
            let state = self.get_group_mut(group_jid);
            state.active = false;
            state.idle_waiting = false;
            state.is_task_container = false;
            state.container_name = None;
            state.group_folder = None;
            if success {
                state.retry_count = 0;
            }
        }

        self.active_count = self.active_count.saturating_sub(1);

        if !success {
            self.schedule_retry(group_jid);
        }

        self.drain_group(group_jid)
    }

    pub fn complete_task_run(&mut self, group_jid: &str) -> Vec<QueueAction> {
        {
            let state = self.get_group_mut(group_jid);
            state.active = false;
            state.idle_waiting = false;
            state.is_task_container = false;
            state.running_task_id = None;
            state.container_name = None;
            state.group_folder = None;
        }
        self.active_count = self.active_count.saturating_sub(1);
        self.drain_group(group_jid)
    }

    pub fn advance_time(&mut self, elapsed_ms: u64) -> Vec<QueueAction> {
        self.now_ms += elapsed_ms;

        let mut ready = Vec::new();
        let mut pending = Vec::new();
        for retry in self.scheduled_retries.drain(..) {
            if retry.due_at_ms <= self.now_ms {
                ready.push(retry.group_jid);
            } else {
                pending.push(retry);
            }
        }
        self.scheduled_retries = pending;

        let mut actions = Vec::new();
        for group_jid in ready {
            actions.extend(self.enqueue_message_check(&group_jid));
        }
        actions
    }

    pub fn shutdown(&mut self) -> ShutdownReport {
        self.shutting_down = true;
        let detached_containers = self
            .groups
            .values()
            .filter_map(|state| state.container_name.clone())
            .collect::<Vec<_>>();

        ShutdownReport {
            active_count: self.active_count,
            detached_containers,
        }
    }

    fn get_group_mut(&mut self, group_jid: &str) -> &mut GroupState {
        self.groups.entry(group_jid.to_string()).or_default()
    }

    fn enqueue_waiting_group(&mut self, group_jid: &str) {
        if !self.waiting_groups.iter().any(|jid| jid == group_jid) {
            self.waiting_groups.push_back(group_jid.to_string());
        }
    }

    fn start_messages(&mut self, group_jid: &str, reason: RunReason) -> Vec<QueueAction> {
        let state = self.get_group_mut(group_jid);
        state.active = true;
        state.idle_waiting = false;
        state.is_task_container = false;
        state.pending_messages = false;
        self.active_count += 1;
        vec![QueueAction::StartMessages {
            group_jid: group_jid.to_string(),
            reason,
        }]
    }

    fn start_task(&mut self, group_jid: &str, task_id: &str) -> Vec<QueueAction> {
        let state = self.get_group_mut(group_jid);
        state.active = true;
        state.idle_waiting = false;
        state.is_task_container = true;
        state.running_task_id = Some(task_id.to_string());
        self.active_count += 1;
        vec![QueueAction::StartTask {
            group_jid: group_jid.to_string(),
            task_id: task_id.to_string(),
        }]
    }

    fn schedule_retry(&mut self, group_jid: &str) {
        let retry_count = {
            let state = self.get_group_mut(group_jid);
            state.retry_count += 1;
            if state.retry_count > MAX_RETRIES {
                state.retry_count = 0;
                return;
            }
            state.retry_count
        };

        let delay_ms = BASE_RETRY_MS * 2_u64.pow(retry_count - 1);
        self.scheduled_retries.push(ScheduledRetry {
            due_at_ms: self.now_ms + delay_ms,
            group_jid: group_jid.to_string(),
        });
    }

    fn drain_group(&mut self, group_jid: &str) -> Vec<QueueAction> {
        if self.shutting_down {
            return vec![];
        }

        let has_pending_task = self
            .groups
            .get(group_jid)
            .map(|state| !state.pending_tasks.is_empty())
            .unwrap_or(false);
        if has_pending_task {
            let task_id = self
                .get_group_mut(group_jid)
                .pending_tasks
                .pop_front()
                .expect("pending task")
                .id;
            return self.start_task(group_jid, &task_id);
        }

        let has_pending_messages = self
            .groups
            .get(group_jid)
            .map(|state| state.pending_messages)
            .unwrap_or(false);
        if has_pending_messages {
            return self.start_messages(group_jid, RunReason::Drain);
        }

        self.drain_waiting()
    }

    fn drain_waiting(&mut self) -> Vec<QueueAction> {
        let mut actions = Vec::new();
        while self.active_count < self.max_concurrent_containers {
            let Some(next_jid) = self.waiting_groups.pop_front() else {
                break;
            };
            let Some(state) = self.groups.get(&next_jid) else {
                continue;
            };

            if !state.pending_tasks.is_empty() {
                let task_id = self
                    .get_group_mut(&next_jid)
                    .pending_tasks
                    .pop_front()
                    .expect("pending task")
                    .id;
                actions.extend(self.start_task(&next_jid, &task_id));
            } else if state.pending_messages {
                actions.extend(self.start_messages(&next_jid, RunReason::Drain));
            }
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue() -> GroupQueue {
        GroupQueue::new(2)
    }

    #[test]
    fn only_runs_one_container_per_group() {
        let mut queue = queue();

        let first = queue.enqueue_message_check("group1@g.us");
        let second = queue.enqueue_message_check("group1@g.us");

        assert_eq!(
            first,
            vec![QueueAction::StartMessages {
                group_jid: "group1@g.us".to_string(),
                reason: RunReason::Messages,
            }]
        );
        assert!(second.is_empty());

        let drain = queue.complete_message_run("group1@g.us", true);
        assert_eq!(
            drain,
            vec![QueueAction::StartMessages {
                group_jid: "group1@g.us".to_string(),
                reason: RunReason::Drain,
            }]
        );
    }

    #[test]
    fn respects_global_concurrency_limit() {
        let mut queue = queue();

        assert_eq!(queue.enqueue_message_check("group1@g.us").len(), 1);
        assert_eq!(queue.enqueue_message_check("group2@g.us").len(), 1);
        assert!(queue.enqueue_message_check("group3@g.us").is_empty());

        let actions = queue.complete_message_run("group1@g.us", true);
        assert_eq!(
            actions,
            vec![QueueAction::StartMessages {
                group_jid: "group3@g.us".to_string(),
                reason: RunReason::Drain,
            }]
        );
    }

    #[test]
    fn drains_tasks_before_messages() {
        let mut queue = queue();
        queue.enqueue_message_check("group1@g.us");
        let queued_task = queue.enqueue_task("group1@g.us", "task-1");
        let queued_message = queue.enqueue_message_check("group1@g.us");

        assert!(queued_task.is_empty());
        assert!(queued_message.is_empty());

        let actions = queue.complete_message_run("group1@g.us", true);
        assert_eq!(
            actions,
            vec![QueueAction::StartTask {
                group_jid: "group1@g.us".to_string(),
                task_id: "task-1".to_string(),
            }]
        );
    }

    #[test]
    fn retries_with_exponential_backoff() {
        let mut queue = queue();
        queue.enqueue_message_check("group1@g.us");

        assert!(queue.complete_message_run("group1@g.us", false).is_empty());
        assert!(queue.advance_time(4_999).is_empty());
        assert_eq!(
            queue.advance_time(1),
            vec![QueueAction::StartMessages {
                group_jid: "group1@g.us".to_string(),
                reason: RunReason::Messages,
            }]
        );

        assert!(queue.complete_message_run("group1@g.us", false).is_empty());
        assert!(queue.advance_time(9_999).is_empty());
        assert_eq!(
            queue.advance_time(1),
            vec![QueueAction::StartMessages {
                group_jid: "group1@g.us".to_string(),
                reason: RunReason::Messages,
            }]
        );
    }

    #[test]
    fn stops_retrying_after_max_retries() {
        let mut queue = queue();
        queue.enqueue_message_check("group1@g.us");

        for expected_delay in [5_000_u64, 10_000, 20_000, 40_000, 80_000] {
            assert!(queue.complete_message_run("group1@g.us", false).is_empty());
            assert!(queue.advance_time(expected_delay - 1).is_empty());
            assert_eq!(
                queue.advance_time(1),
                vec![QueueAction::StartMessages {
                    group_jid: "group1@g.us".to_string(),
                    reason: RunReason::Messages,
                }]
            );
        }

        assert!(queue.complete_message_run("group1@g.us", false).is_empty());
        assert!(queue.advance_time(200_000).is_empty());
    }

    #[test]
    fn rejects_duplicate_running_or_queued_tasks() {
        let mut queue = queue();

        let first = queue.enqueue_task("group1@g.us", "task-1");
        let duplicate_running = queue.enqueue_task("group1@g.us", "task-1");

        assert_eq!(
            first,
            vec![QueueAction::StartTask {
                group_jid: "group1@g.us".to_string(),
                task_id: "task-1".to_string(),
            }]
        );
        assert!(duplicate_running.is_empty());

        queue.complete_task_run("group1@g.us");

        queue.enqueue_message_check("group1@g.us");
        let queued = queue.enqueue_task("group1@g.us", "task-2");
        let duplicate_queued = queue.enqueue_task("group1@g.us", "task-2");

        assert!(queued.is_empty());
        assert!(duplicate_queued.is_empty());
    }

    #[test]
    fn idle_container_is_preempted_for_pending_tasks() {
        let mut queue = queue();
        queue.enqueue_message_check("group1@g.us");
        queue.register_process("group1@g.us", "container-1", Some("test-group"));
        assert!(queue.enqueue_task("group1@g.us", "task-1").is_empty());

        let actions = queue.notify_idle("group1@g.us");
        assert_eq!(
            actions,
            vec![QueueAction::WriteClose {
                group_jid: "group1@g.us".to_string(),
                group_folder: "test-group".to_string(),
            }]
        );
    }

    #[test]
    fn send_message_resets_idle_waiting() {
        let mut queue = queue();
        queue.enqueue_message_check("group1@g.us");
        queue.register_process("group1@g.us", "container-1", Some("test-group"));
        queue.notify_idle("group1@g.us");

        let send = queue.send_message("group1@g.us", "hello");
        assert_eq!(
            send,
            Some(QueueAction::SendMessage {
                group_jid: "group1@g.us".to_string(),
                group_folder: "test-group".to_string(),
                text: "hello".to_string(),
            })
        );

        let task_actions = queue.enqueue_task("group1@g.us", "task-1");
        assert!(task_actions.is_empty());
    }

    #[test]
    fn send_message_returns_none_for_task_containers() {
        let mut queue = queue();
        queue.enqueue_task("group1@g.us", "task-1");
        queue.register_process("group1@g.us", "container-1", Some("test-group"));

        assert_eq!(queue.send_message("group1@g.us", "hello"), None);
    }

    #[test]
    fn shutdown_prevents_new_enqueues() {
        let mut queue = queue();
        queue.enqueue_message_check("group1@g.us");
        queue.register_process("group1@g.us", "container-1", Some("test-group"));

        let report = queue.shutdown();
        assert_eq!(report.active_count, 1);
        assert_eq!(report.detached_containers, vec!["container-1".to_string()]);
        assert!(queue.enqueue_message_check("group2@g.us").is_empty());
        assert!(queue.enqueue_task("group2@g.us", "task-1").is_empty());
    }
}
