//! Proximity alerts: watch areas, routes, change detection, notifications, reports.
//!
//! "Proximity" means proximity to places and routes the user has saved. There is no
//! position tracking anywhere in this module.

use crate::db::{self, Camera};
use crate::error::{AppError, AppResult};
use crate::fetcher::{fetch_cells, Grouping};
use crate::geo_util::{haversine_m, point_to_polyline, polyline_length_m};
use crate::grid::{cells_for_circle, cells_for_polyline, circle_bbox, BBox, Cell};
use crate::state::AppState;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

pub const RADIUS_RANGE_M: (i64, i64) = (100, 10_000);
pub const CORRIDOR_RANGE_M: (i64, i64) = (50, 1_000);
pub const COVERAGE_DISCLAIMER: &str =
    "Crowdsourced data — coverage is incomplete. Absence of a marker does not mean absence of a camera.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WatchArea {
    pub id: i64,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub radius_m: i64,
    pub created_at: i64,
    pub last_checked: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Route {
    pub id: i64,
    pub name: String,
    pub geojson: String,
    pub corridor_m: i64,
    pub created_at: i64,
    pub last_checked: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertEvent {
    pub id: i64,
    pub target_type: String,
    pub target_id: i64,
    pub osm_type: String,
    pub osm_id: i64,
    pub event: String,
    pub occurred_at: i64,
    pub notified: bool,
}

pub type Key = (String, i64);

// ---------------------------------------------------------------------------
// Watch areas
// ---------------------------------------------------------------------------

fn row_to_area(r: &rusqlite::Row) -> rusqlite::Result<WatchArea> {
    Ok(WatchArea {
        id: r.get(0)?,
        name: r.get(1)?,
        lat: r.get(2)?,
        lon: r.get(3)?,
        radius_m: r.get(4)?,
        created_at: r.get(5)?,
        last_checked: r.get(6)?,
    })
}

const AREA_COLS: &str = "id, name, lat, lon, radius_m, created_at, last_checked";

pub fn list_areas(conn: &Connection) -> AppResult<Vec<WatchArea>> {
    let mut stmt = conn.prepare_cached(&format!("SELECT {AREA_COLS} FROM watch_areas ORDER BY created_at"))?;
    let rows = stmt.query_map([], row_to_area)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_area(conn: &Connection, id: i64) -> AppResult<Option<WatchArea>> {
    let mut stmt = conn.prepare_cached(&format!("SELECT {AREA_COLS} FROM watch_areas WHERE id = ?1"))?;
    Ok(stmt.query_row(params![id], row_to_area).optional()?)
}

pub fn create_area(conn: &Connection, name: &str, lat: f64, lon: f64, radius_m: i64) -> AppResult<WatchArea> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Invalid("watch area needs a name".into()));
    }
    if !crate::geo_util::valid_coord(lat, lon) {
        return Err(AppError::Invalid("watch area centre is outside the valid range".into()));
    }
    if radius_m < RADIUS_RANGE_M.0 || radius_m > RADIUS_RANGE_M.1 {
        return Err(AppError::Invalid(format!(
            "radius must be between {} m and {} m",
            RADIUS_RANGE_M.0, RADIUS_RANGE_M.1
        )));
    }
    conn.execute(
        "INSERT INTO watch_areas(name, lat, lon, radius_m, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![name, lat, lon, radius_m, db::now()],
    )?;
    let id = conn.last_insert_rowid();
    get_area(conn, id)?.ok_or_else(|| AppError::Other("watch area vanished".into()))
}

pub fn rename_area(conn: &Connection, id: i64, name: &str) -> AppResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Invalid("watch area needs a name".into()));
    }
    conn.execute("UPDATE watch_areas SET name = ?2 WHERE id = ?1", params![id, name])?;
    Ok(())
}

