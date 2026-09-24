//! Worldwide camera sync: every ALPR camera in the world, committed in one transaction.
//! Every zoom level of the map is served from the local copy this produces; moving the map
//! never touches the network.
//!
//! The data normally comes from the published daily snapshot (see `snapshot.rs`), which is
//! checked once a day. When no snapshot is published, or it is more than two weeks old,
//! the app runs the worldwide Overpass query itself at the user's sync interval instead.

use crate::db::{self, Settings, SyncSummary};
use crate::error::{AppError, AppResult};
use crate::overpass;
use crate::snapshot::{self, Manifest};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// After a failed attempt, the scheduler waits this long before trying again on its own.
pub const RETRY_AFTER_SECS: i64 = 3600;
/// Whole-request budget for the download (Overpass itself gives up after its query timeout).
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(900);
/// Emit a progress event at most once per this many downloaded bytes.
const PROGRESS_STEP_BYTES: u64 = 2 * 1024 * 1024;
/// Settings-table key holding `DataState`.
const DATA_STATE_KEY: &str = "sync_data";

/// Emitted with a `SyncStatus` whenever a sync starts, advances or ends.
pub const EVENT_STATUS: &str = "sync:status";
/// Emitted whenever the cameras table changed, so the map reloads its snapshot.
pub const EVENT_CAMERAS_CHANGED: &str = "cameras:changed";

/// In-memory state of the sync that is running right now, if any.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub running: bool,
    pub phase: Option<&'static str>,
    pub bytes: u64,
}

