//! The drivable road network around a trip, used to decide whether a camera-free route exists
//! and to find it.
//!
//! The routing server can only be told about 50 excluded cameras per request, and a dense
//! metro corridor holds thousands, so asking it again and again (routing.rs, phase 1) can run out
//! before it finds a camera-free route that does exist. This module downloads the road network
//! for the trip area from Overpass, removes every road segment within the avoid radius of a
//! camera, and searches what is left. The route it finds is then handed back to the routing
//! server, leg by leg, for real directions and times (routing.rs, phase 2).
//!
//! Roads are fetched in 0.1° tiles and cached in SQLite (compressed) for [`TILE_TTL_SECS`], so a
//! repeat trip through the same area downloads nothing.

use crate::error::{AppError, AppResult};
use crate::geo_util::haversine_m;
use crate::grid::BBox;
use crate::http::HttpClient;
use crate::routing::AvoidCamera;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::io::{Read, Write};
use std::time::Duration;

/// Tile edge, degrees (about 11 km of latitude).
pub const TILE_DEG: f64 = 0.1;
/// Cached tiles older than this are fetched again.
pub const TILE_TTL_SECS: i64 = 30 * 24 * 3600;
/// Cached tiles beyond this many (least recently fetched first) are dropped.
pub const MAX_CACHED_TILES: i64 = 120;
/// One Overpass query per tile (a 0.1° tile of dense Atlanta is about 5 MB of JSON; a 0.2°
/// block, 21 MB, took over three minutes on a busy server), this many at a time. Overpass gives
/// each client at least two query slots.
const PARALLEL_DOWNLOADS: usize = 2;
/// Server-side timeout for one block query, seconds.
const QUERY_TIMEOUT_SECS: u64 = 180;

/// Road classes a car may use. Service roads (parking aisles, driveways) are left out: a
/// "camera-free" detour through a parking lot is not directions anyone wants.
const HIGHWAY_CLASSES: [&str; 13] = [
    "motorway",
    "trunk",
    "primary",
    "secondary",
    "tertiary",
    "unclassified",
    "residential",
    "living_street",
    "motorway_link",
    "trunk_link",
    "primary_link",
    "secondary_link",
    "tertiary_link",
];

/// Assumed speed when a road has no usable `maxspeed`, km/h.
fn default_kmh(class: &str) -> u8 {
    match class {
        "motorway" => 100,
        "trunk" => 80,
        "primary" => 60,
        "secondary" => 55,
        "tertiary" => 50,
        "unclassified" => 45,
        "residential" => 40,
        "living_street" => 20,
        "motorway_link" => 60,
        "trunk_link" => 50,
        _ => 45,
    }
}

/// One drivable OSM way.
#[derive(Debug, Clone, PartialEq)]
pub struct Way {
    pub id: i64,
    /// 1 = only in drawing order, -1 = only against it, 0 = both ways.
    pub oneway: i8,
    pub kmh: u8,
    /// (lat, lon) in 1e-7 degrees, as Overpass reports them. Ways meet where they share a point.
    pub pts: Vec<(i32, i32)>,
}

impl Way {
    pub(crate) fn bbox(&self) -> BBox {
        let (mut s, mut w, mut n, mut e) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for &(la, lo) in &self.pts {
            s = s.min(la);
            n = n.max(la);
            w = w.min(lo);
            e = e.max(lo);
        }
        BBox::new(s as f64 / 1e7, w as f64 / 1e7, n as f64 / 1e7, e as f64 / 1e7)
    }
}

pub(crate) fn intersects(a: &BBox, b: &BBox) -> bool {
    a.south <= b.north && b.south <= a.north && a.west <= b.east && b.west <= a.east
}

// ---------------------------------------------------------------------------
// Overpass
// ---------------------------------------------------------------------------

pub fn build_query(bbox: &BBox) -> String {
    format!(
        "[out:json][timeout:{QUERY_TIMEOUT_SECS}];way[\"highway\"~\"^({})$\"]({:.6},{:.6},{:.6},{:.6});out geom qt;",
        HIGHWAY_CLASSES.join("|"),
        bbox.south,
        bbox.west,
        bbox.north,
        bbox.east
    )
}

#[derive(Deserialize)]
struct OverpassBody {
    #[serde(default)]
    elements: Vec<OverpassWay>,
    #[serde(default)]
    remark: Option<String>,
}

#[derive(Deserialize)]
struct OverpassWay {
    #[serde(rename = "type")]
    kind: String,
    id: i64,
    #[serde(default)]
    tags: HashMap<String, String>,
    #[serde(default)]
    geometry: Vec<Option<OverpassPoint>>,
}

#[derive(Deserialize, Clone, Copy)]
struct OverpassPoint {
    lat: f64,
    lon: f64,
}

