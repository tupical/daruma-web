//! Artifact Registry panel.
//!
//! Shown on the right rail when a single project is selected, alongside
//! `PlansPanel`/`DocumentsPanel`. Lists the project's artifacts (registry
//! entries for named, versioned resources — `artifact://`, `file://`,
//! `contract://`, `env://`), each row showing its kind, a compact
//! pending→active→committed status ladder (deprecated rendered separately,
//! since it isn't "further along" than committed — it's a terminal/orthogonal
//! state), owner/current lease holder, and version. Clicking a row lazily
//! fetches and expands its downstream impact neighborhood (what depends on
//! it / what implements it) as a plain list — no graph visualization.
//!
//! Unlike `PlansPanel`/`DocumentsPanel`, which patch their cached snapshot in
//! place from individual WS events, this panel just refetches the list on
//! any `Channel::Artifacts` event: the Artifact Registry command/event layer
//! is still landing (parallel work), so coupling to specific event payload
//! shapes here would be premature — a blunt refetch is simpler and just as
//! correct for a read-only viewer.

use crate::api::{self, ArtifactRow, GraphNeighborhood};
use crate::projects_ctx::{ProjectFilter, ProjectsCtx};
use crate::ws::WsCtx;
use daruma_events::{Channel, EventEnvelope};
use leptos::prelude::*;
use std::collections::HashMap;

/// Last 8 non-hyphen characters of an id string (mirrors `task_row.rs`'s
/// `short_id` — same convention, small enough not to be worth sharing).
fn short_id(id: &str) -> String {
    let compact: String = id.chars().filter(|&c| c != '-').collect();
    if compact.len() >= 8 {
        compact[compact.len() - 8..].to_string()
    } else {
        compact
    }
}

fn kind_class(kind: &str) -> String {
    format!("artifact-kind-badge artifact-kind-badge--{kind}")
}

/// Status ladder step index: pending=0, active=1, committed=2. `None` for
/// "deprecated" (and any unrecognized value) — not part of the progression.
fn status_step(status: &str) -> Option<usize> {
    match status {
        "pending" => Some(0),
        "active" => Some(1),
        "committed" => Some(2),
        _ => None,
    }
}

const LADDER_LABELS: [&str; 3] = ["pending", "active", "committed"];

// ── ArtifactsPanel ───────────────────────────────────────────────────────────