pub fn delete_area(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM watch_areas WHERE id = ?1", params![id])?;
    conn.execute(
        "DELETE FROM alert_events WHERE target_type = 'area' AND target_id = ?1",
        params![id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

fn row_to_route(r: &rusqlite::Row) -> rusqlite::Result<Route> {
    Ok(Route {
        id: r.get(0)?,
        name: r.get(1)?,
        geojson: r.get(2)?,
        corridor_m: r.get(3)?,
        created_at: r.get(4)?,
        last_checked: r.get(5)?,
    })
}

const ROUTE_COLS: &str = "id, name, geojson, corridor_m, created_at, last_checked";

pub fn list_routes(conn: &Connection) -> AppResult<Vec<Route>> {
    let mut stmt = conn.prepare_cached(&format!("SELECT {ROUTE_COLS} FROM routes ORDER BY created_at"))?;
    let rows = stmt.query_map([], row_to_route)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_route(conn: &Connection, id: i64) -> AppResult<Option<Route>> {
    let mut stmt = conn.prepare_cached(&format!("SELECT {ROUTE_COLS} FROM routes WHERE id = ?1"))?;
    Ok(stmt.query_row(params![id], row_to_route).optional()?)
}

/// GeoJSON LineString for a (lat, lon) polyline.
pub fn linestring_geojson(points: &[(f64, f64)]) -> String {
    let coords: Vec<[f64; 2]> = points.iter().map(|(lat, lon)| [*lon, *lat]).collect();
    serde_json::json!({ "type": "LineString", "coordinates": coords }).to_string()
}

/// (lat, lon) polyline from a GeoJSON LineString (or a Feature wrapping one).
pub fn parse_linestring(geojson: &str) -> AppResult<Vec<(f64, f64)>> {
    let v: serde_json::Value = serde_json::from_str(geojson)?;
    let geom = if v.get("type").and_then(|t| t.as_str()) == Some("Feature") {
        v.get("geometry").cloned().unwrap_or(serde_json::Value::Null)
    } else {
        v
    };
    if geom.get("type").and_then(|t| t.as_str()) != Some("LineString") {
        return Err(AppError::Invalid("route geometry must be a GeoJSON LineString".into()));
    }
    let coords = geom
        .get("coordinates")
        .and_then(|c| c.as_array())
        .ok_or_else(|| AppError::Invalid("LineString has no coordinates".into()))?;
    let mut out = Vec::with_capacity(coords.len());
    for c in coords {
        let arr = c.as_array().ok_or_else(|| AppError::Invalid("bad coordinate".into()))?;
        let lon = arr.first().and_then(|x| x.as_f64());
        let lat = arr.get(1).and_then(|x| x.as_f64());
        match (lat, lon) {
            (Some(lat), Some(lon)) if crate::geo_util::valid_coord(lat, lon) => out.push((lat, lon)),
            _ => return Err(AppError::Invalid("coordinate out of range in route".into())),
        }
    }
    Ok(out)
}

pub fn create_route(conn: &Connection, name: &str, points: &[(f64, f64)], corridor_m: i64) -> AppResult<Route> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Invalid("route needs a name".into()));
    }
    if points.len() < 2 {
        return Err(AppError::Invalid("route needs at least two points".into()));
    }
    if corridor_m < CORRIDOR_RANGE_M.0 || corridor_m > CORRIDOR_RANGE_M.1 {
        return Err(AppError::Invalid(format!(
            "corridor width must be between {} m and {} m",
            CORRIDOR_RANGE_M.0, CORRIDOR_RANGE_M.1
        )));
    }
    conn.execute(
        "INSERT INTO routes(name, geojson, corridor_m, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![name, linestring_geojson(points), corridor_m, db::now()],
    )?;
    let id = conn.last_insert_rowid();
    get_route(conn, id)?.ok_or_else(|| AppError::Other("route vanished".into()))
}

pub fn delete_route(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM routes WHERE id = ?1", params![id])?;
    conn.execute(
        "DELETE FROM alert_events WHERE target_type = 'route' AND target_id = ?1",
        params![id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Membership computation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct AreaCamera {
    pub camera: Camera,
    pub distance_m: f64,
}

pub fn cameras_in_area(conn: &Connection, area: &WatchArea) -> AppResult<Vec<AreaCamera>> {
    let bbox = circle_bbox(area.lat, area.lon, area.radius_m as f64);
    let mut out: Vec<AreaCamera> = db::cameras_in_bbox(conn, &bbox)?
        .into_iter()
        .filter(|c| c.stale_since.is_none())
        .map(|c| {
            let d = haversine_m(area.lat, area.lon, c.lat, c.lon);
            AreaCamera { camera: c, distance_m: d }
        })
        .filter(|ac| ac.distance_m <= area.radius_m as f64)
        .collect();
    out.sort_by(|a, b| a.distance_m.total_cmp(&b.distance_m));
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteCamera {
    pub camera: Camera,
    /// Perpendicular distance from the route, metres.
    pub distance_m: f64,
    /// Distance along the route from its start, metres.
    pub along_m: f64,
    pub segment: usize,
}

fn polyline_bbox(points: &[(f64, f64)], pad_m: f64) -> BBox {
    let mut south = f64::MAX;
    let mut north = f64::MIN;
    let mut west = f64::MAX;
    let mut east = f64::MIN;
    for (lat, lon) in points {
        south = south.min(*lat);
        north = north.max(*lat);
        west = west.min(*lon);
        east = east.max(*lon);
    }
    let dlat = pad_m / 111_320.0;
    let cos = ((south + north) / 2.0).to_radians().cos().abs().max(0.01);
    let dlon = pad_m / (111_320.0 * cos);
    BBox::new(south - dlat, west - dlon, north + dlat, east + dlon)
}

pub fn cameras_along_route(conn: &Connection, route: &Route) -> AppResult<Vec<RouteCamera>> {
    let points = parse_linestring(&route.geojson)?;
    cameras_along_polyline(conn, &points, route.corridor_m as f64)
}

pub fn cameras_along_polyline(conn: &Connection, points: &[(f64, f64)], corridor_m: f64) -> AppResult<Vec<RouteCamera>> {
    if points.is_empty() {
        return Ok(Vec::new());
    }
    let bbox = polyline_bbox(points, corridor_m);
    let mut out = Vec::new();
    for camera in db::cameras_in_bbox(conn, &bbox)? {
        if camera.stale_since.is_some() {
            continue;
        }
        if let Some(hit) = point_to_polyline(camera.lat, camera.lon, points) {
            if hit.distance_m <= corridor_m {
                out.push(RouteCamera {
                    camera,
                    distance_m: hit.distance_m,
                    along_m: hit.along_m,
                    segment: hit.segment,
                });
            }
        }
    }
    out.sort_by(|a, b| a.along_m.total_cmp(&b.along_m));
    Ok(out)
}

/// Cells intersecting every saved target.
pub fn cells_for_targets(areas: &[WatchArea], routes: &[Route]) -> AppResult<Vec<Cell>> {
    let mut set = BTreeSet::new();
    for a in areas {
        set.extend(cells_for_circle(a.lat, a.lon, a.radius_m as f64));
    }
    for r in routes {
        let pts = parse_linestring(&r.geojson)?;
        set.extend(cells_for_polyline(&pts, r.corridor_m as f64));
    }
    Ok(set.into_iter().collect())
}

// ---------------------------------------------------------------------------
// Event log and diffing
// ---------------------------------------------------------------------------

fn row_to_event(r: &rusqlite::Row) -> rusqlite::Result<AlertEvent> {
    Ok(AlertEvent {
        id: r.get(0)?,
        target_type: r.get(1)?,
        target_id: r.get(2)?,
        osm_type: r.get(3)?,
        osm_id: r.get(4)?,
        event: r.get(5)?,
        occurred_at: r.get(6)?,
        notified: r.get::<_, i64>(7)? != 0,
    })
}

const EVENT_COLS: &str = "id, target_type, target_id, osm_type, osm_id, event, occurred_at, notified";

pub fn history(conn: &Connection, target_type: &str, target_id: i64) -> AppResult<Vec<AlertEvent>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {EVENT_COLS} FROM alert_events WHERE target_type = ?1 AND target_id = ?2 ORDER BY occurred_at DESC, id DESC"
    ))?;
    let rows = stmt
        .query_map(params![target_type, target_id], row_to_event)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The set of cameras this target was last known to contain, replayed from its events.
pub fn known_set(conn: &Connection, target_type: &str, target_id: i64) -> AppResult<Option<HashSet<Key>>> {
    let mut stmt = conn.prepare_cached(
        "SELECT osm_type, osm_id, event FROM alert_events WHERE target_type = ?1 AND target_id = ?2 ORDER BY occurred_at, id",
    )?;
    let rows = stmt.query_map(params![target_type, target_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))
    })?;
    let mut set = HashSet::new();
    let mut any = false;
    for row in rows {
        let (t, id, ev) = row?;
        any = true;
        match ev.as_str() {
            "baseline" | "added" => {
                set.insert((t, id));
            }
            "removed" => {
                set.remove(&(t, id));
            }
            _ => {}
        }
    }
    Ok(if any { Some(set) } else { None })
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct CheckDiff {
    pub baseline: bool,
    pub added: Vec<Key>,
    pub removed: Vec<Key>,
    pub count: usize,
}

/// Compare the current membership with the last known set and record events.
/// The first check for a target records a silent baseline.
pub fn record_check(
    conn: &mut Connection,
    target_type: &str,
    target_id: i64,
    current: &HashSet<Key>,
    now: i64,
) -> AppResult<CheckDiff> {
    let known = known_set(conn, target_type, target_id)?;
    let tx = conn.transaction()?;
    let mut diff = CheckDiff {
        count: current.len(),
        ..Default::default()
    };
    {
        let mut insert = tx.prepare_cached(
            "INSERT INTO alert_events(target_type, target_id, osm_type, osm_id, event, occurred_at, notified)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
        )?;
        match known {
            None => {
                diff.baseline = true;
                let mut sorted: Vec<&Key> = current.iter().collect();
                sorted.sort();
                for k in sorted {
                    insert.execute(params![target_type, target_id, k.0, k.1, "baseline", now])?;
                }
            }
            Some(known) => {
                let mut added: Vec<Key> = current.difference(&known).cloned().collect();
                let mut removed: Vec<Key> = known.difference(current).cloned().collect();
                added.sort();
                removed.sort();
                for k in &added {
                    insert.execute(params![target_type, target_id, k.0, k.1, "added", now])?;
                }
                for k in &removed {
                    insert.execute(params![target_type, target_id, k.0, k.1, "removed", now])?;
                }
                diff.added = added;
                diff.removed = removed;
            }
        }
    }
    let table = if target_type == "area" { "watch_areas" } else { "routes" };
    tx.execute(
        &format!("UPDATE {table} SET last_checked = ?2 WHERE id = ?1"),
        params![target_id, now],
    )?;
    tx.commit()?;
    Ok(diff)
}

pub fn mark_notified(conn: &Connection, target_type: &str, target_id: i64, keys: &[Key], now: i64) -> AppResult<()> {
    let mut stmt = conn.prepare_cached(
        "UPDATE alert_events SET notified = 1 WHERE target_type = ?1 AND target_id = ?2
         AND osm_type = ?3 AND osm_id = ?4 AND event = 'added' AND occurred_at = ?5",
    )?;
    for k in keys {
        stmt.execute(params![target_type, target_id, k.0, k.1, now])?;
    }
    Ok(())
}

/// Number of `added` events newer than the last time the Alerts panel was opened.
pub fn unseen_added_count(conn: &Connection) -> AppResult<i64> {
    let seen: i64 = db::get_json(conn, "alerts_seen_at")?.unwrap_or(0);
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM alert_events WHERE event = 'added' AND occurred_at > ?1",
        params![seen],
        |r| r.get(0),
    )?)
}