/// `maxspeed` in km/h: "50", "35 mph", "50;60" (first value). Anything else (e.g. "signals",
/// "walk", "none") is unusable.
fn parse_maxspeed(v: &str) -> Option<u8> {
    let first = v.split(';').next()?.trim();
    let mph = first.ends_with("mph");
    let n: f64 = first.trim_end_matches("mph").trim().parse().ok()?;
    let kmh = if mph { n * 1.609_344 } else { n };
    (5.0..=150.0).contains(&kmh).then(|| kmh.round() as u8)
}

/// A way a car may drive on, or `None` (not a road class we route on, no access, an area).
pub(crate) fn way_from(id: i64, tags: &HashMap<String, String>, pts: Vec<(i32, i32)>) -> Option<Way> {
    let class = tags.get("highway")?.as_str();
    if !HIGHWAY_CLASSES.contains(&class) || pts.len() < 2 || tags.get("area").is_some_and(|v| v == "yes") {
        return None;
    }
    let tag = |k: &str| tags.get(k).map(String::as_str).unwrap_or("");
    if matches!(tag("access"), "private" | "no" | "agricultural" | "forestry" | "delivery")
        || matches!(tag("motor_vehicle"), "private" | "no")
        || matches!(tag("motorcar"), "private" | "no")
    {
        return None;
    }
    // Reversible lanes (express lanes whose direction flips by time of day) and alternating
    // one-lane stretches: the routing server never routes on them, so neither may the map.
    if matches!(tag("oneway"), "reversible" | "alternating") {
        return None;
    }
    let oneway = match tag("oneway") {
        "yes" | "1" | "true" => 1,
        "-1" | "reverse" => -1,
        "no" | "0" | "false" => 0,
        _ if class == "motorway" || tag("junction") == "roundabout" || tag("junction") == "circular" => 1,
        _ => 0,
    };
    let kmh = parse_maxspeed(tag("maxspeed")).unwrap_or_else(|| default_kmh(class));
    Some(Way { id, oneway, kmh, pts })
}

