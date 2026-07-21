//! Shared page skeleton for the top-level views: header (title + host nav,
//! optional project bar), main content, optional aside, status footer.

use crate::components::{HostShellNav, ProjectBar, StatusBar};
use leptos::prelude::*;

/// The `header → nav → main → StatusBar` chrome every `*App` route wrapper
/// used to spell out by hand. The routed view goes into the default slot;
/// an optional aside (workspace right rail) renders between main and the
/// status bar.
#[component]
pub fn Shell(
    /// `app` wrapper class, e.g. "app app--graph".
    #[prop(into)]
    app_class: String,
    /// `main` class, e.g. "main main--graph".
    #[prop(into)]
    main_class: String,
    /// Render the <ProjectBar /> under the header row.
    #[prop(optional)]
    project_bar: bool,
    /// Optional aside rendered between main and the status bar.
    #[prop(into, optional)]
    aside: Option<AnyView>,
    children: Children,
) -> impl IntoView {
    view! {
        <div class=app_class>
            <div class="header">
                <div class="header-row">
                    <div class="viewer-title">"Daruma OSS Viewer"</div>
                    <HostShellNav />
                </div>
                {project_bar.then(|| view! { <ProjectBar /> })}
            </div>
            <main class=main_class>{children()}</main>
            {aside}
            <StatusBar />
        </div>
    }
}
