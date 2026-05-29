use leptos::prelude::*;
use std::collections::HashMap;
use taskagent_domain::Project;
use taskagent_shared::ProjectId;

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
            .unwrap_or_else(|| pid.to_string()),
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

    wasm_bindgen_futures::spawn_local(async move {
        match crate::api::list_projects().await {
            Ok(ps) => {
                set_projects.set(ps.clone());
                current_filter.set(resolve_filter(&initial_segment, &ps));
            }
            Err(e) => leptos::logging::log!("list_projects failed: {:?}", e),
        }
    });

    ProjectsCtx {
        projects,
        names,
        workspace_slug,
        current_filter,
    }
}
