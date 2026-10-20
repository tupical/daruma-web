//! Bearer-token storage and bootstrap for the Leptos/WASM frontend.
//!
//! All state is kept in `localStorage` under the key `daruma_token`.
//! No reactive signals: callers that need reactivity wrap `current()` in a
//! Leptos signal themselves.

use gloo_net::http::Request;
use js_sys::Uint8Array;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;

const STORAGE_KEY: &str = "daruma_token";
const WORKSPACE_STORAGE_KEY: &str = "daruma_workspace_id";
const CLIENT_ID_STORAGE_KEY: &str = "daruma_oauth_client_id";
const VERIFIER_SESSION_KEY: &str = "daruma_oauth_code_verifier";
const STATE_SESSION_KEY: &str = "daruma_oauth_state";
const ENDPOINTS_SESSION_KEY: &str = "daruma_oauth_endpoints";
/// RFC 9728 metadata of the API server the bundle is served next to.
const PROTECTED_RESOURCE_PATH: &str = "/.well-known/oauth-protected-resource";
/// RFC 8414 well-known suffix of the authorization server metadata.
const AUTHORIZATION_SERVER_WELL_KNOWN: &str = "/.well-known/oauth-authorization-server";
/// The bundle's `public_url` (Trunk.toml) on the page's own origin is where
/// the authorization server sends the browser back.
const REDIRECT_PATH: &str = "/web/";
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

#[derive(Deserialize)]
struct ProtectedResourceMetadata {
    resource: String,
    #[serde(default)]
    authorization_servers: Vec<String>,
}

#[derive(Deserialize)]
struct AuthorizationServerMetadata {
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
    #[serde(default)]
    code_challenge_methods_supported: Option<Vec<String>>,
}

/// OAuth endpoints of this deployment. None of them is compiled in: they are
/// discovered at startup from the API server (see [`discover_oauth`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthEndpoints {
    authorize_url: String,
    token_url: String,
    register_url: String,
    resource: String,
    redirect_uri: String,
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

/// Discover OAuth sign-in from the API server: its RFC 9728
/// protected-resource metadata names the authorization server, whose RFC 8414
/// metadata gives the endpoints. `Ok(None)` means the server publishes no
/// OAuth sign-in (the OSS daruma-server); the viewer then needs `?token=`.
pub async fn discover_oauth() -> Result<Option<OAuthEndpoints>, String> {
    let origin = web_sys::window()
        .ok_or_else(|| "window unavailable".to_string())?
        .location()
        .origin()
        .map_err(js_error)?;
    // A static host may answer an unknown path with its SPA fallback page:
    // a document that is not metadata means "no OAuth" just like a 404.
    let resource = match get_metadata::<ProtectedResourceMetadata>(PROTECTED_RESOURCE_PATH).await {
        Ok(Some(resource)) => resource,
        Ok(None) => return Ok(None),
        Err(MetadataError::Body(err)) => {
            leptos::logging::log!("[auth] no OAuth metadata: {err}");
            return Ok(None);
        }
        Err(MetadataError::Request(err)) => return Err(err),
    };
    let Some(issuer) = resource.authorization_servers.first() else {
        return Ok(None);
    };
    let metadata_url = authorization_server_metadata_url(issuer)?;
    let server = match get_metadata::<AuthorizationServerMetadata>(&metadata_url).await {
        Ok(Some(server)) => server,
        Ok(None) => return Err(format!("{metadata_url}: HTTP 404")),
        Err(MetadataError::Body(err) | MetadataError::Request(err)) => return Err(err),
    };
    endpoints_from_metadata(resource, server, &origin).map(Some)
}

