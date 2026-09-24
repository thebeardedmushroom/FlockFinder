//! OpenStreetMap OAuth 2.0 (PKCE) sign-in and one-node-per-changeset uploads.
//!
//! The access token lives in the OS keychain (via `keyring`), never in SQLite.

use crate::error::{AppError, AppResult};
use crate::http::HttpClient;
use crate::submissions::{osm_tags, xml_escape, Submission};
use base64::Engine;
use serde::Serialize;
use sha2::{Digest, Sha256};

pub const AUTHORIZE_URL: &str = "https://www.openstreetmap.org/oauth2/authorize";
pub const TOKEN_URL: &str = "https://www.openstreetmap.org/oauth2/token";
pub const API_BASE: &str = "https://api.openstreetmap.org/api/0.6";
pub const REDIRECT_URI: &str = "flockfinder://oauth/callback";
pub const SCOPE: &str = "write_api";

const KEYRING_SERVICE: &str = "FlockFinder";
const KEYRING_USER: &str = "osm_access_token";
/// A sign-in that has not completed within this window is discarded.
pub const PENDING_TTL_SECS: i64 = 15 * 60;

#[derive(Debug, Clone)]
pub struct PendingAuth {
    pub state: String,
    pub verifier: String,
    pub client_id: String,
    pub started_at: i64,
}

fn random_bytes(n: usize) -> AppResult<Vec<u8>> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).map_err(|e| AppError::Other(format!("secure randomness unavailable: {e}")))?;
    Ok(buf)
}

pub fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn pkce_challenge(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

/// (verifier, challenge) per RFC 7636, S256 method.
pub fn pkce_pair() -> AppResult<(String, String)> {
    let verifier = b64url(&random_bytes(48)?); // 64 url-safe chars
    let challenge = pkce_challenge(&verifier);
    Ok((verifier, challenge))
}

pub fn new_state() -> AppResult<String> {
    Ok(b64url(&random_bytes(24)?))
}

pub fn authorize_url(client_id: &str, state: &str, challenge: &str) -> String {
    let mut url = url::Url::parse(AUTHORIZE_URL).expect("static url");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", SCOPE)
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    url.to_string()
}

/// Extract `(code, state)` from the deep-link callback URL.
pub fn parse_callback(url: &url::Url) -> AppResult<(String, String)> {
    if url.scheme() != "flockfinder" {
        return Err(AppError::Invalid(format!("unexpected callback scheme {}", url.scheme())));
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            "error_description" => error = Some(v.into_owned()),
            _ => {}
        }
    }
    if let Some(e) = error {
        return Err(AppError::Other(format!("OSM refused the sign-in: {e}")));
    }
    match (code, state) {
        (Some(c), Some(s)) => Ok((c, s)),
        _ => Err(AppError::Invalid("callback URL is missing code or state".into())),
    }
}

pub async fn exchange_code(http: &HttpClient, client_id: &str, code: &str, verifier: &str) -> AppResult<String> {
    let resp = http
        .send_with_backoff("OSM token endpoint", || {
            http.client.post(TOKEN_URL).form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", REDIRECT_URI),
                ("client_id", client_id),
                ("code_verifier", verifier),
            ])
        })
        .await?;
    let body: serde_json::Value = resp.json().await?;
    body.get("access_token")
        .and_then(|t| t.as_str())
        .map(String::from)
        .ok_or_else(|| AppError::Parse("token response has no access_token".into()))
}

// ---------------------------------------------------------------------------
// Keychain
// ---------------------------------------------------------------------------

// Desktop uses keyring's v1 API; Android has no v1 store, so it talks to keyring-core
// directly with the Keystore-backed Shared Preferences store. Same Entry/Error shape.
#[cfg(not(target_os = "android"))]
use keyring::{Entry, Error as KeyringError};
#[cfg(target_os = "android")]
use keyring_core::{Entry, Error as KeyringError};

/// Install the Android store as keyring-core's default, once per process.
#[cfg(target_os = "android")]
fn android_store() -> AppResult<()> {
    static INIT: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        let store = android_native_keyring_store::Store::new().map_err(|e| e.to_string())?;
        keyring_core::set_default_store(store);
        Ok(())
    })
    .clone()
    .map_err(|e| AppError::Other(format!("Android keystore unavailable: {e}")))
}

fn entry() -> AppResult<Entry> {
    #[cfg(target_os = "android")]
    android_store()?;
    Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .map_err(|e| AppError::Other(format!("OS keychain unavailable: {e}")))
}

pub fn store_token(token: &str) -> AppResult<()> {
    entry()?
        .set_password(token)
        .map_err(|e| AppError::Other(format!("could not store token in OS keychain: {e}")))
}

pub fn load_token() -> AppResult<Option<String>> {
    match entry()?.get_password() {
        Ok(t) if !t.is_empty() => Ok(Some(t)),
        Ok(_) => Ok(None),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(e) => Err(AppError::Other(format!("could not read OS keychain: {e}"))),
    }
}

