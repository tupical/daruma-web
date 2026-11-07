use gloo_net::http::Request;
use leptos::prelude::*;
use serde::Deserialize;

const CONFIG_URL: &str = "/.well-known/daruma-shell.json";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct HostShellConfig {
    #[serde(default)]
    pub home_url: Option<String>,
    #[serde(default)]
    pub switcher_url: Option<String>,
    #[serde(default)]
    pub current_workspace_label: Option<String>,
}

impl HostShellConfig {
    fn normalize(mut self) -> Option<Self> {
        self.home_url = clean_url(self.home_url);
        self.switcher_url = clean_url(self.switcher_url);
        self.current_workspace_label = clean_label(self.current_workspace_label);

        if self.home_url.is_none()
            && self.switcher_url.is_none()
            && self.current_workspace_label.is_none()
        {
            None
        } else {
            Some(self)
        }
    }

    pub fn primary_url(&self) -> Option<&str> {
        self.switcher_url
            .as_deref()
            .or(self.home_url.as_deref())
            .filter(|url| !url.is_empty())
    }
}

pub type HostShellSignal = ReadSignal<Option<HostShellConfig>>;

pub fn init_host_shell() -> HostShellSignal {
    let (config, set_config) = signal(None);

    wasm_bindgen_futures::spawn_local(async move {
        if let Some(loaded) = load_config().await {
            set_config.set(Some(loaded));
        }
    });

    config.into()
}

async fn load_config() -> Option<HostShellConfig> {
    let resp = Request::get(CONFIG_URL).send().await.ok()?;
    if resp.status() == 404 {
        return None;
    }
    if !(200..300).contains(&(resp.status() as u32)) {
        leptos::logging::log!("host shell config ignored: HTTP {}", resp.status());
        return None;
    }
    resp.json::<HostShellConfig>()
        .await
        .ok()
        .and_then(HostShellConfig::normalize)
}

/// Length limits count Unicode scalar values (Rust `char`s) of the trimmed
/// value, not UTF-8 bytes, so the README's "chars" hold for non-ASCII text.
const MAX_URL_CHARS: usize = 2048;
const MAX_LABEL_CHARS: usize = 80;

fn exceeds_chars(value: &str, max: usize) -> bool {
    value.chars().nth(max).is_some()
}

fn clean_url(value: Option<String>) -> Option<String> {
    let value = value?.trim().to_string();
    if value.is_empty() || exceeds_chars(&value, MAX_URL_CHARS) {
        return None;
    }
    if value.starts_with('/') || value.starts_with("http://") || value.starts_with("https://") {
        Some(value)
    } else {
        None
    }
}

fn clean_label(value: Option<String>) -> Option<String> {
    let value = value?.trim().to_string();
    if value.is_empty() || exceeds_chars(&value, MAX_LABEL_CHARS) {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::{clean_label, clean_url, HostShellConfig, MAX_LABEL_CHARS, MAX_URL_CHARS};

    fn label(value: &str) -> Option<String> {
        clean_label(Some(value.to_string()))
    }

    fn url(value: &str) -> Option<String> {
        clean_url(Some(value.to_string()))
    }

    #[test]
    fn label_limit_counts_chars_not_bytes() {
        for unit in ["a", "ж", "😀"] {
            let at_limit = unit.repeat(MAX_LABEL_CHARS);
            assert_eq!(
                label(&at_limit).as_deref(),
                Some(at_limit.as_str()),
                "{unit}"
            );
            assert_eq!(label(&unit.repeat(MAX_LABEL_CHARS + 1)), None, "{unit}");
        }
    }

    #[test]
    fn label_of_41_cyrillic_letters_is_kept() {
        let value = "Ж".repeat(41);
        assert_eq!(value.len(), 82);
        assert_eq!(label(&value).as_deref(), Some(value.as_str()));
    }

    #[test]
    fn label_is_trimmed_before_counting_and_blank_is_dropped() {
        let padded = format!("  {}  ", "я".repeat(MAX_LABEL_CHARS));
        assert_eq!(label(&padded), Some("я".repeat(MAX_LABEL_CHARS)));
        assert_eq!(label("   "), None);
        assert_eq!(clean_label(None), None);
    }

    #[test]
    fn url_limit_counts_chars_not_bytes() {
        for unit in ["a", "ж", "😀"] {
            let prefix = "/switch/";
            let fill = MAX_URL_CHARS - prefix.chars().count();
            let at_limit = format!("{prefix}{}", unit.repeat(fill));
            assert_eq!(url(&at_limit).as_deref(), Some(at_limit.as_str()), "{unit}");
            let over = format!("{prefix}{}", unit.repeat(fill + 1));
            assert_eq!(url(&over), None, "{unit}");
        }
    }

    #[test]
    fn url_scheme_rules_are_unchanged() {
        assert_eq!(url(" /workspaces ").as_deref(), Some("/workspaces"));
        assert_eq!(url("http://h/x").as_deref(), Some("http://h/x"));
        assert_eq!(url("https://h/путь").as_deref(), Some("https://h/путь"));
        assert_eq!(url("javascript:alert(1)"), None);
        assert_eq!(url("ftp://h/x"), None);
        assert_eq!(url("h/x"), None);
        assert_eq!(url(""), None);
    }

    #[test]
    fn config_with_only_dropped_values_is_standalone() {
        let config = HostShellConfig {
            home_url: Some("x".repeat(MAX_URL_CHARS + 1)),
            switcher_url: Some("javascript:void(0)".to_string()),
            current_workspace_label: Some("ж".repeat(MAX_LABEL_CHARS + 1)),
        };
        assert_eq!(config.normalize(), None);

        let config = HostShellConfig {
            home_url: None,
            switcher_url: None,
            current_workspace_label: Some("Ж".repeat(41)),
        };
        assert_eq!(
            config.normalize().and_then(|c| c.current_workspace_label),
            Some("Ж".repeat(41))
        );
    }
}
