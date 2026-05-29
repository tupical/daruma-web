//! Documents panel — PR1 §10 task #8.
//!
//! Shown on the right rail when a single project is selected. Lists the
//! project's `Document`s (auto-created `Interview` + `Human Log` plus any
//! user-created additions), each rendered as a collapsible card with a
//! read-only view of the raw markdown body and an inline `Edit` →
//! `<textarea>` + `Save` flow that issues `PATCH /v1/documents/{id}` with
//! `{content}`.
//!
//! There is no in-browser markdown rendering library wired into this crate,
//! so the read-only view shows the raw markdown inside a `<pre>` block —
//! whitespace-preserving and adequate for the artefacts these documents
//! hold (Interview transcripts, Human Log entries).

use crate::api::{self, DocumentPatch};
use crate::projects_ctx::{ProjectFilter, ProjectsCtx};
use crate::ws::WsCtx;
use leptos::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use taskagent_domain::{Document, DocumentKind};
use taskagent_events::{Event, EventEnvelope};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

fn kind_label(k: DocumentKind) -> &'static str {
    match k {
        DocumentKind::Interview => "interview",
        DocumentKind::HumanLog => "human log",
    }
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
        if cache.with_untracked(|m| m.contains_key(&pid)) {
            return;
        }
        let snapshot_at = ws_events.with_untracked(|v| v.len());
        let my_seq = fetch_seq.with_untracked(|m| m.get(&pid).copied().unwrap_or(0)) + 1;
        fetch_seq.update(|m| {
            m.insert(pid.clone(), my_seq);
        });

        spawn_local(async move {
            let mut ds = api::list_project_documents(&pid).await.unwrap_or_default();
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
                                if ds.is_empty() {
                                    view! {
                                        <p class="documents-empty">"No documents yet."</p>
                                    }.into_any()
                                } else {
                                    let cards: Vec<AnyView> = ds
                                        .into_iter()
                                        .map(document_card_view)
                                        .collect();
                                    view! {
                                        <div class="documents-list">{cards}</div>
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

/// One document card: collapsible header + body (view / edit modes).
///
/// Plain function (not `#[component]`) to keep parity with `plan_node_view`
/// in `plans_panel.rs` — avoids `IntoView` type juggling when collecting
/// into `Vec<AnyView>`.
fn document_card_view(doc: Document) -> AnyView {
    // `Rc` so handler closures (Fn, called many times) can clone cheaply
    // without taking ownership.
    let doc_id_str: Arc<String> = Arc::new(doc.id.to_string());
    let title = doc.title.clone();
    let kind = doc.kind;
    let kind_lbl = kind_label(kind);
    let initial_content: Arc<String> = Arc::new(doc.content.clone());

    // Track per-card collapsed state. Default collapsed for `human_log`
    // (can grow large), expanded for `interview` (typically the active
    // artefact during intake).
    let expanded = RwSignal::new(matches!(kind, DocumentKind::Interview));

    // Edit-mode state. `draft` holds the in-flight textarea contents; it
    // resets to the document's current body whenever Edit is (re-)entered.
    let editing = RwSignal::new(false);
    let draft: RwSignal<String> = RwSignal::new(doc.content.clone());
    // Surfaced inline below the textarea when PATCH fails.
    let save_error: RwSignal<Option<String>> = RwSignal::new(None);
    // Disables the Save/Cancel buttons while a request is in flight.
    let saving = RwSignal::new(false);

    let on_toggle = move |_| {
        expanded.update(|v| *v = !*v);
    };

    let on_textarea_input = move |ev: web_sys::Event| {
        if let Some(target) = ev.target() {
            if let Ok(ta) = target.dyn_into::<web_sys::HtmlTextAreaElement>() {
                draft.set(ta.value());
            }
        }
    };

    // Body section — switches between view-mode (pre + Edit) and
    // edit-mode (textarea + Cancel/Save). Each handler is recreated on
    // every render of this `move ||` block so the outer closure stays
    // `Fn`-compatible without juggling `Arc<dyn Fn>` wrappers.
    let initial_for_body = Arc::clone(&initial_content);
    let id_for_body = Arc::clone(&doc_id_str);
    let body = move || {
        if editing.get() {
            let id_for_save = Arc::clone(&id_for_body);
            let on_cancel = move |e: web_sys::MouseEvent| {
                e.stop_propagation();
                editing.set(false);
                save_error.set(None);
            };
            let on_save = move |e: web_sys::MouseEvent| {
                e.stop_propagation();
                if saving.get_untracked() {
                    return;
                }
                saving.set(true);
                save_error.set(None);
                let id = (*id_for_save).clone();
                let next = draft.get_untracked();
                wasm_bindgen_futures::spawn_local(async move {
                    let patch = DocumentPatch {
                        title: None,
                        content: Some(next),
                    };
                    match api::patch_document(&id, &patch).await {
                        Ok(_) => {
                            editing.set(false);
                            // The DocumentContentReplaced event arriving on
                            // Channel::Documents applies into the cache via
                            // the WS-apply Effect.
                        }
                        Err(err) => {
                            leptos::logging::log!("patch_document failed: {:?}", err);
                            save_error.set(Some(err.to_string()));
                        }
                    }
                    saving.set(false);
                });
            };
            view! {
                <textarea
                    class="document-card__textarea"
                    prop:value=move || draft.get()
                    on:input=on_textarea_input
                    rows="14"
                    spellcheck="false"
                />
                {move || save_error.get().map(|msg| view! {
                    <p class="document-card__error">
                        {format!("Save failed: {msg}")}
                    </p>
                })}
                <div class="document-card__actions">
                    <button
                        type="button"
                        class="document-card__btn"
                        on:click=on_cancel
                        disabled=move || saving.get()
                    >
                        "Cancel"
                    </button>
                    <button
                        type="button"
                        class="document-card__btn document-card__btn--primary"
                        on:click=on_save
                        disabled=move || saving.get()
                    >
                        {move || if saving.get() { "Saving…" } else { "Save" }}
                    </button>
                </div>
            }
            .into_any()
        } else {
            let body_text = (*initial_for_body).clone();
            let body_for_view = body_text.clone();
            let initial_for_edit = Arc::clone(&initial_for_body);
            let on_edit = move |e: web_sys::MouseEvent| {
                e.stop_propagation();
                draft.set((*initial_for_edit).clone());
                save_error.set(None);
                editing.set(true);
                expanded.set(true);
            };
            let view_body = if body_text.trim().is_empty() {
                view! {
                    <p class="document-card__empty">
                        "(empty — click Edit to add content)"
                    </p>
                }
                .into_any()
            } else {
                view! {
                    <pre class="document-card__view">{body_for_view}</pre>
                }
                .into_any()
            };
            view! {
                {view_body}
                <div class="document-card__actions">
                    <button
                        type="button"
                        class="document-card__btn"
                        on:click=on_edit
                    >
                        "Edit"
                    </button>
                </div>
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
                <span class="document-card__title">{title}</span>
                <span class="document-card__kind">{kind_lbl}</span>
            </button>
            <Show when=move || expanded.get() fallback=|| view! { <></> }>
                <div class="document-card__body">
                    {body.clone()}
                </div>
            </Show>
        </div>
    }
    .into_any()
}
