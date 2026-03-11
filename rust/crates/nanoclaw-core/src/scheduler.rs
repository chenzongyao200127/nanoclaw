use std::str::FromStr;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use cron::Schedule;

use crate::types::{ScheduleType, ScheduledTask};

pub fn compute_next_run(task: &ScheduledTask, timezone: &str) -> Option<String> {
    compute_next_run_at(task, timezone, Utc::now())
}

pub fn compute_next_run_at(
    task: &ScheduledTask,
    timezone: &str,
    now: DateTime<Utc>,
) -> Option<String> {
    match task.schedule_type {
        ScheduleType::Once => None,
        ScheduleType::Cron => compute_next_cron_run(&task.schedule_value, timezone, now),
        ScheduleType::Interval => compute_next_interval_run(task, now),
    }
}

fn compute_next_cron_run(
    expression: &str,
    timezone: &str,
    now: DateTime<Utc>,
) -> Option<String> {
    let schedule = Schedule::from_str(expression).ok()?;
    let tz = timezone.parse::<Tz>().unwrap_or(chrono_tz::UTC);
    let now_local = now.with_timezone(&tz);
    schedule.after(&now_local).next().map(|next| next.to_rfc3339())
}

fn compute_next_interval_run(task: &ScheduledTask, now: DateTime<Utc>) -> Option<String> {
    let ms = task.schedule_value.parse::<i64>().ok().filter(|ms| *ms > 0)?;
    let base = task
        .next_run
        .as_deref()
        .and_then(parse_rfc3339_utc)
        .unwrap_or(now);

    let mut next = base + Duration::milliseconds(ms);
    while next <= now {
        next += Duration::milliseconds(ms);
    }

    Some(next.to_rfc3339())
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::types::{ContextMode, TaskStatus};

    fn make_task(schedule_type: ScheduleType, schedule_value: &str, next_run: Option<&str>) -> ScheduledTask {
        ScheduledTask {
            id: "task-1".to_string(),
            group_folder: "test".to_string(),
            chat_jid: "test@g.us".to_string(),
            prompt: "test".to_string(),
            schedule_type,
            schedule_value: schedule_value.to_string(),
            context_mode: ContextMode::Isolated,
            next_run: next_run.map(ToString::to_string),
            last_run: None,
            last_result: None,
            status: TaskStatus::Active,
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
        }
    }

    #[test]
    fn returns_none_for_once_tasks() {
        let task = make_task(
            ScheduleType::Once,
            "2026-01-01T00:00:00.000Z",
            Some("2026-01-01T00:00:00.000Z"),
        );
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 1).unwrap();
        assert_eq!(compute_next_run_at(&task, "UTC", now), None);
    }

    #[test]
    fn anchors_interval_to_scheduled_time() {
        let task = make_task(
            ScheduleType::Interval,
            "60000",
            Some("2026-01-01T00:00:00.000Z"),
        );
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 2).unwrap();
        let actual = compute_next_run_at(&task, "UTC", now).expect("next run");
        assert_eq!(actual, "2026-01-01T00:01:00+00:00");
    }

    #[test]
    fn skips_missed_intervals_without_drift() {
        let task = make_task(
            ScheduleType::Interval,
            "60000",
            Some("2026-01-01T00:00:00.000Z"),
        );
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 10, 30).unwrap();
        let actual = compute_next_run_at(&task, "UTC", now).expect("next run");
        assert_eq!(actual, "2026-01-01T00:11:00+00:00");
    }

    #[test]
    fn returns_none_for_invalid_interval() {
        let task = make_task(
            ScheduleType::Interval,
            "not-a-number",
            Some("2026-01-01T00:00:00.000Z"),
        );
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 1).unwrap();
        assert_eq!(compute_next_run_at(&task, "UTC", now), None);
    }

    #[test]
    fn computes_next_cron_run() {
        let task = make_task(ScheduleType::Cron, "0 * * * * *", None);
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 30).unwrap();
        let actual = compute_next_run_at(&task, "UTC", now).expect("cron next run");
        assert_eq!(actual, "2026-01-01T00:01:00+00:00");
    }
}
