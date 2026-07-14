//! Documents panel.
//!
//! Shown on the right rail when a single project is selected. Lists the
//! project's `Document`s (auto-created `Interview` + `Human Log` plus any
//! user-created additions), each rendered as a collapsible card with a
//! read-only view of the raw markdown body.
//!
//! There is no in-browser markdown rendering library wired into this crate,
//! so the read-only view shows the raw markdown inside a `<pre>` block —
//! whitespace-preserving and adequate for the artefacts these documents
//! hold (Interview transcripts, Human Log entries).
//!
//! VIZ-7 adds a second, lazy view per card: a "narrative" feed reconstructed
//! from the document's own `Channel::Documents` events (`DocumentCreated` /
//! `ContentAppended` / `ContentReplaced` / `Renamed` / `Archived`), each shown
//! as one authored, timestamped block — this is what actually carries "who
//! wrote this paragraph and when", which the `Document` snapshot itself
//! doesn't (it's a single flat `content: String`). No new server endpoint was
//! needed: `EventStoreCtx.graph_events` already replays full history from
//! seq 0 on cold start and is already Documents-inclusive.

use crate::api;
use crate::event_store::EventStoreCtx;
use crate::projects_ctx::{ProjectFilter, ProjectsCtx};
use crate::ws::WsCtx;
use daruma_domain::{Actor, Document, DocumentKind};
use daruma_events::{Channel, Event, EventEnvelope};
use leptos::prelude::*;
use std::collections::HashMap;

fn kind_label(k: DocumentKind) -> &'static str {
    match k {
        DocumentKind::Interview => "interview",
        DocumentKind::HumanLog => "human log",
    }
}

// ── Freshness + timestamp formatting ────────────────────────────────────────

fn format_ts(ts: daruma_shared::time::Timestamp) -> String {
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

/// Relative "how fresh" label — falls back to an absolute timestamp past a
/// week, since "42d ago" stops being a useful unit at that point.
fn freshness_label(ts: daruma_shared::time::Timestamp) -> String {
    use chrono::Utc;
    let dt: chrono::DateTime<Utc> = ts.into();
    let secs = Utc::now().signed_duration_since(dt).num_seconds();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else if secs < 86400 * 7 {
        format!("{}d ago", secs / 86400)
    } else {
        format_ts(ts)
    }
}

// ── Entity-reference recognition ────────────────────────────────────────────
//
// PR #6's auto-narrative text mentions tasks/plans/artifacts by their
// prefixed id form (`tsk_<uuid>`, `pln_<uuid>`, `art_<uuid>` — see
// daruma_shared::ids). Recognized here and rendered as small kind-labeled
// badges. No actual cross-page navigation is wired up: there's no "focus
// this node" param on the /graph route and no id-filter in the activity
// feed to jump to (same gap already flagged and deliberately deferred for
// the equivalent VIZ-6 task-in-graph-to-feed ask) — this is the cheap,
// useful half: a reader can see and copy what's being referenced.

#[derive(Clone, Copy, PartialEq)]
enum EntityRefKind {
    Task,
    Plan,
    Artifact,
}

impl EntityRefKind {
    fn label(self) -> &'static str {
        match self {
            EntityRefKind::Task => "task",
            EntityRefKind::Plan => "plan",
            EntityRefKind::Artifact => "artifact",
        }
    }

    fn css_class(self) -> &'static str {
        match self {
            EntityRefKind::Task => "narrative-ref narrative-ref--task",
            EntityRefKind::Plan => "narrative-ref narrative-ref--plan",
            EntityRefKind::Artifact => "narrative-ref narrative-ref--artifact",
        }
    }

    fn graph_kind(self) -> &'static str {
        self.label()
    }
}

const ENTITY_PREFIXES: &[(&str, EntityRefKind)] = &[
    ("tsk_", EntityRefKind::Task),
    ("pln_", EntityRefKind::Plan),
    ("art_", EntityRefKind::Artifact),
];

fn is_id_char(c: char) -> bool {
    c.is_ascii_hexdigit() || c == '-'
}