/// What the cameras table currently reflects.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DataState {
    /// `snapshot`, `overpass`, `fixture` or `file`.
    source: Option<String>,
    /// OSM database timestamp of the data (`osm3s.timestamp_osm_base`).
    osm_base: Option<String>,
    /// `generated_at` of the snapshot applied last, so an unchanged one is not re-downloaded.
    snapshot_generated_at: Option<i64>,
    /// When data was last actually applied (not just checked).
    ingested_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncStatus {
    pub running: bool,
    /// `downloading`, `parsing` or `saving` while running.
    pub phase: Option<String>,
    /// Bytes received so far, while running.
    pub bytes: u64,
    pub last_ok_at: Option<i64>,
    pub last_attempt_at: Option<i64>,
    /// `ok`, `error` or `offline` for the latest finished attempt.
    pub last_outcome: Option<String>,
    pub last_error: Option<String>,
    /// Elements and bytes of the latest sync that downloaded data.
    pub last_elements: Option<i64>,
    pub last_bytes: Option<i64>,
    pub last_duration_secs: Option<i64>,
    /// Cameras currently counted (not stale), and stale ones kept hollow.
    pub cameras: i64,
    pub stale_cameras: i64,
    pub interval_days: u32,
    pub next_due_at: i64,
    pub fixture_mode: bool,
    /// Where syncs come from under the current settings: `snapshot` or `overpass`.
    pub source: String,
    /// Where the stored cameras came from, and the OSM database time they reflect.
    pub data_source: Option<String>,
    pub data_as_of: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncOutcome {
    pub elements: usize,
    pub skipped: usize,
    pub upserted: usize,
    pub marked_stale: usize,
    pub purged: usize,
    pub bytes: usize,
    /// `snapshot`, `overpass`, `fixture` or `file`; empty when nothing was downloaded.
    pub source: String,
    /// True when the check found nothing newer than the stored data.
    pub unchanged: bool,
}

fn fixture_mode() -> bool {
    std::env::var("FLOCKFINDER_OFFLINE_FIXTURE").map_or(false, |v| v == "1")
}

/// How often the scheduler syncs: daily against the snapshot (a check that finds nothing
/// new costs one tiny request), at the user's interval when querying Overpass directly.
pub fn interval_secs(settings: &Settings) -> i64 {
    if snapshot::manifest_url(settings).is_some() {
        snapshot::CHECK_INTERVAL_SECS.min(settings.ttl_secs())
    } else {
        settings.ttl_secs()
    }
}

/// When the next automatic sync is due. Never synced → now. A failed attempt newer than the
/// last success postpones the retry by `RETRY_AFTER_SECS` so a broken network or an
/// overloaded server is not hammered once a minute.
pub fn next_due(summary: &SyncSummary, interval_secs: i64, now: i64) -> i64 {
    let by_age = summary.last_ok_at.map_or(now, |t| t + interval_secs);
    let failed_since_ok = match (summary.last_attempt_at, summary.last_ok_at) {
        (Some(a), Some(ok)) => a > ok && summary.last_outcome.as_deref() != Some("ok"),
        (Some(_), None) => true,
        _ => false,
    };
    if failed_since_ok {
        by_age.max(summary.last_attempt_at.unwrap_or(now) + RETRY_AFTER_SECS)
    } else {
        by_age
    }
}

fn load_data_state(conn: &rusqlite::Connection) -> DataState {
    db::get_json(conn, DATA_STATE_KEY).ok().flatten().unwrap_or_default()
}

pub fn status(app: &AppHandle) -> AppResult<SyncStatus> {
    let state = app.state::<AppState>();
    let progress = state.sync_progress().clone();
    let conn = state.conn();
    let summary = db::sync_summary(&conn)?;
    let settings = db::load_settings(&conn)?;
    let data = load_data_state(&conn);
    Ok(SyncStatus {
        running: progress.running,
        phase: progress.phase.map(String::from),
        bytes: progress.bytes,
        next_due_at: next_due(&summary, interval_secs(&settings), db::now()),
        last_ok_at: summary.last_ok_at,
        last_attempt_at: summary.last_attempt_at,
        last_outcome: summary.last_outcome,
        last_error: summary.last_error,
        last_elements: summary.last_elements,
        last_bytes: summary.last_bytes,
        last_duration_secs: summary.last_duration_secs,
        cameras: summary.cameras,
        stale_cameras: summary.stale_cameras,
        interval_days: settings.cache_ttl_days,
        fixture_mode: fixture_mode(),
        source: if snapshot::manifest_url(&settings).is_some() { "snapshot" } else { "overpass" }.into(),
        data_source: data.source,
        data_as_of: data.osm_base,
    })
}

fn emit_status(app: &AppHandle) {
    match status(app) {
        Ok(s) => {
            let _ = app.emit(EVENT_STATUS, &s);
        }
        Err(e) => log::warn!("could not read sync status: {e}"),
    }
}

fn set_progress(app: &AppHandle, phase: &'static str, bytes: u64) {
    {
        let state = app.state::<AppState>();
        let mut p = state.sync_progress();
        p.phase = Some(phase);
        p.bytes = bytes;
    }
    emit_status(app);
}

/// Run one worldwide sync now. Refuses to start while another is running. `manual` ("Sync
/// now") may fall back to a direct Overpass query even when the stored data is still
/// within the sync interval; the scheduler may not.
pub async fn run_sync(app: &AppHandle, manual: bool) -> AppResult<SyncOutcome> {
    let state = app.state::<AppState>();
    {
        let mut p = state.sync_progress();
        if p.running {
            return Err(AppError::Other("a camera sync is already running".into()));
        }
        *p = Progress {
            running: true,
            phase: Some("downloading"),
            bytes: 0,
        };
    }
    let started = db::now();
    let run_id = {
        let conn = state.conn();
        db::sync_run_start(&conn, started)
    };
    let run_id = match run_id {
        Ok(id) => id,
        Err(e) => {
            *state.sync_progress() = Progress::default();
            return Err(e);
        }
    };
    emit_status(app);
    let result = sync_inner(app, started, manual).await;
    {
        let conn = state.conn();
        let recorded = match &result {
            Ok(o) if o.unchanged => db::sync_run_finish(&conn, run_id, "ok", Some("unchanged"), None, None, None),
            Ok(o) => db::sync_run_finish(&conn, run_id, "ok", Some(&o.source), Some(o.elements), Some(o.marked_stale), Some(o.bytes)),
            Err(e) => {
                let outcome = if e.is_offline() { "offline" } else { "error" };
                db::sync_run_finish(&conn, run_id, outcome, Some(&e.to_string()), None, None, None)
            }
        };
        if let Err(e) = recorded {
            log::warn!("could not record the sync run: {e}");
        }
    }
    *state.sync_progress() = Progress::default();
    emit_status(app);

    match &result {
        Ok(o) if o.unchanged => log::info!("camera sync: nothing newer than the stored data"),
        Ok(o) => {
            log::info!(
                "camera sync done from {}: {} elements ({} skipped, {} bytes), {} marked stale, {} purged in {}s",
                o.source,
                o.elements,
                o.skipped,
                o.bytes,
                o.marked_stale,
                o.purged,
                db::now() - started
            );
            let _ = app.emit(EVENT_CAMERAS_CHANGED, ());
            // New cameras inside saved watch areas and routes notify like any other refresh.
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = crate::alerts::run_refresh_local(&app).await {
                    log::info!("post-sync alert check skipped: {e}");
                }
            });
        }
        Err(e) => log::warn!("camera sync failed: {e}"),
    }
    result
}

