//! Published camera snapshot: the worldwide Overpass answer, fetched once a day by the
//! `camera-snapshot` GitHub Actions workflow and attached to a rolling release. Installs
//! download that file instead of each running the ~200 s worldwide query themselves, so
//! Overpass serves one request a day in total however many copies of the app exist.
//!
//! The release carries `manifest.json` (small, checked daily) and one gzipped response
//! named after its generation time. The manifest is uploaded last, so it only ever
//! names a file that is already complete.

use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;

/// Release tag the workflow publishes to.
pub const RELEASE_TAG: &str = "camera-snapshot";
pub const MANIFEST_FILE: &str = "manifest.json";
/// Manifest format this build understands.
pub const FORMAT: u32 = 1;
/// How often installs look at the manifest. A check that finds nothing new downloads
/// only the manifest (a few hundred bytes).
pub const CHECK_INTERVAL_SECS: i64 = 24 * 3600;
/// A snapshot older than this means the workflow has stopped; the app then falls back
/// to querying Overpass itself (at the user's sync interval, not daily).
pub const MAX_AGE_SECS: i64 = 14 * 24 * 3600;
/// Refuse to inflate more than this (the 2026-09 response is 57 MB of JSON).
const MAX_JSON_BYTES: u64 = 1 << 30;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub format: u32,
    /// Unix seconds when the workflow queried Overpass.
    pub generated_at: i64,
    /// `osm3s.timestamp_osm_base` of the answer, e.g. `2026-09-24T04:17:02Z`.
    #[serde(default)]
    pub osm_base: Option<String>,
    pub elements: i64,
    /// File name of the gzipped response, next to the manifest.
    pub file: String,
    /// Compressed size in bytes.
    pub bytes: u64,
    /// SHA-256 of the compressed file, lowercase hex.
    pub sha256: String,
}

/// The manifest URL implied by the crate's `repository` (a GitHub URL), or `None` while
/// that is still the `OWNER` placeholder or not on GitHub.
pub fn default_manifest_url() -> Option<String> {
    manifest_url_for_repo(env!("CARGO_PKG_REPOSITORY"))
}

fn manifest_url_for_repo(repo: &str) -> Option<String> {
    let repo = repo.trim().trim_end_matches('/').trim_end_matches(".git");
    let path = repo.strip_prefix("https://github.com/")?;
    let mut parts = path.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    if parts.next().is_some() || owner.is_empty() || name.is_empty() || owner == "OWNER" {
        return None;
    }
    Some(format!("{repo}/releases/download/{RELEASE_TAG}/{MANIFEST_FILE}"))
}

/// The manifest to use under `settings`, or `None` to query Overpass directly.
pub fn manifest_url(settings: &crate::db::Settings) -> Option<String> {
    if settings.sync_source == "overpass" {
        return None;
    }
    if !settings.snapshot_url.is_empty() {
        return Some(settings.snapshot_url.clone());
    }
    default_manifest_url()
}

pub fn parse_manifest(body: &str) -> AppResult<Manifest> {
    let m: Manifest = serde_json::from_str(body)
        .map_err(|e| AppError::Parse(format!("snapshot manifest is not valid: {e}")))?;
    if m.format != FORMAT {
        return Err(AppError::Parse(format!(
            "snapshot manifest format {} is not supported by this version (expects {FORMAT})",
            m.format
        )));
    }
    let safe_name = !m.file.is_empty()
        && !m.file.starts_with('.')
        && m.file.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !safe_name {
        return Err(AppError::Parse(format!("snapshot manifest names an invalid file: {:?}", m.file)));
    }
    if m.sha256.len() != 64 || !m.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::Parse("snapshot manifest has an invalid sha256".into()));
    }
    if m.elements <= 0 {
        return Err(AppError::Parse("snapshot manifest lists no cameras".into()));
    }
    Ok(m)
}

