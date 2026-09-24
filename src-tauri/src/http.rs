//! Shared HTTP client: descriptive User-Agent, exponential backoff, Nominatim rate limit.
//!
//! Every outbound request in the app goes through this module.

use crate::error::{AppError, AppResult};
use std::time::{Duration, Instant};

/// Backoff schedule for HTTP 429 / 504 (seconds). Never more than three retries.
pub const BACKOFF_SECS: [u64; 3] = [2, 8, 30];
/// Minimum spacing between Nominatim requests.
pub const NOMINATIM_MIN_INTERVAL: Duration = Duration::from_millis(1000);

pub fn user_agent() -> String {
    format!(
        "FlockFinder/{} (+{})",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_REPOSITORY")
    )
}

pub fn should_retry(status: u16) -> bool {
    status == 429 || status == 504
}

pub struct HttpClient {
    pub client: reqwest::Client,
    nominatim_last: tokio::sync::Mutex<Option<Instant>>,
}

impl HttpClient {
    pub fn new() -> AppResult<Self> {
        let builder = reqwest::Client::builder()
            .user_agent(user_agent())
            .timeout(Duration::from_secs(90))
            .connect_timeout(Duration::from_secs(15));
        #[cfg(target_os = "android")]
        let builder = builder.tls_backend_preconfigured(android_tls_config());
        let client = builder
            .build()
            .map_err(|e| AppError::Other(format!("failed to build HTTP client: {e}")))?;
        Ok(HttpClient {
            client,
            nominatim_last: tokio::sync::Mutex::new(None),
        })
    }

    /// Send a request, retrying on 429/504 with the fixed backoff schedule.
    /// Connection failures are reported as `Offline` immediately (no retry — offline is
    /// an expected state, not an error to hammer through).
    pub async fn send_with_backoff<F>(&self, label: &str, make: F) -> AppResult<reqwest::Response>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut attempt = 0usize;
        loop {
            let resp = make().send().await?;
            let status = resp.status();
            if should_retry(status.as_u16()) {
                if attempt >= BACKOFF_SECS.len() {
                    return Err(AppError::RateLimited(label.to_string()));
                }
                let delay = BACKOFF_SECS[attempt];
                attempt += 1;
                log::warn!("{label} returned HTTP {status}; retrying in {delay}s (attempt {attempt})");
                tokio::time::sleep(Duration::from_secs(delay)).await;
                continue;
            }
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                let body: String = body.chars().take(400).collect();
                return Err(AppError::Http {
                    status: status.as_u16(),
                    endpoint: label.to_string(),
                    body,
                });
            }
            return Ok(resp);
        }
    }

    /// Block until at least one second has passed since the previous Nominatim request.
    pub async fn nominatim_slot(&self) {
        let mut last = self.nominatim_last.lock().await;
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < NOMINATIM_MIN_INTERVAL {
                tokio::time::sleep(NOMINATIM_MIN_INTERVAL - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }
}

/// Android TLS: rustls on `ring` with Mozilla's bundled root store (see Cargo.toml for why).
/// A preconfigured config must carry its own ALPN list, or reqwest falls back to HTTP/1.1.
#[cfg(target_os = "android")]
fn android_tls_config() -> rustls::ClientConfig {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let mut config =
        rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring supports the default TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_has_version_and_contact() {
        let ua = user_agent();
        assert!(ua.starts_with("FlockFinder/"));
        assert!(ua.contains("(+http"), "ua={ua}");
    }

    #[test]
    fn retry_policy_matches_spec() {
        assert!(should_retry(429));
        assert!(should_retry(504));
        assert!(!should_retry(500));
        assert!(!should_retry(404));
        assert!(!should_retry(200));
        assert_eq!(BACKOFF_SECS, [2, 8, 30]);
    }
}
