//! Tauri commands: the full frontend ↔ Rust contract.
//!
//! Argument names are camelCase on the JavaScript side (Tauri's default); struct
//! fields keep their Rust snake_case names in JSON.

use crate::alerts::{self, AlertEvent, RefreshOutcome, Route, RouteReport, WatchArea};
use crate::db::{self, Camera, Settings};
use crate::error::{AppError, AppResult};
use crate::fetcher::{fetch_cells, FetchStats, Grouping};
use crate::grid::{cells_for_bbox, BBox};
use crate::nominatim::{self, GeocodeResult};
use crate::osm::{self, PendingAuth, UploadResult};
use crate::overpass;
use crate::state::AppState;
use crate::submissions::{self, Proximity, Submission, SubmissionInput};
use crate::wifi::{self, WifiSighting};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{Read, Write};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_fs::{FilePath, FsExt, OpenOptions};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

pub const DISCLAIMER: &str = alerts::COVERAGE_DISCLAIMER;

// ---------------------------------------------------------------------------
// App info, settings, view state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct AppInfo {
    pub version: String,
    pub repo_url: String,
    pub user_agent: String,
    pub db_path: String,
    pub first_run: bool,
    pub fixture_mode: bool,
    pub disclaimer: String,
    pub osm_redirect_uri: String,
    /// Tags surfaced in the detail panel when present, in display order.
    pub display_tags: Vec<String>,
}

#[tauri::command]
pub async fn get_app_info(app: AppHandle, state: State<'_, AppState>) -> AppResult<AppInfo> {
    let settings = {
        let conn = state.conn();
        db::load_settings(&conn)?
    };
    let db_path = app
        .path()
        .app_data_dir()
        .map(|p| p.join("flockfinder.sqlite").display().to_string())
        .unwrap_or_default();
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        repo_url: env!("CARGO_PKG_REPOSITORY").into(),
        user_agent: crate::http::user_agent(),
        db_path,
        first_run: !settings.first_run_done,
        fixture_mode: std::env::var("FLOCKFINDER_OFFLINE_FIXTURE").map_or(false, |v| v == "1"),
        disclaimer: DISCLAIMER.into(),
        osm_redirect_uri: osm::REDIRECT_URI.into(),
        display_tags: overpass::DISPLAY_TAGS.iter().map(|s| s.to_string()).collect(),
    })
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> AppResult<Settings> {
    let conn = state.conn();
    db::load_settings(&conn)
}

#[tauri::command]
pub async fn save_settings(state: State<'_, AppState>, settings: Settings) -> AppResult<Settings> {
    let conn = state.conn();
    db::save_settings(&conn, &settings)
}

