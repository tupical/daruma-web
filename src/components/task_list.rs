use super::task_row::TaskRow;
use crate::api;
use crate::projects_ctx::{ProjectFilter, ProjectsCtx};
use crate::relations_ctx::RelationsCtx;
use crate::ws::WsCtx;
use leptos::prelude::*;
use std::collections::{HashMap, HashSet};
use daruma_domain::{NewTask, Priority, Status, Task, TaskPatch};
use daruma_events::{Event, EventEnvelope};
use daruma_shared::{time::Timestamp, ProjectId, TaskId};
use wasm_bindgen_futures::spawn_local;

/// Display order for status groups. Groups missing from `tasks` are skipped.
const GROUP_ORDER: &[Status] = &[
    Status::InProgress,
    Status::InReview,
    Status::Todo,
    Status::Inbox,
    Status::Done,
    Status::Cancelled,
];

fn group_label(s: Status) -> &'static str {
    match s {
        Status::InProgress => "In Progress",
        Status::InReview => "In Review",
        Status::Todo => "Todo",
        // Inbox also acts as the idea bucket for unsorted intake.
        Status::Inbox => "Inbox · Idea",
        Status::Done => "Done",
        Status::Cancelled => "Cancelled",
    }
}

fn group_slug(s: Status) -> &'static str {
    match s {
        Status::InProgress => "in-progress",
        Status::InReview => "in-review",
        Status::Todo => "todo",
        Status::Inbox => "inbox",
        Status::Done => "done",
        Status::Cancelled => "cancelled",
    }
}

fn priority_rank(p: Priority) -> u8 {
    match p {
        Priority::P0 => 0,
        Priority::P1 => 1,
        Priority::P2 => 2,
        Priority::P3 => 3,
    }
}

fn sort_tasks(ts: &mut [Task]) {
    ts.sort_by_key(|t| priority_rank(t.priority));
}

/// Apply a WS `TaskUpdated` patch without calling `TaskPatch::apply` (that sets
/// `updated_at` via `time::now()`, which panics on wasm without chrono wasmbind).
fn apply_task_patch(patch: &TaskPatch, task: &mut Task, at: Timestamp) {
    if let Some(t) = &patch.title {
        task.title = t.clone();
    }
    if let Some(d) = &patch.description {
        task.description = d.clone();
    }
    if let Some(s) = patch.status {
        task.status = s;
    }
    if let Some(p) = patch.priority {
        task.priority = p;
    }
    if let Some(t) = &patch.triage_state {
        task.triage_state = t.clone();
    }
    if let Some(d) = &patch.due_at {
        task.due_at = d.clone();
    }
    if let Some(p) = &patch.project_id {
        task.project_id = p.clone();
    }
    task.updated_at = at;
}

/// String key for the per-filter cache. `all` and `inbox` are sentinels;
/// a concrete project filter uses the project UUID's display form.
fn filter_key(f: &ProjectFilter) -> String {
    match f {
        ProjectFilter::All => "all".to_string(),
        ProjectFilter::Inbox => "inbox".to_string(),
        ProjectFilter::Of(pid) => pid.to_string(),
    }
}

fn in_scope_for_key(key: &str, project_id: Option<ProjectId>) -> bool {
    match key {
        "all" => true,
        "inbox" => project_id.is_none(),
        pid_str => project_id.map(|p| p.to_string()).as_deref() == Some(pid_str),
    }
}

/// Build a [`Task`] from a `TaskCreated` event without losing the envelope's
/// `actor` and `occurred_at` (used as `created_by` / `created_at` to match the
/// server-side projection). Returns `None` if the inline `NewTask` has no id —
/// the server always sets it, but we ignore malformed events rather than
/// fabricating one.
fn task_from_event(env: &EventEnvelope, input: &NewTask) -> Option<Task> {
    let id = input.id?;
    Some(Task {
        id,
        project_id: input.project_id,
        title: input.title.clone(),
        description: input.description.clone().unwrap_or_default(),
        status: input.status.unwrap_or_default(),
        priority: input.priority.unwrap_or_default(),
        triage_state: input.triage_state,
        due_at: input.due_at,
        created_at: env.occurred_at,
        updated_at: env.occurred_at,
        started_at: None,
        completed_at: None,
        created_by: Some(env.actor.clone()),
        completed_by: None,
        updated_by: Some(env.actor.clone()),
        updated_event_id: Some(env.id),
        updated_event_seq: Some(env.seq),
        source_event_id: None,
    })
}

