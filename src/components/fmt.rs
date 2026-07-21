//! Shared display helpers: timestamps, short ids, and task-status pill
//! class/label mappings. Pure formatting — no reactive or DOM logic.
//!
//! These were previously private copies in `task_row`, `plans_panel`,
//! `activity_feed`, `agent_ops_panel`, `artifacts_panel`, `documents_panel`
//! and `time_machine`; the byte-identical copies live here now. Note that
//! `time_machine` keeps its own `status_label` (lowercase `Status::as_str`
//! discriminants) and `short_id` (first-13-chars) — those differ on purpose.

use daruma_domain::Status;
use daruma_shared::time::Timestamp;

/// Last 8 non-hyphen characters of an id string (mirrors the original
/// apps/web task-row shortId convention).
pub fn short_id(id: &str) -> String {
    let compact: String = id.chars().filter(|&c| c != '-').collect();
    if compact.len() >= 8 {
        compact[compact.len() - 8..].to_string()
    } else {
        compact
    }
}

/// "HH:MM:SS" (UTC).
pub fn format_time(ts: Timestamp) -> String {
    // Timestamp is chrono::DateTime<Utc> via daruma_shared::time.
    use chrono::Timelike;
    let dt: chrono::DateTime<chrono::Utc> = ts.into();
    format!("{:02}:{:02}:{:02}", dt.hour(), dt.minute(), dt.second())
}

/// "YYYY-MM-DD HH:MM:SS" (UTC).
pub fn format_ts(ts: Timestamp) -> String {
    use chrono::{Datelike, Timelike};
    let dt: chrono::DateTime<chrono::Utc> = ts.into();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

pub fn ts_millis(ts: Timestamp) -> i64 {
    let dt: chrono::DateTime<chrono::Utc> = ts.into();
    dt.timestamp_millis()
}

/// Task-status pill class. The existing `.status-*` colors apply with no new
/// CSS; `PlanGraphNode.status` is the same `Status` enum as `Task.status`.
pub fn status_class(s: Status) -> &'static str {
    match s {
        Status::Inbox => "status status-inbox",
        Status::Todo => "status status-todo",
        Status::InProgress => "status status-in-progress",
        Status::InReview => "status status-in-review",
        Status::Done => "status status-done",
        Status::Cancelled => "status status-cancelled",
    }
}

/// Title-case display label ("In Progress", …). Undecorated — callers that
/// want ✓/✗ markers on Done/Cancelled (e.g. `task_row`) add them themselves.
pub fn status_label(s: Status) -> &'static str {
    match s {
        Status::Inbox => "Inbox",
        Status::Todo => "Todo",
        Status::InProgress => "In Progress",
        Status::InReview => "In Review",
        Status::Done => "Done",
        Status::Cancelled => "Cancelled",
    }
}