/// URL of the data file: `file` resolved next to the manifest.
pub fn data_url(manifest_url: &str, m: &Manifest) -> AppResult<String> {
    let base = url::Url::parse(manifest_url)
        .map_err(|e| AppError::Invalid(format!("snapshot URL {manifest_url:?}: {e}")))?;
    base.join(&m.file)
        .map(String::from)
        .map_err(|e| AppError::Invalid(format!("snapshot file URL: {e}")))
}

/// Verify the compressed file against the manifest and inflate it to the Overpass JSON.
pub fn decode(gz: &[u8], m: &Manifest) -> AppResult<String> {
    if gz.len() as u64 != m.bytes {
        return Err(AppError::Parse(format!(
            "snapshot is {} bytes but the manifest says {}",
            gz.len(),
            m.bytes
        )));
    }
    let digest = Sha256::digest(gz);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    if !hex.eq_ignore_ascii_case(&m.sha256) {
        return Err(AppError::Parse("snapshot checksum does not match its manifest".into()));
    }
    let mut out = String::new();
    flate2::read::GzDecoder::new(gz)
        .take(MAX_JSON_BYTES)
        .read_to_string(&mut out)
        .map_err(|e| AppError::Parse(format!("snapshot could not be decompressed: {e}")))?;
    Ok(out)
}

/// Whether a manifest is too old to trust as current data.
pub fn is_stale(m: &Manifest, now: i64) -> bool {
    now - m.generated_at > MAX_AGE_SECS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn gz(s: &str) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(s.as_bytes()).unwrap();
        e.finish().unwrap()
    }

    fn manifest_for(data: &[u8]) -> Manifest {
        Manifest {
            format: 1,
            generated_at: 1_790_000_000,
            osm_base: Some("2026-09-24T04:17:02Z".into()),
            elements: 1,
            file: "cameras-1790000000.json.gz".into(),
            bytes: data.len() as u64,
            sha256: Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect(),
        }
    }

    #[test]
    fn repository_placeholder_has_no_snapshot() {
        assert_eq!(manifest_url_for_repo("https://github.com/OWNER/flockfinder"), None);
        assert_eq!(manifest_url_for_repo("https://gitlab.com/a/b"), None);
        assert_eq!(
            manifest_url_for_repo("https://github.com/alice/flockfinder/"),
            Some("https://github.com/alice/flockfinder/releases/download/camera-snapshot/manifest.json".into())
        );
    }

    #[test]
    fn manifest_validation() {
        let good = r#"{"format":1,"generated_at":1790000000,"osm_base":"2026-09-24T04:17:02Z","elements":150947,
            "file":"cameras-1790000000.json.gz","bytes":3900000,"sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#;
        let m = parse_manifest(good).unwrap();
        assert_eq!(m.elements, 150_947);
        assert!(parse_manifest(&good.replace("\"format\":1", "\"format\":2")).is_err());
        assert!(parse_manifest(&good.replace("cameras-1790000000.json.gz", "../evil")).is_err());
        assert!(parse_manifest(&good.replace("cameras-1790000000.json.gz", "https:x")).is_err());
        assert!(parse_manifest(&good.replace("0123456789abcdef0123", "zz")).is_err());
        assert!(parse_manifest("not json").is_err());
    }

    #[test]
    fn data_file_resolves_next_to_the_manifest() {
        let m = manifest_for(b"x");
        assert_eq!(
            data_url("https://github.com/a/b/releases/download/camera-snapshot/manifest.json", &m).unwrap(),
            "https://github.com/a/b/releases/download/camera-snapshot/cameras-1790000000.json.gz"
        );
    }

    #[test]
    fn decode_checks_size_and_checksum() {
        let body = crate::overpass::SAMPLE_FIXTURE;
        let data = gz(body);
        let m = manifest_for(&data);
        assert_eq!(decode(&data, &m).unwrap(), body);

        let mut tampered = data.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(decode(&tampered, &m).is_err());
        assert!(decode(&data[..data.len() - 1], &m).is_err());
    }

    #[test]
    fn staleness() {
        let m = manifest_for(b"x");
        assert!(!is_stale(&m, m.generated_at + MAX_AGE_SECS));
        assert!(is_stale(&m, m.generated_at + MAX_AGE_SECS + 1));
    }
}