/// Parse an Overpass `out geom` answer into drivable ways.
pub fn parse_ways(body: &str) -> AppResult<Vec<Way>> {
    let parsed: OverpassBody = serde_json::from_str(body)
        .map_err(|e| AppError::Parse(format!("Overpass road data is not valid JSON: {e}")))?;
    if let Some(remark) = parsed.remark.as_deref() {
        // Timeouts and memory exhaustion come back as HTTP 200 with partial data.
        if remark.to_lowercase().contains("runtime error") {
            return Err(AppError::Parse(format!("Overpass reported: {remark}")));
        }
    }
    Ok(parsed
        .elements
        .into_iter()
        .filter(|e| e.kind == "way")
        .filter_map(|e| {
            let pts: Vec<(i32, i32)> = e
                .geometry
                .iter()
                .flatten()
                .map(|p| ((p.lat * 1e7).round() as i32, (p.lon * 1e7).round() as i32))
                .collect();
            way_from(e.id, &e.tags, pts)
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Compact storage (cache and test fixtures)
// ---------------------------------------------------------------------------

const MAGIC: &[u8; 4] = b"FFRN";
const FORMAT: u8 = 1;

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> AppResult<u8> {
        let v = *self.b.get(self.i).ok_or_else(|| AppError::Parse("road data is truncated".into()))?;
        self.i += 1;
        Ok(v)
    }

    fn varint(&mut self) -> AppResult<u64> {
        let (mut v, mut shift) = (0u64, 0u32);
        loop {
            let b = self.byte()?;
            if shift > 63 {
                return Err(AppError::Parse("road data is malformed".into()));
            }
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(v);
            }
            shift += 7;
        }
    }
}

/// Ways as gzip-compressed, delta-encoded varints (about 3 bytes a point before compression).
pub fn encode(ways: &[Way]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(ways.len() * 32);
    raw.extend_from_slice(MAGIC);
    raw.push(FORMAT);
    put_varint(&mut raw, ways.len() as u64);
    for w in ways {
        put_varint(&mut raw, zigzag(w.id));
        raw.push((w.oneway + 1) as u8);
        raw.push(w.kmh);
        put_varint(&mut raw, w.pts.len() as u64);
        let (mut pla, mut plo) = (0i64, 0i64);
        for &(la, lo) in &w.pts {
            put_varint(&mut raw, zigzag(la as i64 - pla));
            put_varint(&mut raw, zigzag(lo as i64 - plo));
            pla = la as i64;
            plo = lo as i64;
        }
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    // Writing to a Vec cannot fail.
    let _ = gz.write_all(&raw);
    gz.finish().unwrap_or_default()
}

pub fn decode(gz: &[u8]) -> AppResult<Vec<Way>> {
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_end(&mut raw)
        .map_err(|e| AppError::Parse(format!("road data is not valid gzip: {e}")))?;
    if raw.len() < 5 || &raw[..4] != MAGIC || raw[4] != FORMAT {
        return Err(AppError::Parse("road data has an unknown format".into()));
    }
    let mut r = Reader { b: &raw, i: 5 };
    let n = r.varint()? as usize;
    let mut ways = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let id = unzigzag(r.varint()?);
        let oneway = r.byte()? as i8 - 1;
        let kmh = r.byte()?;
        let len = r.varint()? as usize;
        let mut pts = Vec::with_capacity(len.min(1 << 16));
        let (mut la, mut lo) = (0i64, 0i64);
        for _ in 0..len {
            la += unzigzag(r.varint()?);
            lo += unzigzag(r.varint()?);
            pts.push((la as i32, lo as i32));
        }
        ways.push(Way { id, oneway, kmh, pts });
    }
    Ok(ways)
}

// ---------------------------------------------------------------------------
// Tiles and cache
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Tile {
    pub row: i32,
    pub col: i32,
}

impl Tile {
    pub fn key(&self) -> String {
        format!("{}:{}", self.row, self.col)
    }

    pub fn bbox(&self) -> BBox {
        BBox::new(
            self.row as f64 * TILE_DEG,
            self.col as f64 * TILE_DEG,
            (self.row + 1) as f64 * TILE_DEG,
            (self.col + 1) as f64 * TILE_DEG,
        )
    }
}

pub fn tiles_for(bbox: &BBox) -> Vec<Tile> {
    let r0 = (bbox.south / TILE_DEG).floor() as i32;
    let r1 = (bbox.north / TILE_DEG).floor() as i32;
    let c0 = (bbox.west / TILE_DEG).floor() as i32;
    let c1 = (bbox.east / TILE_DEG).floor() as i32;
    let mut out = Vec::new();
    for row in r0..=r1 {
        for col in c0..=c1 {
            out.push(Tile { row, col });
        }
    }
    out
}

pub fn cache_get(conn: &Connection, tile: &Tile, now: i64) -> AppResult<Option<Vec<u8>>> {
    Ok(conn
        .query_row(
            "SELECT data FROM road_tiles WHERE tile = ?1 AND fetched_at > ?2",
            params![tile.key(), now - TILE_TTL_SECS],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn cache_put(conn: &Connection, tile: &Tile, data: &[u8], now: i64) -> AppResult<()> {
    conn.execute(
        "INSERT INTO road_tiles(tile, fetched_at, data) VALUES (?1, ?2, ?3)
         ON CONFLICT(tile) DO UPDATE SET fetched_at = excluded.fetched_at, data = excluded.data",
        params![tile.key(), now, data],
    )?;
    // Expired tiles, then the oldest beyond the cap.
    conn.execute("DELETE FROM road_tiles WHERE fetched_at <= ?1", params![now - TILE_TTL_SECS])?;
    conn.execute(
        "DELETE FROM road_tiles WHERE tile NOT IN
           (SELECT tile FROM road_tiles ORDER BY fetched_at DESC LIMIT ?1)",
        params![MAX_CACHED_TILES],
    )?;
    Ok(())
}

pub fn cache_clear(conn: &Connection) -> AppResult<usize> {
    Ok(conn.execute("DELETE FROM road_tiles", [])?)
}

/// What loading the roads for a trip took.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Loaded {
    pub ways: Vec<Way>,
    pub tiles: usize,
    /// Tiles downloaded now (the rest came from the cache).
    pub downloaded: usize,
    pub bytes: u64,
    /// Some tiles couldn't be downloaded (Overpass busy or unreachable). A route found on a
    /// partial map is real; "no route" on one proves nothing.
    pub partial: bool,
    /// Tiles that came from the cache.
    pub cached: usize,
}

/// Overpass limits how often one client may run queries: after a heavy query its slot is
/// blocked for a while, and further queries get HTTP 429 until it frees. Waiting times come
/// from `/api/status` ("Slot available after: …, in 33 seconds."). HTTP 504 means the server
/// as a whole is overloaded (its status page still shows free slots), so that waits longer each
/// time instead.
const MAX_SLOT_WAITS: usize = 4;
const MAX_SLOT_WAIT_SECS: u64 = 60;
const OVERLOAD_WAITS_SECS: [u64; MAX_SLOT_WAITS] = [10, 20, 40, 60];

/// Seconds until the next query slot, from an Overpass `/api/status` page: 0 when a slot is
/// free, `None` when the page says neither.
pub fn parse_slot_wait(status: &str) -> Option<u64> {
    if status.contains("slots available now") || status.contains("slot available now") {
        return Some(0);
    }
    status
        .lines()
        .filter(|l| l.starts_with("Slot available after"))
        .filter_map(|l| l.rsplit(", in ").next()?.split_whitespace().next()?.parse::<u64>().ok())
        .min()
}

fn status_url(endpoint: &str) -> Option<String> {
    endpoint.strip_suffix("/interpreter").map(|base| format!("{base}/status"))
}

/// The part of the map a trip needs: a box, and the straight line from start to destination
/// with a margin around it. Tiles in the box but farther than the margin from the line (the far
/// corners of a diagonal trip's box) aren't loaded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub bbox: BBox,
    pub start: (f64, f64),
    pub end: (f64, f64),
    pub margin_m: f64,
}

impl Area {
    pub fn tiles(&self) -> Vec<Tile> {
        tiles_for(&self.bbox).into_iter().filter(|t| self.line_dist_m(t) <= self.margin_m).collect()
    }

    /// Distance from the start–destination line to the tile (conservatively: to its centre,
    /// less half its diagonal).
    pub fn line_dist_m(&self, t: &Tile) -> f64 {
        let b = t.bbox();
        let c = ((b.south + b.north) / 2.0, (b.west + b.east) / 2.0);
        let half_diag = haversine_m(b.south, b.west, b.north, b.east) / 2.0;
        (seg_dist_m(c, self.start, self.end) - half_diag).max(0.0)
    }
}

/// Where road data comes from: Overpass with the tile cache in the app, a fixture in tests.
pub(crate) trait RoadSource {
    async fn load(&self, area: &Area, progress: &(dyn Fn(String) + Sync)) -> AppResult<Loaded>;
}

/// Overpass, through the SQLite tile cache.
pub struct OverpassRoads<'a> {
    pub http: &'a HttpClient,
    pub endpoint: String,
    pub db: &'a std::sync::Mutex<Connection>,
    /// Stop downloading after this long (waits included) and search what arrived; the rest
    /// stays for next time. `None`: no limit (recording test fixtures).
    pub budget: Option<Duration>,
}

/// Time a trip may spend downloading the road map in the app.
pub const LOAD_BUDGET: Duration = Duration::from_secs(180);

impl OverpassRoads<'_> {
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// One block query. When Overpass says it's busy (429), wait for a free slot as its
    /// status page reports; when it's overloaded (504, or the query timed out), wait longer each
    /// time. A few tries, never past `deadline`.
    async fn fetch_block(
        &self,
        query: &str,
        deadline: Option<std::time::Instant>,
        progress: &(dyn Fn(String) + Sync),
    ) -> AppResult<String> {
        for attempt in 0..=MAX_SLOT_WAITS {
            // A query in flight when the budget runs out is abandoned, not waited for.
            let mut timeout = Duration::from_secs(QUERY_TIMEOUT_SECS + 30);
            if let Some(d) = deadline {
                let left = d.saturating_duration_since(std::time::Instant::now());
                if left < Duration::from_secs(5) {
                    break;
                }
                timeout = timeout.min(left);
            }
            let sent = self
                .http
                .client
                .post(&self.endpoint)
                .timeout(timeout)
                .form(&[("data", query)])
                .send()
                .await;
            let status = match sent {
                Ok(resp) if resp.status().is_success() => match resp.text().await {
                    Ok(body) => return Ok(body),
                    // The body stopped arriving: the same as a timeout.
                    Err(e) if e.is_timeout() || e.is_body() || e.is_decode() => 504,
                    Err(e) => return Err(e.into()),
                },
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if !crate::http::should_retry(status) {
                        let body: String = resp.text().await.unwrap_or_default().chars().take(400).collect();
                        return Err(AppError::Http { status, endpoint: "Overpass".into(), body });
                    }
                    status
                }
                // A query that ran out of time is Overpass overloaded, not us offline.
                Err(e) if e.is_timeout() => 504,
                Err(e) => return Err(e.into()),
            };
            if attempt == MAX_SLOT_WAITS {
                break;
            }
            let wait = if status == 429 {
                let reported = match status_url(&self.endpoint) {
                    Some(url) => match self.http.client.get(url).timeout(Duration::from_secs(15)).send().await {
                        Ok(r) => parse_slot_wait(&r.text().await.unwrap_or_default()),
                        Err(_) => None,
                    },
                    None => None,
                };
                reported.unwrap_or(crate::http::BACKOFF_SECS[attempt.min(2)]).clamp(2, MAX_SLOT_WAIT_SECS) + 1
            } else {
                OVERLOAD_WAITS_SECS[attempt]
            };
            if deadline.is_some_and(|d| std::time::Instant::now() + Duration::from_secs(wait) >= d) {
                break;
            }
            log::info!("Overpass returned HTTP {status} for a road tile block; waiting {wait} s for a free slot");
            progress(format!("Waiting for the road map server (Overpass is busy; about {wait} s)"));
            tokio::time::sleep(Duration::from_secs(wait)).await;
        }
        Err(AppError::RateLimited("Overpass".into()))
    }
}

