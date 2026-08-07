use std::sync::OnceLock;

static MOUNT_BASE: OnceLock<String> = OnceLock::new();

/// Only `/web` — **not** `/app`.
///
/// `/app` is one of the app's own routes (`path!("/app/:project?")`, produced by
/// `canonical_path` when there is no workspace slug), so treating it as a mount
/// base too made `/app/acme` ambiguous: base `/app` leaves the router the path
/// `/acme`, which matches `/:workspace/:project?` and reads the project as a
/// workspace. The hosted deployment serves the bundle at `/app/` with absolute
/// asset URLs, so it works with an empty base — the route handles it.
fn mount_base_from_path(path: &str) -> String {
    ["/web"]
        .into_iter()
        .find(|base| {
            path.strip_prefix(base)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
        .unwrap_or_default()
        .to_string()
}

pub fn mount_base() -> String {
    MOUNT_BASE
        .get_or_init(|| {
            let path = web_sys::window()
                .and_then(|window| window.location().pathname().ok())
                .unwrap_or_default();
            mount_base_from_path(&path)
        })
        .clone()
}

/// The path as the router sees it: `location.pathname` with the mount base
/// stripped. Everything that reasons about routes must go through this — the
/// raw pathname still carries `/web` or `/app`.
pub fn route_path() -> String {
    let path = web_sys::window()
        .and_then(|window| window.location().pathname().ok())
        .unwrap_or_default();
    strip_base(&path, &mount_base()).to_string()
}

pub fn strip_base<'a>(path: &'a str, base: &str) -> &'a str {
    path.strip_prefix(base).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::mount_base_from_path;

    #[test]
    fn detects_supported_mount_bases() {
        assert_eq!(mount_base_from_path("/"), "");
        assert_eq!(mount_base_from_path("/graph"), "");
        assert_eq!(mount_base_from_path("/web"), "/web");
        assert_eq!(mount_base_from_path("/web/graph"), "/web");
        // `/app` is a route, not a mount base — stripping it made the project
        // segment look like a workspace slug and reset the filter to "all".
        assert_eq!(mount_base_from_path("/app/mcpbox-cloud"), "");
        assert_eq!(mount_base_from_path("/web/app/mcpbox-cloud"), "/web");
        assert_eq!(mount_base_from_path("/application"), "");
        assert_eq!(mount_base_from_path("/website"), "");
    }
}