/// Split `text` into plain runs and recognized entity-id tokens, rendering
/// the latter as small badges. Uses `match_indices`/`chars()` throughout —
/// no manual byte-offset arithmetic — since this prose is full of em-dashes
/// and other multi-byte characters and a naive byte-stepping scanner would
/// panic slicing mid-character.
fn linkify_entity_refs(text: &str) -> Vec<AnyView> {
    let mut matches: Vec<(usize, usize, EntityRefKind)> = Vec::new();
    for (prefix, kind) in ENTITY_PREFIXES {
        for (start, _) in text.match_indices(prefix) {
            let boundary_ok = text[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric());
            if !boundary_ok {
                continue;
            }
            let id_start = start + prefix.len();
            let id_len: usize = text[id_start..]
                .chars()
                .take_while(|&c| is_id_char(c))
                .map(char::len_utf8)
                .sum();
            if id_len >= 8 {
                matches.push((start, id_start + id_len, *kind));
            }
        }
    }
    matches.sort_by_key(|m| m.0);
    let mut filtered: Vec<(usize, usize, EntityRefKind)> = Vec::new();
    let mut last_end = 0usize;
    for m in matches {
        if m.0 >= last_end {
            last_end = m.1;
            filtered.push(m);
        }
    }

    let mut out = Vec::new();
    let mut cursor = 0usize;
    for (start, end, kind) in filtered {
        if cursor < start {
            out.push(view! { <span>{text[cursor..start].to_string()}</span> }.into_any());
        }
        let id_text = text[start..end].to_string();
        let href = format!("/graph?node={}:{}", kind.graph_kind(), id_text);
        out.push(
            view! {
                <a class=kind.css_class() title=kind.label() href=href>{id_text}</a>
            }
            .into_any(),
        );
        cursor = end;
    }
    if cursor < text.len() {
        out.push(view! { <span>{text[cursor..].to_string()}</span> }.into_any());
    }
    out
}

// ── Narrative feed ───────────────────────────────────────────────────────────

#[derive(Clone)]
enum NarrativeBlockKind {
    Created { initial_content: Option<String> },
    Appended { text: String },
    Replaced { text: String },
    Renamed { title: String },
    Archived,
}

#[derive(Clone)]
struct NarrativeBlock {
    at: daruma_shared::time::Timestamp,
    actor: Actor,
    kind: NarrativeBlockKind,
}

fn actor_label(actor: &Actor) -> String {
    match actor {
        Actor::User => "user".to_string(),
        Actor::Agent { name, .. } => name.clone(),
    }
}

fn actor_chip_class(actor: &Actor) -> &'static str {
    match actor {
        Actor::User => "actor-chip actor-user",
        Actor::Agent { .. } => "actor-chip actor-agent",
    }
}

/// Reconstruct one document's narrative from the shared event log —
/// `events` is expected chronological (append-only), so the result is too.
fn build_narrative(
    doc_id: daruma_shared::DocumentId,
    events: &[EventEnvelope],
) -> Vec<NarrativeBlock> {
    let mut blocks = Vec::new();
    for env in events {
        if env.payload.channel() != Channel::Documents {
            continue;
        }
        let kind = match &env.payload {
            Event::DocumentCreated { document } if document.id == doc_id => {
                let initial = if document.content.trim().is_empty() {
                    None
                } else {
                    Some(document.content.clone())
                };
                Some(NarrativeBlockKind::Created {
                    initial_content: initial,
                })
            }
            Event::DocumentContentAppended {
                document_id,
                append,
                ..
            } if *document_id == doc_id => Some(NarrativeBlockKind::Appended {
                text: append.clone(),
            }),
            Event::DocumentContentReplaced {
                document_id,
                content,
                ..
            } if *document_id == doc_id => Some(NarrativeBlockKind::Replaced {
                text: content.clone(),
            }),
            Event::DocumentRenamed {
                document_id, title, ..
            } if *document_id == doc_id => Some(NarrativeBlockKind::Renamed {
                title: title.clone(),
            }),
            Event::DocumentArchived { document_id, .. } if *document_id == doc_id => {
                Some(NarrativeBlockKind::Archived)
            }
            _ => None,
        };
        if let Some(kind) = kind {
            blocks.push(NarrativeBlock {
                at: env.occurred_at,
                actor: env.actor.clone(),
                kind,
            });
        }
    }
    blocks
}