async fn refresh_relations(tasks_snapshot: &[Task], relations_ctx: RelationsCtx) {
    let ids: Vec<TaskId> = tasks_snapshot.iter().map(|t| t.id).collect();
    let done_ids: HashSet<TaskId> = tasks_snapshot
        .iter()
        .filter(|t| matches!(t.status, Status::Done))
        .map(|t| t.id)
        .collect();
    let id_strs: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
    match api::list_relations_for_tasks(&id_strs).await {
        Ok(rels) => relations_ctx.set_from_relations(&ids, &done_ids, &rels),
        Err(err) => {
            leptos::logging::log!("list_relations_for_tasks failed: {:?}", err);
            relations_ctx.set_from_relations(&ids, &done_ids, &[]);
        }
    }
}

/// Apply one WS event to a single cached snapshot. All operations are
/// idempotent (push checks by id, find-and-replace is set-to-value, delete is
/// retain), so double-applying an event already reflected by a server snapshot
/// is safe — required for the in-flight refetch catch-up.
fn apply_event_to_vec(
    env: &EventEnvelope,
    list: &mut Vec<Task>,
    key: &str,
    need_relations_refresh: &mut bool,
) {
    match &env.payload {
        Event::TaskCreated { task } => {
            if !in_scope_for_key(key, task.project_id) {
                return;
            }
            let Some(new) = task_from_event(env, task) else {
                return;
            };
            let id = new.id;
            if !list.iter().any(|t| t.id == id) {
                list.push(new);
                sort_tasks(list);
            }
            *need_relations_refresh = true;
        }
        Event::TaskUpdated { task_id, patch } => {
            let resort = patch.priority.is_some();
            if let Some(t) = list.iter_mut().find(|t| t.id == *task_id) {
                apply_task_patch(patch, t, env.occurred_at);
            }
            if resort {
                sort_tasks(list);
            }
            // Drop tasks that moved out of this key's scope. Tasks that moved
            // INTO scope but aren't in this snapshot yet stay missing until
            // the next refetch — accepted edge.
            if patch.project_id.is_some() {
                list.retain(|t| in_scope_for_key(key, t.project_id));
            }
        }
        Event::TaskStatusChanged { task_id, to, .. } => {
            let to_terminal = to.is_terminal();
            if let Some(t) = list.iter_mut().find(|t| t.id == *task_id) {
                t.status = *to;
            }
            if to_terminal {
                *need_relations_refresh = true;
            }
        }
        Event::TaskPriorityChanged { task_id, to, .. } => {
            let to_priority = *to;
            if let Some(t) = list.iter_mut().find(|t| t.id == *task_id) {
                t.priority = to_priority;
            }
            sort_tasks(list);
        }
        Event::TaskCompleted {
            task_id,
            completed_at,
            ..
        } => {
            let at = *completed_at;
            if let Some(t) = list.iter_mut().find(|t| t.id == *task_id) {
                t.completed_at = Some(at);
            }
        }
        Event::TaskReopened { task_id, .. } => {
            if let Some(t) = list.iter_mut().find(|t| t.id == *task_id) {
                t.completed_at = None;
            }
        }
        Event::TaskClosed { task_id, by, at } => {
            let by = by.clone();
            let at = *at;
            if let Some(t) = list.iter_mut().find(|t| t.id == *task_id) {
                t.completed_at = Some(at);
                t.completed_by = Some(by);
            }
        }
        Event::TaskDeleted { task_id } => {
            let id = *task_id;
            list.retain(|t| t.id != id);
            *need_relations_refresh = true;
        }
        Event::TaskLinked { .. }
        | Event::TaskUnlinked { .. }
        | Event::TaskUnblocked { .. }
        | Event::TaskRelationKindChanged { .. } => {
            *need_relations_refresh = true;
        }
        _ => {}
    }
}