/// Start Authorization Code + PKCE login. This redirects the browser.
pub async fn start_oauth(endpoints: &OAuthEndpoints) -> Result<(), String> {
    let verifier = random_urlsafe(32)?;
    let challenge = code_challenge(&verifier).await?;
    let state = random_urlsafe(24)?;
    let client_id = oauth_client_id(endpoints).await?;

    let session = session_storage()?;
    session
        .set_item(VERIFIER_SESSION_KEY, &verifier)
        .map_err(js_error)?;
    session
        .set_item(STATE_SESSION_KEY, &state)
        .map_err(js_error)?;
    // The code exchange must repeat the redirect URI and resource the
    // authorization request carried, so it reuses these exact endpoints.
    let stored = serde_json::to_string(endpoints).map_err(|e| e.to_string())?;
    session
        .set_item(ENDPOINTS_SESSION_KEY, &stored)
        .map_err(js_error)?;

    let authorize_url = format!(
        "{}{}response_type=code&client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256&scope={}&resource={}",
        endpoints.authorize_url,
        if endpoints.authorize_url.contains('?') { '&' } else { '?' },
        encode_component(&client_id),
        encode_component(&endpoints.redirect_uri),
        encode_component(&state),
        encode_component(&challenge),
        encode_component(SCOPE),
        encode_component(&endpoints.resource),
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
    let endpoints = session
        .get_item(ENDPOINTS_SESSION_KEY)
        .map_err(js_error)?
        .ok_or_else(|| "oauth endpoints not found".to_string())?;
    let endpoints: OAuthEndpoints = serde_json::from_str(&endpoints).map_err(|e| e.to_string())?;
    let client_id = oauth_client_id(&endpoints).await?;
    let body = form_body(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &endpoints.redirect_uri),
        ("client_id", &client_id),
        ("code_verifier", &verifier),
        ("resource", &endpoints.resource),
    ]);

    let resp = Request::post(&endpoints.token_url)
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
    let _ = session.remove_item(ENDPOINTS_SESSION_KEY);
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

enum MetadataError {
    /// The request failed or the server answered with an error status.
    Request(String),
    /// The server answered 2xx with a document that is not this metadata.
    Body(String),
}

async fn get_metadata<T: DeserializeOwned>(url: &str) -> Result<Option<T>, MetadataError> {
    let resp = Request::get(url)
        .send()
        .await
        .map_err(|e| MetadataError::Request(format!("{url}: {e}")))?;
    if resp.status() == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&(resp.status() as u32)) {
        return Err(MetadataError::Request(format!(
            "{url}: HTTP {}",
            resp.status()
        )));
    }
    resp.json::<T>()
        .await
        .map(Some)
        .map_err(|e| MetadataError::Body(format!("{url}: {e}")))
}

/// RFC 8414 §3.1: the well-known suffix goes between the issuer's host and
/// its path component.
fn authorization_server_metadata_url(issuer: &str) -> Result<String, String> {
    let issuer = issuer.trim().trim_end_matches('/');
    let (scheme, rest) = issuer
        .split_once("://")
        .filter(|(scheme, rest)| matches!(*scheme, "https" | "http") && !rest.is_empty())
        .ok_or_else(|| format!("authorization server is not an http(s) URL: {issuer}"))?;
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    Ok(format!(
        "{scheme}://{host}{AUTHORIZATION_SERVER_WELL_KNOWN}{path}"
    ))
}

fn endpoints_from_metadata(
    resource: ProtectedResourceMetadata,
    server: AuthorizationServerMetadata,
    origin: &str,
) -> Result<OAuthEndpoints, String> {
    if let Some(methods) = &server.code_challenge_methods_supported {
        if !methods.iter().any(|method| method == "S256") {
            return Err("authorization server does not support PKCE S256".to_string());
        }
    }
    let register_url = server.registration_endpoint.ok_or_else(|| {
        "authorization server has no dynamic client registration endpoint".to_string()
    })?;
    Ok(OAuthEndpoints {
        authorize_url: absolute_url(&server.authorization_endpoint, "authorization_endpoint")?,
        token_url: absolute_url(&server.token_endpoint, "token_endpoint")?,
        register_url: absolute_url(&register_url, "registration_endpoint")?,
        resource: absolute_url(&resource.resource, "resource")?,
        redirect_uri: format!("{}{REDIRECT_PATH}", origin.trim_end_matches('/')),
    })
}

fn absolute_url(value: &str, field: &str) -> Result<String, String> {
    let value = value.trim();
    if value.starts_with("https://") || value.starts_with("http://") {
        Ok(value.to_string())
    } else {
        Err(format!("{field} is not an absolute http(s) URL: {value}"))
    }
}

/// A client id is only valid at the server that registered it.
fn client_id_storage_key(register_url: &str) -> String {
    format!("{CLIENT_ID_STORAGE_KEY}:{register_url}")
}