fn render_narrative_block(block: &NarrativeBlock) -> AnyView {
    let (action, body) = match &block.kind {
        NarrativeBlockKind::Created { initial_content } => {
            ("created the document".to_string(), initial_content.clone())
        }
        NarrativeBlockKind::Appended { text } => ("appended".to_string(), Some(text.clone())),
        NarrativeBlockKind::Replaced { text } => {
            ("replaced the content".to_string(), Some(text.clone()))
        }
        NarrativeBlockKind::Renamed { title } => (format!("renamed it to \"{title}\""), None),
        NarrativeBlockKind::Archived => ("archived the document".to_string(), None),
    };

    view! {
        <div class="narrative-block">
            <div class="narrative-block__meta">
                <span class=actor_chip_class(&block.actor)>
                    {actor_label(&block.actor).chars().next().unwrap_or('?').to_string()}
                </span>
                <span class="narrative-block__author">{actor_label(&block.actor)}</span>
                <span class="narrative-block__action">{action}</span>
                <span class="narrative-block__time">{format_ts(block.at)}</span>
            </div>
            { body.filter(|b| !b.trim().is_empty()).map(|b| view! {
                <div class="narrative-block__body">{linkify_entity_refs(&b)}</div>
            })}
        </div>
    }
    .into_any()
}

fn render_narrative_feed(doc_id: daruma_shared::DocumentId, events: &[EventEnvelope]) -> AnyView {
    let blocks = build_narrative(doc_id, events);
    if blocks.is_empty() {
        return view! {
            <p class="document-card__empty">
                "No narrative history yet (older than the client's replay window, or created before this feature)."
            </p>
        }
        .into_any();
    }
    let rows: Vec<AnyView> = blocks.iter().map(render_narrative_block).collect();
    view! {
        <div class="document-card__narrative">{rows}</div>
    }
    .into_any()
}

/// Apply one WS event to a single per-project document list. Idempotent by id.
/// Archived documents are dropped from the list — the panel only renders
/// non-archived ones, matching `list_project_documents`' default behaviour.
fn apply_document_event(env: &EventEnvelope, list: &mut Vec<Document>, project_id: &str) {
    match &env.payload {
        Event::DocumentCreated { document } => {
            if document.project_id.to_string() != project_id {
                return;
            }
            if document.archived_at.is_some() {
                return;
            }
            if !list.iter().any(|d| d.id == document.id) {
                list.push(document.clone());
            }
        }
        Event::DocumentContentReplaced {
            document_id,
            content,
            at,
        } => {
            if let Some(d) = list.iter_mut().find(|d| d.id == *document_id) {
                d.content = content.clone();
                d.updated_at = *at;
            }
        }
        Event::DocumentContentAppended {
            document_id,
            append,
            at,
        } => {
            if let Some(d) = list.iter_mut().find(|d| d.id == *document_id) {
                d.content.push_str(append);
                d.updated_at = *at;
            }
        }
        Event::DocumentRenamed {
            document_id,
            title,
            at,
        } => {
            if let Some(d) = list.iter_mut().find(|d| d.id == *document_id) {
                d.title = title.clone();
                d.updated_at = *at;
            }
        }
        Event::DocumentArchived { document_id, .. } => {
            list.retain(|d| d.id != *document_id);
        }
        _ => {}
    }
}