impl RoadSource for OverpassRoads<'_> {
    async fn load(&self, area: &Area, progress: &(dyn Fn(String) + Sync)) -> AppResult<Loaded> {
        let now = crate::db::now();
        let tiles = area.tiles();
        let mut by_id: HashMap<i64, Way> = HashMap::new();
        let mut missing: Vec<Tile> = Vec::new();
        for t in &tiles {
            let cached = cache_get(&self.conn(), t, now)?;
            match cached.map(|d| decode(&d)) {
                Some(Ok(ways)) => by_id.extend(ways.into_iter().map(|w| (w.id, w))),
                _ => missing.push(*t),
            }
        }
        // The start and destination tiles first (nothing can be searched without them), then
        // nearest the trip's line: if the budget runs out, what arrived is what matters most.
        // (Sorted by tile first so equal distances keep a fixed order.)
        let holds_end = |t: &Tile| [area.start, area.end].iter().any(|p| t.bbox().contains(p.0, p.1));
        missing.sort();
        missing.sort_by(|a, b| {
            holds_end(b).cmp(&holds_end(a)).then(area.line_dist_m(a).total_cmp(&area.line_dist_m(b)))
        });
        let deadline = self.budget.map(|b| std::time::Instant::now() + b);
        let mut bytes = 0u64;
        let mut downloaded = 0usize;
        let mut failed: Option<AppError> = None;
        for batch in missing.chunks(PARALLEL_DOWNLOADS) {
            if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                log::warn!("road map: download budget used up after {downloaded} of {} tiles", missing.len());
                break;
            }
            progress(format!("Downloading the road map ({} of {} areas)", downloaded + batch.len(), missing.len()));
            let queries: Vec<String> = batch.iter().map(|t| build_query(&t.bbox())).collect();
            let answers = match queries.as_slice() {
                [a, b] => {
                    let (x, y) = tokio::join!(self.fetch_block(a, deadline, progress), self.fetch_block(b, deadline, progress));
                    vec![x, y]
                }
                _ => vec![self.fetch_block(&queries[0], deadline, progress).await],
            };
            for (t, answer) in batch.iter().zip(answers) {
                let body = match answer {
                    Ok(b) => b,
                    Err(e) => {
                        log::warn!("road map: tile {} failed: {e}", t.key());
                        failed = Some(e);
                        continue;
                    }
                };
                bytes += body.len() as u64;
                let ways = parse_ways(&body)?;
                log::info!("road map: tile {}: {} ways, {} KB", t.key(), ways.len(), body.len() / 1024);
                cache_put(&self.conn(), t, &encode(&ways), now)?;
                downloaded += 1;
                by_id.extend(ways.into_iter().map(|w| (w.id, w)));
            }
            if failed.is_some() {
                break;
            }
        }
        let mut ways: Vec<Way> = by_id.into_values().filter(|w| intersects(&w.bbox(), &area.bbox)).collect();
        if let Some(e) = failed {
            // Nothing at all to search: report why.
            if ways.is_empty() {
                return Err(e);
            }
        }
        // Deterministic order (graph node numbering, and so the route, must not depend on
        // hash order).
        ways.sort_by_key(|w| w.id);
        Ok(Loaded { ways, tiles: tiles.len(), downloaded, bytes, partial: downloaded < missing.len(), cached: tiles.len() - missing.len() })
    }
}

