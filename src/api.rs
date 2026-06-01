//! HTTP API client for the Leptos/WASM frontend.
//!
//! All functions require the server running at [`API_BASE`].  Each request
//! attaches `Authorization: Bearer …` when [`crate::auth::current`] returns
//! a token.

use gloo_net::http::Request;
use serde::{Deserialize, Serialize};
use taskagent_domain::{Document, Plan, Project, Task};

use crate::auth;

pub use taskagent_api_dto::PlanWithProgress;
pub use taskagent_domain::{Relation, TaskRelations};

// Empty = same-origin relative URLs. In dev (`trunk serve`), Trunk's [[proxy]]
// in Trunk.toml forwards /v1/* to the local API on :8080.
const API_BASE: &str = "";

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ApiError {
    Network(String),
    Status(u16, String),
    Decode(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Network(msg) => write!(f, "network error: {msg}"),
            ApiError::Status(code, msg) => write!(f, "HTTP {code}: {msg}"),
            ApiError::Decode(msg) => write!(f, "decode error: {msg}"),
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Optionally attach bearer auth and always send same-origin cookies.
fn with_auth(builder: gloo_net::http::RequestBuilder) -> gloo_net::http::RequestBuilder {
    let builder = builder.credentials(web_sys::RequestCredentials::Include);
    match auth::current() {
        Some(token) => builder.header("Authorization", &format!("Bearer {token}")),
        None => builder,
    }
}

async fn get_json<T: for<'de> Deserialize<'de>>(url: &str) -> Result<T, ApiError> {
    let resp = with_auth(Request::get(url))
        .send()
        .await
        .map_err(|e| ApiError::Network(e.to_string()))?;

    let status = resp.status();
    if !(200..300).contains(&(status as u32)) {
        let body = resp.text().await.unwrap_or_default();
        return Err(ApiError::Status(status, body));
    }
    resp.json::<T>()
        .await
        .map_err(|e| ApiError::Decode(e.to_string()))
}

async fn post_json<B: Serialize, T: for<'de> Deserialize<'de>>(
    url: &str,
    body: &B,
) -> Result<T, ApiError> {
    let body_str = serde_json::to_string(body).map_err(|e| ApiError::Decode(e.to_string()))?;

    let resp = with_auth(Request::post(url))
        .header("Content-Type", "application/json")
        .body(body_str)
        .map_err(|e| ApiError::Network(e.to_string()))?
        .send()
        .await
        .map_err(|e| ApiError::Network(e.to_string()))?;

    let status = resp.status();
    if !(200..300).contains(&(status as u32)) {
        let body = resp.text().await.unwrap_or_default();
        return Err(ApiError::Status(status, body));
    }
    resp.json::<T>()
        .await
        .map_err(|e| ApiError::Decode(e.to_string()))
}

// ── Public API ────────────────────────────────────────────────────────────────

/// `GET /v1/tasks[?project_id=…]`
///
/// Pass `Some("inbox")` to list tasks with no project; `None` for all tasks.
pub async fn list_tasks(project_id: Option<&str>) -> Result<Vec<Task>, ApiError> {
    let url = match project_id {
        Some(pid) => format!("{API_BASE}/v1/tasks?project_id={pid}"),
        None => format!("{API_BASE}/v1/tasks"),
    };
    get_json(&url).await
}

/// `GET /v1/projects`
pub async fn list_projects() -> Result<Vec<Project>, ApiError> {
    get_json(&format!("{API_BASE}/v1/projects")).await
}

/// `GET /v1/plans?project_id=…`
pub async fn list_plans(project_id: &str) -> Result<Vec<Plan>, ApiError> {
    let url = format!("{API_BASE}/v1/plans?project_id={project_id}");
    get_json(&url).await
}

/// `GET /v1/plans/{id}` — returns plan + progress snapshot.
#[allow(dead_code)] // PlanWithProgress fetching deferred; progress shown via criteria count
pub async fn get_plan(id: &str) -> Result<PlanWithProgress, ApiError> {
    get_json(&format!("{API_BASE}/v1/plans/{id}")).await
}

/// `GET /v1/tasks/{id}/plans` — every plan that contains this task.
#[allow(dead_code)] // available for future use
pub async fn list_task_plans(task_id: &str) -> Result<Vec<Plan>, ApiError> {
    get_json(&format!("{API_BASE}/v1/tasks/{task_id}/plans")).await
}

/// `GET /v1/tasks/{id}/relations` — 5-group projection (blocks / blocked_by /
/// relates_to / duplicates / duplicated_by) for a task.
pub async fn list_task_relations(task_id: &str) -> Result<TaskRelations, ApiError> {
    get_json(&format!("{API_BASE}/v1/tasks/{task_id}/relations")).await
}

/// `POST /v1/relations/query` — bulk fetch of relations whose either endpoint
/// matches any of the given task ids. Empty input → empty output.
pub async fn list_relations_for_tasks(task_ids: &[String]) -> Result<Vec<Relation>, ApiError> {
    if task_ids.is_empty() {
        return Ok(Vec::new());
    }

    #[derive(Serialize)]
    struct Body<'a> {
        task_ids: &'a [String],
    }

    post_json(
        &format!("{API_BASE}/v1/relations/query"),
        &Body { task_ids },
    )
    .await
}

// ── Documents (PR1) ──────────────────────────────────────────────────────────

/// `GET /v1/projects/{project_id}/documents` — list non-archived documents
/// belonging to a project. Server returns the bare `Vec<Document>`.
pub async fn list_project_documents(project_id: &str) -> Result<Vec<Document>, ApiError> {
    let url = format!("{API_BASE}/v1/projects/{project_id}/documents");
    get_json(&url).await
}
