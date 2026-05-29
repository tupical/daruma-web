use crate::api;
use crate::projects_ctx::{ProjectFilter, ProjectsCtx};
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

fn example_binary_path() -> String {
    let ua = web_sys::window()
        .and_then(|w| w.navigator().user_agent().ok())
        .unwrap_or_default()
        .to_lowercase();
    if ua.contains("win") {
        r"C:\Tools\taskagent-mcp.exe".to_string()
    } else {
        "/usr/local/bin/taskagent-mcp".to_string()
    }
}

fn copy_text(text: &str) {
    if let Some(window) = web_sys::window() {
        let clipboard = window.navigator().clipboard();
        let text = text.to_string();
        spawn_local(async move {
            let _ = wasm_bindgen_futures::JsFuture::from(clipboard.write_text(&text)).await;
        });
    }
}

fn mcp_json_example(
    api_url: &str,
    token: &str,
    workspace_id: Option<&str>,
    project_id: Option<&str>,
    binary_path: &str,
) -> String {
    let workspace_line = workspace_id
        .map(|id| format!(",\n        \"TASKAGENT_WORKSPACE_ID\": \"{id}\""))
        .unwrap_or_default();
    let project_line = project_id
        .map(|id| format!(",\n        \"TASKAGENT_PROJECT_ID\": \"{id}\""))
        .unwrap_or_default();
    format!(
        r#"{{
  "mcpServers": {{
    "taskagent": {{
      "type": "stdio",
      "command": "{binary_path}",
      "args": [],
      "env": {{
        "TASKAGENT_API_URL": "{api_url}",
        "TASKAGENT_TOKEN": "{token}"{workspace_line}{project_line}
      }}
    }}
  }}
}}"#
    )
}

fn read_selected_project(ctx: &ProjectsCtx) -> Option<String> {
    match ctx.current_filter.get() {
        ProjectFilter::Of(id) => Some(id.to_string()),
        _ => None,
    }
}

