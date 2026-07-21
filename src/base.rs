use std::sync::OnceLock;

static MOUNT_BASE: OnceLock<String> = OnceLock::new();

fn mount_base_from_path(path: &str) -> String {
    ["/web", "/app"]
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

#[cfg(test)]
mod tests {
    use super::mount_base_from_path;

    #[test]
    fn detects_supported_mount_bases() {
        assert_eq!(mount_base_from_path("/"), "");
        assert_eq!(mount_base_from_path("/graph"), "");
        assert_eq!(mount_base_from_path("/web"), "/web");
        assert_eq!(mount_base_from_path("/web/graph"), "/web");
        assert_eq!(mount_base_from_path("/app/workspace/"), "/app");
        assert_eq!(mount_base_from_path("/application"), "");
        assert_eq!(mount_base_from_path("/website"), "");
    }
}