#[component]
pub fn ArtifactsPanel() -> impl IntoView {
    let projects_ctx = use_context::<ProjectsCtx>().expect("ProjectsCtx");
    let ws_ctx = use_context::<WsCtx>().expect("WsCtx");
    let current_filter = projects_ctx.current_filter;
    let ws_events = ws_ctx.events;

    // Derive project_id from filter — only Some when Of(pid), same as
    // PlansPanel/DocumentsPanel.
    let project_id_opt = Memo::new(move |_| match current_filter.get() {
        ProjectFilter::Of(pid) => Some(pid.to_string()),
        _ => None,
    });

    let cache: RwSignal<HashMap<String, Vec<ArtifactRow>>> = RwSignal::new(HashMap::new());
    let fetch_seq: RwSignal<HashMap<String, u64>> = RwSignal::new(HashMap::new());
    let fetch_error: RwSignal<Option<String>> = RwSignal::new(None);
    let applied_cursor: RwSignal<usize> = RwSignal::new(0);
    // Bumped whenever a Channel::Artifacts event lands (effect 2 below) so
    // effect 1 re-runs even though its only other tracked input is the
    // project filter. See module docs: refetch, not in-place patch.
    let refetch_trigger: RwSignal<u32> = RwSignal::new(0);

    let artifacts: Memo<Vec<ArtifactRow>> = Memo::new(move |_| {
        let Some(pid) = project_id_opt.get() else {
            return Vec::new();
        };
        cache.with(|m| m.get(&pid).cloned().unwrap_or_default())
    });

    let loaded: Memo<bool> = Memo::new(move |_| {
        let Some(pid) = project_id_opt.get() else {
            return true;
        };
        cache.with(|m| m.contains_key(&pid))
    });

    // 1) Fetch on first visit to a project, and again whenever
    //    `refetch_trigger` is bumped.
    Effect::new(move |_| {
        let Some(pid) = project_id_opt.get() else {
            return;
        };
        // Tracked: a bump forces this effect to re-run even on a cache hit.
        refetch_trigger.get();
        // Clear before the cache-hit check below: `fetch_error` isn't keyed
        // per-project, so a stale error from a previous failed project must
        // not linger over a different project that's actually a cache hit.
        fetch_error.set(None);
        if cache.with_untracked(|m| m.contains_key(&pid)) {
            return;
        }
        let my_seq = fetch_seq.with_untracked(|m| m.get(&pid).copied().unwrap_or(0)) + 1;
        fetch_seq.update(|m| {
            m.insert(pid.clone(), my_seq);
        });

        // Cancel-on-cleanup: reads component-owned signals after the await,
        // so a plain spawn would panic if the route is disposed mid-fetch.
        // See task_list.rs for the full rationale.
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            match api::list_artifacts(Some(&pid), None, None, None).await {
                Ok(items) => {
                    let latest_seq =
                        fetch_seq.with_untracked(|m| m.get(&pid).copied().unwrap_or(0));
                    if latest_seq != my_seq {
                        return; // superseded by a newer fetch for this key
                    }
                    cache.update(|m| {
                        m.insert(pid.clone(), items);
                    });
                }
                Err(err) => {
                    leptos::logging::log!("list_artifacts failed for project={pid}: {err:?}");
                    fetch_error.set(Some(err.friendly()));
                }
            }
        });
    });

    // 2) Any Channel::Artifacts event → drop the cache and bump the trigger
    //    so effect 1 refetches.
    Effect::new(move |_| {
        let len = ws_events.with(|v| v.len());
        let start = applied_cursor.get_untracked();
        if start >= len {
            return;
        }
        let saw_artifact_event = ws_events.with_untracked(|evs| {
            evs[start..len]
                .iter()
                .any(|env: &EventEnvelope| env.payload.channel() == Channel::Artifacts)
        });
        applied_cursor.set(len);
        if saw_artifact_event {
            cache.update(|m| m.clear());
            refetch_trigger.update(|n| *n = n.wrapping_add(1));
        }
    });

    view! {
        {move || {
            match current_filter.get() {
                ProjectFilter::Of(_) => {
                    view! {
                        <div class="artifacts-panel">
                            <div class="artifacts-header">
                                <span class="artifacts-title">"Artifacts"</span>
                            </div>
                            {move || {
                                if let Some(err) = fetch_error.get() {
                                    view! {
                                        <p class="fetch-error__message">{err}</p>
                                    }.into_any()
                                } else if !loaded.get() {
                                    view! {
                                        <div class="artifacts-empty">"Loading…"</div>
                                    }.into_any()
                                } else {
                                    let items = artifacts.get();
                                    if items.is_empty() {
                                        view! {
                                            <p class="artifacts-empty">"No artifacts yet."</p>
                                        }.into_any()
                                    } else {
                                        view! {
                                            <ul class="artifact-list">
                                                <For
                                                    each={ let items = items.clone(); move || items.clone() }
                                                    key=|a: &ArtifactRow| a.id.clone()
                                                    let:artifact
                                                >
                                                    <ArtifactRowView artifact=artifact />
                                                </For>
                                            </ul>
                                        }.into_any()
                                    }
                                }
                            }}
                        </div>
                    }
                    .into_any()
                }
                _ => view! { <div class="artifacts-aside-hidden" /> }.into_any(),
            }
        }}
    }
}

// ── ArtifactRowView ──────────────────────────────────────────────────────────