#[component]
pub fn DocumentsPanel() -> impl IntoView {
    let projects_ctx = use_context::<ProjectsCtx>().expect("ProjectsCtx");
    let ws_ctx = use_context::<WsCtx>().expect("WsCtx");
    let event_store = use_context::<EventStoreCtx>().expect("EventStoreCtx");
    let current_filter = projects_ctx.current_filter;
    let ws_events = ws_ctx.events;

    // Derive project_id from filter — only Some when a specific project is
    // selected; "All" and "Inbox" hide the panel entirely.
    let project_id_opt = Memo::new(move |_| match current_filter.get() {
        ProjectFilter::Of(pid) => Some(pid.to_string()),
        _ => None,
    });

    // Per-project document cache, kept in sync via WS apply.
    let cache: RwSignal<HashMap<String, Vec<Document>>> = RwSignal::new(HashMap::new());
    let applied_cursor: RwSignal<usize> = RwSignal::new(0);
    let fetch_seq: RwSignal<HashMap<String, u64>> = RwSignal::new(HashMap::new());
    // Most recent fetch failure for the current project, if any — see
    // plans_panel.rs for the same pattern and rationale.
    let fetch_error: RwSignal<Option<String>> = RwSignal::new(None);

    let docs: Memo<Vec<Document>> = Memo::new(move |_| {
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

    // 1) Fetch only on first visit to a project — cache hit reuses WS-applied
    //    snapshot.
    Effect::new(move |_| {
        let Some(pid) = project_id_opt.get() else {
            return;
        };
        // Clear before the cache-hit check below: `fetch_error` isn't keyed
        // per-project, so a stale error from a previous failed project must
        // not linger over a different project that's actually a cache hit.
        fetch_error.set(None);
        if cache.with_untracked(|m| m.contains_key(&pid)) {
            return;
        }
        let snapshot_at = ws_events.with_untracked(|v| v.len());
        let my_seq = fetch_seq.with_untracked(|m| m.get(&pid).copied().unwrap_or(0)) + 1;
        fetch_seq.update(|m| {
            m.insert(pid.clone(), my_seq);
        });

        // Cancel-on-cleanup: the future reads component-owned signals
        // (`fetch_seq`) after the await, so a plain spawn would panic if the
        // route is disposed mid-fetch. See task_list.rs for the full rationale.
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            let mut ds = match api::list_project_documents(&pid).await {
                Ok(ds) => ds,
                Err(err) => {
                    leptos::logging::log!(
                        "list_project_documents failed for project={pid}: {err:?}"
                    );
                    fetch_error.set(Some(err.friendly()));
                    Vec::new()
                }
            };
            ws_events.with_untracked(|evs| {
                let now_len = evs.len();
                if snapshot_at < now_len {
                    for env in &evs[snapshot_at..now_len] {
                        apply_document_event(env, &mut ds, &pid);
                    }
                }
            });

            let latest_seq = fetch_seq.with_untracked(|m| m.get(&pid).copied().unwrap_or(0));
            if latest_seq != my_seq {
                return;
            }

            cache.update(|m| {
                m.insert(pid.clone(), ds);
            });
        });
    });

    // 2) Apply WS events to every cached snapshot.
    Effect::new(move |_| {
        let len = ws_events.with(|v| v.len());
        let start = applied_cursor.get_untracked();
        if start >= len {
            return;
        }
        ws_events.with_untracked(|evs| {
            cache.update(|m| {
                for env in &evs[start..len] {
                    for (pid, list) in m.iter_mut() {
                        apply_document_event(env, list, pid);
                    }
                }
            });
        });
        applied_cursor.set(len);
    });

    view! {
        {move || {
            match current_filter.get() {
                ProjectFilter::Of(_) => view! {
                    <div class="documents-panel">
                        <div class="documents-header">
                            <span class="documents-title">"Documents"</span>
                        </div>
                        <Show
                            when=move || loaded.get()
                            fallback=|| view! {
                                <div class="documents-empty">"Loading…"</div>
                            }
                        >
                            {move || {
                                let ds = docs.get();
                                if let Some(err) = fetch_error.get() {
                                    view! {
                                        <p class="fetch-error__message">{err}</p>
                                    }.into_any()
                                } else if ds.is_empty() {
                                    view! {
                                        <p class="documents-empty">"No documents yet."</p>
                                    }.into_any()
                                } else {
                                    view! {
                                        <div class="documents-list">
                                            <For
                                                each=move || docs.get()
                                                key=|d: &Document| d.id
                                                let:doc
                                            >
                                                {document_card_view(
                                                    doc.id,
                                                    doc.kind,
                                                    docs,
                                                    event_store.graph_events,
                                                )}
                                            </For>
                                        </div>
                                    }.into_any()
                                }
                            }}
                        </Show>
                    </div>
                }.into_any(),
                _ => view! { <div class="documents-aside-hidden" /> }.into_any(),
            }
        }}
    }
}