// ---------------------------------------------------------------------------
// Graph and search
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Edge {
    to: u32,
    secs: f32,
    /// Within the avoid radius of a camera that may be avoided.
    blocked: bool,
    /// The routing server wouldn't drive it (a turn restriction, gate or closure this map
    /// doesn't know about); never used again in this search.
    closed: bool,
}

pub struct Graph {
    /// (lat, lon)
    pub pts: Vec<(f64, f64)>,
    adj: Vec<Vec<Edge>>,
    pub segments: usize,
    pub blocked_segments: usize,
}

/// Planar distance (metres) from `p` to segment a–b, all (lat, lon).
fn seg_dist_m(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let c = p.0.to_radians().cos().max(0.01);
    let xy = |q: (f64, f64)| ((q.1 - p.1) * 111_320.0 * c, (q.0 - p.0) * 111_320.0);
    let (ax, ay) = xy(a);
    let (bx, by) = xy(b);
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (-(ax * dx + ay * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (ax + t * dx).hypot(ay + t * dy)
}

/// Build the graph. Segments within `radius_m` of any camera in `cameras` are marked blocked;
/// cameras whose key is in `exempt` (at the start or destination) block nothing.
pub fn build_graph(ways: &[Way], cameras: &[AvoidCamera], exempt: &HashSet<String>, radius_m: f64) -> Graph {
    // Cameras bucketed on a grid a little larger than the radius.
    const CELL: f64 = 0.002;
    let mut grid: HashMap<(i64, i64), Vec<(f64, f64)>> = HashMap::new();
    for c in cameras.iter().filter(|c| !exempt.contains(&c.key)) {
        grid.entry(((c.lat / CELL).floor() as i64, (c.lon / CELL).floor() as i64)).or_default().push((c.lat, c.lon));
    }
    let near_camera = |a: (f64, f64), b: (f64, f64)| {
        let r0 = (a.0.min(b.0) / CELL).floor() as i64 - 1;
        let r1 = (a.0.max(b.0) / CELL).floor() as i64 + 1;
        let c0 = (a.1.min(b.1) / CELL).floor() as i64 - 1;
        let c1 = (a.1.max(b.1) / CELL).floor() as i64 + 1;
        (r0..=r1).any(|r| (c0..=c1).any(|c| grid.get(&(r, c)).is_some_and(|l| l.iter().any(|&p| seg_dist_m(p, a, b) <= radius_m))))
    };
    let mut index: HashMap<(i32, i32), u32> = HashMap::new();
    let mut pts: Vec<(f64, f64)> = Vec::new();
    let mut adj: Vec<Vec<Edge>> = Vec::new();
    let mut id_of = |p: (i32, i32), pts: &mut Vec<(f64, f64)>, adj: &mut Vec<Vec<Edge>>| {
        *index.entry(p).or_insert_with(|| {
            pts.push((p.0 as f64 / 1e7, p.1 as f64 / 1e7));
            adj.push(Vec::new());
            (pts.len() - 1) as u32
        })
    };
    let (mut segments, mut blocked_segments) = (0, 0);
    for w in ways {
        for pair in w.pts.windows(2) {
            let u = id_of(pair[0], &mut pts, &mut adj);
            let v = id_of(pair[1], &mut pts, &mut adj);
            if u == v {
                continue;
            }
            let (a, b) = (pts[u as usize], pts[v as usize]);
            let secs = (haversine_m(a.0, a.1, b.0, b.1) / (w.kmh as f64 / 3.6)) as f32;
            let blocked = near_camera(a, b);
            segments += 1;
            if blocked {
                blocked_segments += 1;
            }
            if w.oneway >= 0 {
                adj[u as usize].push(Edge { to: v, secs, blocked, closed: false });
            }
            if w.oneway <= 0 {
                adj[v as usize].push(Edge { to: u, secs, blocked, closed: false });
            }
        }
    }
    Graph { pts, adj, segments, blocked_segments }
}

impl Graph {
    /// Nearest node that has a way out (for a start) or in (for a destination), with its
    /// distance in metres.
    pub fn nearest(&self, p: (f64, f64), leaving: bool) -> Option<(u32, f64)> {
        let has_in: Vec<bool> = if leaving {
            Vec::new()
        } else {
            let mut v = vec![false; self.pts.len()];
            for es in &self.adj {
                for e in es {
                    v[e.to as usize] = true;
                }
            }
            v
        };
        self.pts
            .iter()
            .enumerate()
            .filter(|(i, _)| if leaving { !self.adj[*i].is_empty() } else { has_in[*i] })
            .map(|(i, q)| (i as u32, haversine_m(p.0, p.1, q.0, q.1)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Quickest path from `s` to `t`. A blocked segment costs `penalty_secs` extra; with
    /// `None` blocked segments are not used at all.
    pub fn path(&self, s: u32, t: u32, penalty_secs: Option<f64>) -> Option<GraphPath> {
        #[derive(PartialEq)]
        struct Item(f64, u32);
        impl Eq for Item {}
        impl Ord for Item {
            fn cmp(&self, o: &Self) -> Ordering {
                o.0.total_cmp(&self.0).then_with(|| o.1.cmp(&self.1))
            }
        }
        impl PartialOrd for Item {
            fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
                Some(self.cmp(o))
            }
        }
        let n = self.pts.len();
        let mut dist = vec![f64::INFINITY; n];
        let mut prev = vec![u32::MAX; n];
        let mut heap = BinaryHeap::new();
        dist[s as usize] = 0.0;
        heap.push(Item(0.0, s));
        while let Some(Item(d, u)) = heap.pop() {
            if u == t {
                break;
            }
            if d > dist[u as usize] {
                continue;
            }
            for e in self.adj[u as usize].iter().filter(|e| !e.closed) {
                let extra = match (e.blocked, penalty_secs) {
                    (false, _) => 0.0,
                    (true, Some(p)) => p,
                    (true, None) => continue,
                };
                let nd = d + e.secs as f64 + extra;
                if nd < dist[e.to as usize] {
                    dist[e.to as usize] = nd;
                    prev[e.to as usize] = u;
                    heap.push(Item(nd, e.to));
                }
            }
        }
        if !dist[t as usize].is_finite() {
            return None;
        }
        let mut nodes = vec![t];
        while *nodes.last()? != s {
            let p = prev[*nodes.last()? as usize];
            if p == u32::MAX {
                return None;
            }
            nodes.push(p);
        }
        nodes.reverse();
        let (mut secs, mut blocked) = (0.0, 0usize);
        for w in nodes.windows(2) {
            if let Some(e) = self.adj[w[0] as usize].iter().filter(|e| e.to == w[1] && !e.closed).min_by(|a, b| a.secs.total_cmp(&b.secs)) {
                secs += e.secs as f64;
                blocked += usize::from(e.blocked);
            }
        }
        Some(GraphPath { pts: nodes.iter().map(|&i| self.pts[i as usize]).collect(), nodes, secs, blocked_segments: blocked })
    }

    /// Close the road between two nodes, both ways.
    pub fn close(&mut self, u: u32, v: u32) {
        for (a, b) in [(u, v), (v, u)] {
            for e in self.adj[a as usize].iter_mut().filter(|e| e.to == b) {
                e.closed = true;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphPath {
    /// (lat, lon)
    pub pts: Vec<(f64, f64)>,
    /// Graph node of each point.
    pub nodes: Vec<u32>,
    /// Estimated driving time from road classes and speed limits (rough; the routing server's
    /// time is the one shown).
    pub secs: f64,
    pub blocked_segments: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam(key: &str, lat: f64, lon: f64) -> AvoidCamera {
        AvoidCamera {
            key: key.into(),
            lat,
            lon,
            category: "alpr".into(),
            source: "osm".into(),
            direction: None,
            operator: None,
        }
    }

    fn e7(lat: f64, lon: f64) -> (i32, i32) {
        ((lat * 1e7).round() as i32, (lon * 1e7).round() as i32)
    }

    fn way(id: i64, oneway: i8, pts: &[(f64, f64)]) -> Way {
        Way { id, oneway, kmh: 50, pts: pts.iter().map(|&(a, b)| e7(a, b)).collect() }
    }

    #[test]
    fn parses_drivable_ways_only() {
        let body = r#"{"elements":[
          {"type":"way","id":1,"tags":{"highway":"residential"},"geometry":[{"lat":39.0,"lon":-105.0},{"lat":39.001,"lon":-105.0}]},
          {"type":"way","id":2,"tags":{"highway":"residential","access":"private"},"geometry":[{"lat":39.0,"lon":-105.0},{"lat":39.001,"lon":-105.0}]},
          {"type":"way","id":3,"tags":{"highway":"footway"},"geometry":[{"lat":39.0,"lon":-105.0},{"lat":39.001,"lon":-105.0}]},
          {"type":"way","id":4,"tags":{"highway":"primary","oneway":"-1","maxspeed":"35 mph"},"geometry":[{"lat":39.0,"lon":-105.0},null,{"lat":39.001,"lon":-105.0}]},
          {"type":"way","id":5,"tags":{"highway":"motorway"},"geometry":[{"lat":39.0,"lon":-105.0},{"lat":39.001,"lon":-105.0}]},
          {"type":"way","id":6,"tags":{"highway":"tertiary","junction":"roundabout","oneway":"no"},"geometry":[{"lat":39.0,"lon":-105.0},{"lat":39.001,"lon":-105.0}]},
          {"type":"way","id":7,"tags":{"highway":"residential"},"geometry":[{"lat":39.0,"lon":-105.0}]},
          {"type":"way","id":8,"tags":{"highway":"motorway_link","oneway":"reversible"},"geometry":[{"lat":39.0,"lon":-105.0},{"lat":39.001,"lon":-105.0}]}
        ]}"#;
        let ways = parse_ways(body).unwrap();
        let ids: Vec<i64> = ways.iter().map(|w| w.id).collect();
        assert_eq!(ids, [1, 4, 5, 6], "private, footway, one-point and reversible ways dropped");
        assert_eq!(ways[1].oneway, -1);
        assert_eq!(ways[1].kmh, 56, "35 mph");
        assert_eq!(ways[1].pts.len(), 2, "null geometry points skipped");
        assert_eq!(ways[2].oneway, 1, "motorways are one-way");
        assert_eq!(ways[3].oneway, 0, "an explicit oneway=no wins over the roundabout default");
        assert!(parse_ways(r#"{"elements":[],"remark":"runtime error: Query timed out"}"#).is_err());
        assert!(parse_ways("<html>").is_err());
    }

    #[test]
    fn maxspeed_values() {
        assert_eq!(parse_maxspeed("50"), Some(50));
        assert_eq!(parse_maxspeed("25 mph"), Some(40));
        assert_eq!(parse_maxspeed("50;60"), Some(50));
        assert_eq!(parse_maxspeed("signals"), None);
        assert_eq!(parse_maxspeed("0"), None);
    }

    #[test]
    fn encoding_round_trips() {
        let ways = vec![
            way(123_456_789_012, 1, &[(33.8480001, -84.3730002), (33.849, -84.372), (33.85, -84.371)]),
            way(-5, -1, &[(0.0, 0.0), (-0.0000001, 179.9999999)]),
        ];
        let data = encode(&ways);
        assert_eq!(decode(&data).unwrap(), ways);
        assert!(decode(b"not gzip").is_err());
        let mut truncated = encode(&ways);
        truncated.truncate(truncated.len() / 2);
        assert!(decode(&truncated).is_err());
    }

    #[test]
    fn reads_overpass_slot_waits() {
        let busy = "Connected as: 123\nCurrent time: 2026-09-25T13:50:00Z\nRate limit: 2\nSlot available after: 2026-09-25T13:50:33Z, in 33 seconds.\nSlot available after: 2026-09-25T13:51:40Z, in 100 seconds.\nCurrently running queries (pid, space limit, time limit, start time):\n";
        assert_eq!(parse_slot_wait(busy), Some(33));
        assert_eq!(parse_slot_wait("Rate limit: 2\n2 slots available now.\n"), Some(0));
        assert_eq!(parse_slot_wait("<html>"), None);
        assert_eq!(status_url("https://overpass-api.de/api/interpreter").as_deref(), Some("https://overpass-api.de/api/status"));
        assert_eq!(status_url("https://example.org/custom"), None);
    }

    #[test]
    fn a_diagonal_trip_skips_the_far_corners_of_its_box() {
        // Decatur → Marietta: 30 km diagonal, 7.7 km margin.
        let (start, end) = ((33.7748, -84.2963), (33.9526, -84.5499));
        let margin_m = 7_700.0;
        let dl = margin_m / 111_320.0;
        let dlo = dl / 33.86f64.to_radians().cos();
        let bbox = BBox::new(start.0 - dl, end.1 - dlo, end.0 + dl, start.1 + dlo);
        let area = Area { bbox, start, end, margin_m };
        let all = tiles_for(&bbox).len();
        let kept = area.tiles();
        assert!(kept.len() < all, "{} of {all}", kept.len());
        // The tiles holding the start and the destination are always kept.
        let at = |p: (f64, f64)| Tile { row: (p.0 / TILE_DEG).floor() as i32, col: (p.1 / TILE_DEG).floor() as i32 };
        assert!(kept.contains(&at(start)) && kept.contains(&at(end)));
        // The far corners (south-west, north-east of the line) are dropped.
        assert!(!kept.contains(&at((bbox.south + 0.001, bbox.west + 0.001))));
    }

    #[test]
    fn tiles_cover_the_box() {
        let t = tiles_for(&BBox::new(33.95, -84.45, 34.05, -84.25));
        assert_eq!(t.len(), 2 * 3);
        assert!(t.contains(&Tile { row: 339, col: -845 }));
        assert!(t.contains(&Tile { row: 340, col: -843 }));
        let b = Tile { row: 339, col: -845 }.bbox();
        assert!((b.south - 33.9).abs() < 1e-9 && (b.east + 84.4).abs() < 1e-9);
    }

    #[test]
    fn cache_keeps_fresh_tiles_and_drops_expired() {
        let c = crate::db::test_conn();
        let t = Tile { row: 1, col: 2 };
        cache_put(&c, &t, b"abc", 1_000_000).unwrap();
        assert_eq!(cache_get(&c, &t, 1_000_000).unwrap().as_deref(), Some(&b"abc"[..]));
        assert_eq!(cache_get(&c, &t, 1_000_000 + TILE_TTL_SECS).unwrap(), None);
        assert_eq!(cache_clear(&c).unwrap(), 1);
    }

    /// A ladder: the direct road along lat 39.000 and a parallel road along 39.010, joined
    /// at both ends. A camera sits on the direct road.
    fn ladder() -> Vec<Way> {
        vec![
            way(1, 0, &[(39.0, -105.0), (39.0, -104.95), (39.0, -104.9)]),
            way(2, 0, &[(39.0, -105.0), (39.01, -105.0)]),
            way(3, 0, &[(39.01, -105.0), (39.01, -104.9)]),
            way(4, 0, &[(39.01, -104.9), (39.0, -104.9)]),
        ]
    }

    #[test]
    fn finds_the_camera_free_road_and_the_fewest_camera_fallback() {
        let cams = vec![cam("node/1", 39.0001, -104.95)];
        let g = build_graph(&ladder(), &cams, &HashSet::new(), 30.0);
        assert_eq!(g.blocked_segments, 2, "both halves of the direct road touch the camera");
        let s = g.nearest((39.0, -105.0), true).unwrap().0;
        let t = g.nearest((39.0, -104.9), false).unwrap().0;
        let free = g.path(s, t, None).unwrap();
        assert_eq!(free.blocked_segments, 0);
        assert!(free.pts.iter().any(|p| (p.0 - 39.01).abs() < 1e-9), "takes the parallel road");
        let any = g.path(s, t, Some(0.0)).unwrap();
        assert!(any.secs < free.secs, "the direct road is quicker");

        // With the parallel road one-way the wrong way, only the camera road is left.
        let mut ways = ladder();
        ways[2].oneway = -1;
        let g = build_graph(&ways, &cams, &HashSet::new(), 30.0);
        assert!(g.path(s, t, None).is_none());
        let fewest = g.path(s, t, Some(3600.0)).unwrap();
        assert_eq!(fewest.blocked_segments, 2);

        // A closed road is never used: with the camera road closed too, nothing is left.
        let mut g = build_graph(&ladder(), &cams, &HashSet::new(), 30.0);
        let free = g.path(s, t, None).unwrap();
        assert_eq!(free.nodes.len(), free.pts.len());
        g.close(free.nodes[1], free.nodes[2]);
        let round = g.path(s, t, Some(3600.0)).unwrap();
        assert_eq!(round.blocked_segments, 2, "only the camera road is left");
        g.close(round.nodes[0], round.nodes[1]);
        assert!(g.path(s, t, Some(3600.0)).is_none());

        // A camera exempted (at the start or destination) blocks nothing.
        let g = build_graph(&ladder(), &cams, &HashSet::from(["node/1".to_string()]), 30.0);
        assert_eq!(g.blocked_segments, 0);
    }
}