#[component]
pub fn TaskList() -> impl IntoView {
    let projects_ctx = use_context::<ProjectsCtx>().expect("ProjectsCtx");
    let relations_ctx = use_context::<RelationsCtx>().expect("RelationsCtx");
    let ws_ctx = use_context::<WsCtx>().expect("WsCtx");
    let filter = projects_ctx.current_filter;
    let ws_events = ws_ctx.events;

    // Collapsed groups, keyed by group slug (Status does not impl Hash).
    let collapsed: RwSignal<HashSet<&'static str>> =
        RwSignal::new(HashSet::from(["done", "cancelled"]));

    // Per-filter snapshot cache. Re-selecting a project shows its prior
    // snapshot instantly; WS events apply to every cached snapshot so they
    // stay in sync without per-event refetch.
    let cache: RwSignal<HashMap<String, Vec<Task>>> = RwSignal::new(HashMap::new());
    // Cursor into `ws_events`: indices `< applied_cursor` have already been
    // applied to every cached snapshot. Refetched snapshots catch up to the
    // *same* cursor before being inserted, so server-vs-WS race is handled.
    let applied_cursor: RwSignal<usize> = RwSignal::new(0);
    // Per-key fetch generation — drops stale results when the same key is
    // refetched concurrently (e.g. A → B → A in quick succession).
    let fetch_seq: RwSignal<HashMap<String, u64>> = RwSignal::new(HashMap::new());
    // Most recent fetch failure for the current key, if any — cleared when a
    // new fetch starts. Lets the view show an actionable message instead of
    // spinning on the skeleton forever (a failed fetch never writes `cache`,
    // so `loaded` alone can't distinguish "still loading" from "gave up").
    let fetch_error: RwSignal<Option<String>> = RwSignal::new(None);

    let current_key = Memo::new(move |_| filter_key(&filter.get()));

    let tasks: Memo<Vec<Task>> = Memo::new(move |_| {
        let key = current_key.get();
        cache.with(|m| m.get(&key).cloned().unwrap_or_default())
    });

    // Loaded for the *current* filter — controls whether the skeleton
    // fallback shows. Cache hit on filter change → no skeleton; cache miss
    // (first visit) → skeleton until the fetch lands.
    let loaded: Memo<bool> = Memo::new(move |_| {
        let key = current_key.get();
        cache.with(|m| m.contains_key(&key))
    });

    // 1) Refetch on filter change — but only when this filter has never been
    // loaded before. WS keeps cached snapshots in sync (Resync + since_seq
    // handle reconnects), so re-visiting a project is a pure local switch.
    Effect::new(move |_| {
        let key = current_key.get();
        // Clear before the cache-hit check below: `fetch_error` isn't keyed
        // per-filter, so a stale error from a previous failed filter must not
        // linger over a different filter that's actually a cache hit.
        fetch_error.set(None);
        // Cache hit → trust the WS-applied snapshot, no HTTP.
        if cache.with_untracked(|m| m.contains_key(&key)) {
            return;
        }
        let f = filter.get_untracked();
        let snapshot_at = ws_events.with_untracked(|v| v.len());
        let my_seq = fetch_seq.with_untracked(|m| m.get(&key).copied().unwrap_or(0)) + 1;
        fetch_seq.update(|m| {
            m.insert(key.clone(), my_seq);
        });

        // Scoped + cancel-on-cleanup: this future reads component-owned signals
        // (`fetch_seq`, `current_key`, `cache`) *after* the await. If the route
        // is torn down mid-fetch (navigating between `/:workspace` and
        // `/:workspace/:project`, which are distinct router matches), a plain
        // spawn would resume and touch disposed signals → panic. Cancellation
        // aborts it on unmount, and on filter change it drops the stale fetch
        // that `fetch_seq` would have discarded anyway.
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            let pid_arg: Option<String> = match &f {
                ProjectFilter::All => None,
                ProjectFilter::Inbox => Some("inbox".to_string()),
                ProjectFilter::Of(pid) => Some(pid.to_string()),
            };
            // Surface a fetch failure to the console rather than caching
            // an empty vec — caching an empty result would flip `loaded`
            // to `true` and make the task list silently disappear.
            let mut ts = match api::list_tasks(pid_arg.as_deref()).await {
                Ok(ts) => ts,
                Err(err) => {
                    leptos::logging::log!("list_tasks failed for filter={:?}: {:?}", f, err);
                    fetch_error.set(Some(err.friendly()));
                    return;
                }
            };
            sort_tasks(&mut ts);

            // Catch up the fresh snapshot to any events that arrived during
            // the in-flight HTTP request. Safe to over-apply: every handler
            // is idempotent by id.
            let mut need_relations_refresh = false;
            ws_events.with_untracked(|evs| {
                let now_len = evs.len();
                if snapshot_at < now_len {
                    for env in &evs[snapshot_at..now_len] {
                        apply_event_to_vec(env, &mut ts, &key, &mut need_relations_refresh);
                    }
                    sort_tasks(&mut ts);
                }
            });

            // Drop stale results: another fetch for this key already started.
            let latest_seq = fetch_seq.with_untracked(|m| m.get(&key).copied().unwrap_or(0));
            if latest_seq != my_seq {
                return;
            }

            cache.update(|m| {
                m.insert(key.clone(), ts);
            });

            // Refresh relations only when this fetch's filter is still active.
            if current_key.get_untracked() == key {
                let snapshot = cache.with_untracked(|m| m.get(&key).cloned().unwrap_or_default());
                refresh_relations(&snapshot, relations_ctx).await;
            }
        });
    });

    // 2) Apply incoming WS events to every cached snapshot.
    Effect::new(move |_| {
        let len = ws_events.with(|v| v.len());
        let start = applied_cursor.get_untracked();
        if start >= len {
            return;
        }

        let mut need_relations_refresh = false;
        ws_events.with_untracked(|evs| {
            cache.update(|m| {
                for env in &evs[start..len] {
                    for (key, list) in m.iter_mut() {
                        apply_event_to_vec(env, list, key, &mut need_relations_refresh);
                    }
                }
            });
        });
        applied_cursor.set(len);

        if need_relations_refresh {
            let key = current_key.get_untracked();
            let snapshot = cache.with_untracked(|m| m.get(&key).cloned().unwrap_or_default());
            spawn_local(async move {
                refresh_relations(&snapshot, relations_ctx).await;
            });
        }
    });

    // Group projection — `tasks` mutates one entry at a time, so the Memo
    // recomputes cheaply and `<For key>` rebinds only the moved row.
    let groups = Memo::new(move |_| {
        let ts = tasks.get();
        let mut buckets: [Vec<Task>; 6] = Default::default();
        for t in ts.iter() {
            if let Some(idx) = GROUP_ORDER.iter().position(|&s| s == t.status) {
                buckets[idx].push(t.clone());
            }
        }
        GROUP_ORDER
            .iter()
            .copied()
            .zip(buckets)
            .filter_map(|(s, items)| {
                if items.is_empty() {
                    None
                } else {
                    Some((s, items))
                }
            })
            .collect::<Vec<(Status, Vec<Task>)>>()
    });

    view! {
        {move || {
            if let Some(err) = fetch_error.get() {
                view! {
                    <div class="task-groups-error">
                        <p class="fetch-error__message">{err}</p>
                    </div>
                }.into_any()
            } else if !loaded.get() {
                view! {
                    <div class="task-groups skeleton">
                        <section class="task-group">
                            <div class="task-group__header skeleton-header">
                                <span class="skeleton-bar skeleton-bar--header"></span>
                            </div>
                            <ul class="task-list">
                                { (0..6).map(|_| view! {
                                    <li class="task-row-wrapper">
                                        <div class="task-row skeleton-row">
                                            <span class="skeleton-bar skeleton-bar--priority"></span>
                                            <span class="skeleton-bar skeleton-bar--id"></span>
                                            <span class="skeleton-bar skeleton-bar--title"></span>
                                            <span class="skeleton-bar skeleton-bar--status"></span>
                                        </div>
                                    </li>
                                }).collect_view() }
                            </ul>
                        </section>
                    </div>
                }.into_any()
            } else {
                let gs = groups.get();
                if gs.is_empty() {
                    view! {
                        <div class="task-groups-empty">"No tasks yet."</div>
                    }.into_any()
                } else {
                    let all_done = gs
                        .iter()
                        .all(|(s, _)| matches!(s, Status::Done | Status::Cancelled));
                    view! {
                        <div class="task-groups">
                            <Show when=move || all_done fallback=|| view! { <></> }>
                                <p class="task-groups-caught-up">"All tasks done."</p>
                            </Show>
                            { gs.into_iter().map(|(status, items)| {
                                let count = items.len();
                                let slug = group_slug(status);
                                let is_collapsed = move || collapsed.get().contains(slug);
                                let toggle = move |_| {
                                    collapsed.update(|set| {
                                        if !set.insert(slug) { set.remove(slug); }
                                    });
                                };
                                let header_class = move || {
                                    format!(
                                        "task-group__header task-group__header--{}{}",
                                        slug,
                                        if is_collapsed() { " collapsed" } else { "" },
                                    )
                                };
                                view! {
                                    <section class="task-group">
                                        <button
                                            class=header_class
                                            type="button"
                                            on:click=toggle
                                            aria-expanded=move || (!is_collapsed()).to_string()
                                        >
                                            <span class="task-group__toggle">
                                                { move || if is_collapsed() { "▸" } else { "▾" } }
                                            </span>
                                            <span class="task-group__label">{ group_label(status) }</span>
                                            <span class="task-group__count">{ count }</span>
                                        </button>
                                        <Show when=move || !is_collapsed() fallback=|| view! { <></> }>
                                            <ul class="task-list">
                                                <For
                                                    each={ let items = items.clone(); move || items.clone() }
                                                    key=|t: &Task| t.id
                                                    let:task
                                                >
                                                    <TaskRow task=task />
                                                </For>
                                            </ul>
                                        </Show>
                                    </section>
                                }
                            }).collect_view() }
                        </div>
                    }.into_any()
                }
            }
        }}
    }
}