/// One document card: collapsible header + read-only body.
///
/// Plain function (not `#[component]`) to avoid `IntoView` type juggling when
/// collecting into `Vec<AnyView>`.
fn document_card_view(
    document_id: daruma_shared::DocumentId,
    kind: DocumentKind,
    docs: Memo<Vec<Document>>,
    events: ReadSignal<Vec<EventEnvelope>>,
) -> AnyView {
    let kind_lbl = kind_label(kind);
    let document = Memo::new(move |_| docs.get().into_iter().find(|d| d.id == document_id));

    // Track per-card collapsed state. Default collapsed for `human_log`
    // (can grow large), expanded for `interview` (typically the active
    // artefact during intake).
    let expanded = RwSignal::new(matches!(kind, DocumentKind::Interview));

    let on_toggle = move |_| {
        expanded.update(|v| *v = !*v);
    };

    let narrative = RwSignal::new(false);

    let raw_body = move || {
        let content = document.get().map(|d| d.content).unwrap_or_default();
        if content.trim().is_empty() {
            view! {
                <p class="document-card__empty">"(empty)"</p>
            }
            .into_any()
        } else {
            view! {
                <pre class="document-card__view">{content}</pre>
            }
            .into_any()
        }
    };

    view! {
        <div class="document-card">
            <button
                class="document-card__header"
                type="button"
                on:click=on_toggle
                aria-expanded=move || expanded.get().to_string()
            >
                <span class="document-card__toggle">
                    {move || if expanded.get() { "▾" } else { "▸" }}
                </span>
                <span class="document-card__title">
                    {move || document.get().map(|d| d.title).unwrap_or_default()}
                </span>
                <span class="document-card__kind">{kind_lbl}</span>
                <span class="document-card__freshness">
                    {move || document.get().map(|d| freshness_label(d.updated_at)).unwrap_or_default()}
                </span>
            </button>
            <Show when=move || expanded.get() fallback=|| view! { <></> }>
                <div class="document-card__body">
                    <button
                        type="button"
                        class="document-card__mode"
                        on:click=move |_| narrative.update(|v| *v = !*v)
                    >
                        {move || if narrative.get() { "Raw document" } else { "Narrative" }}
                    </button>
                    {move || if narrative.get() {
                        events.with(|events| render_narrative_feed(document_id, events))
                    } else {
                        raw_body()
                    }}
                </div>
            </Show>
        </div>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use daruma_shared::{time, DocumentId, ProjectId};

    #[test]
    fn narrative_keeps_only_the_selected_document_in_event_order() {
        let now = time::now();
        let document_id = DocumentId::new();
        let other_id = DocumentId::new();
        let document = Document {
            id: document_id,
            project_id: ProjectId::new(),
            kind: DocumentKind::Interview,
            title: "Interview".into(),
            content: "initial".into(),
            status: Default::default(),
            task_id: None,
            trigger_kind: None,
            consumer: None,
            created_at: now,
            updated_at: now,
            archived_at: None,
            last_read_at: None,
            last_read_by: None,
            read_count: 0,
        };
        let events = vec![
            EventEnvelope::new(Actor::user(), Event::DocumentCreated { document }),
            EventEnvelope::new(
                Actor::user(),
                Event::DocumentContentAppended {
                    document_id: other_id,
                    append: "ignore".into(),
                    at: now,
                },
            ),
            EventEnvelope::new(
                Actor::user(),
                Event::DocumentContentAppended {
                    document_id,
                    append: "next".into(),
                    at: now,
                },
            ),
        ];

        let blocks = build_narrative(document_id, &events);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(blocks[0].kind, NarrativeBlockKind::Created { .. }));
        assert!(matches!(
            &blocks[1].kind,
            NarrativeBlockKind::Appended { text } if text == "next"
        ));
    }
}
