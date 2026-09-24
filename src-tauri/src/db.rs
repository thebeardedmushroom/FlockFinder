//! SQLite access: connection setup, numbered migrations, camera cache, settings.

use crate::error::AppResult;
use crate::grid::{BBox, Cell};
use crate::overpass::{classify, ParsedElement};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// Numbered migrations, applied in order inside a transaction each. Never edit a
/// shipped file; add a new one.
const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_initial.sql")),
    (2, include_str!("../migrations/0002_app_tables.sql")),
    (3, include_str!("../migrations/0003_dark_basemap.sql")),
    (4, include_str!("../migrations/0004_wifi_sightings.sql")),
    (5, include_str!("../migrations/0005_sync_runs.sql")),
    (6, include_str!("../migrations/0006_camera_version.sql")),
];

/// Cameras absent from a fresh fetch are kept (hollow marker) for this long, then deleted.
pub const STALE_RETENTION_SECS: i64 = 30 * 24 * 3600;

/// A worldwide response with fewer cameras than this share of the ones already stored is
/// treated as incomplete and not applied (applying it would mark most of the world stale).
pub const SYNC_MIN_PLAUSIBLE_SHARE: f64 = 0.5;
/// The plausibility check only applies once the store holds at least this many cameras.
pub const SYNC_PLAUSIBILITY_FLOOR: i64 = 1000;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn open(path: &Path) -> AppResult<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    configure(&conn)?;
    Ok(conn)
}

#[cfg(test)]
pub fn open_in_memory() -> AppResult<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(())
}

pub fn migrate(conn: &mut Connection) -> AppResult<Vec<i64>> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL
        );",
    )?;
    let mut applied = Vec::new();
    for (version, sql) in MIGRATIONS {
        let done: Option<i64> = conn
            .query_row(
                "SELECT version FROM schema_migrations WHERE version = ?1",
                params![version],
                |r| r.get(0),
            )
            .optional()?;
        if done.is_some() {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
            params![version, now()],
        )?;
        tx.commit()?;
        log::info!("applied migration {version}");
        applied.push(*version);
    }
    Ok(applied)
}

// ---------------------------------------------------------------------------
// Cameras
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Camera {
    pub osm_type: String,
    pub osm_id: i64,
    pub lat: f64,
    pub lon: f64,
    pub category: String,
    pub tags: BTreeMap<String, String>,
    pub first_seen: i64,
    pub last_seen: i64,
    pub stale_since: Option<i64>,
}

impl Camera {
    pub fn key(&self) -> String {
        format!("{}/{}", self.osm_type, self.osm_id)
    }
}

const CAMERA_COLS: &str =
    "osm_type, osm_id, lat, lon, category, tags_json, first_seen, last_seen, stale_since";

fn row_to_camera(row: &rusqlite::Row) -> rusqlite::Result<Camera> {
    let tags_json: String = row.get(5)?;
    Ok(Camera {
        osm_type: row.get(0)?,
        osm_id: row.get(1)?,
        lat: row.get(2)?,
        lon: row.get(3)?,
        category: row.get(4)?,
        tags: serde_json::from_str(&tags_json).unwrap_or_default(),
        first_seen: row.get(6)?,
        last_seen: row.get(7)?,
        stale_since: row.get(8)?,
    })
}

/// Cameras within a bbox (antimeridian-crossing boxes are handled).
pub fn cameras_in_bbox(conn: &Connection, bbox: &BBox) -> AppResult<Vec<Camera>> {
    let mut out = Vec::new();
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {CAMERA_COLS} FROM cameras
         WHERE lat >= ?1 AND lat <= ?2 AND lon >= ?3 AND lon <= ?4"
    ))?;
    for part in bbox.sanitized().split_antimeridian() {
        let rows = stmt.query_map(
            params![part.south, part.north, part.west, part.east],
            row_to_camera,
        )?;
        for r in rows {
            out.push(r?);
        }
    }
    Ok(out)
}

pub fn camera_get(conn: &Connection, osm_type: &str, osm_id: i64) -> AppResult<Option<Camera>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {CAMERA_COLS} FROM cameras WHERE osm_type = ?1 AND osm_id = ?2"
    ))?;
    Ok(stmt
        .query_row(params![osm_type, osm_id], row_to_camera)
        .optional()?)
}