#[component]
fn ArtifactRowView(artifact: ArtifactRow) -> impl IntoView {
    let expanded = RwSignal::new(false);
    let impact: RwSignal<Option<Result<GraphNeighborhood, String>>> = RwSignal::new(None);

    let artifact_id = artifact.id.clone();
    let kind = artifact.kind.clone();
    let title = artifact.title.clone();
    let status = artifact.status.clone();

    let is_deprecated = status == "deprecated";
    let step = status_step(&status);
    let status_title = status.clone();

    let meta_text = {
        let mut parts = Vec::new();
        if let Some(o) = &artifact.owner_agent_id {
            parts.push(format!("owner {}", short_id(o)));
        }
        if let Some(h) = &artifact.current_holder_agent_id {
            parts.push(format!("held by {}", short_id(h)));
        }
        if let Some(v) = &artifact.version {
            parts.push(format!("v{v}"));
        }
        parts.join(" · ")
    };

    let on_toggle = move |_| {
        expanded.update(|v| *v = !*v);
        // Lazy-load impact on first expand.
        if expanded.get_untracked() && impact.get_untracked().is_none() {
            let id = artifact_id.clone();
            leptos::task::spawn_local_scoped_with_cancellation(async move {
                let res = api::artifact_impact(&id, None)
                    .await
                    .map_err(|e| e.friendly());
                impact.set(Some(res));
            });
        }
    };

    view! {
        <li class="artifact-row-wrapper">
            <div class="artifact-row" on:click=on_toggle>
                <span class=kind_class(&kind)>{kind.clone()}</span>
                <span class="artifact-title">{title}</span>
                { if is_deprecated {
                    view! {
                        <span class="artifact-status-deprecated">"deprecated"</span>
                    }.into_any()
                } else {
                    let steps: Vec<AnyView> = LADDER_LABELS
                        .iter()
                        .enumerate()
                        .map(|(i, label)| {
                            let filled = step.is_some_and(|s| i <= s);
                            let cls = if filled {
                                "artifact-status-ladder__step artifact-status-ladder__step--filled"
                            } else {
                                "artifact-status-ladder__step"
                            };
                            view! { <span class=cls title=*label></span> }.into_any()
                        })
                        .collect();
                    view! {
                        <span class="artifact-status-ladder" title=status_title>{steps}</span>
                    }.into_any()
                }}
                { if meta_text.is_empty() {
                    view! { <span></span> }.into_any()
                } else {
                    view! { <span class="artifact-meta">{meta_text}</span> }.into_any()
                }}
            </div>
            <Show when=move || expanded.get() fallback=|| view! { <></> }>
                <div class="artifact-body">
                    {move || match impact.get() {
                        None => view! {
                            <div class="artifact-impact-loading">"loading impact…"</div>
                        }.into_any(),
                        Some(Err(err)) => view! {
                            <div class="artifact-impact-error">{err}</div>
                        }.into_any(),
                        Some(Ok(neigh)) => render_impact(&neigh),
                    }}
                </div>
            </Show>
        </li>
    }
}

/// Node label: title if non-empty, else a text preview, else the bare
/// source id — the same fallback chain a workspace-graph node display uses.
fn node_label(node: &api::GraphNode) -> String {
    if !node.title.trim().is_empty() {
        node.title.clone()
    } else if !node.text.trim().is_empty() {
        node.text.chars().take(60).collect()
    } else {
        node.source_id.clone()
    }
}

/// Plain list render of an impact neighborhood — nodes, then edges with
/// endpoints resolved to labels where possible. No graph visualization.
fn render_impact(neigh: &GraphNeighborhood) -> AnyView {
    if neigh.nodes.is_empty() && neigh.edges.is_empty() {
        return view! {
            <p class="artifact-impact-empty">"Nothing depends on or implements this artifact yet."</p>
        }
        .into_any();
    }

    let labels: HashMap<&str, String> = neigh
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), node_label(n)))
        .collect();

    let nodes: Vec<AnyView> = neigh
        .nodes
        .iter()
        .map(|n| {
            view! {
                <li class="artifact-impact-node">
                    <span class="artifact-impact-node__kind">{n.kind.clone()}</span>
                    <span class="artifact-impact-node__label">{node_label(n)}</span>
                </li>
            }
            .into_any()
        })
        .collect();

    let edges: Vec<AnyView> = neigh
        .edges
        .iter()
        .map(|e| {
            let from = labels
                .get(e.from_id.as_str())
                .cloned()
                .unwrap_or_else(|| e.from_id.clone());
            let to = labels
                .get(e.to_id.as_str())
                .cloned()
                .unwrap_or_else(|| e.to_id.clone());
            view! {
                <li class="artifact-impact-edge">
                    <span class="artifact-impact-edge__from">{from}</span>
                    <span class="artifact-impact-edge__kind">{e.kind.clone()}</span>
                    <span class="artifact-impact-edge__to">{to}</span>
                </li>
            }
            .into_any()
        })
        .collect();

    view! {
        <div class="artifact-impact">
            <div class="artifact-impact-summary">
                {format!("{} node(s) · {} edge(s)", neigh.nodes.len(), neigh.edges.len())}
            </div>
            <ul class="artifact-impact-nodes">{nodes}</ul>
            <ul class="artifact-impact-edges">{edges}</ul>
        </div>
    }
    .into_any()
}