#[tauri::command]
pub async fn mark_first_run_done(state: State<'_, AppState>) -> AppResult<()> {
    let conn = state.conn();
    let mut s = db::load_settings(&conn)?;
    s.first_run_done = true;
    db::save_settings(&conn, &s)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ViewState {
    pub lat: f64,
    pub lon: f64,
    pub zoom: f64,
}

/// Continental US fallback.
const CONUS_VIEW: ViewState = ViewState {
    lat: 39.5,
    lon: -98.35,
    zoom: 4.0,
};

#[tauri::command]
pub async fn get_initial_view(state: State<'_, AppState>) -> AppResult<ViewState> {
    let (saved, geo_done) = {
        let conn = state.conn();
        (
            db::get_json::<ViewState>(&conn, "last_view")?,
            db::get_json::<bool>(&conn, "ip_geolocation_done")?.unwrap_or(false),
        )
    };
    if let Some(v) = saved {
        return Ok(v);
    }
    if geo_done {
        return Ok(CONUS_VIEW);
    }
    // One-time approximate IP geolocation; recorded as done whether or not it works.
    let view = match ip_geolocate(&state).await {
        Ok(Some(v)) => v,
        Ok(None) => CONUS_VIEW,
        Err(e) => {
            log::info!("IP geolocation unavailable ({e}); using continental US view");
            CONUS_VIEW
        }
    };
    let conn = state.conn();
    db::set_json(&conn, "ip_geolocation_done", &true)?;
    Ok(view)
}

async fn ip_geolocate(state: &AppState) -> AppResult<Option<ViewState>> {
    let resp = state
        .http
        .client
        .get("https://ipapi.co/json/")
        .timeout(std::time::Duration::from_secs(6))
        .send()
        .await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let v: serde_json::Value = resp.json().await?;
    let lat = v.get("latitude").and_then(|x| x.as_f64());
    let lon = v.get("longitude").and_then(|x| x.as_f64());
    Ok(match (lat, lon) {
        (Some(lat), Some(lon)) if crate::geo_util::valid_coord(lat, lon) => Some(ViewState { lat, lon, zoom: 10.0 }),
        _ => None,
    })
}

#[tauri::command]
pub async fn save_view(state: State<'_, AppState>, view: ViewState) -> AppResult<()> {
    let conn = state.conn();
    db::set_json(&conn, "last_view", &view)
}

// ---------------------------------------------------------------------------
// Map data
// ---------------------------------------------------------------------------

/// Every camera in the local store as a compact binary snapshot (see `points.rs`). The map
/// aggregates and draws this at every zoom level; it arrives in JavaScript as an ArrayBuffer.
#[tauri::command]
pub async fn get_camera_points(app: AppHandle, state: State<'_, AppState>) -> AppResult<tauri::ipc::Response> {
    let started = std::time::Instant::now();
    let path = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::Other(format!("no app data dir: {e}")))?
        .join("camera-points.bin");
    let (bytes, cached) = {
        let conn = state.conn();
        crate::points::cached_or_encode(&conn, &path)?
    };
    log::info!(
        "camera snapshot: {} bytes in {:?} ({})",
        bytes.len(),
        started.elapsed(),
        if cached { "cached" } else { "encoded" }
    );
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
pub async fn get_sync_status(app: AppHandle) -> AppResult<crate::sync::SyncStatus> {
    crate::sync::status(&app)
}

/// Run the worldwide camera sync now ("Sync now"). Progress arrives as `sync:status` events.
#[tauri::command]
pub async fn sync_now(app: AppHandle) -> AppResult<crate::sync::SyncOutcome> {
    crate::sync::run_sync(&app, true).await
}

#[tauri::command]
pub async fn get_cached_cameras(state: State<'_, AppState>, bbox: BBox) -> AppResult<Vec<Camera>> {
    let conn = state.conn();
    db::cameras_in_bbox(&conn, &bbox)
}

/// Cameras within the square of half-width `radius_m` around a point, from the local store.
/// Used by live proximity alerts; never touches the network, so following your position
/// tells no server where you are.
#[tauri::command]
pub async fn cameras_near(state: State<'_, AppState>, lat: f64, lon: f64, radius_m: f64) -> AppResult<Vec<Camera>> {
    if !crate::geo_util::valid_coord(lat, lon) {
        return Err(AppError::Invalid("invalid position".into()));
    }
    let r = radius_m.clamp(100.0, 5000.0);
    let d_lat = r / 111_320.0;
    let d_lon = d_lat / lat.to_radians().cos().max(0.01);
    let bbox = BBox::new(lat - d_lat, lon - d_lon, lat + d_lat, lon + d_lon).sanitized();
    let conn = state.conn();
    db::cameras_in_bbox(&conn, &bbox)
}

#[derive(Debug, Clone, Serialize)]
pub struct CacheStats {
    pub cameras: i64,
    pub stale_cameras: i64,
    pub cells: i64,
    pub oldest_fetch: Option<i64>,
    pub newest_fetch: Option<i64>,
}

#[tauri::command]
pub async fn get_cache_stats(state: State<'_, AppState>) -> AppResult<CacheStats> {
    let conn = state.conn();
    let cameras = db::camera_count(&conn)?;
    let stale_cameras: i64 = conn.query_row(
        "SELECT COUNT(*) FROM cameras WHERE stale_since IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    let (cells, oldest, newest): (i64, Option<i64>, Option<i64>) = conn.query_row(
        "SELECT COUNT(*), MIN(fetched_at), MAX(fetched_at) FROM grid_cells",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(CacheStats {
        cameras,
        stale_cameras,
        cells,
        oldest_fetch: oldest,
        newest_fetch: newest,
    })
}

#[tauri::command]
pub async fn clear_cache(state: State<'_, AppState>) -> AppResult<()> {
    let conn = state.conn();
    db::clear_cell_cache(&conn)
}

#[derive(Debug, Clone, Serialize)]
pub struct FixtureLoad {
    pub stats: FetchStats,
    pub bbox: BBox,
}

/// Ingest the bundled Overpass sample so the UI can be exercised with no network.
#[tauri::command]
pub async fn load_fixture(app: AppHandle, state: State<'_, AppState>) -> AppResult<FixtureLoad> {
    let parsed = overpass::parse_response(overpass::SAMPLE_FIXTURE)?;
    let bbox = BBox::new(39.60, -105.15, 39.85, -104.85);
    let cells = cells_for_bbox(&bbox);
    let area = crate::grid::union_bbox(&cells).unwrap_or(bbox);
    let ingest = {
        let mut conn = state.conn();
        db::ingest_fetch(&mut conn, &area, &cells, &parsed.elements, db::now())?
    };
    let _ = app.emit(crate::sync::EVENT_CAMERAS_CHANGED, ());
    Ok(FixtureLoad {
        stats: FetchStats {
            requests: 0,
            cells_fetched: ingest.cells_marked,
            elements: parsed.elements.len(),
            skipped: parsed.skipped,
        },
        bbox,
    })
}

// ---------------------------------------------------------------------------
// Geocoding
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn geocode(state: State<'_, AppState>, query: String) -> AppResult<Vec<GeocodeResult>> {
    let key = query.trim().to_lowercase();
    if key.is_empty() {
        return Ok(Vec::new());
    }
    {
        let conn = state.conn();
        let cached: Option<String> = conn
            .query_row(
                "SELECT result_json FROM geocode_cache WHERE query = ?1",
                rusqlite::params![key],
                |r| r.get(0),
            )
            .ok();
        if let Some(json) = cached {
            if let Ok(results) = serde_json::from_str::<Vec<GeocodeResult>>(&json) {
                return Ok(results);
            }
        }
    }
    let results = nominatim::search(&state.http, query.trim()).await?;
    let conn = state.conn();
    conn.execute(
        "INSERT INTO geocode_cache(query, result_json, fetched_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(query) DO UPDATE SET result_json = excluded.result_json, fetched_at = excluded.fetched_at",
        rusqlite::params![key, serde_json::to_string(&results)?, db::now()],
    )?;
    Ok(results)
}

#[derive(Debug, Clone, Serialize)]
pub struct LandCheck {
    /// False when the check could not be performed (offline); callers should not block.
    pub checked: bool,
    pub on_land: bool,
    pub place: Option<String>,
}

/// Reverse-geocode a point to catch pins dropped in open water. Offline → `checked: false`.
#[tauri::command]
pub async fn check_location(state: State<'_, AppState>, lat: f64, lon: f64) -> AppResult<LandCheck> {
    if !crate::geo_util::valid_coord(lat, lon) {
        return Err(AppError::Invalid("coordinates are outside the valid range".into()));
    }
    match nominatim::reverse(&state.http, lat, lon).await {
        Ok(Some(place)) => Ok(LandCheck {
            checked: true,
            on_land: true,
            place: Some(place),
        }),
        Ok(None) => Ok(LandCheck {
            checked: true,
            on_land: false,
            place: None,
        }),
        Err(e) if e.is_offline() || matches!(e, AppError::RateLimited(_)) => Ok(LandCheck {
            checked: false,
            on_land: true,
            place: None,
        }),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// Submissions
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_submissions(state: State<'_, AppState>) -> AppResult<Vec<Submission>> {
    let conn = state.conn();
    submissions::list(&conn)
}

#[tauri::command]
pub async fn create_submission(state: State<'_, AppState>, input: SubmissionInput) -> AppResult<Submission> {
    let conn = state.conn();
    submissions::create(&conn, &input)
}

#[tauri::command]
pub async fn update_submission(state: State<'_, AppState>, id: i64, input: SubmissionInput) -> AppResult<Submission> {
    let conn = state.conn();
    submissions::update(&conn, id, &input)
}

#[tauri::command]
pub async fn delete_submission(state: State<'_, AppState>, id: i64) -> AppResult<bool> {
    let conn = state.conn();
    submissions::delete(&conn, id)
}

#[tauri::command]
pub async fn check_submission_proximity(
    state: State<'_, AppState>,
    lat: f64,
    lon: f64,
    exclude_id: Option<i64>,
) -> AppResult<Proximity> {
    let conn = state.conn();
    submissions::proximity(&conn, lat, lon, exclude_id)
}

async fn pick_save_path(app: &AppHandle, title: &str, file_name: &str, filter: (&str, &[&str])) -> AppResult<Option<FilePath>> {
    let builder = app
        .dialog()
        .file()
        .set_title(title)
        .set_file_name(file_name)
        .add_filter(filter.0, filter.1);
    tokio::task::spawn_blocking(move || builder.blocking_save_file())
        .await
        .map_err(|e| AppError::Other(format!("dialog task failed: {e}")))
}

// Picked files go through the fs plugin: on Android the pickers return content:// URIs,
// which std::fs cannot open. On desktop these are plain paths.

fn read_picked(app: &AppHandle, fp: &FilePath) -> AppResult<Vec<u8>> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    let mut bytes = Vec::new();
    app.fs().open(fp.clone(), opts)?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn write_picked(app: &AppHandle, fp: &FilePath, content: &[u8]) -> AppResult<()> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    app.fs().open(fp.clone(), opts)?.write_all(content)?;
    Ok(())
}

/// File name without extension, for display. A content:// URI may only carry an opaque
/// document id (`msf:1000`), in which case that id is what you get.
fn picked_stem(fp: &FilePath) -> Option<String> {
    let name = match fp {
        FilePath::Path(p) => p.file_name()?.to_string_lossy().into_owned(),
        FilePath::Url(u) => {
            let segment = percent_decode(u.path_segments()?.next_back()?);
            segment.rsplit(['/', ':']).next()?.to_string()
        }
    };
    let stem = std::path::Path::new(&name).file_stem()?.to_string_lossy().into_owned();
    (!stem.is_empty()).then_some(stem)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Android 13+ only shows notifications once POST_NOTIFICATIONS is granted, so ask when an
/// alert target is saved: that is when notifications start to matter. Desktop reports
/// `Granted`, making this a no-op there.
fn ensure_notification_permission(app: &AppHandle) {
    use tauri::plugin::PermissionState;
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || match app.notification().permission_state() {
        Ok(PermissionState::Prompt | PermissionState::PromptWithRationale) => {
            if let Err(e) = app.notification().request_permission() {
                log::warn!("notification permission request failed: {e}");
            }
        }
        Ok(_) => {}
        Err(e) => log::warn!("could not read notification permission: {e}"),
    });
}

/// Export submissions as JOSM-compatible `.osm` XML. Returns the written path, or
/// `None` if the user cancelled the save dialog.
#[tauri::command]
pub async fn export_josm(app: AppHandle, state: State<'_, AppState>, ids: Vec<i64>) -> AppResult<Option<String>> {
    let subs: Vec<Submission> = {
        let conn = state.conn();
        let all = submissions::list(&conn)?;
        if ids.is_empty() {
            all.into_iter().filter(|s| s.status == "local").collect()
        } else {
            all.into_iter().filter(|s| ids.contains(&s.id)).collect()
        }
    };
    if subs.is_empty() {
        return Err(AppError::Invalid("no submissions selected for export".into()));
    }
    let Some(fp) = pick_save_path(&app, "Export for JOSM", "flockfinder-submissions.osm", ("OSM XML", &["osm"])).await? else {
        return Ok(None);
    };
    write_picked(&app, &fp, submissions::josm_xml(&subs).as_bytes())?;
    Ok(Some(fp.to_string()))
}

// ---------------------------------------------------------------------------
// Alerts: watch areas and routes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct TargetCreated<T> {
    pub target: T,
    pub count: usize,
    /// True when the baseline could not be recorded yet (offline); the next
    /// successful refresh records it silently.
    pub baseline_pending: bool,
    pub offline: bool,
}

/// Outcome of recording a new target's baseline: camera count, whether the baseline is
/// still pending, whether we were offline, and whether the fetch changed the cameras table.
type Baseline = (usize, bool, bool, bool);

async fn baseline_area(state: &AppState, area: &WatchArea) -> AppResult<Baseline> {
    let cells = crate::grid::cells_for_circle(area.lat, area.lon, area.radius_m as f64);
    let fetched = fetch_cells(state, &cells, false, Grouping::SuperCells).await;
    let mut conn = state.conn();
    let current: HashSet<alerts::Key> = alerts::cameras_in_area(&conn, area)?
        .into_iter()
        .map(|a| (a.camera.osm_type, a.camera.osm_id))
        .collect();
    match fetched {
        Ok(stats) => {
            alerts::record_check(&mut conn, "area", area.id, &current, db::now())?;
            Ok((current.len(), false, false, stats.requests > 0))
        }
        Err(e) if e.is_offline() => Ok((current.len(), true, true, false)),
        Err(e) => Err(e),
    }
}

#[tauri::command]
pub async fn create_watch_area(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    lat: f64,
    lon: f64,
    radius_m: i64,
) -> AppResult<TargetCreated<WatchArea>> {
    ensure_notification_permission(&app);
    let area = {
        let conn = state.conn();
        alerts::create_area(&conn, &name, lat, lon, radius_m)?
    };
    let (count, pending, offline, changed) = baseline_area(&state, &area).await?;
    if changed {
        let _ = app.emit(crate::sync::EVENT_CAMERAS_CHANGED, ());
    }
    let target = {
        let conn = state.conn();
        alerts::get_area(&conn, area.id)?.unwrap_or(area)
    };
    Ok(TargetCreated {
        target,
        count,
        baseline_pending: pending,
        offline,
    })
}

#[tauri::command]
pub async fn list_watch_areas(state: State<'_, AppState>) -> AppResult<Vec<WatchArea>> {
    let conn = state.conn();
    alerts::list_areas(&conn)
}

#[tauri::command]
pub async fn rename_watch_area(state: State<'_, AppState>, id: i64, name: String) -> AppResult<()> {
    let conn = state.conn();
    alerts::rename_area(&conn, id, &name)
}

#[tauri::command]
pub async fn delete_watch_area(state: State<'_, AppState>, id: i64) -> AppResult<()> {
    let conn = state.conn();
    alerts::delete_area(&conn, id)
}

async fn baseline_route(state: &AppState, route: &Route) -> AppResult<Baseline> {
    let points = alerts::parse_linestring(&route.geojson)?;
    let cells = crate::grid::cells_for_polyline(&points, route.corridor_m as f64);
    let fetched = fetch_cells(state, &cells, false, Grouping::SuperCells).await;
    let mut conn = state.conn();
    let current: HashSet<alerts::Key> = alerts::cameras_along_polyline(&conn, &points, route.corridor_m as f64)?
        .into_iter()
        .map(|r| (r.camera.osm_type, r.camera.osm_id))
        .collect();
    match fetched {
        Ok(stats) => {
            alerts::record_check(&mut conn, "route", route.id, &current, db::now())?;
            Ok((current.len(), false, false, stats.requests > 0))
        }
        Err(e) if e.is_offline() => Ok((current.len(), true, true, false)),
        Err(e) => Err(e),
    }
}

/// `points` are `[lat, lon]` pairs.
#[tauri::command]
pub async fn create_route(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    points: Vec<[f64; 2]>,
    corridor_m: i64,
) -> AppResult<TargetCreated<Route>> {
    ensure_notification_permission(&app);
    let pts: Vec<(f64, f64)> = points.iter().map(|p| (p[0], p[1])).collect();
    let route = {
        let conn = state.conn();
        alerts::create_route(&conn, &name, &pts, corridor_m)?
    };
    let (count, pending, offline, changed) = baseline_route(&state, &route).await?;
    if changed {
        let _ = app.emit(crate::sync::EVENT_CAMERAS_CHANGED, ());
    }
    let target = {
        let conn = state.conn();
        alerts::get_route(&conn, route.id)?.unwrap_or(route)
    };
    Ok(TargetCreated {
        target,
        count,
        baseline_pending: pending,
        offline,
    })
}

#[tauri::command]
pub async fn list_routes(state: State<'_, AppState>) -> AppResult<Vec<Route>> {
    let conn = state.conn();
    alerts::list_routes(&conn)
}

#[tauri::command]
pub async fn delete_route(state: State<'_, AppState>, id: i64) -> AppResult<()> {
    let conn = state.conn();
    alerts::delete_route(&conn, id)
}

#[derive(Debug, Clone, Serialize)]
pub struct GpxImport {
    pub name: String,
    pub points: Vec<[f64; 2]>,
    pub length_m: f64,
    pub path: String,
}

/// Open a GPX file via the native dialog and parse it. `None` if cancelled.
#[tauri::command]
pub async fn import_gpx(app: AppHandle) -> AppResult<Option<GpxImport>> {
    let builder = app
        .dialog()
        .file()
        .set_title("Import GPX track")
        .add_filter("GPX", &["gpx", "xml"]);
    let picked = tokio::task::spawn_blocking(move || builder.blocking_pick_file())
        .await
        .map_err(|e| AppError::Other(format!("dialog task failed: {e}")))?;
    let Some(fp) = picked else { return Ok(None) };
    let xml = String::from_utf8_lossy(&read_picked(&app, &fp)?).into_owned();
    let points = crate::gpx::parse_gpx(&xml)?;
    let name = picked_stem(&fp).unwrap_or_else(|| "Imported route".into());
    Ok(Some(GpxImport {
        name,
        length_m: crate::geo_util::polyline_length_m(&points),
        points: points.iter().map(|(lat, lon)| [*lat, *lon]).collect(),
        path: fp.to_string(),
    }))
}

#[derive(Debug, Clone, Serialize)]
pub struct AreaSummary {
    pub area: WatchArea,
    pub count: usize,
    pub camera_keys: Vec<String>,
    pub baseline_recorded: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteSummary {
    pub route: Route,
    pub count: usize,
    pub camera_keys: Vec<String>,
    pub baseline_recorded: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AlertState {
    pub areas: Vec<AreaSummary>,
    pub routes: Vec<RouteSummary>,
    pub last_refresh: Option<i64>,
    pub last_skipped_offline: Option<i64>,
    pub unseen_added: i64,
    pub refresh_running: bool,
    pub disclaimer: String,
}

#[tauri::command]
pub async fn get_alert_state(state: State<'_, AppState>) -> AppResult<AlertState> {
    let conn = state.conn();
    let mut areas = Vec::new();
    for area in alerts::list_areas(&conn)? {
        let cams = alerts::cameras_in_area(&conn, &area)?;
        let baseline_recorded = alerts::known_set(&conn, "area", area.id)?.is_some();
        areas.push(AreaSummary {
            count: cams.len(),
            camera_keys: cams.iter().map(|c| c.camera.key()).collect(),
            area,
            baseline_recorded,
        });
    }
    let mut routes = Vec::new();
    for route in alerts::list_routes(&conn)? {
        let cams = alerts::cameras_along_route(&conn, &route)?;
        let baseline_recorded = alerts::known_set(&conn, "route", route.id)?.is_some();
        routes.push(RouteSummary {
            count: cams.len(),
            camera_keys: cams.iter().map(|c| c.camera.key()).collect(),
            route,
            baseline_recorded,
        });
    }
    Ok(AlertState {
        areas,
        routes,
        last_refresh: db::get_json(&conn, "last_alert_refresh")?,
        last_skipped_offline: db::get_json(&conn, "last_refresh_skipped_at")?,
        unseen_added: alerts::unseen_added_count(&conn)?,
        refresh_running: state.refresh_running.load(std::sync::atomic::Ordering::SeqCst),
        disclaimer: DISCLAIMER.into(),
    })
}

#[tauri::command]
pub async fn get_alert_history(state: State<'_, AppState>, target_type: String, target_id: i64) -> AppResult<Vec<AlertEvent>> {
    let conn = state.conn();
    alerts::history(&conn, &target_type, target_id)
}

#[tauri::command]
pub async fn get_target_cameras(state: State<'_, AppState>, target_type: String, target_id: i64) -> AppResult<Vec<Camera>> {
    let conn = state.conn();
    match target_type.as_str() {
        "area" => {
            let area = alerts::get_area(&conn, target_id)?.ok_or_else(|| AppError::Invalid("watch area not found".into()))?;
            Ok(alerts::cameras_in_area(&conn, &area)?.into_iter().map(|a| a.camera).collect())
        }
        "route" => {
            let route = alerts::get_route(&conn, target_id)?.ok_or_else(|| AppError::Invalid("route not found".into()))?;
            Ok(alerts::cameras_along_route(&conn, &route)?.into_iter().map(|r| r.camera).collect())
        }
        _ => Err(AppError::Invalid("target_type must be area or route".into())),
    }
}

#[tauri::command]
pub async fn get_cameras_by_keys(state: State<'_, AppState>, keys: Vec<String>) -> AppResult<Vec<Camera>> {
    let conn = state.conn();
    let mut out = Vec::new();
    for key in keys {
        if let Some((t, id)) = key.split_once('/') {
            if let Ok(id) = id.parse::<i64>() {
                if let Some(c) = db::camera_get(&conn, t, id)? {
                    out.push(c);
                }
            }
        }
    }
    Ok(out)
}

#[tauri::command]
pub async fn acknowledge_alerts(state: State<'_, AppState>) -> AppResult<()> {
    let conn = state.conn();
    db::set_json(&conn, "alerts_seen_at", &db::now())
}

#[tauri::command]
pub async fn run_alert_refresh(app: AppHandle) -> AppResult<RefreshOutcome> {
    alerts::run_refresh(&app, true).await
}

#[tauri::command]
pub async fn get_route_report(state: State<'_, AppState>, route_id: i64) -> AppResult<RouteReport> {
    let conn = state.conn();
    alerts::route_report(&conn, route_id)
}

/// `format` is `csv` or `geojson`. Returns the written path or `None` if cancelled.
#[tauri::command]
pub async fn export_route_report(
    app: AppHandle,
    state: State<'_, AppState>,
    route_id: i64,
    format: String,
) -> AppResult<Option<String>> {
    let report = {
        let conn = state.conn();
        alerts::route_report(&conn, route_id)?
    };
    let safe_name: String = report
        .route
        .name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let (content, file_name, filter): (String, String, (&str, &[&str])) = match format.as_str() {
        "csv" => (alerts::report_csv(&report), format!("{safe_name}-cameras.csv"), ("CSV", &["csv"])),
        "geojson" => (
            alerts::report_geojson(&report),
            format!("{safe_name}-cameras.geojson"),
            ("GeoJSON", &["geojson", "json"]),
        ),
        _ => return Err(AppError::Invalid("format must be csv or geojson".into())),
    };
    let Some(fp) = pick_save_path(&app, "Export route report", &file_name, filter).await? else {
        return Ok(None);
    };
    write_picked(&app, &fp, content.as_bytes())?;
    Ok(Some(fp.to_string()))
}

// ---------------------------------------------------------------------------
// OSM sign-in and upload
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct OsmAuthStatus {
    pub configured: bool,
    pub signed_in: bool,
    pub client_id: String,
    pub pending: bool,
    pub keychain_error: Option<String>,
}

#[tauri::command]
pub async fn osm_auth_status(state: State<'_, AppState>) -> AppResult<OsmAuthStatus> {
    let settings = {
        let conn = state.conn();
        db::load_settings(&conn)?
    };
    let (signed_in, keychain_error) = match osm::load_token() {
        Ok(t) => (t.is_some(), None),
        Err(e) => (false, Some(e.to_string())),
    };
    let pending = state
        .oauth_pending
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map_or(false, |p| db::now() - p.started_at < osm::PENDING_TTL_SECS);
    Ok(OsmAuthStatus {
        configured: !settings.osm_client_id.is_empty(),
        signed_in,
        client_id: settings.osm_client_id,
        pending,
        keychain_error,
    })
}

/// Start the PKCE flow: opens the OSM authorization page in the system browser.
#[tauri::command]
pub async fn osm_sign_in(app: AppHandle, state: State<'_, AppState>) -> AppResult<String> {
    let settings = {
        let conn = state.conn();
        db::load_settings(&conn)?
    };
    if settings.osm_client_id.is_empty() {
        return Err(AppError::NotConfigured(
            "enter your OSM OAuth client ID in Settings first, or use the JOSM export instead".into(),
        ));
    }
    let (verifier, challenge) = osm::pkce_pair()?;
    let auth_state = osm::new_state()?;
    let url = osm::authorize_url(&settings.osm_client_id, &auth_state, &challenge);
    {
        let mut pending = state.oauth_pending.lock().unwrap_or_else(|p| p.into_inner());
        *pending = Some(PendingAuth {
            state: auth_state,
            verifier,
            client_id: settings.osm_client_id.clone(),
            started_at: db::now(),
        });
    }
    app.opener()
        .open_url(url.clone(), None::<&str>)
        .map_err(|e| AppError::Other(format!("could not open browser: {e}")))?;
    Ok(url)
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthOutcome {
    pub ok: bool,
    pub message: String,
}

/// Finish the PKCE flow from the deep-link callback (also callable with a pasted URL).
#[tauri::command]
pub async fn osm_complete_auth(app: AppHandle, url: String) -> AppResult<AuthOutcome> {
    let parsed = url::Url::parse(url.trim()).map_err(|e| AppError::Invalid(format!("not a valid callback URL: {e}")))?;
    let outcome = complete_auth_inner(&app, &parsed).await;
    let result = match outcome {
        Ok(()) => AuthOutcome {
            ok: true,
            message: "Signed in to OpenStreetMap.".into(),
        },
        Err(e) => AuthOutcome {
            ok: false,
            message: e.to_string(),
        },
    };
    let _ = app.emit("osm:auth", &result);
    Ok(result)
}

async fn complete_auth_inner(app: &AppHandle, url: &url::Url) -> AppResult<()> {
    let state = app.state::<AppState>();
    let (code, returned_state) = osm::parse_callback(url)?;
    let pending = {
        let mut guard = state.oauth_pending.lock().unwrap_or_else(|p| p.into_inner());
        guard.take()
    };
    let Some(pending) = pending else {
        return Err(AppError::Invalid("no sign-in is waiting for a callback; start again from Settings".into()));
    };
    if db::now() - pending.started_at > osm::PENDING_TTL_SECS {
        return Err(AppError::Invalid("the sign-in attempt expired; start again from Settings".into()));
    }
    if pending.state != returned_state {
        return Err(AppError::Invalid("callback state mismatch; sign-in rejected".into()));
    }
    let token = osm::exchange_code(&state.http, &pending.client_id, &code, &pending.verifier).await?;
    osm::store_token(&token)?;
    Ok(())
}

/// A one-off system notification for a live camera proximity alert (the frontend only asks
/// when the window is not in front). Respects the notifications setting. Returns whether a
/// notification was shown; failures are logged, not raised.
#[tauri::command]
pub fn notify(app: AppHandle, state: State<'_, AppState>, title: String, body: String) -> AppResult<bool> {
    let enabled = {
        let conn = state.conn();
        db::load_settings(&conn)?.notifications_enabled
    };
    if !enabled {
        return Ok(false);
    }
    match app.notification().builder().title(&title).body(&body).show() {
        Ok(()) => Ok(true),
        Err(e) => {
            log::warn!("proximity notification failed (in-app banner still shows): {e}");
            Ok(false)
        }
    }
}

/// Called from the deep-link plugin when the OS hands us a `flockfinder://` URL.
pub async fn handle_deep_link(app: AppHandle, url: url::Url) {
    if url.scheme() == "flockfinder" && url.host_str() == Some("oauth") {
        // Android brings the activity to the front itself when it delivers the link.
        #[cfg(desktop)]
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
        if let Err(e) = osm_complete_auth(app, url.to_string()).await {
            log::warn!("deep link auth failed: {e}");
        }
    } else {
        log::info!("ignoring unrecognised deep link {url}");
    }
}

#[tauri::command]
pub async fn osm_sign_out(state: State<'_, AppState>) -> AppResult<()> {
    osm::clear_token()?;
    let mut pending = state.oauth_pending.lock().unwrap_or_else(|p| p.into_inner());
    *pending = None;
    Ok(())
}

/// The tags that would be written for a submission — shown verbatim in the preflight dialog.
#[tauri::command]
pub async fn preview_submission_tags(state: State<'_, AppState>, id: i64) -> AppResult<Vec<[String; 2]>> {
    let conn = state.conn();
    let sub = submissions::get(&conn, id)?.ok_or_else(|| AppError::Invalid("submission not found".into()))?;
    Ok(submissions::osm_tags(&sub).into_iter().map(|(k, v)| [k, v]).collect())
}

/// Upload one submission as one changeset. `confirmed` must be true — the backend
/// refuses uploads that did not pass through the preflight dialog.
#[tauri::command]
pub async fn osm_upload_submission(
    state: State<'_, AppState>,
    id: i64,
    comment: String,
    confirmed: bool,
) -> AppResult<UploadResult> {
    if !confirmed {
        return Err(AppError::Invalid(
            "upload requires confirming you personally observed the camera at this location".into(),
        ));
    }
    let sub = {
        let conn = state.conn();
        submissions::get(&conn, id)?.ok_or_else(|| AppError::Invalid("submission not found".into()))?
    };
    if sub.status != "local" {
        return Err(AppError::Invalid("this submission has already been uploaded".into()));
    }
    let token = osm::load_token()?.ok_or(AppError::AuthRequired)?;
    let result = osm::upload_submission(&state.http, &token, &sub, &comment).await?;
    let conn = state.conn();
    submissions::mark_uploaded(&conn, id, result.node_id)?;
    Ok(result)
}

// ---------------------------------------------------------------------------
// Wi-Fi fingerprint sightings (suspected devices; separate from OSM cameras)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WifiDatasetMeta {
    pub downloaded_at: Option<i64>,
    pub upstream_generated: Option<String>,
    pub upstream_total: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WifiDatasetStatus {
    pub total: i64,
    pub upstream: i64,
    pub imported: i64,
    pub downloaded_at: Option<i64>,
    pub upstream_generated: Option<String>,
    pub upstream_total: Option<i64>,
    pub dataset_url: String,
    pub repo_url: String,
    pub policy_url: String,
    pub oui_count: usize,
    pub retention_days: i64,
}

#[tauri::command]
pub async fn wifi_dataset_status(state: State<'_, AppState>) -> AppResult<WifiDatasetStatus> {
    let conn = state.conn();
    let counts = wifi::counts(&conn)?;
    let meta: WifiDatasetMeta = db::get_json(&conn, "wifi_dataset")?.unwrap_or_default();
    Ok(WifiDatasetStatus {
        total: counts.total,
        upstream: counts.upstream,
        imported: counts.imported,
        downloaded_at: meta.downloaded_at,
        upstream_generated: meta.upstream_generated,
        upstream_total: meta.upstream_total,
        dataset_url: wifi::DATASET_CSV_URL.into(),
        repo_url: wifi::UPSTREAM_REPO.into(),
        policy_url: wifi::DATA_POLICY_URL.into(),
        oui_count: wifi::oui_list().len(),
        retention_days: wifi::RETENTION_DAYS,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct WifiIngestResult {
    pub inserted: usize,
    pub stats: wifi::ParseStats,
    pub upstream_generated: Option<String>,
}

/// Download the published Flock Finder dataset (≈24 MB CSV) and replace the local copy.
#[tauri::command]
pub async fn wifi_download_dataset(state: State<'_, AppState>) -> AppResult<WifiIngestResult> {
    // Small stats file first: gives the upstream generation timestamp and total.
    let stats_resp = state
        .http
        .send_with_backoff("Flock Finder dataset (stats)", || state.http.client.get(wifi::DATASET_STATS_URL))
        .await?;
    let stats_json: serde_json::Value = stats_resp.json().await.unwrap_or(serde_json::Value::Null);
    let upstream_generated = stats_json
        .get("scan_timestamp")
        .and_then(|v| v.as_str())
        .map(String::from);
    let upstream_total = stats_json.get("total_cameras").and_then(|v| v.as_i64());

    let resp = state
        .http
        .send_with_backoff("Flock Finder dataset", || {
            state
                .http
                .client
                .get(wifi::DATASET_CSV_URL)
                .timeout(std::time::Duration::from_secs(600))
        })
        .await?;
    let bytes = resp.bytes().await?;
    let now = db::now();
    let (rows, stats) = tokio::task::spawn_blocking(move || wifi::parse_upstream_csv(bytes.as_ref(), now))
        .await
        .map_err(|e| AppError::Other(format!("parse task failed: {e}")))??;

    let mut conn = state.conn();
    wifi::clear(&conn, Some(wifi::SOURCE_UPSTREAM))?;
    let inserted = wifi::upsert(&mut conn, &rows)?;
    wifi::prune_old(&conn, now)?;
    db::set_json(
        &conn,
        "wifi_dataset",
        &WifiDatasetMeta {
            downloaded_at: Some(now),
            upstream_generated: upstream_generated.clone(),
            upstream_total,
        },
    )?;
    log::info!(
        "wifi dataset: {inserted} sightings stored ({} parsed, {} stale, {} invalid, {} duplicates)",
        stats.parsed, stats.skipped_old, stats.skipped_invalid, stats.deduplicated
    );
    Ok(WifiIngestResult {
        inserted,
        stats,
        upstream_generated,
    })
}

/// Import a Wigle-format wardriving CSV chosen via the native file dialog. `None` if cancelled.
#[tauri::command]
pub async fn wifi_import_wigle(app: AppHandle, state: State<'_, AppState>) -> AppResult<Option<WifiIngestResult>> {
    let builder = app
        .dialog()
        .file()
        .set_title("Import Wigle CSV")
        .add_filter("Wigle CSV", &["csv", "txt"]);
    let picked = tokio::task::spawn_blocking(move || builder.blocking_pick_file())
        .await
        .map_err(|e| AppError::Other(format!("dialog task failed: {e}")))?;
    let Some(fp) = picked else { return Ok(None) };
    let bytes = read_picked(&app, &fp)?;
    let now = db::now();
    let (rows, stats) = wifi::parse_wigle_csv(bytes.as_slice(), now)?;
    let mut conn = state.conn();
    let inserted = wifi::upsert(&mut conn, &rows)?;
    Ok(Some(WifiIngestResult {
        inserted,
        stats,
        upstream_generated: None,
    }))
}

/// Delete sightings: `source` is `upstream`, `wigle_import`, or omitted for all.
#[tauri::command]
pub async fn wifi_clear(state: State<'_, AppState>, source: Option<String>) -> AppResult<usize> {
    let conn = state.conn();
    let n = wifi::clear(&conn, source.as_deref())?;
    if source.as_deref() != Some(wifi::SOURCE_IMPORT) {
        db::set_json(&conn, "wifi_dataset", &WifiDatasetMeta::default())?;
    }
    Ok(n)
}

#[tauri::command]
pub async fn get_wifi_sightings(state: State<'_, AppState>, bbox: BBox) -> AppResult<Vec<WifiSighting>> {
    let conn = state.conn();
    wifi::in_bbox(&conn, &bbox)
}

#[tauri::command]
pub async fn wifi_oui_list() -> AppResult<Vec<wifi::OuiEntry>> {
    Ok(wifi::oui_list().to_vec())
}

// ---------------------------------------------------------------------------
// Misc
// ---------------------------------------------------------------------------

/// Forward a frontend console error into the Rust log so it lands in the log file.
#[tauri::command]
pub async fn frontend_log(level: String, message: String) -> AppResult<()> {
    let message: String = message.chars().take(2000).collect();
    match level.as_str() {
        "error" => log::error!("[webview] {message}"),
        "warn" => log::warn!("[webview] {message}"),
        _ => log::info!("[webview] {message}"),
    }
    Ok(())
}

/// Open an http(s) URL in the system browser (OSM element links, docs).
#[tauri::command]
pub async fn open_external(app: AppHandle, url: String) -> AppResult<()> {
    let parsed = url::Url::parse(&url).map_err(|e| AppError::Invalid(format!("bad URL: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AppError::Invalid("only http(s) links can be opened".into()));
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| AppError::Other(format!("could not open browser: {e}")))
}