/// The body to ingest, or nothing new to apply.
enum Fetched {
    Body {
        body: String,
        /// Bytes transferred (compressed for the snapshot).
        bytes: usize,
        source: &'static str,
        manifest: Option<Manifest>,
    },
    Unchanged,
}

/// Why the snapshot could not be used.
enum SnapshotError {
    /// There is no usable snapshot (not published, outdated, a format this build does not
    /// read): query Overpass directly instead.
    Unavailable(String),
    /// A transient failure (offline, server error, corrupt download): retry later.
    Failed(AppError),
}

impl From<AppError> for SnapshotError {
    fn from(e: AppError) -> Self {
        SnapshotError::Failed(e)
    }
}

async fn sync_inner(app: &AppHandle, started: i64, manual: bool) -> AppResult<SyncOutcome> {
    let state = app.state::<AppState>();
    let (settings, data, has_cameras) = {
        let conn = state.conn();
        let mut data = load_data_state(&conn);
        // Installs from before the snapshot existed have sync runs but no data state yet.
        if data.ingested_at.is_none() {
            data.ingested_at = db::sync_summary(&conn)?.last_ok_at;
        }
        (db::load_settings(&conn)?, data, db::camera_count(&conn)? > 0)
    };
    let fetched = if fixture_mode() {
        log::info!("FLOCKFINDER_OFFLINE_FIXTURE=1: syncing the bundled fixture instead of Overpass");
        let body = overpass::SAMPLE_FIXTURE.to_string();
        Fetched::Body { bytes: body.len(), body, source: "fixture", manifest: None }
    } else if let Some(path) = std::env::var_os("FLOCKFINDER_SYNC_FILE") {
        log::info!("FLOCKFINDER_SYNC_FILE: syncing from {} instead of Overpass", path.to_string_lossy());
        let body = std::fs::read_to_string(path)?;
        Fetched::Body { bytes: body.len(), body, source: "file", manifest: None }
    } else {
        fetch_remote(app, &state, &settings, &data, has_cameras, manual).await?
    };
    let (body, bytes, source, manifest) = match fetched {
        Fetched::Body { body, bytes, source, manifest } => (body, bytes, source, manifest),
        Fetched::Unchanged => {
            return Ok(SyncOutcome {
                elements: 0,
                skipped: 0,
                upserted: 0,
                marked_stale: 0,
                purged: 0,
                bytes: 0,
                source: String::new(),
                unchanged: true,
            })
        }
    };

    set_progress(app, "parsing", body.len() as u64);
    let parsed = tokio::task::spawn_blocking(move || overpass::parse_response(&body))
        .await
        .map_err(|e| AppError::Other(format!("parse task failed: {e}")))??;

    set_progress(app, "saving", bytes as u64);
    let app2 = app.clone();
    let elements = parsed.elements;
    let new_state = DataState {
        source: Some(source.to_string()),
        osm_base: parsed.osm_base.or_else(|| manifest.as_ref().and_then(|m| m.osm_base.clone())),
        snapshot_generated_at: manifest.as_ref().map(|m| m.generated_at),
        ingested_at: Some(db::now()),
    };
    let ingest = tokio::task::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        let mut conn = state.conn();
        let stats = db::ingest_global(&mut conn, &elements, started, db::now())?;
        db::set_json(&conn, DATA_STATE_KEY, &new_state)?;
        Ok::<_, AppError>((stats, elements.len()))
    })
    .await
    .map_err(|e| AppError::Other(format!("save task failed: {e}")))??;

    Ok(SyncOutcome {
        elements: ingest.1,
        skipped: parsed.skipped,
        upserted: ingest.0.upserted,
        marked_stale: ingest.0.marked_stale,
        purged: ingest.0.purged,
        bytes,
        source: source.to_string(),
        unchanged: false,
    })
}