// ---------------------------------------------------------------------------
// Refresh orchestration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct TargetOutcome {
    pub target_type: String,
    pub target_id: i64,
    pub name: String,
    pub baseline: bool,
    pub added: Vec<Key>,
    pub removed: Vec<Key>,
    pub count: usize,
    pub notified: bool,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct RefreshOutcome {
    pub started_at: i64,
    pub finished_at: i64,
    pub offline: bool,
    pub requests: usize,
    pub cells_fetched: usize,
    pub targets: Vec<TargetOutcome>,
}

fn log_refresh(conn: &Connection, started: i64, outcome: &str, detail: Option<&str>, cells: usize) {
    let _ = conn.execute(
        "INSERT INTO refresh_log(started_at, finished_at, outcome, detail, cells) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![started, db::now(), outcome, detail, cells as i64],
    );
}

/// Run one alert refresh: re-query the cells intersecting every watch area and route,
/// diff each target against its last known set, notify for genuinely new cameras.
/// An offline network is an expected state: it is logged and reported, not an error.
pub async fn run_refresh(app: &AppHandle, force_fetch: bool) -> AppResult<RefreshOutcome> {
    run_guarded(app, Some(force_fetch)).await
}

/// Re-diff every target against the local cameras table without any network request. Runs
/// after a worldwide sync so cameras it brought in notify like any other refresh.
pub async fn run_refresh_local(app: &AppHandle) -> AppResult<RefreshOutcome> {
    run_guarded(app, None).await
}

