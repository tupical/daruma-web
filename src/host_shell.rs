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

fn clean_url(value: Option<String>) -> Option<String> {
    let value = value?.trim().to_string();
    if value.is_empty() || value.len() > 2048 {
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
    if value.is_empty() || value.len() > 80 {
        None
    } else {
        Some(value)
    }
}
