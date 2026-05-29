//! HTTP API client for the Leptos/WASM frontend.
//!
//! All functions require the server running at [`API_BASE`].  Each request
//! attaches `Authorization: Bearer …` when [`crate::auth::current`] returns
//! a token.

use gloo_net::http::Request;
use serde::{Deserialize, Serialize};
use taskagent_domain::{Document, Plan, PlanStatus, Project, Task};

use crate::auth;

pub use taskagent_api_dto::{Command, CommandEnvelope, MutationResponse, PlanWithProgress};
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

async fn get_bytes(url: &str) -> Result<Vec<u8>, ApiError> {
    let resp = with_auth(Request::get(url))
        .send()
        .await
        .map_err(|e| ApiError::Network(e.to_string()))?;

    let status = resp.status();
    if !(200..300).contains(&(status as u32)) {
        let body = resp.text().await.unwrap_or_default();
        return Err(ApiError::Status(status, body));
    }
    resp.binary()
        .await
        .map_err(|e| ApiError::Network(e.to_string()))
}

async fn patch_json<B: Serialize, T: for<'de> Deserialize<'de>>(
    url: &str,
    body: &B,
) -> Result<T, ApiError> {
    let body_str = serde_json::to_string(body).map_err(|e| ApiError::Decode(e.to_string()))?;

    let resp = with_auth(Request::patch(url))
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

/// `POST /v1/commands` — dispatch any command envelope.
pub async fn dispatch_command(envelope: &CommandEnvelope) -> Result<MutationResponse, ApiError> {
    post_json(&format!("{API_BASE}/v1/commands"), envelope).await
}

/// `POST /v1/ai/parse` — AI-parse free text into a `Command`.
pub async fn ai_parse(text: &str) -> Result<Command, ApiError> {
    #[derive(Serialize)]
    struct Body<'a> {
        input: &'a str,
    }
    post_json(&format!("{API_BASE}/v1/ai/parse"), &Body { input: text }).await
}

// ── Documents (PR1) ──────────────────────────────────────────────────────────

/// `GET /v1/projects/{project_id}/documents` — list non-archived documents
/// belonging to a project. Server returns the bare `Vec<Document>`.
pub async fn list_project_documents(project_id: &str) -> Result<Vec<Document>, ApiError> {
    let url = format!("{API_BASE}/v1/projects/{project_id}/documents");
    get_json(&url).await
}

#[derive(Serialize, Default)]
pub struct DocumentPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// `PATCH /v1/documents/{id}` — rename and/or replace content.
/// The server requires at least one of `title` / `content` to be set.
pub async fn patch_document(id: &str, patch: &DocumentPatch) -> Result<MutationResponse, ApiError> {
    patch_json(&format!("{API_BASE}/v1/documents/{id}"), patch).await
}

/// `POST /v1/plans/{id}/status` — transition plan lifecycle status.
pub async fn set_plan_status(
    plan_id: &str,
    status: PlanStatus,
) -> Result<MutationResponse, ApiError> {
    #[derive(Serialize)]
    struct Body {
        status: PlanStatus,
    }
    post_json(
        &format!("{API_BASE}/v1/plans/{plan_id}/status"),
        &Body { status },
    )
    .await
}

// ── Settings / MCP ───────────────────────────────────────────────────────────

/// Capability mask for MCP tool access (all task/plan/run scopes, no token mint).
pub const MCP_TOKEN_CAPABILITIES: u32 = 16_769_023;

#[derive(Deserialize)]
pub struct CreatedTokenResponse {
    pub secret: String,
}

#[derive(Serialize)]
struct CreateMcpTokenBody {
    kind: &'static str,
    agent_id: String,
    capabilities: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    projects: Option<ProjectFilterBody>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ProjectFilterBody {
    Only { projects: Vec<String> },
}

#[derive(Deserialize)]
struct McpDownloadInfo {
    platforms: McpPlatforms,
}

#[derive(Deserialize)]
struct McpPlatforms {
    linux: bool,
    windows: bool,
}

/// `GET /v1/cloud/session` — cloud tenant id for MCP env (cookie auth).
pub async fn cloud_session() -> Result<CloudSessionResponse, ApiError> {
    get_json(&format!("{API_BASE}/v1/cloud/session")).await
}

#[derive(Deserialize)]
pub struct CloudSessionResponse {
    pub workspace_id: String,
    _workspace_slug: String,
}

/// `POST /v1/tokens` — mint an MCP service token (plaintext returned once).
pub async fn create_mcp_token(project_id: Option<&str>) -> Result<String, ApiError> {
    let projects = project_id.map(|id| ProjectFilterBody::Only {
        projects: vec![id.to_string()],
    });
    let body = CreateMcpTokenBody {
        kind: "svc",
        agent_id: uuid::Uuid::new_v4().to_string(),
        capabilities: MCP_TOKEN_CAPABILITIES,
        projects,
    };
    let resp: CreatedTokenResponse = post_json(&format!("{API_BASE}/v1/tokens"), &body).await?;
    Ok(resp.secret)
}

/// `GET /v1/downloads/taskagent-mcp` — which platform binaries are bundled.
pub async fn mcp_download_platforms() -> Result<(bool, bool), ApiError> {
    let info: McpDownloadInfo = get_json(&format!("{API_BASE}/v1/downloads/taskagent-mcp")).await?;
    Ok((info.platforms.linux, info.platforms.windows))
}

/// Download `taskagent-mcp` binary and trigger a browser save dialog.
pub async fn download_mcp_binary(platform: &str, filename: &str) -> Result<(), ApiError> {
    let bytes = get_bytes(&format!("{API_BASE}/v1/downloads/taskagent-mcp/{platform}")).await?;
    trigger_browser_download(&bytes, filename).map_err(ApiError::Network)
}

fn trigger_browser_download(bytes: &[u8], filename: &str) -> Result<(), String> {
    use wasm_bindgen::JsCast;
    use web_sys::{Blob, BlobPropertyBag, HtmlAnchorElement, Url};

    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let array = js_sys::Uint8Array::from(bytes);
    let parts = js_sys::Array::new();
    parts.push(&array);
    let bag = BlobPropertyBag::new();
    bag.set_type("application/octet-stream");
    let blob = Blob::new_with_u8_array_sequence_and_options(&parts, &bag)
        .map_err(|_| "blob create failed".to_string())?;
    let url =
        Url::create_object_url_with_blob(&blob).map_err(|_| "object url failed".to_string())?;
    let anchor = document
        .create_element("a")
        .map_err(|_| "anchor create failed".to_string())?
        .dyn_into::<HtmlAnchorElement>()
        .map_err(|_| "anchor cast failed".to_string())?;
    anchor.set_href(&url);
    anchor.set_download(filename);
    anchor.click();
    Url::revoke_object_url(&url).ok();
    Ok(())
}

/// Same-origin API base for MCP env vars (`https://host` or `http://localhost:8080`).
pub fn api_origin() -> String {
    web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_default()
}