async fn run_guarded(app: &AppHandle, fetch: Option<bool>) -> AppResult<RefreshOutcome> {
    let state = app.state::<AppState>();
    if state
        .refresh_running
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        return Err(AppError::Other("an alert refresh is already running".into()));
    }
    let result = run_refresh_inner(app, &state, fetch).await;
    state
        .refresh_running
        .store(false, std::sync::atomic::Ordering::SeqCst);
    result
}

/// `fetch`: `Some(force)` re-queries the target cells first; `None` uses local data only.
async fn run_refresh_inner(app: &AppHandle, state: &AppState, fetch: Option<bool>) -> AppResult<RefreshOutcome> {
    let started = db::now();
    let (areas, routes, settings) = {
        let conn = state.conn();
        (list_areas(&conn)?, list_routes(&conn)?, db::load_settings(&conn)?)
    };
    let mut outcome = RefreshOutcome {
        started_at: started,
        ..Default::default()
    };
    if areas.is_empty() && routes.is_empty() {
        outcome.finished_at = db::now();
        return Ok(outcome);
    }

    let fetched = match fetch {
        Some(force_fetch) => {
            let cells = cells_for_targets(&areas, &routes)?;
            Some(fetch_cells(state, &cells, force_fetch, Grouping::SuperCells).await)
        }
        None => None,
    };
    match fetched {
        None => {}
        Some(Ok(stats)) => {
            outcome.requests = stats.requests;
            outcome.cells_fetched = stats.cells_fetched;
            if stats.requests > 0 {
                let _ = app.emit(crate::sync::EVENT_CAMERAS_CHANGED, ());
            }
        }
        Some(Err(e)) if e.is_offline() => {
            log::info!("alert refresh skipped: offline ({e})");
            let conn = state.conn();
            log_refresh(&conn, started, "skipped_offline", Some(&e.to_string()), 0);
            db::set_json(&conn, "last_refresh_skipped_at", &db::now())?;
            outcome.offline = true;
            outcome.finished_at = db::now();
            let _ = app.emit("alerts:refreshed", &outcome);
            return Ok(outcome);
        }
        Some(Err(e)) => {
            let conn = state.conn();
            log_refresh(&conn, started, "error", Some(&e.to_string()), 0);
            return Err(e);
        }
    }

    let now = db::now();
    {
        let mut conn = state.conn();
        for area in &areas {
            let current: HashSet<Key> = cameras_in_area(&conn, area)?
                .into_iter()
                .map(|ac| (ac.camera.osm_type, ac.camera.osm_id))
                .collect();
            let diff = record_check(&mut conn, "area", area.id, &current, now)?;
            outcome.targets.push(TargetOutcome {
                target_type: "area".into(),
                target_id: area.id,
                name: area.name.clone(),
                baseline: diff.baseline,
                added: diff.added,
                removed: diff.removed,
                count: diff.count,
                notified: false,
            });
        }
        for route in &routes {
            let current: HashSet<Key> = cameras_along_route(&conn, route)?
                .into_iter()
                .map(|rc| (rc.camera.osm_type, rc.camera.osm_id))
                .collect();
            let diff = record_check(&mut conn, "route", route.id, &current, now)?;
            outcome.targets.push(TargetOutcome {
                target_type: "route".into(),
                target_id: route.id,
                name: route.name.clone(),
                baseline: diff.baseline,
                added: diff.added,
                removed: diff.removed,
                count: diff.count,
                notified: false,
            });
        }
        db::set_json(&conn, "last_alert_refresh", &now)?;
        log_refresh(&conn, started, "ok", None, outcome.cells_fetched);
    }

    // Notify only for newly present cameras, once per target. Baselines are silent.
    for t in outcome.targets.iter_mut() {
        if t.baseline || t.added.is_empty() {
            continue;
        }
        if settings.notifications_enabled {
            let n = t.added.len();
            let where_ = if t.target_type == "area" { "near" } else { "along" };
            let body = format!(
                "{n} new ALPR camera{} {where_} {}.",
                if n == 1 { "" } else { "s" },
                t.name
            );
            match app.notification().builder().title("Flock Finder").body(&body).show() {
                Ok(()) => {
                    t.notified = true;
                    let conn = state.conn();
                    mark_notified(&conn, &t.target_type, t.target_id, &t.added, now)?;
                }
                Err(e) => log::warn!("desktop notification failed (in-app badge still updates): {e}"),
            }
        }
    }

    outcome.finished_at = db::now();
    let _ = app.emit("alerts:refreshed", &outcome);
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// Route report + exports
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct RouteReport {
    pub route: Route,
    pub disclaimer: String,
    pub generated_at: i64,
    pub length_m: f64,
    pub cameras: Vec<RouteCamera>,
}

pub fn route_report(conn: &Connection, route_id: i64) -> AppResult<RouteReport> {
    let route = get_route(conn, route_id)?.ok_or_else(|| AppError::Invalid(format!("route {route_id} not found")))?;
    let points = parse_linestring(&route.geojson)?;
    let cameras = cameras_along_polyline(conn, &points, route.corridor_m as f64)?;
    Ok(RouteReport {
        disclaimer: COVERAGE_DISCLAIMER.to_string(),
        generated_at: db::now(),
        length_m: polyline_length_m(&points),
        route,
        cameras,
    })
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub fn report_csv(report: &RouteReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Flock Finder route report: {}\n", report.route.name));
    out.push_str(&format!("# {}\n", report.disclaimer));
    out.push_str("along_m,distance_from_route_m,category,osm_type,osm_id,lat,lon,operator,brand,manufacturer,direction\n");
    for c in &report.cameras {
        let t = &c.camera.tags;
        out.push_str(&format!(
            "{:.0},{:.0},{},{},{},{:.7},{:.7},{},{},{},{}\n",
            c.along_m,
            c.distance_m,
            c.camera.category,
            c.camera.osm_type,
            c.camera.osm_id,
            c.camera.lat,
            c.camera.lon,
            csv_escape(t.get("operator").map(String::as_str).unwrap_or("")),
            csv_escape(t.get("brand").map(String::as_str).unwrap_or("")),
            csv_escape(t.get("manufacturer").map(String::as_str).unwrap_or("")),
            csv_escape(t.get("direction").map(String::as_str).unwrap_or("")),
        ));
    }
    out
}

pub fn report_geojson(report: &RouteReport) -> String {
    let route_geom: serde_json::Value = serde_json::from_str(&report.route.geojson).unwrap_or(serde_json::Value::Null);
    let mut features = vec![serde_json::json!({
        "type": "Feature",
        "properties": { "kind": "route", "name": report.route.name, "corridor_m": report.route.corridor_m },
        "geometry": route_geom,
    })];
    for c in &report.cameras {
        features.push(serde_json::json!({
            "type": "Feature",
            "properties": {
                "kind": "camera",
                "category": c.camera.category,
                "osm_type": c.camera.osm_type,
                "osm_id": c.camera.osm_id,
                "along_m": (c.along_m * 10.0).round() / 10.0,
                "distance_from_route_m": (c.distance_m * 10.0).round() / 10.0,
                "tags": c.camera.tags,
            },
            "geometry": { "type": "Point", "coordinates": [c.camera.lon, c.camera.lat] },
        }));
    }
    serde_json::json!({
        "type": "FeatureCollection",
        "disclaimer": report.disclaimer,
        "generated_at": report.generated_at,
        "features": features,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;
    use crate::grid::cells_for_bbox;
    use crate::overpass::ParsedElement;
    use std::collections::BTreeMap;

    fn seed(conn: &mut Connection, cams: &[(i64, f64, f64)], now: i64) {
        let area = BBox::new(39.70, -105.00, 39.80, -104.90);
        let cells = cells_for_bbox(&area);
        let elements: Vec<ParsedElement> = cams
            .iter()
            .map(|(id, lat, lon)| ParsedElement {
                osm_type: "node".into(),
                osm_id: *id,
                lat: *lat,
                lon: *lon,
                tags: BTreeMap::from([("surveillance:type".to_string(), "ALPR".to_string())]),
            })
            .collect();
        db::ingest_fetch(conn, &area, &cells, &elements, now).unwrap();
    }

    #[test]
    fn area_membership_uses_haversine() {
        let mut c = test_conn();
        seed(&mut c, &[(1, 39.7500, -104.9500), (2, 39.7500, -104.9400), (3, 39.7600, -104.9500)], 1);
        let area = create_area(&c, "Home", 39.75, -104.95, 900).unwrap();
        let inside = cameras_in_area(&c, &area).unwrap();
        // 1 at centre; 2 is ~857 m east (inside); 3 is ~1112 m north (outside).
        let ids: Vec<i64> = inside.iter().map(|a| a.camera.osm_id).collect();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn first_check_is_silent_baseline_then_diffs() {
        let mut c = test_conn();
        seed(&mut c, &[(1, 39.7500, -104.9500)], 1);
        let area = create_area(&c, "Home", 39.75, -104.95, 500).unwrap();
        let cur = |c: &Connection| -> HashSet<Key> {
            cameras_in_area(c, &area)
                .unwrap()
                .into_iter()
                .map(|a| (a.camera.osm_type, a.camera.osm_id))
                .collect()
        };
        let s0 = cur(&c);
        let d0 = record_check(&mut c, "area", area.id, &s0, 10).unwrap();
        assert!(d0.baseline);
        assert!(d0.added.is_empty());
        assert_eq!(d0.count, 1);
        assert_eq!(get_area(&c, area.id).unwrap().unwrap().last_checked, Some(10));

        // A new camera appears; the old one disappears.
        seed(&mut c, &[(2, 39.7501, -104.9501)], 20);
        let s1 = cur(&c);
        let d1 = record_check(&mut c, "area", area.id, &s1, 20).unwrap();
        assert!(!d1.baseline);
        assert_eq!(d1.added, vec![("node".to_string(), 2)]);
        assert_eq!(d1.removed, vec![("node".to_string(), 1)]);

        // Nothing changed: no new events.
        let s2 = cur(&c);
        let d2 = record_check(&mut c, "area", area.id, &s2, 30).unwrap();
        assert!(d2.added.is_empty() && d2.removed.is_empty());
        let hist = history(&c, "area", area.id).unwrap();
        assert_eq!(hist.len(), 3);
        assert_eq!(known_set(&c, "area", area.id).unwrap().unwrap().len(), 1);
    }

    #[test]
    fn route_report_orders_by_distance_along() {
        let mut c = test_conn();
        // Route runs north along lon -104.95 from 39.70 to 39.80.
        seed(&mut c, &[(1, 39.7800, -104.9502), (2, 39.7200, -104.9501), (3, 39.7500, -104.9400)], 1);
        let route = create_route(&c, "Commute", &[(39.70, -104.95), (39.80, -104.95)], 100).unwrap();
        let report = route_report(&c, route.id).unwrap();
        let ids: Vec<i64> = report.cameras.iter().map(|r| r.camera.osm_id).collect();
        assert_eq!(ids, vec![2, 1]); // 3 is ~860 m off the route
        assert!(report.cameras[0].along_m < report.cameras[1].along_m);
        assert!((report.length_m - 11_120.0).abs() < 100.0);
        assert!(report.disclaimer.contains("incomplete"));
        let csv = report_csv(&report);
        assert!(csv.starts_with("# Flock Finder route report: Commute\n# Crowdsourced"));
        assert_eq!(csv.lines().count(), 5);
        let gj: serde_json::Value = serde_json::from_str(&report_geojson(&report)).unwrap();
        assert_eq!(gj["features"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn linestring_round_trip_and_validation() {
        let pts = vec![(39.7, -105.0), (39.8, -104.9)];
        let gj = linestring_geojson(&pts);
        assert_eq!(parse_linestring(&gj).unwrap(), pts);
        assert!(parse_linestring(r#"{"type":"Point","coordinates":[1,2]}"#).is_err());
        assert!(parse_linestring(r#"{"type":"LineString","coordinates":[[200,1],[1,1]]}"#).is_err());
    }

    #[test]
    fn validation_ranges() {
        let c = test_conn();
        assert!(create_area(&c, "x", 0.0, 0.0, 50).is_err());
        assert!(create_area(&c, "x", 0.0, 0.0, 20_000).is_err());
        assert!(create_area(&c, "  ", 0.0, 0.0, 500).is_err());
        assert!(create_route(&c, "r", &[(0.0, 0.0)], 100).is_err());
        assert!(create_route(&c, "r", &[(0.0, 0.0), (1.0, 1.0)], 10).is_err());
        assert!(create_route(&c, "r", &[(0.0, 0.0), (1.0, 1.0)], 100).is_ok());
    }

    #[test]
    fn long_track_corridor_is_fast() {
        // ~200 km track with 4,000 points and 500 candidate cameras.
        let mut c = test_conn();
        // The track runs up the middle of a cell column so its 100 m corridor stays in one column.
        let pts: Vec<(f64, f64)> = (0..4000).map(|i| (39.0 + i as f64 * 0.00045, -104.975)).collect();
        let cams: Vec<(i64, f64, f64)> = (0..500).map(|i| (i, 39.0 + i as f64 * 0.0036, -104.975 + 0.0005 * (i % 3) as f64)).collect();
        let area = BBox::new(38.9, -105.1, 40.9, -104.9);
        let elements: Vec<ParsedElement> = cams
            .iter()
            .map(|(id, lat, lon)| ParsedElement { osm_type: "node".into(), osm_id: *id, lat: *lat, lon: *lon, tags: BTreeMap::new() })
            .collect();
        db::ingest_fetch(&mut c, &area, &cells_for_bbox(&area), &elements, 1).unwrap();
        let start = std::time::Instant::now();
        let hits = cameras_along_polyline(&c, &pts, 100.0).unwrap();
        let elapsed = start.elapsed();
        assert!(elapsed.as_secs_f64() < 5.0, "took {elapsed:?}");
        assert!(hits.len() >= 160, "hits={}", hits.len());
        let cells = cells_for_polyline(&pts, 100.0);
        assert!(cells.len() <= 40, "cells={}", cells.len());
    }
}
