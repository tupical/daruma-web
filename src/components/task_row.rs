use super::fmt::{short_id, status_class, status_label};
use crate::api::{self, TaskRelations};
use crate::projects_ctx::{ProjectFilter, ProjectsCtx};
use crate::relations_ctx::RelationsCtx;
use daruma_domain::{Actor, Priority, Relation, RelationKind, Task};
use daruma_shared::ProjectId;
use leptos::prelude::*;

#[component]
pub fn TaskRow(task: Task) -> impl IntoView {
    let projects_ctx = use_context::<ProjectsCtx>().expect("ProjectsCtx");
    let relations_ctx = use_context::<RelationsCtx>().expect("RelationsCtx");
    let expanded = RwSignal::new(false);

    let task_id_str = task.id.to_string();
    let task_id_short = short_id(&task_id_str);
    let task_id_for_copy = task_id_str.clone();
    let task_id_for_relations = task_id_str.clone();
    let title = task.title.clone();
    let description = task.description.clone();
    let status = task.status;
    let priority = task.priority;
    let project_id: Option<ProjectId> = task.project_id;

    let this_task_id = task.id;

    // Authorship is read directly from denormalized Task fields. The server
    // writes these on TaskCreated/TaskCompleted and the UI receives them in the
    // normal task-list refetch.
    let created_by = task.created_by.clone();
    let completed_by = task.completed_by.clone();

    // Surface AI-authored rows in the collapsed view so idea triage remains
    // scannable without expanding every row.
    let created_by_is_agent = created_by.as_ref().map(Actor::is_agent).unwrap_or(false);
    let ai_creator_name: Option<String> = created_by.as_ref().and_then(|a| match a {
        Actor::Agent { name, .. } => Some(name.clone()),
        Actor::User => None,
    });

    // Relations are fetched lazily on first expand.
    // `None` = not loaded yet; `Some(Err(_))` = load failed; `Some(Ok(_))` = ready.
    let relations: RwSignal<Option<Result<TaskRelations, String>>> = RwSignal::new(None);

    let row_class = if status == daruma_domain::Status::Done {
        "task-row done"
    } else {
        "task-row"
    };

    // task_row decorates Done/Cancelled with ✓/✗; the other statuses take
    // the shared undecorated label.
    let status_label = match status {
        daruma_domain::Status::Done => "✓ Done",
        daruma_domain::Status::Cancelled => "✗ Cancelled",
        s => status_label(s),
    };

    let status_class = status_class(status);

    let filter = projects_ctx.current_filter;
    let names = projects_ctx.names;

    let on_toggle = move |_| {
        expanded.update(|v| *v = !*v);
        // Lazy-load relations on first expand.
        if expanded.get_untracked() && relations.get_untracked().is_none() {
            let id = task_id_for_relations.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let res = api::list_task_relations(&id)
                    .await
                    .map_err(|e| e.to_string());
                relations.set(Some(res));
            });
        }
    };

    let on_copy_id = {
        let id_for_copy = task_id_for_copy.clone();
        move |e: web_sys::MouseEvent| {
            e.stop_propagation();
            let id = id_for_copy.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Some(win) = web_sys::window() {
                    let clipboard = win.navigator().clipboard();
                    let promise = clipboard.write_text(&id);
                    if let Err(err) = wasm_bindgen_futures::JsFuture::from(promise).await {
                        leptos::logging::log!("clipboard write failed: {:?}", err);
                    }
                }
            });
        }
    };

    let (priority_class, priority_symbol, priority_title) = priority_repr(priority);
    let priority_href = format!("#{priority_symbol}");

    view! {
        <li class="task-row-wrapper">
            <div class=row_class on:click=on_toggle>
                <svg class=priority_class aria-label=priority_title>
                    <use href=priority_href />
                </svg>
                <span
                    class="id-badge"
                    title="Copy task id"
                    on:click=on_copy_id
                >{"#"}{task_id_short}</span>
                <span class="title">{title}</span>

                // "From AI" badge for rows whose creator is an Agent.
                { if created_by_is_agent {
                    let tooltip = ai_creator_name
                        .as_deref()
                        .map(|n| format!("Created by AI agent: {n}"))
                        .unwrap_or_else(|| "Created by AI agent".to_string());
                    view! {
                        <span class="ai-badge" title=tooltip>"From AI"</span>
                    }.into_any()
                } else {
                    view! { <span></span> }.into_any()
                }}

                // Status pill — replaced by a lock indicator when the task is
                // blocked by another open task.  Done tasks always show their
                // status (the blockers no longer gate progress).
                { move || {
                    let map = relations_ctx.counts.get();
                    let c = map.get(&this_task_id).copied().unwrap_or_default();
                    let is_done = status == daruma_domain::Status::Done;
                    if c.blocked_by > 0 && !is_done {
                        let title_attr = format!("blocked by {} task(s)", c.blocked_by);
                        view! {
                            <span class="status status-blocked" title=title_attr>
                                {"🔒 blocked"}
                            </span>
                        }
                        .into_any()
                    } else {
                        view! { <span class=status_class>{status_label}</span> }.into_any()
                    }
                }}

                // Project chip — hidden when filter != All
                { move || {
                    let f = filter.get();
                    if f != ProjectFilter::All {
                        return view! { <span></span> }.into_any();
                    }
                    let chip_class = if project_id.is_none() {
                        "project-chip inbox"
                    } else {
                        "project-chip"
                    };
                    let chip_text = match project_id {
                        Some(pid) => {
                            let n = names.get();
                            n.get(&pid).cloned().unwrap_or_else(|| "project".to_string())
                        }
                        None => "inbox".to_string(),
                    };
                    view! {
                        <span class=chip_class>{chip_text}</span>
                    }.into_any()
                }}

            </div>

            // Expanded body with description + relations
            { move || {
                if expanded.get() {
                    let desc = description.clone();
                    let desc_section = if desc.trim().is_empty() {
                        view! { <div class="task-body-section empty">"(no description)"</div> }.into_any()
                    } else {
                        view! { <div class="task-body-section">{desc}</div> }.into_any()
                    };

                    let relations_section = move || match relations.get() {
                        None => view! {
                            <div class="task-body-section relations-loading">"loading relations…"</div>
                        }.into_any(),
                        Some(Err(err)) => view! {
                            <div class="task-body-section relations-error">{format!("relations: {err}")}</div>
                        }.into_any(),
                        Some(Ok(rels)) => render_relations(this_task_id, &rels),
                    };

                    let actors_section = render_actors(created_by.as_ref(), completed_by.as_ref());

                    view! {
                        <div class="task-body">
                            {desc_section}
                            {actors_section}
                            {relations_section}
                        </div>
                    }.into_any()
                } else {
                    view! { <span></span> }.into_any()
                }
            }}
        </li>
    }
}

