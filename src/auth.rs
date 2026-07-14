//! Bearer-token storage and bootstrap for the Leptos/WASM frontend.
//!
//! All state is kept in `localStorage` under the key `daruma_token`.
//! No reactive signals: callers that need reactivity wrap `current()` in a
//! Leptos signal themselves.

use gloo_net::http::Request;
use js_sys::Uint8Array;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;

const STORAGE_KEY: &str = "daruma_token";
const WORKSPACE_STORAGE_KEY: &str = "daruma_workspace_id";
const CLIENT_ID_STORAGE_KEY: &str = "daruma_oauth_client_id";
const VERIFIER_SESSION_KEY: &str = "daruma_oauth_code_verifier";
const STATE_SESSION_KEY: &str = "daruma_oauth_state";
const REDIRECT_URI: &str = "https://daruma.mcpbox.ru/web/";
const AUTHORIZE_URL: &str = "https://mcpbox.ru/oauth/authorize";
const RESOURCE: &str = "https://daruma.mcpbox.ru/v1/mcp";
const SCOPE: &str = "workspace:default mcp:tools";

#[derive(Serialize)]
struct RegisterRequest<'a> {
    client_name: &'a str,
    redirect_uris: [&'a str; 1],
    scope: &'a str,
}

#[derive(Deserialize)]
struct RegisterResponse {
    client_id: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    workspace_id: String,
}

/// Return the stored token, if any.
pub fn current() -> Option<String> {
    let storage = web_sys::window()?.local_storage().ok().flatten()?;
    storage.get_item(STORAGE_KEY).ok().flatten()
}

/// Persist `token` to `localStorage`.
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

/// Return the OAuth authorization code from the current URL, if present.
pub fn code_from_url() -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    query_param(&search, "code")
}

/// Start Authorization Code + PKCE login. This redirects the browser.
pub async fn start_oauth() -> Result<(), String> {
    let verifier = random_urlsafe(32)?;
    let challenge = code_challenge(&verifier).await?;
    let state = random_urlsafe(24)?;
    let client_id = oauth_client_id().await?;

    let session = session_storage()?;
    session
        .set_item(VERIFIER_SESSION_KEY, &verifier)
        .map_err(js_error)?;
    session
        .set_item(STATE_SESSION_KEY, &state)
        .map_err(js_error)?;

    let authorize_url = format!(
        "{AUTHORIZE_URL}?response_type=code&client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256&scope={}&resource={}",
        encode_component(&client_id),
        encode_component(REDIRECT_URI),
        encode_component(&state),
        encode_component(&challenge),
        encode_component(SCOPE),
        encode_component(RESOURCE),
    );
    web_sys::window()
        .ok_or_else(|| "window unavailable".to_string())?
        .location()
        .set_href(&authorize_url)
        .map_err(js_error)
}

/// Exchange the OAuth authorization code in the current URL for a bearer token.
pub async fn exchange_code(code: String) -> Result<(), String> {
    let search = web_sys::window()
        .ok_or_else(|| "window unavailable".to_string())?
        .location()
        .search()
        .map_err(js_error)?;
    let returned_state =
        query_param(&search, "state").ok_or_else(|| "oauth state missing".to_string())?;
    let session = session_storage()?;
    let expected_state = session
        .get_item(STATE_SESSION_KEY)
        .map_err(js_error)?
        .ok_or_else(|| "oauth state not found".to_string())?;
    if returned_state != expected_state {
        return Err("oauth state mismatch".to_string());
    }
    let verifier = session
        .get_item(VERIFIER_SESSION_KEY)
        .map_err(js_error)?
        .ok_or_else(|| "oauth code verifier not found".to_string())?;
    let client_id = oauth_client_id().await?;
    let body = form_body(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", &client_id),
        ("code_verifier", &verifier),
        ("resource", RESOURCE),
    ]);

    let resp = Request::post("/oauth/token")
        .credentials(web_sys::RequestCredentials::Include)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !(200..300).contains(&(resp.status() as u32)) {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("oauth token exchange failed: {body}"));
    }
    let token = resp
        .json::<TokenResponse>()
        .await
        .map_err(|e| e.to_string())?;
    set(&token.access_token);
    if let Some(storage) = local_storage() {
        let _ = storage.set_item(WORKSPACE_STORAGE_KEY, &token.workspace_id);
    }
    let _ = session.remove_item(VERIFIER_SESSION_KEY);
    let _ = session.remove_item(STATE_SESSION_KEY);
    Ok(())
}

