//! HTTP API client for the Leptos/WASM frontend.
//!
//! All functions require the server running at [`API_BASE`].  Each request
//! attaches `Authorization: Bearer …` when [`crate::auth::current`] returns
//! a token.

use gloo_net::http::Request;
use serde::{Deserialize, Serialize};
use daruma_domain::{Document, Plan, Project, Task};
use daruma_events::EventEnvelope;

use crate::auth;

pub use daruma_api_dto::PlanWithProgress;
pub use daruma_domain::{Relation, TaskRelations};

// Empty = same-origin relative URLs. In dev (`trunk serve`), Trunk's [[proxy]]
// in Trunk.toml forwards /v1/* to the local API on :8080.
const API_BASE: &str = "";

/// Required by `/v1/tasks` since the status filter became mandatory.
/// The viewer shows every status group, so request the full archive explicitly.
const TASK_LIST_STATUS: &str = "all";

/// Required by `/v1/plans` — same contract as tasks.
const PLAN_LIST_STATUS: &str = "all";

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

/// `GET /v1/tasks?status=…[&project_id=…]`
///
/// Pass `Some("inbox")` to list tasks with no project; `None` for all projects.
pub async fn list_tasks(project_id: Option<&str>) -> Result<Vec<Task>, ApiError> {
    let mut url = format!("{API_BASE}/v1/tasks?status={TASK_LIST_STATUS}");
    if let Some(pid) = project_id {
        url.push_str(&format!("&project_id={pid}"));
    }
    get_json(&url).await
}

/// `GET /v1/projects`
pub async fn list_projects() -> Result<Vec<Project>, ApiError> {
    get_json(&format!("{API_BASE}/v1/projects")).await
}

/// `GET /v1/plans?project_id=…&status=…`
pub async fn list_plans(project_id: &str) -> Result<Vec<Plan>, ApiError> {
    let url = format!("{API_BASE}/v1/plans?project_id={project_id}&status={PLAN_LIST_STATUS}");
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

// ── Documents ────────────────────────────────────────────────────────────────

/// `GET /v1/projects/{project_id}/documents` — list non-archived documents
/// belonging to a project. Server returns the bare `Vec<Document>`.
pub async fn list_project_documents(project_id: &str) -> Result<Vec<Document>, ApiError> {
    let url = format!("{API_BASE}/v1/projects/{project_id}/documents");
    get_json(&url).await
}

// ── WorkspaceGraph types ──────────────────────────────────────────────────────

#[derive(Clone, Debug, serde::Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub kind: String,
    pub source_id: String,
    pub project_id: Option<String>,
    pub title: String,
    pub text: String,
    pub updated_at: String,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct GraphEdge {
    pub from_id: String,
    pub to_id: String,
    pub kind: String,
    pub source_event_seq: Option<i64>,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct GraphNeighborhood {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct GraphContextItem {
    pub node: GraphNode,
    pub edge: GraphEdge,
    pub direction: GraphDirection,
}

#[derive(Clone, Debug, serde::Deserialize, PartialEq, Eq)]
pub enum GraphDirection {
    Incoming,
    Outgoing,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct GraphSearchHit {
    pub node: GraphNode,
    pub score: f64,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct GraphStatus {
    pub schema_version: u32,
    pub node_count: u64,
    pub edge_count: u64,
    pub last_event_seq: Option<u64>,
    pub last_error: Option<String>,
}

// ── WorkspaceGraph fetchers ───────────────────────────────────────────────────

/// `GET /v1/workspacegraph/status`
pub async fn workspacegraph_status() -> Result<GraphStatus, ApiError> {
    get_json(&format!("{API_BASE}/v1/workspacegraph/status")).await
}

/// `GET /v1/workspacegraph/context?node_id=…&limit=…`
pub async fn workspacegraph_context(
    node_id: &str,
    limit: u32,
) -> Result<Vec<GraphContextItem>, ApiError> {
    let url = format!(
        "{API_BASE}/v1/workspacegraph/context?node_id={}&limit={}",
        urlencoding_simple(node_id),
        limit
    );
    get_json(&url).await
}

/// `GET /v1/workspacegraph/related?node_id=…&depth=…&limit=…`
pub async fn workspacegraph_related(
    node_id: &str,
    depth: u32,
    limit: u32,
) -> Result<GraphNeighborhood, ApiError> {
    let url = format!(
        "{API_BASE}/v1/workspacegraph/related?node_id={}&depth={}&limit={}",
        urlencoding_simple(node_id),
        depth,
        limit
    );
    get_json(&url).await
}

/// `GET /v1/workspacegraph/search?query=…&limit=…[&project_id=…]`
pub async fn workspacegraph_search(
    query: &str,
    limit: u32,
    project_id: Option<&str>,
) -> Result<Vec<GraphSearchHit>, ApiError> {
    let mut url = format!(
        "{API_BASE}/v1/workspacegraph/search?query={}&limit={}",
        urlencoding_simple(query),
        limit
    );
    if let Some(pid) = project_id {
        url.push_str(&format!("&project_id={}", urlencoding_simple(pid)));
    }
    get_json(&url).await
}

/// `GET /v1/workspacegraph/impact?node_id=…&limit=…`
pub async fn workspacegraph_impact(
    node_id: &str,
    limit: u32,
) -> Result<GraphNeighborhood, ApiError> {
    let url = format!(
        "{API_BASE}/v1/workspacegraph/impact?node_id={}&limit={}",
        urlencoding_simple(node_id),
        limit
    );
    get_json(&url).await
}

/// Percent-encode a query-parameter value without pulling in a URL library.
/// Encodes space, `+`, `&`, `=`, `#`, and non-ASCII bytes.
fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

// ── Event history ─────────────────────────────────────────────────────────────

/// `GET /v1/events?since={seq}&limit={limit}` — load up to `limit` events
/// with `seq > since_seq`, ordered ascending.
///
/// Used for catch-up on connect/reconnect: pass the highest `seq` the client
/// has already seen as `since_seq` (or `0` on the very first load).  The
/// server returns at most `limit` envelopes; call again with the last returned
/// `seq` until the response is shorter than `limit` (or empty).
///
/// `limit` is capped by the server at 500; callers should pass 200–500.
pub async fn events_since(since_seq: u64, limit: usize) -> Result<Vec<EventEnvelope>, ApiError> {
    let url = format!("{API_BASE}/v1/events?since={since_seq}&limit={limit}");
    get_json(&url).await
}