pub fn camera_count(conn: &Connection) -> AppResult<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM cameras", [], |r| r.get(0))?)
}

/// Which of `cells` need a (re-)fetch: never fetched, or fetched longer than `ttl_secs` ago.
pub fn stale_cells(conn: &Connection, cells: &[Cell], ttl_secs: i64, now: i64) -> AppResult<Vec<Cell>> {
    let mut stmt = conn.prepare_cached("SELECT fetched_at FROM grid_cells WHERE cell_key = ?1")?;
    let mut out = Vec::new();
    for cell in cells {
        let fetched: Option<i64> = stmt
            .query_row(params![cell.key()], |r| r.get(0))
            .optional()?;
        match fetched {
            Some(t) if now - t < ttl_secs => {}
            _ => out.push(*cell),
        }
    }
    Ok(out)
}

#[cfg(test)]
pub fn cell_fetched_at(conn: &Connection, cell: &Cell) -> AppResult<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT fetched_at FROM grid_cells WHERE cell_key = ?1",
            params![cell.key()],
            |r| r.get(0),
        )
        .optional()?)
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct IngestStats {
    pub upserted: usize,
    pub marked_stale: usize,
    pub cells_marked: usize,
}

/// Insert or refresh every element; a camera seen again stops being stale.
fn upsert_elements(conn: &Connection, elements: &[ParsedElement], now: i64) -> AppResult<usize> {
    let mut upsert = conn.prepare_cached(
        "INSERT INTO cameras(osm_type, osm_id, lat, lon, category, tags_json, first_seen, last_seen, stale_since)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, NULL)
         ON CONFLICT(osm_type, osm_id) DO UPDATE SET
           lat = excluded.lat, lon = excluded.lon, category = excluded.category,
           tags_json = excluded.tags_json, last_seen = excluded.last_seen, stale_since = NULL",
    )?;
    for el in elements {
        let tags_json = serde_json::to_string(&el.tags)?;
        upsert.execute(params![
            el.osm_type,
            el.osm_id,
            el.lat,
            el.lon,
            classify(&el.tags).as_str(),
            tags_json,
            now
        ])?;
    }
    Ok(elements.len())
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GlobalIngestStats {
    pub upserted: usize,
    pub marked_stale: usize,
    pub purged: usize,
}

/// Atomically apply a worldwide sync: upsert every element, then flag every stored camera
/// that the response did not contain. `started_at` is when the sync began; a camera written
/// after that (by an alert refresh racing the download) is left alone.
///
/// A response far smaller than what is already stored is refused rather than applied, so a
/// truncated or degraded answer can never hollow out the whole map.
pub fn ingest_global(
    conn: &mut Connection,
    elements: &[ParsedElement],
    started_at: i64,
    now: i64,
) -> AppResult<GlobalIngestStats> {
    let active: i64 = conn.query_row("SELECT COUNT(*) FROM cameras WHERE stale_since IS NULL", [], |r| r.get(0))?;
    if active >= SYNC_PLAUSIBILITY_FLOOR && (elements.len() as f64) < active as f64 * SYNC_MIN_PLAUSIBLE_SHARE {
        return Err(crate::error::AppError::Parse(format!(
            "the worldwide response has only {} cameras but {active} are stored; it looks incomplete and was not applied",
            elements.len()
        )));
    }
    let tx = conn.transaction()?;
    let upserted = upsert_elements(&tx, elements, now)?;
    let marked_stale = tx.execute(
        "UPDATE cameras SET stale_since = ?1 WHERE last_seen < ?2 AND stale_since IS NULL",
        params![now, started_at],
    )?;
    tx.commit()?;
    let purged = purge_stale(conn, now)?;
    Ok(GlobalIngestStats {
        upserted,
        marked_stale,
        purged,
    })
}

// ---------------------------------------------------------------------------
// Sync runs
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Serialize)]
pub struct SyncSummary {
    /// Finish time of the latest successful sync.
    pub last_ok_at: Option<i64>,
    /// Finish time of the latest finished attempt, successful or not.
    pub last_attempt_at: Option<i64>,
    pub last_outcome: Option<String>,
    pub last_error: Option<String>,
    pub last_elements: Option<i64>,
    pub last_bytes: Option<i64>,
    pub last_duration_secs: Option<i64>,
    pub cameras: i64,
    pub stale_cameras: i64,
}

