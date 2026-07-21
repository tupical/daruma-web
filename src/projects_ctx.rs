use daruma_domain::Project;
use daruma_events::{Event, EventEnvelope};
use daruma_shared::ProjectId;
use leptos::prelude::*;
use std::collections::HashMap;

use crate::ws::WsCtx;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectFilter {
    All,
    Inbox,
    Of(ProjectId),
}

#[derive(Clone)]
pub struct ProjectsCtx {
    pub projects: ReadSignal<Vec<Project>>,
    pub names: Memo<HashMap<ProjectId, String>>,
    pub workspace_slug: ReadSignal<String>,
    pub current_filter: RwSignal<ProjectFilter>,
    /// Set when the initial `GET /v1/projects` fails; cleared on success.
    /// `projects` stays empty in that case, so this is what lets the UI tell
    /// "fresh install, no projects yet" apart from "couldn't load projects".
    pub projects_error: RwSignal<Option<String>>,
}

fn current_path_segments() -> Vec<String> {
    web_sys::window()
        .and_then(|w| w.location().pathname().ok())
        .map(|p| {
            p.trim_matches('/')
                .split('/')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_workspace_slug_from_path() -> String {
    let segs = current_path_segments();
    if segs.first().is_some_and(|s| s == "app") {
        return String::new();
    }
    segs.first().cloned().unwrap_or_default()
}

fn parse_project_segment_from_path() -> String {
    let segs = current_path_segments();
    if segs.first().is_some_and(|s| s == "app") {
        return segs.get(1).cloned().unwrap_or_else(|| "all".into());
    }
    segs.get(1).cloned().unwrap_or_else(|| "all".into())
}

pub fn resolve_filter(segment: &str, projects: &[Project]) -> ProjectFilter {
    match segment {
        "all" => ProjectFilter::All,
        "inbox" => ProjectFilter::Inbox,
        other => {
            if let Ok(pid) = other.parse::<ProjectId>() {
                return ProjectFilter::Of(pid);
            }
            projects
                .iter()
                .find(|p| p.slug == other)
                .map(|p| ProjectFilter::Of(p.id))
                .unwrap_or(ProjectFilter::All)
        }
    }
}

pub fn filter_to_segment(filter: &ProjectFilter, projects: &[Project]) -> String {
    match filter {
        ProjectFilter::All => "all".into(),
        ProjectFilter::Inbox => "inbox".into(),
        ProjectFilter::Of(pid) => projects
            .iter()
            .find(|p| p.id == *pid)
            .map(|p| p.slug.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| pid.to_string()),
    }
}

/// Apply one WS event to the project list. Idempotent by project id.
fn apply_project_event(env: &EventEnvelope, list: &mut Vec<Project>) {
    match &env.payload {
        Event::ProjectCreated { project } => {
            if let Some(existing) = list.iter_mut().find(|p| p.id == project.id) {
                // Pre-migration-0018 `project_created` events replay with an
                // empty slug; keep the slug we already have from the API.
                let slug = if project.slug.is_empty() {
                    std::mem::take(&mut existing.slug)
                } else {
                    project.slug.clone()
                };
                *existing = project.clone();
                existing.slug = slug;
            } else {
                list.push(project.clone());
            }
        }
        Event::ProjectUpdated {
            project_id,
            title,
            description,
        } => {
            if let Some(project) = list.iter_mut().find(|p| p.id == *project_id) {
                if let Some(title) = title {
                    project.title = title.clone();
                }
                if let Some(description) = description {
                    project.description = description.clone();
                }
                project.updated_at = env.occurred_at;
            }
        }
        Event::ProjectDeleted { project_id } => {
            list.retain(|p| p.id != *project_id);
        }
        _ => {}
    }
}

pub fn canonical_path(
    workspace_slug: &str,
    filter: &ProjectFilter,
    projects: &[Project],
) -> String {
    if workspace_slug.is_empty() {
        return format!("/app/{}", filter_to_segment(filter, projects));
    }
    format!(
        "/{}/{}",
        workspace_slug,
        filter_to_segment(filter, projects)
    )
}

pub fn init_projects_ctx() -> ProjectsCtx {
    let (projects, set_projects) = signal(Vec::<Project>::new());
    let ws_ctx = use_context::<WsCtx>().expect("WsCtx");
    let ws_events = ws_ctx.events;
    let names = Memo::new(move |_| {
        projects
            .get()
            .into_iter()
            .map(|p| (p.id, p.title.clone()))
            .collect()
    });
    let workspace_slug = RwSignal::new(parse_workspace_slug_from_path()).read_only();
    let initial_segment = parse_project_segment_from_path();
    let current_filter = RwSignal::new(resolve_filter(&initial_segment, &[]));
    let projects_error: RwSignal<Option<String>> = RwSignal::new(None);

    wasm_bindgen_futures::spawn_local(async move {
        match crate::api::list_projects().await {
            Ok(ps) => {
                set_projects.set(ps.clone());
                current_filter.set(resolve_filter(&initial_segment, &ps));
            }
            Err(e) => {
                leptos::logging::log!("list_projects failed: {:?}", e);
                projects_error.set(Some(e.friendly()));
            }
        }
    });

    let applied_cursor: RwSignal<usize> = RwSignal::new(ws_events.with_untracked(|v| v.len()));
    Effect::new(move |_| {
        let len = ws_events.with(|v| v.len());
        let start = applied_cursor.get_untracked();
        if start >= len {
            return;
        }
        ws_events.with_untracked(|evs| {
            set_projects.update(|list| {
                for env in &evs[start..len] {
                    apply_project_event(env, list);
                }
            });
        });
        applied_cursor.set(len);
    });

    ProjectsCtx {
        projects,
        names,
        workspace_slug,
        current_filter,
        projects_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daruma_domain::Actor;

    #[test]
    fn apply_project_event_creates_idempotently_updates_and_deletes() {
        let mut list = Vec::new();
        let project = Project::new("one", Some("before".into()));
        let project_id = project.id;

        apply_project_event(
            &EventEnvelope::new(
                Actor::user(),
                Event::ProjectCreated {
                    project: project.clone(),
                },
            ),
            &mut list,
        );
        assert_eq!(list, vec![project.clone()]);

        apply_project_event(
            &EventEnvelope::new(
                Actor::user(),
                Event::ProjectCreated {
                    project: project.clone(),
                },
            ),
            &mut list,
        );
        assert_eq!(list.len(), 1);

        // Replay of a pre-slug event (empty slug) must not wipe the slug
        // we already have from `GET /v1/projects`.
        let mut pre_slug = project.clone();
        pre_slug.slug = String::new();
        apply_project_event(
            &EventEnvelope::new(Actor::user(), Event::ProjectCreated { project: pre_slug }),
            &mut list,
        );
        assert_eq!(list[0].slug, project.slug);

        apply_project_event(
            &EventEnvelope::new(
                Actor::user(),
                Event::ProjectUpdated {
                    project_id,
                    title: Some("renamed".into()),
                    description: Some(None),
                },
            ),
            &mut list,
        );
        assert_eq!(list[0].title, "renamed");
        assert_eq!(list[0].description, None);

        apply_project_event(
            &EventEnvelope::new(Actor::user(), Event::ProjectDeleted { project_id }),
            &mut list,
        );
        assert!(list.is_empty());

        apply_project_event(
            &EventEnvelope::new(Actor::user(), Event::ProjectDeleted { project_id }),
            &mut list,
        );
        assert!(list.is_empty());
    }
}
