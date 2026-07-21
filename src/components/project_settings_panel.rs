//! Project settings panel.
//!
//! Shown on the right rail when a single project is selected. Exposes the
//! two auto-append toggles — "Interview" (AI/agent activity log) and "Human
//! Log" (human-readable milestone feed) — backed by
//! `GET`/`PATCH /v1/projects/{id}/settings`. Both default to on; a missing
//! stored row (pre-existing projects) still resolves to on via the server.
//!
//! Realtime: `ProjectSettingsChanged` rides `Channel::Tasks`, already part
//! of the WS subscription in `ws.rs`, so a change from another client (or
//! this one's own PATCH echoing back) updates the view without a refetch.

use crate::api;
use crate::projects_ctx::{ProjectFilter, ProjectsCtx};
use crate::ws::WsCtx;
use daruma_domain::{AutoAppendPatch, AutoAppendSettings};
use daruma_events::{Event, EventEnvelope};
use leptos::prelude::*;
use std::collections::HashMap;

/// Which toggle a UI action targets — avoids threading two near-identical
/// closures/branches through the fetch/optimistic-update/revert plumbing.
#[derive(Clone, Copy, PartialEq)]
enum SettingField {
    Interview,
    HumanLog,
}

impl SettingField {
    fn set(self, settings: &mut AutoAppendSettings, value: bool) {
        match self {
            SettingField::Interview => settings.interview = value,
            SettingField::HumanLog => settings.human_log = value,
        }
    }

    fn patch(self, value: bool) -> AutoAppendPatch {
        match self {
            SettingField::Interview => AutoAppendPatch {
                interview: Some(value),
                human_log: None,
            },
            SettingField::HumanLog => AutoAppendPatch {
                interview: None,
                human_log: Some(value),
            },
        }
    }
}

/// Apply one WS event to the per-project settings cache. Idempotent —
/// replaying the same `ProjectSettingsChanged` twice just overwrites with
/// the same value.
fn apply_settings_event(env: &EventEnvelope, cache: &mut HashMap<String, AutoAppendSettings>) {
    if let Event::ProjectSettingsChanged {
        project_id,
        auto_append,
        ..
    } = &env.payload
    {
        cache.insert(project_id.to_string(), *auto_append);
    }
}