pub fn sync_run_start(conn: &Connection, started_at: i64) -> AppResult<i64> {
    conn.execute(
        "INSERT INTO sync_runs(started_at, outcome) VALUES (?1, 'running')",
        params![started_at],
    )?;
    Ok(conn.last_insert_rowid())
}

#[allow(clippy::too_many_arguments)]
pub fn sync_run_finish(
    conn: &Connection,
    id: i64,
    outcome: &str,
    detail: Option<&str>,
    elements: Option<usize>,
    marked_stale: Option<usize>,
    bytes: Option<usize>,
) -> AppResult<()> {
    conn.execute(
        "UPDATE sync_runs SET finished_at = ?2, outcome = ?3, detail = ?4, elements = ?5, marked_stale = ?6, bytes = ?7
         WHERE id = ?1",
        params![
            id,
            now(),
            outcome,
            detail,
            elements.map(|v| v as i64),
            marked_stale.map(|v| v as i64),
            bytes.map(|v| v as i64)
        ],
    )?;
    Ok(())
}

/// A run still marked `running` at startup was cut short by the app closing.
pub fn sync_runs_mark_interrupted(conn: &Connection) -> AppResult<usize> {
    Ok(conn.execute(
        "UPDATE sync_runs SET outcome = 'error', detail = 'interrupted (the app closed during the sync)',
           finished_at = coalesce(finished_at, started_at)
         WHERE outcome = 'running'",
        [],
    )?)
}