/// Remove OAuth response params after a successful exchange.
pub fn clean_oauth_url() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(history) = window.history() else {
        return;
    };
    let location = window.location();
    let path = location.pathname().unwrap_or_else(|_| "/web/".to_string());
    let hash = location.hash().unwrap_or_default();
    let clean = format!("{path}{hash}");
    let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&clean));
}

/// Extract the value of the `token` query parameter from a raw search string
/// like `"?token=abc&foo=bar"`.
fn extract_token_param(search: &str) -> Option<String> {
    query_param(search, "token")
}

async fn oauth_client_id() -> Result<String, String> {
    if let Some(storage) = local_storage() {
        if let Ok(Some(client_id)) = storage.get_item(CLIENT_ID_STORAGE_KEY) {
            if !client_id.trim().is_empty() {
                return Ok(client_id);
            }
        }
    }

    let payload = RegisterRequest {
        client_name: "daruma-web",
        redirect_uris: [REDIRECT_URI],
        scope: SCOPE,
    };
    let body = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    let resp = Request::post("/oauth/register")
        .credentials(web_sys::RequestCredentials::Include)
        .header("content-type", "application/json")
        .body(body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !(200..300).contains(&(resp.status() as u32)) {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("oauth client register failed: {body}"));
    }
    let registered = resp
        .json::<RegisterResponse>()
        .await
        .map_err(|e| e.to_string())?;
    if let Some(storage) = local_storage() {
        let _ = storage.set_item(CLIENT_ID_STORAGE_KEY, &registered.client_id);
    }
    Ok(registered.client_id)
}

async fn code_challenge(verifier: &str) -> Result<String, String> {
    let crypto = web_sys::window()
        .ok_or_else(|| "window unavailable".to_string())?
        .crypto()
        .map_err(js_error)?;
    let subtle = crypto.subtle();
    let digest = JsFuture::from(
        subtle
            .digest_with_str_and_u8_array("SHA-256", verifier.as_bytes())
            .map_err(js_error)?,
    )
    .await
    .map_err(js_error)?;
    Ok(base64_url_no_pad(&Uint8Array::new(&digest).to_vec()))
}

fn random_urlsafe(byte_len: usize) -> Result<String, String> {
    let crypto = web_sys::window()
        .ok_or_else(|| "window unavailable".to_string())?
        .crypto()
        .map_err(js_error)?;
    let mut bytes = vec![0u8; byte_len];
    crypto
        .get_random_values_with_u8_array(&mut bytes)
        .map_err(js_error)?;
    Ok(base64_url_no_pad(&bytes))
}

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

fn session_storage() -> Result<web_sys::Storage, String> {
    web_sys::window()
        .ok_or_else(|| "window unavailable".to_string())?
        .session_storage()
        .map_err(js_error)?
        .ok_or_else(|| "sessionStorage unavailable".to_string())
}

pub(crate) fn query_param(search: &str, name: &str) -> Option<String> {
    let s = search.trim_start_matches('?');
    for pair in s.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?;
        if key == name {
            let val = parts.next().unwrap_or("").trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

fn form_body(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(key, value)| format!("{}={}", encode_component(key), encode_component(value)))
        .collect::<Vec<_>>()
        .join("&")
}

pub(crate) fn encode_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn base64_url_no_pad(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((bytes.len() * 4).div_ceil(3));
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        }
    }
    out
}

fn js_error(value: JsValue) -> String {
    value
        .as_string()
        .unwrap_or_else(|| "browser API error".to_string())
}