#[component]
pub fn SettingsPanel(open: RwSignal<bool>) -> impl IntoView {
    let projects_ctx = use_context::<ProjectsCtx>().expect("ProjectsCtx");
    let (fresh_token, set_fresh_token) = signal(None::<String>);
    let (token_error, set_token_error) = signal(None::<String>);
    let (generating, set_generating) = signal(false);
    let (linux_available, set_linux_available) = signal(false);
    let (windows_available, set_windows_available) = signal(false);
    let (download_error, set_download_error) = signal(None::<String>);
    let (downloading, set_downloading) = signal(None::<String>);
    let (cloud_workspace_id, set_cloud_workspace_id) = signal(None::<String>);

    let api_url = api::api_origin();
    let mcp_snippet = Memo::new(move |_| {
        let project = read_selected_project(&projects_ctx);
        fresh_token.get().map(|token| {
            mcp_json_example(
                &api_url,
                &token,
                cloud_workspace_id.get().as_deref(),
                project.as_deref(),
                &example_binary_path(),
            )
        })
    });

    Effect::new(move |_| {
        if !open.get() {
            return;
        }
        spawn_local(async move {
            match api::mcp_download_platforms().await {
                Ok((linux, windows)) => {
                    set_linux_available.set(linux);
                    set_windows_available.set(windows);
                }
                Err(e) => set_download_error.set(Some(format!("{e}"))),
            }
            if let Ok(session) = api::cloud_session().await {
                set_cloud_workspace_id.set(Some(session.workspace_id));
            }
        });
    });

    view! {
        <Show when=move || open.get()>
            <div
                class="settings-overlay"
                on:click=move |_| open.set(false)
            >
                <section
                    class="settings-panel"
                    role="dialog"
                    aria-labelledby="settings-title"
                    on:click=|ev| ev.stop_propagation()
                >
                    <header class="settings-header">
                        <h2 id="settings-title">"Settings"</h2>
                        <button
                            type="button"
                            class="settings-close"
                            aria-label="Close settings"
                            on:click=move |_| open.set(false)
                        >
                            "×"
                        </button>
                    </header>

                    <div class="settings-section">
                        <h3>"MCP access token"</h3>
                        <p class="settings-hint">
                            "Generate a service token for Cursor / Claude Desktop. "
                            "The secret is shown once — copy it before closing this panel."
                        </p>
                        <button
                            type="button"
                            class="settings-btn"
                            disabled=move || generating.get()
                            on:click=move |_| {
                                set_token_error.set(None);
                                set_generating.set(true);
                                spawn_local(async move {
                                    match api::create_mcp_token(None).await {
                                        Ok(secret) => set_fresh_token.set(Some(secret)),
                                        Err(e) => set_token_error.set(Some(format!("{e}"))),
                                    }
                                    set_generating.set(false);
                                });
                            }
                        >
                            {move || if generating.get() { "Generating…" } else { "Generate MCP token" }}
                        </button>
                        {move || token_error.get().map(|e| view! {
                            <p class="settings-error">{e}</p>
                        })}
                        {move || fresh_token.get().map(|secret| view! {
                            <div class="settings-token-box">
                                <p class="settings-warning">"Copy now — this token will not be shown again."</p>
                                <code class="settings-token">{secret.clone()}</code>
                                <button
                                    type="button"
                                    class="settings-btn settings-btn-secondary"
                                    on:click=move |_| copy_text(&secret)
                                >
                                    "Copy token"
                                </button>
                            </div>
                        })}
                    </div>

                    <div class="settings-section">
                        <h3>"Download taskagent-mcp"</h3>
                        <p class="settings-hint">
                            "Stdio MCP shim that forwards tool calls to the TaskAgent API. "
                            "Save the binary, make it executable on Linux, then point your IDE at it."
                        </p>
                        <div class="settings-downloads">
                            <button
                                type="button"
                                class="settings-btn"
                                disabled=move || !windows_available.get() || downloading.get().is_some()
                                on:click=move |_| {
                                    set_download_error.set(None);
                                    set_downloading.set(Some("windows".into()));
                                    spawn_local(async move {
                                        let result = api::download_mcp_binary("windows", "taskagent-mcp.exe").await;
                                        if let Err(e) = result {
                                            set_download_error.set(Some(format!("{e}")));
                                        }
                                        set_downloading.set(None);
                                    });
                                }
                            >
                                {move || if downloading.get().as_deref() == Some("windows") {
                                    "Downloading…"
                                } else if windows_available.get() {
                                    "Download for Windows (.exe)"
                                } else {
                                    "Windows build unavailable"
                                }}
                            </button>
                            <button
                                type="button"
                                class="settings-btn settings-btn-secondary"
                                disabled=move || !linux_available.get() || downloading.get().is_some()
                                on:click=move |_| {
                                    set_download_error.set(None);
                                    set_downloading.set(Some("linux".into()));
                                    spawn_local(async move {
                                        let result = api::download_mcp_binary("linux", "taskagent-mcp").await;
                                        if let Err(e) = result {
                                            set_download_error.set(Some(format!("{e}")));
                                        }
                                        set_downloading.set(None);
                                    });
                                }
                            >
                                {move || if downloading.get().as_deref() == Some("linux") {
                                    "Downloading…"
                                } else if linux_available.get() {
                                    "Download for Linux"
                                } else {
                                    "Linux build unavailable"
                                }}
                            </button>
                        </div>
                        {move || download_error.get().map(|e| view! {
                            <p class="settings-error">{e}</p>
                        })}
                    </div>

                    <div class="settings-section">
                        <h3>"Cursor / IDE setup"</h3>
                        <ol class="settings-steps">
                            <li>"Download the binary above and note its full path."</li>
                            <li>"Generate an MCP token and copy it."</li>
                            <li>
                                "Create "
                                <code>".cursor/mcp.json"</code>
                                " (project) or edit global MCP settings with the snippet below."
                            </li>
                            <li>"Restart Cursor / Claude Desktop so the MCP server reloads."</li>
                        </ol>
                        <p class="settings-hint">
                            "On Linux after download: "
                            <code>"chmod +x ~/Downloads/taskagent-mcp"</code>
                        </p>
                        {move || mcp_snippet.get().map(|snippet| view! {
                            <div class="settings-snippet-wrap">
                                <pre class="settings-snippet"><code>{snippet.clone()}</code></pre>
                                <button
                                    type="button"
                                    class="settings-btn settings-btn-secondary"
                                    on:click=move |_| copy_text(&snippet)
                                >
                                    "Copy mcp.json snippet"
                                </button>
                            </div>
                        })}
                        {move || (fresh_token.get().is_none()).then(|| view! {
                            <p class="settings-hint">"Generate a token to preview the mcp.json snippet."</p>
                        })}
                    </div>
                </section>
            </div>
        </Show>
    }
}

#[component]
pub fn SettingsButton(open: RwSignal<bool>) -> impl IntoView {
    view! {
        <button
            type="button"
            class="settings-gear"
            title="Settings — MCP token & binary"
            aria-label="Open settings"
            on:click=move |_| open.set(true)
        >
            "⚙"
        </button>
    }
}