pub fn sync_summary(conn: &Connection) -> AppResult<SyncSummary> {
    let mut s = SyncSummary::default();
    s.last_ok_at = conn.query_row(
        "SELECT MAX(finished_at) FROM sync_runs WHERE outcome = 'ok'",
        [],
        |r| r.get(0),
    )?;
    if let Some((at, outcome, detail)) = conn
        .query_row(
            "SELECT coalesce(finished_at, started_at), outcome, detail FROM sync_runs
             WHERE outcome != 'running' ORDER BY coalesce(finished_at, started_at) DESC, id DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)),
        )
        .optional()?
    {
        s.last_attempt_at = Some(at);
        s.last_error = if outcome == "ok" { None } else { detail };
        s.last_outcome = Some(outcome);
    }
    if let Some((elements, bytes, duration)) = conn
        .query_row(
            "SELECT elements, bytes, finished_at - started_at FROM sync_runs
             WHERE outcome = 'ok' AND elements IS NOT NULL ORDER BY finished_at DESC, id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
    {
        s.last_elements = elements;
        s.last_bytes = bytes;
        s.last_duration_secs = duration;
    }
    let (active, stale): (i64, i64) = conn.query_row(
        "SELECT coalesce(SUM(stale_since IS NULL), 0), coalesce(SUM(stale_since IS NOT NULL), 0) FROM cameras",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    s.cameras = active;
    s.stale_cameras = stale;
    Ok(s)
}

/// Atomically record a completed fetch of `area` (a single non-antimeridian bbox that
/// exactly covers `cells`): upsert every element, flag cached cameras inside the area
/// that were absent from the response, and stamp the cells as fetched. If the process
/// dies mid-way nothing is committed, so no cell is ever half-cached.
pub fn ingest_fetch(
    conn: &mut Connection,
    area: &BBox,
    cells: &[Cell],
    elements: &[ParsedElement],
    now: i64,
) -> AppResult<IngestStats> {
    let tx = conn.transaction()?;
    let mut stats = IngestStats {
        upserted: upsert_elements(&tx, elements, now)?,
        ..Default::default()
    };

    {
        let present: HashSet<(String, i64)> = elements
            .iter()
            .map(|e| (e.osm_type.clone(), e.osm_id))
            .collect();
        let mut select = tx.prepare_cached(
            "SELECT osm_type, osm_id FROM cameras
             WHERE lat >= ?1 AND lat <= ?2 AND lon >= ?3 AND lon <= ?4 AND stale_since IS NULL",
        )?;
        let cached: Vec<(String, i64)> = select
            .query_map(params![area.south, area.north, area.west, area.east], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        let mut mark = tx.prepare_cached(
            "UPDATE cameras SET stale_since = ?3 WHERE osm_type = ?1 AND osm_id = ?2 AND stale_since IS NULL",
        )?;
        for key in cached {
            if !present.contains(&key) {
                mark.execute(params![key.0, key.1, now])?;
                stats.marked_stale += 1;
            }
        }
    }

    {
        let mut stamp = tx.prepare_cached(
            "INSERT INTO grid_cells(cell_key, fetched_at, element_count) VALUES (?1, ?2, ?3)
             ON CONFLICT(cell_key) DO UPDATE SET fetched_at = excluded.fetched_at, element_count = excluded.element_count",
        )?;
        for cell in cells {
            let bbox = cell.bbox();
            let count = elements.iter().filter(|e| bbox.contains(e.lat, e.lon)).count();
            stamp.execute(params![cell.key(), now, count as i64])?;
            stats.cells_marked += 1;
        }
    }

    tx.commit()?;
    Ok(stats)
}

/// Delete cameras that have been stale for longer than the retention window.
pub fn purge_stale(conn: &Connection, now: i64) -> AppResult<usize> {
    let n = conn.execute(
        "DELETE FROM cameras WHERE stale_since IS NOT NULL AND stale_since < ?1",
        params![now - STALE_RETENTION_SECS],
    )?;
    Ok(n)
}

/// Drop all cache stamps so the next viewport change re-fetches (used by "clear cache").
pub fn clear_cell_cache(conn: &Connection) -> AppResult<()> {
    conn.execute("DELETE FROM grid_cells", [])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Where the worldwide camera sync comes from: `snapshot` (the published daily
    /// snapshot, falling back to Overpass when there is none) or `overpass` (always query).
    pub sync_source: String,
    /// Snapshot manifest URL. Empty means the one derived from the crate's repository.
    pub snapshot_url: String,
    pub overpass_endpoint: String,
    /// 1–30 days.
    pub cache_ttl_days: u32,
    /// MapLibre style URL. Empty string means "not configured".
    pub style_url: String,
    /// 0 means "manual only"; otherwise 6–168.
    pub refresh_interval_hours: u32,
    pub osm_client_id: String,
    pub notifications_enabled: bool,
    pub first_run_done: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            sync_source: "snapshot".to_string(),
            snapshot_url: String::new(),
            overpass_endpoint: crate::overpass::DEFAULT_ENDPOINT.to_string(),
            cache_ttl_days: 7,
            style_url: "https://tiles.openfreemap.org/styles/dark".to_string(),
            refresh_interval_hours: 24,
            osm_client_id: String::new(),
            notifications_enabled: true,
            first_run_done: false,
        }
    }
}

impl Settings {
    pub fn validated(mut self) -> Self {
        self.cache_ttl_days = self.cache_ttl_days.clamp(1, 30);
        if self.refresh_interval_hours != 0 {
            self.refresh_interval_hours = self.refresh_interval_hours.clamp(6, 168);
        }
        if self.overpass_endpoint.trim().is_empty() {
            self.overpass_endpoint = crate::overpass::DEFAULT_ENDPOINT.to_string();
        }
        self.overpass_endpoint = self.overpass_endpoint.trim().to_string();
        if self.sync_source != "overpass" {
            self.sync_source = "snapshot".to_string();
        }
        self.snapshot_url = self.snapshot_url.trim().to_string();
        self.style_url = self.style_url.trim().to_string();
        self.osm_client_id = self.osm_client_id.trim().to_string();
        self
    }

    pub fn ttl_secs(&self) -> i64 {
        self.cache_ttl_days as i64 * 24 * 3600
    }
}

pub fn get_setting(conn: &Connection, key: &str) -> AppResult<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0))
        .optional()?)
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> AppResult<()> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