async fn fetch_remote(
    app: &AppHandle,
    state: &AppState,
    settings: &Settings,
    data: &DataState,
    has_cameras: bool,
    manual: bool,
) -> AppResult<Fetched> {
    if let Some(url) = snapshot::manifest_url(settings) {
        match fetch_snapshot(app, state, &url, data, has_cameras).await {
            Ok(f) => return Ok(f),
            Err(SnapshotError::Failed(e)) => return Err(e),
            Err(SnapshotError::Unavailable(reason)) => {
                // Without a snapshot, keep the direct query to the user's interval: the
                // daily snapshot check must not turn into a daily worldwide Overpass query.
                let now = db::now();
                let fresh = has_cameras && data.ingested_at.map_or(false, |t| now - t < settings.ttl_secs());
                if fresh && !manual {
                    log::info!("{reason}; stored data is within the sync interval, not querying Overpass");
                    return Ok(Fetched::Unchanged);
                }
                log::info!("{reason}; querying Overpass directly");
            }
        }
    }
    let (body, bytes) = download_overpass(app, state, &settings.overpass_endpoint).await?;
    Ok(Fetched::Body { body, bytes, source: "overpass", manifest: None })
}

async fn fetch_snapshot(
    app: &AppHandle,
    state: &AppState,
    manifest_url: &str,
    data: &DataState,
    has_cameras: bool,
) -> Result<Fetched, SnapshotError> {
    let resp = match state
        .http
        .send_with_backoff("camera snapshot", || state.http.client.get(manifest_url))
        .await
    {
        Ok(r) => r,
        Err(AppError::Http { status: 404, .. }) => {
            return Err(SnapshotError::Unavailable(format!("no camera snapshot is published at {manifest_url}")))
        }
        Err(e) => return Err(e.into()),
    };
    let text = resp.text().await.map_err(AppError::from)?;
    let manifest = snapshot::parse_manifest(&text).map_err(|e| SnapshotError::Unavailable(e.to_string()))?;
    if snapshot::is_stale(&manifest, db::now()) {
        return Err(SnapshotError::Unavailable(format!(
            "the published camera snapshot is from {} and out of date",
            chrono::DateTime::from_timestamp(manifest.generated_at, 0).map_or_else(String::new, |t| t.to_rfc3339())
        )));
    }
    if has_cameras && data.snapshot_generated_at.map_or(false, |t| t >= manifest.generated_at) {
        return Ok(Fetched::Unchanged);
    }

    let file_url = snapshot::data_url(manifest_url, &manifest)?;
    let resp = state
        .http
        .send_with_backoff("camera snapshot", || state.http.client.get(&file_url).timeout(DOWNLOAD_TIMEOUT))
        .await?;
    let gz = read_body(app, resp, manifest.bytes as usize).await?;
    let bytes = gz.len();
    let m = manifest.clone();
    let body = tokio::task::spawn_blocking(move || snapshot::decode(&gz, &m))
        .await
        .map_err(|e| AppError::Other(format!("decompress task failed: {e}")))??;
    Ok(Fetched::Body { body, bytes, source: "snapshot", manifest: Some(manifest) })
}