async fn oauth_client_id(endpoints: &OAuthEndpoints) -> Result<String, String> {
    let storage_key = client_id_storage_key(&endpoints.register_url);
    if let Some(storage) = local_storage() {
        if let Ok(Some(client_id)) = storage.get_item(&storage_key) {
            if !client_id.trim().is_empty() {
                return Ok(client_id);
            }
        }
    }

    let payload = RegisterRequest {
        client_name: "daruma-web",
        redirect_uris: [&endpoints.redirect_uri],
        scope: SCOPE,
    };
    let body = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    let resp = Request::post(&endpoints.register_url)
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
        let _ = storage.set_item(&storage_key, &registered.client_id);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(json: &str) -> ProtectedResourceMetadata {
        serde_json::from_str(json).unwrap()
    }

    fn server(json: &str) -> AuthorizationServerMetadata {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn metadata_url_puts_well_known_before_issuer_path() {
        assert_eq!(
            authorization_server_metadata_url("https://daruma.example").unwrap(),
            "https://daruma.example/.well-known/oauth-authorization-server"
        );
        assert_eq!(
            authorization_server_metadata_url("https://auth.example/tenant/").unwrap(),
            "https://auth.example/.well-known/oauth-authorization-server/tenant"
        );
        assert!(authorization_server_metadata_url("/relative").is_err());
        assert!(authorization_server_metadata_url("ftp://auth.example").is_err());
    }

    /// The hosted cloud answers on the tool subdomain with its own origin;
    /// the result must match what the viewer used to hardcode.
    #[test]
    fn hosted_metadata_yields_same_origin_endpoints() {
        let endpoints = endpoints_from_metadata(
            resource(
                r#"{"resource":"https://daruma.mcpbox.ru/v1/mcp",
                    "authorization_servers":["https://daruma.mcpbox.ru"],
                    "scopes_supported":["workspace:default","mcp:tools"]}"#,
            ),
            server(
                r#"{"issuer":"https://daruma.mcpbox.ru",
                    "authorization_endpoint":"https://daruma.mcpbox.ru/oauth/authorize",
                    "token_endpoint":"https://daruma.mcpbox.ru/oauth/token",
                    "registration_endpoint":"https://daruma.mcpbox.ru/oauth/register",
                    "code_challenge_methods_supported":["S256"]}"#,
            ),
            "https://daruma.mcpbox.ru",
        )
        .unwrap();
        assert_eq!(
            endpoints,
            OAuthEndpoints {
                authorize_url: "https://daruma.mcpbox.ru/oauth/authorize".into(),
                token_url: "https://daruma.mcpbox.ru/oauth/token".into(),
                register_url: "https://daruma.mcpbox.ru/oauth/register".into(),
                resource: "https://daruma.mcpbox.ru/v1/mcp".into(),
                redirect_uri: "https://daruma.mcpbox.ru/web/".into(),
            }
        );
    }

    #[test]
    fn self_hosted_origin_drives_redirect_uri() {
        let endpoints = endpoints_from_metadata(
            resource(r#"{"resource":"http://127.0.0.1:8080/v1/mcp"}"#),
            server(
                r#"{"authorization_endpoint":"http://127.0.0.1:8080/oauth/authorize",
                    "token_endpoint":"http://127.0.0.1:8080/oauth/token",
                    "registration_endpoint":"http://127.0.0.1:8080/oauth/register"}"#,
            ),
            "http://127.0.0.1:8080/",
        )
        .unwrap();
        assert_eq!(endpoints.redirect_uri, "http://127.0.0.1:8080/web/");
        assert_eq!(endpoints.resource, "http://127.0.0.1:8080/v1/mcp");
    }

    #[test]
    fn unusable_authorization_server_is_rejected() {
        let res = || resource(r#"{"resource":"https://d.example/v1/mcp"}"#);
        let no_s256 = server(
            r#"{"authorization_endpoint":"https://d.example/a","token_endpoint":"https://d.example/t",
                "registration_endpoint":"https://d.example/r","code_challenge_methods_supported":["plain"]}"#,
        );
        assert!(endpoints_from_metadata(res(), no_s256, "https://d.example")
            .unwrap_err()
            .contains("S256"));
        let no_registration = server(
            r#"{"authorization_endpoint":"https://d.example/a","token_endpoint":"https://d.example/t"}"#,
        );
        assert!(
            endpoints_from_metadata(res(), no_registration, "https://d.example")
                .unwrap_err()
                .contains("registration")
        );
        let relative = server(
            r#"{"authorization_endpoint":"/oauth/authorize","token_endpoint":"https://d.example/t",
                "registration_endpoint":"https://d.example/r"}"#,
        );
        assert!(
            endpoints_from_metadata(res(), relative, "https://d.example")
                .unwrap_err()
                .contains("authorization_endpoint")
        );
    }
}