/// Render the 5-group relation projection as nested sections.
/// Each group is hidden when empty; entire block collapses when all are empty.
fn render_relations(
    self_id: daruma_shared::TaskId,
    rels: &TaskRelations,
) -> leptos::prelude::AnyView {
    use leptos::prelude::*;

    let total = rels.blocks.len()
        + rels.blocked_by.len()
        + rels.relates_to.len()
        + rels.duplicates.len()
        + rels.duplicated_by.len();

    if total == 0 {
        return view! {
            <div class="task-body-section relations-empty">"(no links)"</div>
        }
        .into_any();
    }

    let blocks = relation_group("→ blocks", "relation-group blocks", self_id, &rels.blocks);
    let blocked_by = relation_group(
        "🔒 blocked by",
        "relation-group blocked-by",
        self_id,
        &rels.blocked_by,
    );
    let relates = relation_group(
        "↔ relates to",
        "relation-group relates",
        self_id,
        &rels.relates_to,
    );
    let duplicates = relation_group(
        "≡ duplicates",
        "relation-group duplicates",
        self_id,
        &rels.duplicates,
    );
    let duplicated_by = relation_group(
        "≡ duplicated by",
        "relation-group duplicated-by",
        self_id,
        &rels.duplicated_by,
    );

    view! {
        <div class="task-body-section relations">
            {blocks}
            {blocked_by}
            {relates}
            {duplicates}
            {duplicated_by}
        </div>
    }
    .into_any()
}

/// Render one labelled list of relation peers. Returns empty span if list is empty.
/// `self_id` is used to pick the "other side" task id of each Relation.
fn relation_group(
    label: &'static str,
    css_class: &'static str,
    self_id: daruma_shared::TaskId,
    rels: &[Relation],
) -> leptos::prelude::AnyView {
    use leptos::prelude::*;

    if rels.is_empty() {
        return view! { <span></span> }.into_any();
    }

    let chips: Vec<_> = rels
        .iter()
        .map(|r| {
            let peer = if r.from == self_id { r.to } else { r.from };
            let peer_full = peer.to_string();
            let peer_short = short_id(&peer_full);
            let kind_label = match r.kind {
                RelationKind::Blocks => "blocks",
                RelationKind::RelatesTo => "relates_to",
                RelationKind::Duplicates => "duplicates",
                RelationKind::WasBlocking => "was_blocking",
            };
            let title_attr = format!("{kind_label} · {peer_full}");
            view! {
                <span class="relation-chip" title=title_attr>{"#"}{peer_short}</span>
            }
        })
        .collect();

    view! {
        <div class=css_class>
            <span class="relation-label">{label}</span>
            <span class="relation-chips">{chips}</span>
        </div>
    }
    .into_any()
}

/// CSS class + sprite-symbol id + tooltip for `Priority`.
/// Sprite symbols live in `apps/web-leptos/index.html`.
fn priority_repr(priority: Priority) -> (&'static str, &'static str, &'static str) {
    match priority {
        Priority::P0 => ("priority priority-p0", "pri-p0", "p0 · urgent"),
        Priority::P1 => ("priority priority-p1", "pri-p1", "p1 · high"),
        Priority::P2 => ("priority priority-p2", "pri-p2", "p2 · medium"),
        Priority::P3 => ("priority priority-p3", "pri-p3", "p3 · low"),
    }
}

/// Render creator/completer actor chips. Hidden when both are `None`.
fn render_actors(
    created_by: Option<&Actor>,
    completed_by: Option<&Actor>,
) -> leptos::prelude::AnyView {
    use leptos::prelude::*;

    if created_by.is_none() && completed_by.is_none() {
        return view! { <span></span> }.into_any();
    }

    let creator_chip = created_by.map(|a| {
        let label = actor_label(a);
        view! {
            <span class="actor-chip actor-creator" title="Created by">{label}</span>
        }
    });
    let completer_chip = completed_by.map(|a| {
        let label = actor_label(a);
        view! {
            <span class="actor-chip actor-completer" title="Completed by">{label}</span>
        }
    });

    view! {
        <div class="task-body-section actors">
            {creator_chip}
            {completer_chip}
        </div>
    }
    .into_any()
}

/// Format an [`Actor`] as a short display string.
fn actor_label(actor: &Actor) -> String {
    match actor {
        Actor::User => "user".to_string(),
        Actor::Agent { name, .. } => name.clone(),
    }
}