pub fn clear_token() -> AppResult<()> {
    match entry()?.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(e) => Err(AppError::Other(format!("could not clear token: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

pub fn created_by() -> String {
    format!("FlockFinder/{}", env!("CARGO_PKG_VERSION"))
}

pub fn changeset_xml(comment: &str) -> String {
    format!(
        "<osm>\n  <changeset>\n    <tag k=\"created_by\" v=\"{}\"/>\n    <tag k=\"comment\" v=\"{}\"/>\n  </changeset>\n</osm>\n",
        xml_escape(&created_by()),
        xml_escape(comment)
    )
}

pub fn node_xml(changeset_id: i64, lat: f64, lon: f64, tags: &[(String, String)]) -> String {
    let mut xml = format!(
        "<osm>\n  <node changeset=\"{changeset_id}\" lat=\"{lat:.7}\" lon=\"{lon:.7}\">\n"
    );
    for (k, v) in tags {
        xml.push_str(&format!(
            "    <tag k=\"{}\" v=\"{}\"/>\n",
            xml_escape(k),
            xml_escape(v)
        ));
    }
    xml.push_str("  </node>\n</osm>\n");
    xml
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadResult {
    pub node_id: i64,
    pub changeset_id: i64,
}

fn map_api_error(e: AppError) -> AppError {
    match e {
        AppError::Http { status: 401, .. } => {
            let _ = clear_token();
            AppError::AuthRequired
        }
        other => other,
    }
}

async fn api_put(http: &HttpClient, token: &str, path: &str, body: String) -> AppResult<String> {
    let url = format!("{API_BASE}{path}");
    let resp = http
        .send_with_backoff("OSM API", || {
            http.client
                .put(&url)
                .bearer_auth(token)
                .header(reqwest::header::CONTENT_TYPE, "text/xml; charset=utf-8")
                .body(body.clone())
        })
        .await
        .map_err(map_api_error)?;
    Ok(resp.text().await?)
}

/// Upload one submission as its own changeset. Never batched; never retried.
pub async fn upload_submission(http: &HttpClient, token: &str, sub: &Submission, comment: &str) -> AppResult<UploadResult> {
    let comment = comment.trim();
    if comment.is_empty() {
        return Err(AppError::Invalid("a changeset comment is required".into()));
    }
    let cs_body = api_put(http, token, "/changeset/create", changeset_xml(comment)).await?;
    let changeset_id: i64 = cs_body
        .trim()
        .parse()
        .map_err(|_| AppError::Parse(format!("unexpected changeset id response: {cs_body}")))?;

    let node_body = api_put(
        http,
        token,
        "/node/create",
        node_xml(changeset_id, sub.lat, sub.lon, &osm_tags(sub)),
    )
    .await;
    // Always try to close the changeset, even if node creation failed.
    let close = api_put(http, token, &format!("/changeset/{changeset_id}/close"), String::new()).await;
    let node_body = node_body?;
    if let Err(e) = close {
        log::warn!("changeset {changeset_id} close failed (OSM will auto-close it): {e}");
    }
    let node_id: i64 = node_body
        .trim()
        .parse()
        .map_err(|_| AppError::Parse(format!("unexpected node id response: {node_body}")))?;
    Ok(UploadResult { node_id, changeset_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc_example() {
        // RFC 7636 appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(pkce_challenge(verifier), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let (v, c) = pkce_pair().unwrap();
        assert_eq!(v.len(), 64);
        assert_eq!(pkce_challenge(&v), c);
    }

    #[test]
    fn authorize_url_carries_pkce_and_scope() {
        let u = authorize_url("abc", "st", "ch");
        assert!(u.starts_with(AUTHORIZE_URL));
        assert!(u.contains("client_id=abc"));
        assert!(u.contains("scope=write_api"));
        assert!(u.contains("code_challenge=ch"));
        assert!(u.contains("code_challenge_method=S256"));
        assert!(u.contains("redirect_uri=flockfinder%3A%2F%2Foauth%2Fcallback"));
    }

    #[test]
    fn callback_parsing() {
        let u = url::Url::parse("flockfinder://oauth/callback?code=xyz&state=st").unwrap();
        assert_eq!(parse_callback(&u).unwrap(), ("xyz".into(), "st".into()));
        let u = url::Url::parse("flockfinder://oauth/callback?error=access_denied").unwrap();
        assert!(parse_callback(&u).is_err());
        let u = url::Url::parse("https://evil.example/callback?code=x&state=y").unwrap();
        assert!(parse_callback(&u).is_err());
    }

    #[test]
    fn changeset_and_node_xml() {
        let cs = changeset_xml("Add Flock camera at 5th & Main");
        assert!(cs.contains("k=\"created_by\" v=\"FlockFinder/"));
        assert!(cs.contains("v=\"Add Flock camera at 5th &amp; Main\""));
        let node = node_xml(77, 39.7, -105.0, &[("man_made".into(), "surveillance".into())]);
        assert!(node.contains("<node changeset=\"77\" lat=\"39.7000000\" lon=\"-105.0000000\">"));
        assert!(node.contains("<tag k=\"man_made\" v=\"surveillance\"/>"));
    }
}