#[component]
pub fn ProjectSettingsPanel() -> impl IntoView {
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

    // Per-project settings cache, kept in sync via WS apply — same pattern
    // as documents_panel.rs / plans_panel.rs.
    let cache: RwSignal<HashMap<String, AutoAppendSettings>> = RwSignal::new(HashMap::new());
    let applied_cursor: RwSignal<usize> = RwSignal::new(0);
    let fetch_seq: RwSignal<HashMap<String, u64>> = RwSignal::new(HashMap::new());
    // Most recent fetch/mutation failure for the current project, if any —
    // see documents_panel.rs for the same pattern and rationale.
    let error: RwSignal<Option<String>> = RwSignal::new(None);

    let settings: Memo<Option<AutoAppendSettings>> = Memo::new(move |_| {
        let pid = project_id_opt.get()?;
        cache.with(|m| m.get(&pid).copied())
    });

    let loaded: Memo<bool> = Memo::new(move |_| {
        let Some(pid) = project_id_opt.get() else {
            return true;
        };
        cache.with(|m| m.contains_key(&pid))
    });

    // 1) Fetch only on first visit to a project — cache hit reuses WS-applied
    //    snapshot (including one this tab's own PATCH already echoed back).
    Effect::new(move |_| {
        let Some(pid) = project_id_opt.get() else {
            return;
        };
        error.set(None);
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
            let mut s = match api::get_project_settings(&pid).await {
                Ok(s) => s,
                Err(err) => {
                    leptos::logging::log!("get_project_settings failed for project={pid}: {err:?}");
                    error.set(Some(err.friendly()));
                    AutoAppendSettings::default()
                }
            };
            // Catch up to events that arrived during the in-flight fetch.
            ws_events.with_untracked(|evs| {
                let now_len = evs.len();
                if snapshot_at < now_len {
                    for env in &evs[snapshot_at..now_len] {
                        if let Event::ProjectSettingsChanged {
                            project_id,
                            auto_append,
                            ..
                        } = &env.payload
                        {
                            if project_id.to_string() == pid {
                                s = *auto_append;
                            }
                        }
                    }
                }
            });

            let latest_seq = fetch_seq.with_untracked(|m| m.get(&pid).copied().unwrap_or(0));
            if latest_seq != my_seq {
                return;
            }

            cache.update(|m| {
                m.insert(pid.clone(), s);
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
                    apply_settings_event(env, m);
                }
            });
        });
        applied_cursor.set(len);
    });

    // Toggle one flag: optimistic local update, PATCH, revert + surface an
    // error on failure. The WS echo of our own PATCH will also land via (2)
    // above and is idempotent with this optimistic value.
    //
    // Uses a plain `spawn_local`, not `spawn_local_scoped_with_cancellation`:
    // the optimistic `cache.update` just below rebuilds the checkbox's own
    // view (new `settings` memo value), which disposes the reactive owner
    // that was active when the `on:change` handler ran — a scoped spawn tied
    // to that owner gets cancelled before the request is ever sent, silently
    // dropping the PATCH while the checkbox still *looks* toggled. This
    // mutation's signal writes (`cache`/`error`) must outlive that owner, so
    // it's spawned unscoped, matching `projects_ctx.rs`'s init fetch.
    let toggle = move |field: SettingField, current: bool| {
        let Some(pid) = project_id_opt.get_untracked() else {
            return;
        };
        let new_val = !current;
        cache.update(|m| field.set(m.entry(pid.clone()).or_default(), new_val));
        error.set(None);
        let patch = field.patch(new_val);
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(err) = api::update_project_settings(&pid, patch).await {
                leptos::logging::log!("update_project_settings failed for project={pid}: {err:?}");
                cache.update(|m| field.set(m.entry(pid.clone()).or_default(), current));
                error.set(Some(err.friendly()));
            }
        });
    };

    view! {
        {move || {
            match current_filter.get() {
                ProjectFilter::Of(_) => view! {
                    <div class="project-settings-panel">
                        <div class="project-settings-header">
                            <span class="project-settings-title">"Settings"</span>
                        </div>
                        <Show
                            when=move || loaded.get()
                            fallback=|| view! {
                                <div class="project-settings-empty">"Loading…"</div>
                            }
                        >
                            {move || {
                                let s = settings.get().unwrap_or_default();
                                view! {
                                    <div class="project-settings-list">
                                        { error.get().map(|msg| view! {
                                            <p class="fetch-error__message">{msg}</p>
                                        })}
                                        <label class="project-settings-row">
                                            <input
                                                type="checkbox"
                                                checked=s.interview
                                                on:change=move |_| toggle(SettingField::Interview, s.interview)
                                            />
                                            <span class="project-settings-row__label">
                                                "Auto-append to Interview log"
                                            </span>
                                        </label>
                                        <label class="project-settings-row">
                                            <input
                                                type="checkbox"
                                                checked=s.human_log
                                                on:change=move |_| toggle(SettingField::HumanLog, s.human_log)
                                            />
                                            <span class="project-settings-row__label">
                                                "Auto-append to Human Log"
                                            </span>
                                        </label>
                                    </div>
                                }.into_any()
                            }}
                        </Show>
                    </div>
                }.into_any(),
                _ => view! { <div class="project-settings-aside-hidden" /> }.into_any(),
            }
        }}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daruma_domain::Actor;
    use daruma_shared::{time, ProjectId};

    #[test]
    fn apply_settings_event_updates_only_matching_project() {
        let mut cache = HashMap::new();
        let pid = ProjectId::new();
        let other = ProjectId::new();
        cache.insert(
            other.to_string(),
            AutoAppendSettings {
                interview: true,
                human_log: true,
            },
        );

        apply_settings_event(
            &EventEnvelope::new(
                Actor::user(),
                Event::ProjectSettingsChanged {
                    project_id: pid,
                    auto_append: AutoAppendSettings {
                        interview: false,
                        human_log: true,
                    },
                    at: time::now(),
                },
            ),
            &mut cache,
        );

        assert_eq!(cache.get(&pid.to_string()).unwrap().interview, false);
        assert_eq!(cache.get(&other.to_string()).unwrap().interview, true);
    }
}
