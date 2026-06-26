//! Bearer-token storage and bootstrap for the Leptos/WASM frontend.
//!
//! All state is kept in `localStorage` under the key `daruma_token`.
//! No reactive signals: callers that need reactivity wrap `current()` in a
//! Leptos signal themselves.

const STORAGE_KEY: &str = "daruma_token";

/// Return the stored token, if any.
pub fn current() -> Option<String> {
    let storage = web_sys::window()?.local_storage().ok().flatten()?;
    storage.get_item(STORAGE_KEY).ok().flatten()
}

/// Persist `token` to `localStorage`.
#[allow(dead_code)] // reserved for a future login flow
pub fn set(token: &str) {
    if let Some(Ok(Some(storage))) = web_sys::window().map(|w| w.local_storage()) {
        let _ = storage.set_item(STORAGE_KEY, token);
    }
}

/// Read bearer token from URL `?token=…` or fall back to `localStorage`.
pub fn bootstrap() -> Option<String> {
    let window = web_sys::window()?;
    let search = window.location().search().ok()?;

    if let Some(token) = extract_token_param(&search) {
        set(&token);
        return Some(token);
    }

    current()
}

/// Extract the value of the `token` query parameter from a raw search string
/// like `"?token=abc&foo=bar"`.
fn extract_token_param(search: &str) -> Option<String> {
    let s = search.trim_start_matches('?');
    for pair in s.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?;
        if key == "token" {
            let val = parts.next().unwrap_or("").trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}