const SETTINGS_KEY: &str = "app_settings";

pub fn load_settings(conn: &Connection) -> AppResult<Settings> {
    match get_setting(conn, SETTINGS_KEY)? {
        Some(json) => Ok(serde_json::from_str::<Settings>(&json)
            .unwrap_or_default()
            .validated()),
        None => Ok(Settings::default()),
    }
}

pub fn save_settings(conn: &Connection, settings: &Settings) -> AppResult<Settings> {
    let validated = settings.clone().validated();
    set_setting(conn, SETTINGS_KEY, &serde_json::to_string(&validated)?)?;
    Ok(validated)
}

pub fn get_json<T: serde::de::DeserializeOwned>(conn: &Connection, key: &str) -> AppResult<Option<T>> {
    match get_setting(conn, key)? {
        Some(s) => Ok(serde_json::from_str(&s).ok()),
        None => Ok(None),
    }
}

pub fn set_json<T: Serialize>(conn: &Connection, key: &str, value: &T) -> AppResult<()> {
    set_setting(conn, key, &serde_json::to_string(value)?)
}

#[cfg(test)]
pub(crate) fn test_conn() -> Connection {
    let mut c = open_in_memory().unwrap();
    migrate(&mut c).unwrap();
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::cells_for_bbox;

    fn element(id: i64, lat: f64, lon: f64, flock: bool) -> ParsedElement {
        let mut tags = BTreeMap::new();
        tags.insert("man_made".into(), "surveillance".into());
        tags.insert("surveillance:type".into(), "ALPR".into());
        if flock {
            tags.insert("brand".into(), "Flock Safety".into());
        }
        ParsedElement {
            osm_type: "node".into(),
            osm_id: id,
            lat,
            lon,
            tags,
        }
    }

    #[test]
    fn migrations_apply_once() {
        let mut c = open_in_memory().unwrap();
        let first = migrate(&mut c).unwrap();
        assert_eq!(first, vec![1, 2, 3, 4, 5, 6]);
        let second = migrate(&mut c).unwrap();
        assert!(second.is_empty());
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 6);
    }

    #[test]
    fn global_ingest_marks_everything_absent_as_stale() {
        let mut c = test_conn();
        // A camera left over from the old per-viewport cache, somewhere else entirely.
        let area = BBox::new(39.70, -105.00, 39.75, -104.95);
        ingest_fetch(&mut c, &area, &cells_for_bbox(&area), &[element(1, 39.72, -104.97, true)], 50).unwrap();

        let s = ingest_global(&mut c, &[element(2, 51.5, -0.1, false), element(3, -33.9, 151.2, true)], 100, 110).unwrap();
        assert_eq!(s.upserted, 2);
        assert_eq!(s.marked_stale, 1);
        assert_eq!(camera_get(&c, "node", 1).unwrap().unwrap().stale_since, Some(110));
        assert!(camera_get(&c, "node", 2).unwrap().unwrap().stale_since.is_none());

        // The next sync brings camera 1 back and drops camera 3.
        let s = ingest_global(&mut c, &[element(1, 39.72, -104.97, true), element(2, 51.5, -0.1, false)], 200, 210).unwrap();
        assert_eq!(s.marked_stale, 1);
        assert!(camera_get(&c, "node", 1).unwrap().unwrap().stale_since.is_none());
        assert_eq!(camera_get(&c, "node", 3).unwrap().unwrap().stale_since, Some(210));

        let summary = sync_summary(&c).unwrap();
        assert_eq!((summary.cameras, summary.stale_cameras), (2, 1));
    }

    #[test]
    fn global_ingest_leaves_cameras_written_after_the_sync_started() {
        let mut c = test_conn();
        // An alert refresh wrote camera 9 at t=150, while a sync that started at t=100 was
        // still downloading a snapshot taken before camera 9 existed.
        let area = BBox::new(39.70, -105.00, 39.75, -104.95);
        ingest_fetch(&mut c, &area, &cells_for_bbox(&area), &[element(9, 39.72, -104.97, true)], 150).unwrap();
        let s = ingest_global(&mut c, &[element(2, 51.5, -0.1, false)], 100, 160).unwrap();
        assert_eq!(s.marked_stale, 0);
        assert!(camera_get(&c, "node", 9).unwrap().unwrap().stale_since.is_none());
    }

    #[test]
    fn global_ingest_refuses_an_implausibly_small_response() {
        let mut c = test_conn();
        let many: Vec<ParsedElement> = (0..2000).map(|i| element(i, 10.0 + i as f64 * 0.001, 10.0, false)).collect();
        ingest_global(&mut c, &many, 1, 1).unwrap();
        let err = ingest_global(&mut c, &many[..500], 2, 2).unwrap_err();
        assert_eq!(err.kind(), "parse");
        assert!(err.to_string().contains("not applied"));
        let summary = sync_summary(&c).unwrap();
        assert_eq!(summary.stale_cameras, 0, "nothing was marked stale");
        // A modest shrink (cameras genuinely deleted from OSM) still applies.
        assert!(ingest_global(&mut c, &many[..1500], 3, 3).is_ok());
    }

    #[test]
    fn sync_summary_tracks_success_failure_and_interruption() {
        let c = test_conn();
        assert!(sync_summary(&c).unwrap().last_attempt_at.is_none());
        let a = sync_run_start(&c, 10).unwrap();
        sync_run_finish(&c, a, "ok", None, Some(151_000), Some(3), Some(56_000_000)).unwrap();
        let s = sync_summary(&c).unwrap();
        assert!(s.last_ok_at.is_some());
        assert_eq!(s.last_outcome.as_deref(), Some("ok"));
        assert_eq!(s.last_elements, Some(151_000));

        let b = sync_run_start(&c, s.last_ok_at.unwrap() + 1).unwrap();
        sync_run_finish(&c, b, "offline", Some("network unavailable"), None, None, None).unwrap();
        let s = sync_summary(&c).unwrap();
        assert_eq!(s.last_outcome.as_deref(), Some("offline"));
        assert_eq!(s.last_error.as_deref(), Some("network unavailable"));
        assert_eq!(s.last_elements, Some(151_000), "size comes from the last success");

        sync_run_start(&c, 10_000_000_000).unwrap();
        assert_eq!(sync_runs_mark_interrupted(&c).unwrap(), 1);
        let s = sync_summary(&c).unwrap();
        assert_eq!(s.last_outcome.as_deref(), Some("error"));
        assert!(s.last_error.unwrap().contains("interrupted"));
    }

    #[test]
    fn migration_3_moves_old_default_basemap_only() {
        let c = test_conn();
        // Simulate a pre-0003 install on the old default, then re-run the migration body.
        let old = Settings {
            style_url: "https://tiles.openfreemap.org/styles/liberty".into(),
            ..Settings::default()
        };
        set_setting(&c, SETTINGS_KEY, &serde_json::to_string(&old).unwrap()).unwrap();
        c.execute_batch(MIGRATIONS[2].1).unwrap();
        assert_eq!(load_settings(&c).unwrap().style_url, "https://tiles.openfreemap.org/styles/dark");
        // A custom URL is left alone.
        let custom = Settings {
            style_url: "https://example.org/my-style.json".into(),
            ..Settings::default()
        };
        save_settings(&c, &custom).unwrap();
        c.execute_batch(MIGRATIONS[2].1).unwrap();
        assert_eq!(load_settings(&c).unwrap().style_url, "https://example.org/my-style.json");
    }

    #[test]
    fn ingest_upserts_marks_stale_and_stamps_cells() {
        let mut c = test_conn();
        let area = BBox::new(39.70, -105.00, 39.75, -104.95);
        let cells = cells_for_bbox(&area);
        assert_eq!(cells.len(), 1);

        let t0 = 1_000;
        let stats = ingest_fetch(
            &mut c,
            &area,
            &cells,
            &[element(1, 39.72, -104.97, true), element(2, 39.73, -104.96, false)],
            t0,
        )
        .unwrap();
        assert_eq!(stats.upserted, 2);
        assert_eq!(stats.marked_stale, 0);
        assert_eq!(stats.cells_marked, 1);
        assert_eq!(cell_fetched_at(&c, &cells[0]).unwrap(), Some(t0));

        let cams = cameras_in_bbox(&c, &area).unwrap();
        assert_eq!(cams.len(), 2);
        let flock = cams.iter().find(|c| c.osm_id == 1).unwrap();
        assert_eq!(flock.category, "flock");
        assert_eq!(flock.first_seen, t0);

        // Second fetch: element 2 vanished, element 1 still present (moved slightly).
        let t1 = 2_000;
        let stats = ingest_fetch(&mut c, &area, &cells, &[element(1, 39.721, -104.97, true)], t1).unwrap();
        assert_eq!(stats.marked_stale, 1);
        let cams = cameras_in_bbox(&c, &area).unwrap();
        let one = cams.iter().find(|c| c.osm_id == 1).unwrap();
        assert_eq!(one.first_seen, t0);
        assert_eq!(one.last_seen, t1);
        assert!(one.stale_since.is_none());
        let two = cams.iter().find(|c| c.osm_id == 2).unwrap();
        assert_eq!(two.stale_since, Some(t1));

        // A later fetch that has element 2 again clears the stale flag.
        let t2 = 3_000;
        ingest_fetch(
            &mut c,
            &area,
            &cells,
            &[element(1, 39.721, -104.97, true), element(2, 39.73, -104.96, false)],
            t2,
        )
        .unwrap();
        let two = camera_get(&c, "node", 2).unwrap().unwrap();
        assert!(two.stale_since.is_none());
    }

    #[test]
    fn stale_cells_respect_ttl() {
        let mut c = test_conn();
        let area = BBox::new(39.70, -105.00, 39.75, -104.95);
        let cells = cells_for_bbox(&area);
        assert_eq!(stale_cells(&c, &cells, 3600, 5_000).unwrap().len(), 1);
        ingest_fetch(&mut c, &area, &cells, &[], 5_000).unwrap();
        assert!(stale_cells(&c, &cells, 3600, 5_100).unwrap().is_empty());
        assert_eq!(stale_cells(&c, &cells, 3600, 9_000).unwrap().len(), 1);
    }

    #[test]
    fn purge_removes_only_long_stale_cameras() {
        let mut c = test_conn();
        let area = BBox::new(39.70, -105.00, 39.75, -104.95);
        let cells = cells_for_bbox(&area);
        ingest_fetch(&mut c, &area, &cells, &[element(1, 39.72, -104.97, false)], 100).unwrap();
        ingest_fetch(&mut c, &area, &cells, &[], 200).unwrap();
        assert_eq!(purge_stale(&c, 200 + STALE_RETENTION_SECS - 1).unwrap(), 0);
        assert_eq!(purge_stale(&c, 200 + STALE_RETENTION_SECS + 1).unwrap(), 1);
        assert_eq!(camera_count(&c).unwrap(), 0);
    }

    #[test]
    fn settings_round_trip_with_validation() {
        let c = test_conn();
        let loaded = load_settings(&c).unwrap();
        assert_eq!(loaded, Settings::default());
        let saved = save_settings(
            &c,
            &Settings {
                cache_ttl_days: 99,
                refresh_interval_hours: 1,
                ..Settings::default()
            },
        )
        .unwrap();
        assert_eq!(saved.cache_ttl_days, 30);
        assert_eq!(saved.refresh_interval_hours, 6);
        assert_eq!(load_settings(&c).unwrap(), saved);
    }

    #[test]
    fn cameras_in_antimeridian_bbox() {
        let mut c = test_conn();
        let east = BBox::new(-17.0, 179.5, -16.0, 180.0);
        let west = BBox::new(-17.0, -180.0, -16.0, -179.5);
        ingest_fetch(&mut c, &east, &cells_for_bbox(&east), &[element(1, -16.5, 179.9, false)], 1).unwrap();
        ingest_fetch(&mut c, &west, &cells_for_bbox(&west), &[element(2, -16.5, -179.9, false)], 1).unwrap();
        let both = cameras_in_bbox(&c, &BBox::new(-17.0, 179.5, -16.0, -179.5)).unwrap();
        assert_eq!(both.len(), 2);
    }
}