async fn download_overpass(app: &AppHandle, state: &AppState, endpoint: &str) -> AppResult<(String, usize)> {
    let query = overpass::build_global_query();
    let resp = state
        .http
        .send_with_backoff("Overpass", || {
            state
                .http
                .client
                .post(endpoint)
                .timeout(DOWNLOAD_TIMEOUT)
                .form(&[("data", query.as_str())])
        })
        .await?;
    let buf = read_body(app, resp, 32 << 20).await?;
    let len = buf.len();
    let body = String::from_utf8(buf).map_err(|e| AppError::Parse(format!("Overpass response is not UTF-8: {e}")))?;
    Ok((body, len))
}

/// Read a response body, reporting download progress.
async fn read_body(app: &AppHandle, mut resp: reqwest::Response, size_hint: usize) -> AppResult<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::with_capacity(size_hint);
    let mut next_report = PROGRESS_STEP_BYTES;
    while let Some(chunk) = resp.chunk().await? {
        buf.extend_from_slice(&chunk);
        if buf.len() as u64 >= next_report {
            next_report += PROGRESS_STEP_BYTES;
            set_progress(app, "downloading", buf.len() as u64);
        }
    }
    Ok(buf)
}

/// Start a sync if one is due. Called by the scheduler; failures are logged and retried later.
pub async fn maybe_run(app: &AppHandle) {
    let due = {
        let state = app.state::<AppState>();
        if state.sync_progress().running {
            return;
        }
        let conn = state.conn();
        match (db::sync_summary(&conn), db::load_settings(&conn)) {
            (Ok(summary), Ok(settings)) => next_due(&summary, interval_secs(&settings), db::now()) <= db::now(),
            (Err(e), _) | (_, Err(e)) => {
                log::warn!("sync scheduler could not read state: {e}");
                return;
            }
        }
    };
    if due {
        log::info!("scheduled camera sync starting");
        let _ = run_sync(app, false).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(ok: Option<i64>, attempt: Option<i64>, outcome: Option<&str>) -> SyncSummary {
        SyncSummary {
            last_ok_at: ok,
            last_attempt_at: attempt,
            last_outcome: outcome.map(String::from),
            ..Default::default()
        }
    }

    const WEEK: i64 = 7 * 86_400;

    #[test]
    fn never_synced_is_due_now() {
        assert_eq!(next_due(&summary(None, None, None), WEEK, 1_000), 1_000);
    }

    #[test]
    fn success_is_due_again_after_the_interval() {
        let due = next_due(&summary(Some(1_000), Some(1_000), Some("ok")), WEEK, 2_000);
        assert_eq!(due, 1_000 + WEEK);
    }

    #[test]
    fn failures_back_off_instead_of_retrying_every_tick() {
        // Never succeeded, failed at t=5000: retry an hour later, not immediately.
        assert_eq!(next_due(&summary(None, Some(5_000), Some("offline")), WEEK, 5_060), 5_000 + RETRY_AFTER_SECS);
        // Succeeded long ago, the latest attempt failed: still no sooner than the backoff.
        let old_ok = summary(Some(0), Some(10 * 86_400), Some("error"));
        assert_eq!(next_due(&old_ok, WEEK, 10 * 86_400 + 5), 10 * 86_400 + RETRY_AFTER_SECS);
        // A recent success followed by a failed manual retry keeps the regular schedule.
        let recent_ok = summary(Some(100), Some(200), Some("error"));
        assert_eq!(next_due(&recent_ok, WEEK, 300), 100 + WEEK);
    }

    #[test]
    fn snapshot_is_checked_daily_overpass_at_the_user_interval() {
        let overpass = Settings { sync_source: "overpass".into(), ..Settings::default() };
        assert_eq!(interval_secs(&overpass), WEEK);
        let custom = Settings { snapshot_url: "https://example.org/manifest.json".into(), ..Settings::default() };
        assert_eq!(interval_secs(&custom), snapshot::CHECK_INTERVAL_SECS);
    }
}
