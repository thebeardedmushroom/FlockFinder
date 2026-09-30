//! Driving directions that avoid mapped ALPR cameras, computed by a Valhalla server.
//!
//! Every plan has two routes: the fastest one, and one that avoids cameras where possible.
//! A route "passes" a camera when it comes within [`AVOID_RADIUS_M`] of it; that one rule
//! (`cameras_on_route`) decides every count shown, and the same radius goes to the server
//! with each excluded camera and into the road-map search.
//!
//! **Phase 1** (quick, enough for most trips): route, find the cameras the route passes,
//! exclude those (`exclude_locations`), and route again, until the route passes none or
//! the limits are reached: [`MAX_REQUESTS`] requests, and the public server's
//! [`MAX_EXCLUSIONS`] excluded cameras per request. When excluding a camera leaves no route
//! at all, that camera is given up and the rest are tried again.
//!
//! **Phase 2** (only when phase 1 left cameras that aren't at the start or destination):
//! in a dense metro corridor with thousands of cameras, phase 1 can stop before it finds a
//! camera-free route that exists, because the server only learns about the cameras the
//! routes it proposes happen to pass. So the road network for the trip area is loaded
//! (roadnet.rs), every road segment within the radius of a camera is removed, and what is
//! left is searched. If a camera-free path exists, the server is asked to drive it, leg by
//! leg, through waypoints on it; if none exists, the path with the fewest cameras is used
//! instead (the only "soft" avoidance, and only after the hard search found nothing). The
//! route with the fewest cameras from either phase is the avoidance route, and the plan
//! reports what was checked ([`RoadCheck`]) and which limits were hit.
//!
//! Cameras are matched to a route by distance only. v1 treats every camera as seeing all
//! directions; each camera's `direction` tag is carried along for later directional
//! avoidance.

use crate::db::Camera;
use crate::error::{AppError, AppResult};
use crate::geo_util::{haversine_m, point_to_polyline, valid_coord};
use crate::grid::BBox;
use crate::http::HttpClient;
use crate::roadnet::RoadSource;
use crate::submissions::Submission;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[path = "routing_repair.rs"]
pub(crate) mod repair;

/// The FOSSGIS public Valhalla server (planet-wide, no key; fair use, rate limited).
pub const DEFAULT_ENDPOINT: &str = "https://valhalla1.openstreetmap.de";
/// Sent as `X-Client-Id`, which the FOSSGIS server asks published apps to include.
pub const CLIENT_ID: &str = "FlockFinder";
/// A route passing within this distance of a camera "passes" it; also the exclusion radius.
pub const AVOID_RADIUS_M: f64 = 30.0;
/// The public server rejects more `exclude_locations` than this (error 157).
pub const MAX_EXCLUSIONS: usize = 50;
/// Routing requests per plan, including the fastest route.
pub const MAX_REQUESTS: u32 = 8;
/// Alternative routes asked for with every request (more candidates, same request count).
pub const ALTERNATES: u32 = 2;
/// Closer than this, start and destination are "the same place".
pub const MIN_TRIP_M: f64 = 150.0;
/// Only cameras inside the start/destination box grown by this much are ever excluded
/// (the larger of the two buffers).
pub const CORRIDOR_MIN_BUFFER_M: f64 = 3_000.0;
pub const CORRIDOR_BUFFER_FRACTION: f64 = 0.3;
/// The avoidance route is flagged as a long detour when it takes this much longer than the
/// fastest route (either condition).
pub const DETOUR_RATIO: f64 = 0.5;
pub const DETOUR_SECS: f64 = 20.0 * 60.0;
/// A submission this close to a mapped camera is taken to be that camera.
const DEDUPE_M: f64 = 15.0;

/// Phase 2: the road map is loaded for the start/destination box grown by the larger of these.
/// A detour around a camera-lined corridor has to reach the next parallel corridor: the
/// camera-free route from Buckhead to Sandy Springs (a trip due north, so its box is a thin
/// strip) runs 4.1 km outside the box, and 4 km missed it.
pub const ROAD_MAP_MIN_BUFFER_M: f64 = 6_000.0;
pub const ROAD_MAP_BUFFER_FRACTION: f64 = 0.25;
/// Phase 2 is skipped for trips longer than this (straight line): the road map would be too
/// big a download.
pub const ROAD_MAP_MAX_TRIP_M: f64 = 60_000.0;
/// A start or destination farther than this from any mapped road can't be matched to the map.
const ROAD_MAP_MAX_SNAP_M: f64 = 500.0;
/// Fewest-camera fallback: extra cost of each road segment near a camera, seconds.
const CAMERA_PENALTY_SECS: f64 = 3_600.0;
/// Guidance: the road-map path is driven in legs of about this length…
const LEG_M: f64 = 4_000.0;
const MAX_LEGS: usize = 14;
/// …each through this many waypoints on the path (the server takes 10 locations a request)…
const VIAS_PER_LEG: usize = 8;
/// …asking again, with the cameras it strayed onto excluded, up to this many times a leg.
const LEG_ROUNDS: usize = 3;
/// Phase 2 request budget (legs and retries together).
pub const MAX_GUIDED_REQUESTS: u32 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

/// A camera that routes are checked against: mapped (OSM) or one of your submissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvoidCamera {
    /// `node/123` for OSM cameras, `submission/7` for submissions.
    pub key: String,
    pub lat: f64,
    pub lon: f64,
    /// `flock` or `alpr`.
    pub category: String,
    /// `osm` or `submission` (unverified: recorded in this app, not in the synced data).
    pub source: String,
    /// Raw `direction` / `camera:direction` value, kept for directional avoidance later.
    pub direction: Option<String>,
    pub operator: Option<String>,
}

/// Why a camera is still on the avoidance route. Only `Unavoidable` claims there is no way
/// around it; the others say what stopped the search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Remaining {
    /// The start or destination is inside its avoid zone; excluding it would make the
    /// route impossible, so it was never excluded.
    NearEndpoint,
    /// The road map has no camera-free route, and the route with the fewest cameras passes
    /// this one.
    Unavoidable,
    /// A way round exists, but it would add more than the time limit per camera avoided
    /// (`PlanOptions::max_extra_secs_per_camera`), so the route keeps the road.
    LongDetour,
    /// It was excluded, but the route still passes within the radius on another road
    /// (typically a cross street at the same intersection).
    Nearby,
    /// Excluding it left the routing server with no route (and the road map couldn't settle
    /// whether a way around exists).
    NoRoute,
    /// The search stopped (limits, or the server stopped answering) before this camera was
    /// avoided; a route around it may exist.
    SearchLimit,
}

/// What the road-map check (phase 2) found.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RoadCheck {
    /// Not run: phase 1 found a camera-free route, or only cameras at the start or
    /// destination were left.
    NotNeeded,
    /// The road map has a camera-free route.
    CameraFree,
    /// The road map has no camera-free route; the fewest-camera path found passes this many.
    NoneExists { fewest: usize },
    /// The road map couldn't be checked.
    Unavailable { reason: String },
    /// A long trip: the stretches with cameras were fixed one at a time (see
    /// routing_repair.rs); `fixed` of `total` passed fewer cameras afterwards, and `too_slow`
    /// were left because every way round cost more than `limit_min` minutes per camera
    /// (`None`: no limit).
    Stretches { fixed: usize, total: usize, too_slow: usize, limit_min: Option<u32> },
}

/// Limits phase 1 ran into (the public server's cap, and the request budget).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Limits {
    pub exclusion_cap: bool,
    pub request_budget: bool,
}

/// The road map used by phase 2.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RoadMapInfo {
    pub tiles: usize,
    pub downloaded_tiles: usize,
    pub cached_tiles: usize,
    /// Tiles not downloaded in time (a route found without them is still real).
    pub missing_tiles: usize,
    /// Map data received, uncompressed.
    pub downloaded_bytes: u64,
    pub ways: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteCamera {
    #[serde(flatten)]
    pub camera: AvoidCamera,
    /// Distance along the route to the closest point, metres.
    pub along_m: f64,
    /// Distance from the route, metres.
    pub distance_m: f64,
    /// Set on the avoidance route only.
    pub remaining: Option<Remaining>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Maneuver {
    pub instruction: String,
    /// Valhalla maneuver type (1–3 start, 4–6 destination, …).
    pub kind: u32,
    pub distance_m: f64,
    pub duration_s: f64,
    /// Where the maneuver begins.
    pub lat: f64,
    pub lon: f64,
    /// Index of that point in the route's shape.
    #[serde(default)]
    pub shape_index: usize,
    /// The road the maneuver goes onto.
    #[serde(default)]
    pub street_names: Vec<String>,
    /// The road's name where it begins, when that differs (a ramp, say).
    #[serde(default)]
    pub begin_street_names: Vec<String>,
    /// The maneuver's road is a highway (motorway, trunk, …).
    #[serde(default)]
    pub highway: bool,
    /// Spoken text from the server, written for speech ("Take exit 2 39."): a short form for
    /// announcements ahead of the maneuver, the full form for when it is due, and what to say
    /// once it is done ("Continue for 2 miles.").
    #[serde(default)]
    pub verbal_alert: Option<String>,
    #[serde(default)]
    pub verbal_pre: Option<String>,
    #[serde(default)]
    pub verbal_post: Option<String>,
    #[serde(default)]
    pub bearing_before: Option<f64>,
    #[serde(default)]
    pub bearing_after: Option<f64>,
    #[serde(default)]
    pub roundabout_exit_count: Option<u32>,
    #[serde(default)]
    pub exit_number: Option<String>,
}

/// One route as returned by the server, before cameras are matched.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// (lat, lon)
    pub shape: Vec<(f64, f64)>,
    pub distance_m: f64,
    pub duration_s: f64,
    pub maneuvers: Vec<Maneuver>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedRoute {
    /// [lat, lon] pairs.
    pub shape: Vec<[f64; 2]>,
    pub distance_m: f64,
    pub duration_s: f64,
    /// Cameras the route passes, in driving order.
    pub cameras: Vec<RouteCamera>,
    pub maneuvers: Vec<Maneuver>,
    pub bbox: BBox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The avoidance route passes no cameras.
    Clear,
    /// Fewer cameras than the fastest route, but not none.
    Reduced,
    /// No route with fewer cameras than the fastest one was found.
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoutePlan {
    pub fastest: PlannedRoute,
    pub avoid: PlannedRoute,
    /// The avoidance route is the fastest route (it had no cameras, or none could be avoided).
    pub same_route: bool,
    pub outcome: Outcome,
    /// The avoidance route is much longer than the fastest (see `DETOUR_*`).
    pub long_detour: bool,
    pub requests: u32,
    /// Cameras excluded in the request that produced the avoidance route's search.
    pub excluded: usize,
    /// Set when the search stopped early (a failed follow-up request); the result is the
    /// best route found before that.
    pub warning: Option<String>,
    /// Host of the routing server, for the privacy notice.
    pub server: String,
    pub road_check: RoadCheck,
    /// The avoidance route follows the road-map path (phase 2).
    pub avoid_from_road_map: bool,
    pub limits: Limits,
    pub road_map: Option<RoadMapInfo>,
}

/// Progress of a running plan, emitted as `route:progress`.
#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub step: u32,
    pub max: u32,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Camera set
// ---------------------------------------------------------------------------

fn tag(c: &Camera, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| c.tags.get(*k))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The cameras routes are checked against: mapped cameras that are still in the data (stale
/// ones are not counted anywhere), plus every submission (unverified ones included) that
/// isn't already a mapped camera.
pub fn merge_cameras(osm: Vec<Camera>, subs: &[Submission], bbox: &BBox) -> Vec<AvoidCamera> {
    let mut out: Vec<AvoidCamera> = osm
        .into_iter()
        .filter(|c| c.stale_since.is_none())
        .map(|c| AvoidCamera {
            key: c.key(),
            lat: c.lat,
            lon: c.lon,
            direction: tag(&c, &["direction", "camera:direction"]),
            operator: tag(&c, &["operator"]),
            category: c.category,
            source: "osm".into(),
        })
        .collect();
    let mapped = out.len();
    for s in subs.iter().filter(|s| bbox.contains(s.lat, s.lon)) {
        if out[..mapped]
            .iter()
            .any(|c| haversine_m(c.lat, c.lon, s.lat, s.lon) <= DEDUPE_M)
        {
            continue;
        }
        out.push(AvoidCamera {
            key: format!("submission/{}", s.id),
            lat: s.lat,
            lon: s.lon,
            category: s.category.clone(),
            source: "submission".into(),
            direction: s.direction.map(|d| d.to_string()),
            operator: s.operator.clone(),
        });
    }
    out
}

/// `bbox` grown by `m` metres on every side.
pub fn grow(bbox: &BBox, m: f64) -> BBox {
    let d_lat = m / 111_320.0;
    let mid = ((bbox.south + bbox.north) / 2.0).to_radians().cos().max(0.01);
    let d_lon = d_lat / mid;
    BBox::new(bbox.south - d_lat, bbox.west - d_lon, bbox.north + d_lat, bbox.east + d_lon).sanitized()
}

fn bbox_of(points: &[(f64, f64)]) -> BBox {
    let mut b = BBox::new(f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for &(lat, lon) in points {
        b.south = b.south.min(lat);
        b.north = b.north.max(lat);
        b.west = b.west.min(lon);
        b.east = b.east.max(lon);
    }
    b
}

/// The box whose cameras may be excluded: start and destination plus a buffer that grows
/// with the trip, so ordinary detours stay inside it.
pub fn corridor(start: LatLon, end: LatLon) -> BBox {
    let trip = haversine_m(start.lat, start.lon, end.lat, end.lon);
    let buffer = CORRIDOR_MIN_BUFFER_M.max(trip * CORRIDOR_BUFFER_FRACTION);
    grow(&bbox_of(&[(start.lat, start.lon), (end.lat, end.lon)]), buffer)
}

/// Planar distance (metres) from `p` to segment a–b, all (lat, lon); accurate at the tens of
/// metres this is used for.
fn seg_dist_m(p: (f64, f64), a: (f64, f64), b: (f64, f64), cos_lat: f64) -> f64 {
    let to_xy = |q: (f64, f64)| ((q.1 - p.1) * 111_320.0 * cos_lat, (q.0 - p.0) * 111_320.0);
    let (ax, ay) = to_xy(a);
    let (bx, by) = to_xy(b);
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (-(ax * dx + ay * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (ax + t * dx).hypot(ay + t * dy)
}

/// Cameras within `radius_m` of the route, in driving order.
pub fn cameras_on_route(shape: &[(f64, f64)], cameras: &[AvoidCamera], radius_m: f64) -> Vec<RouteCamera> {
    if shape.is_empty() {
        return Vec::new();
    }
    let reach = grow(&bbox_of(shape), radius_m * 1.5);
    // Degrees of latitude that the radius spans, with slack; used to skip far segments cheaply.
    let d_lat = radius_m * 1.5 / 111_320.0;
    let mut out = Vec::new();
    for cam in cameras.iter().filter(|c| reach.contains(c.lat, c.lon)) {
        let cos_lat = cam.lat.to_radians().cos().max(0.01);
        let d_lon = d_lat / cos_lat;
        let p = (cam.lat, cam.lon);
        let near = shape.windows(2).any(|w| {
            let (a, b) = (w[0], w[1]);
            if p.0 < a.0.min(b.0) - d_lat || p.0 > a.0.max(b.0) + d_lat {
                return false;
            }
            if p.1 < a.1.min(b.1) - d_lon || p.1 > a.1.max(b.1) + d_lon {
                return false;
            }
            seg_dist_m(p, a, b, cos_lat) <= radius_m * 1.05
        }) || (shape.len() == 1 && haversine_m(p.0, p.1, shape[0].0, shape[0].1) <= radius_m);
        if !near {
            continue;
        }
        let Some(hit) = point_to_polyline(cam.lat, cam.lon, shape) else { continue };
        if hit.distance_m <= radius_m {
            out.push(RouteCamera {
                camera: cam.clone(),
                along_m: hit.along_m,
                distance_m: hit.distance_m,
                remaining: None,
            });
        }
    }
    out.sort_by(|a, b| a.along_m.total_cmp(&b.along_m));
    out
}

// ---------------------------------------------------------------------------
// Valhalla wire format
// ---------------------------------------------------------------------------

/// Decode an encoded polyline with 6 digits of precision (Valhalla's shape format).
pub fn decode_polyline6(s: &str) -> AppResult<Vec<(f64, f64)>> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let (mut lat, mut lon) = (0i64, 0i64);
    let mut out = Vec::new();
    while i < bytes.len() {
        let mut deltas = [0i64; 2];
        for d in deltas.iter_mut() {
            let (mut result, mut shift) = (0i64, 0u32);
            loop {
                let Some(&byte) = bytes.get(i) else {
                    return Err(AppError::Parse("route shape is truncated".into()));
                };
                i += 1;
                let b = byte as i64 - 63;
                if !(0..64).contains(&b) || shift > 60 {
                    return Err(AppError::Parse("route shape is malformed".into()));
                }
                result |= (b & 0x1f) << shift;
                shift += 5;
                if b < 0x20 {
                    break;
                }
            }
            *d = if result & 1 != 0 { !(result >> 1) } else { result >> 1 };
        }
        lat += deltas[0];
        lon += deltas[1];
        out.push((lat as f64 / 1e6, lon as f64 / 1e6));
    }
    Ok(out)
}

/// A point the route starts, ends or passes through.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Waypoint {
    pub lat: f64,
    pub lon: f64,
    /// Pass through without a stop (`through`) rather than start or end a leg (`break`).
    pub through: bool,
    /// Direction of travel here, degrees clockwise from north.
    pub heading: Option<f64>,
}

impl Waypoint {
    pub fn at(p: LatLon) -> Self {
        Waypoint { lat: p.lat, lon: p.lon, through: false, heading: None }
    }
}

/// One routing request.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteRequest {
    pub locations: Vec<Waypoint>,
    pub exclude: Vec<LatLon>,
    pub alternates: u32,
}

impl RouteRequest {
    pub fn between(start: LatLon, end: LatLon, exclude: &[LatLon]) -> Self {
        RouteRequest {
            locations: vec![Waypoint::at(start), Waypoint::at(end)],
            exclude: exclude.to_vec(),
            alternates: ALTERNATES,
        }
    }
}

pub fn request_body(req: &RouteRequest) -> serde_json::Value {
    let locations: Vec<serde_json::Value> = req
        .locations
        .iter()
        .map(|w| {
            let kind = if w.through { "through" } else { "break" };
            let mut l = serde_json::json!({ "lat": w.lat, "lon": w.lon, "type": kind });
            if let Some(h) = w.heading {
                l["heading"] = serde_json::json!(h.rem_euclid(360.0).round() as u32 % 360);
            }
            l
        })
        .collect();
    // Miles, so the spoken instructions ("Continue for 2 miles.") match the navigation units.
    let mut body = serde_json::json!({
        "locations": locations,
        "costing": "auto",
        "units": "miles",
        "directions_options": { "units": "miles", "language": "en-US" },
    });
    if req.alternates > 0 {
        body["alternates"] = serde_json::json!(req.alternates);
    }
    let exclude = &req.exclude;
    if !exclude.is_empty() {
        // `radius` makes every road within it (not just the nearest) count as the camera's.
        body["exclude_locations"] = exclude
            .iter()
            .map(|p| serde_json::json!({ "lat": p.lat, "lon": p.lon, "radius": AVOID_RADIUS_M as u32 }))
            .collect();
    }
    body
}

fn num(v: &serde_json::Value, key: &str) -> f64 {
    v.get(key).and_then(|x| x.as_f64()).unwrap_or(0.0)
}

pub const METERS_PER_MILE: f64 = 1609.344;

/// Lengths in an answer, in metres (the request asks for miles; see `request_body`).
fn length_m(v: &serde_json::Value, trip: &serde_json::Value) -> f64 {
    // The answer says which (kilometres when it doesn't: Valhalla's default).
    let per_unit = match trip.get("units").and_then(|u| u.as_str()) {
        Some("miles") => METERS_PER_MILE,
        _ => 1000.0,
    };
    num(v, "length") * per_unit
}

fn text(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|s| s.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

fn names(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(String::from))
        .collect()
}

fn parse_trip(trip: &serde_json::Value) -> AppResult<Candidate> {
    let legs = trip
        .get("legs")
        .and_then(|l| l.as_array())
        .filter(|l| !l.is_empty())
        .ok_or_else(|| AppError::Parse("route has no legs".into()))?;
    let mut shape: Vec<(f64, f64)> = Vec::new();
    let mut maneuvers = Vec::new();
    for leg in legs {
        let leg_shape = decode_polyline6(leg.get("shape").and_then(|s| s.as_str()).unwrap_or(""))?;
        // A leg starts where the previous one ended.
        let skip = usize::from(!shape.is_empty() && !leg_shape.is_empty());
        let offset = shape.len() - skip;
        for m in leg.get("maneuvers").and_then(|m| m.as_array()).into_iter().flatten() {
            let index = m
                .get("begin_shape_index")
                .and_then(|i| i.as_u64())
                .map(|i| i as usize)
                .filter(|&i| i < leg_shape.len())
                .unwrap_or(0);
            let at = leg_shape.get(index).copied().unwrap_or((0.0, 0.0));
            let sign = m.get("sign");
            maneuvers.push(Maneuver {
                instruction: text(m, "instruction").unwrap_or_default(),
                kind: m.get("type").and_then(|t| t.as_u64()).unwrap_or(0) as u32,
                distance_m: length_m(m, trip),
                duration_s: num(m, "time"),
                lat: at.0,
                lon: at.1,
                shape_index: offset + index,
                street_names: names(m, "street_names"),
                begin_street_names: names(m, "begin_street_names"),
                highway: m.get("highway").and_then(|h| h.as_bool()).unwrap_or(false),
                verbal_alert: text(m, "verbal_transition_alert_instruction"),
                verbal_pre: text(m, "verbal_pre_transition_instruction"),
                verbal_post: text(m, "verbal_post_transition_instruction"),
                bearing_before: m.get("bearing_before").and_then(|b| b.as_f64()),
                bearing_after: m.get("bearing_after").and_then(|b| b.as_f64()),
                roundabout_exit_count: m.get("roundabout_exit_count").and_then(|c| c.as_u64()).map(|c| c as u32),
                exit_number: sign
                    .and_then(|s| s.get("exit_number_elements"))
                    .and_then(|e| e.as_array())
                    .and_then(|e| e.first())
                    .and_then(|e| text(e, "text")),
            });
        }
        shape.extend(leg_shape.into_iter().skip(skip));
    }
    if shape.len() < 2 {
        return Err(AppError::Parse("route shape is empty".into()));
    }
    let summary = trip.get("summary").cloned().unwrap_or_default();
    Ok(Candidate {
        shape,
        distance_m: length_m(&summary, trip),
        duration_s: num(&summary, "time"),
        maneuvers,
    })
}

/// The main route and any alternates, main route first.
pub fn parse_route_response(body: &str) -> AppResult<Vec<Candidate>> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| AppError::Parse(format!("routing response is not valid JSON: {e}")))?;
    let trip = v
        .get("trip")
        .ok_or_else(|| AppError::Parse("routing response has no trip".into()))?;
    let mut out = vec![parse_trip(trip)?];
    for alt in v.get("alternates").and_then(|a| a.as_array()).into_iter().flatten() {
        // A malformed alternate is dropped, never fatal.
        if let Some(Ok(c)) = alt.get("trip").map(parse_trip) {
            out.push(c);
        }
    }
    Ok(out)
}

#[derive(Debug)]
pub enum RouteError {
    /// The server found no path between the points with these exclusions.
    NoRoute,
    /// The server accepts fewer excluded cameras per request than were sent (its limit, when
    /// it says).
    TooManyExclusions(Option<usize>),
    Api(AppError),
}

impl From<AppError> for RouteError {
    fn from(e: AppError) -> Self {
        RouteError::Api(e)
    }
}

/// Turn a Valhalla error answer (HTTP 400 with `error_code`) into something readable.
pub fn classify_error(status: u16, body: &str) -> RouteError {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let code = v.get("error_code").and_then(|c| c.as_u64());
    let message = v.get("error").and_then(|m| m.as_str()).unwrap_or("").to_string();
    match code {
        Some(442) => RouteError::NoRoute,
        // "Exceeded max avoid locations: 50": a server with a lower limit than this app assumes.
        Some(157) => RouteError::TooManyExclusions(message.rsplit(':').next().and_then(|n| n.trim().parse().ok())),
        // Exclusions never cause this one: the two points are on road networks that don't
        // connect at all.
        Some(170) => RouteError::Api(AppError::Invalid(
            "No driving route connects these points: they're on road networks that don't join (different islands, say).".into(),
        )),
        Some(171) => RouteError::Api(AppError::Invalid(
            "No road was found near the start or destination. Pick a point on or next to a road.".into(),
        )),
        Some(150..=156) => RouteError::Api(AppError::Invalid(format!(
            "This trip is longer than the routing server allows{}.",
            message
                .split("limit: ")
                .nth(1)
                .and_then(|m| m.trim_end_matches(" meters").parse::<f64>().ok())
                .map(|m| format!(" ({:.0} km)", m / 1000.0))
                .unwrap_or_default()
        ))),
        _ => RouteError::Api(AppError::Http {
            status,
            endpoint: "the routing server".into(),
            body: if message.is_empty() { body.chars().take(200).collect() } else { message },
        }),
    }
}

/// Whoever answers routing requests (Valhalla in the app, a fake in tests).
pub(crate) trait Router {
    async fn route(&self, req: &RouteRequest) -> Result<Vec<Candidate>, RouteError>;
}

pub struct Valhalla<'a> {
    pub http: &'a HttpClient,
    /// Base URL, no trailing slash.
    pub endpoint: String,
}

impl Valhalla<'_> {
    /// Send one request; the raw answer, or the error (a non-2xx answer is `AppError::Http`
    /// with its body).
    pub async fn fetch(&self, req: &RouteRequest) -> AppResult<String> {
        self.http.routing_slot().await;
        let url = format!("{}/route", self.endpoint);
        let body = request_body(req);
        let resp = self
            .http
            .send_with_backoff("the routing server", || {
                self.http.client.post(&url).header("X-Client-Id", CLIENT_ID).json(&body)
            })
            .await?;
        Ok(resp.text().await?)
    }
}

/// A raw answer (see `Valhalla::fetch`) as routes or a routing error.
pub fn interpret(answer: AppResult<String>) -> Result<Vec<Candidate>, RouteError> {
    match answer {
        Ok(text) => Ok(parse_route_response(&text)?),
        Err(AppError::Http { status, body, .. }) => Err(classify_error(status, &body)),
        Err(e) => Err(RouteError::Api(e)),
    }
}

impl Router for Valhalla<'_> {
    async fn route(&self, req: &RouteRequest) -> Result<Vec<Candidate>, RouteError> {
        interpret(self.fetch(req).await)
    }
}

/// Settings value → base URL (empty means the default server).
pub fn normalize_endpoint(s: &str) -> String {
    let s = s.trim().trim_end_matches('/');
    let s = s.strip_suffix("/route").unwrap_or(s);
    if s.is_empty() { DEFAULT_ENDPOINT.to_string() } else { s.to_string() }
}

// ---------------------------------------------------------------------------
// Planner
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct Evaluated {
    pub(crate) candidate: Candidate,
    pub(crate) cameras: Vec<RouteCamera>,
}

impl Evaluated {
    fn rank(&self) -> (usize, f64) {
        (self.cameras.len(), self.candidate.duration_s)
    }

    fn better_than(&self, other: &Evaluated) -> bool {
        let (a, b) = (self.rank(), other.rank());
        a.0 < b.0 || (a.0 == b.0 && a.1 < b.1)
    }
}

pub(crate) fn evaluate<C>(candidate: Candidate, cameras: &C) -> AppResult<Evaluated>
where
    C: Fn(&BBox) -> AppResult<Vec<AvoidCamera>>,
{
    let reach = grow(&bbox_of(&candidate.shape), AVOID_RADIUS_M * 2.0);
    let cams = cameras(&reach)?;
    let on_route = cameras_on_route(&candidate.shape, &cams, AVOID_RADIUS_M);
    Ok(Evaluated { candidate, cameras: on_route })
}

fn best_of(list: Vec<Evaluated>) -> Option<Evaluated> {
    list.into_iter().reduce(|a, b| if b.better_than(&a) { b } else { a })
}

pub(crate) fn planned(e: Evaluated) -> PlannedRoute {
    let bbox = bbox_of(&e.candidate.shape);
    PlannedRoute {
        shape: e.candidate.shape.iter().map(|&(lat, lon)| [lat, lon]).collect(),
        distance_m: e.candidate.distance_m,
        duration_s: e.candidate.duration_s,
        cameras: e.cameras,
        maneuvers: e.candidate.maneuvers,
        bbox,
    }
}

pub fn is_long_detour(fastest_s: f64, avoid_s: f64) -> bool {
    let extra = avoid_s - fastest_s;
    extra > DETOUR_SECS || (fastest_s > 0.0 && extra > fastest_s * DETOUR_RATIO)
}

fn host_of(endpoint: &str) -> String {
    url::Url::parse(endpoint)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_else(|| endpoint.to_string())
}

/// Initial direction of travel from `a` to `b` (lat, lon), degrees clockwise from north.
pub fn bearing(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, la2) = (a.0.to_radians(), b.0.to_radians());
    let dl = (b.1 - a.1).to_radians();
    let y = dl.sin() * la2.cos();
    let x = la1.cos() * la2.sin() - la1.sin() * la2.cos() * dl.cos();
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

/// Join legs driven one after another into one route. The legs' own "start" and "arrive"
/// maneuvers at the joins are dropped (nobody stops there); a dropped start's distance and
/// time go to the maneuver before it.
pub fn stitch(legs: Vec<Candidate>) -> Option<Candidate> {
    let n = legs.len();
    let mut out = Candidate { shape: Vec::new(), distance_m: 0.0, duration_s: 0.0, maneuvers: Vec::new() };
    for (i, leg) in legs.into_iter().enumerate() {
        out.distance_m += leg.distance_m;
        out.duration_s += leg.duration_s;
        let skip = usize::from(out.shape.last().is_some_and(|l| leg.shape.first() == Some(l)));
        // Shape indices move with the leg's shape (its first point merges with the last one).
        let offset = out.shape.len() - skip;
        out.shape.extend(leg.shape.into_iter().skip(skip));
        for mut m in leg.maneuvers {
            m.shape_index += offset;
            let starts = (1..=3).contains(&m.kind);
            let arrives = (4..=6).contains(&m.kind);
            if arrives && i + 1 < n {
                continue;
            }
            if starts && i > 0 {
                if let Some(prev) = out.maneuvers.last_mut() {
                    prev.distance_m += m.distance_m;
                    prev.duration_s += m.duration_s;
                }
                continue;
            }
            out.maneuvers.push(m);
        }
    }
    (out.shape.len() >= 2).then_some(out)
}

/// One leg of a road-map path: where it starts and ends, the waypoints that keep the server
/// on the path, and which stretch of the path (vertex indices) it covers.
#[derive(Debug, Clone, PartialEq)]
pub struct Leg {
    pub from: Waypoint,
    pub to: Waypoint,
    pub vias: Vec<Waypoint>,
    pub span: (usize, usize),
}

/// Cut a road-map path (lat, lon) into legs of about [`LEG_M`], the first starting at `from`
/// (the real start, or where an earlier leg ended) and the last ending at the real
/// destination. Waypoints sit mid-segment (never on an intersection, where the server could
/// pick the cross street) with the direction of travel attached. So do the joins between legs:
/// `stitch` drops the "start" maneuver of every leg after the first, and a join on an
/// intersection would drop a turn with it.
pub fn plan_legs(path: &[(f64, f64)], from: Waypoint, end: LatLon, end_heading: Option<f64>) -> Vec<Leg> {
    let last_to = Waypoint { heading: end_heading, ..Waypoint::at(end) };
    if path.len() < 2 {
        return vec![Leg { from, to: last_to, vias: Vec::new(), span: (0, path.len().saturating_sub(1)) }];
    }
    let mut cum = vec![0.0];
    for w in path.windows(2) {
        cum.push(cum.last().copied().unwrap_or(0.0) + haversine_m(w[0].0, w[0].1, w[1].0, w[1].1));
    }
    let total = cum.last().copied().unwrap_or(0.0);
    let n = ((total / LEG_M).ceil() as usize).clamp(1, MAX_LEGS);
    let last = path.len() - 1;
    let vertex_at = |d: f64| cum.partition_point(|&c| c < d).min(last);
    let mut bounds: Vec<usize> = (0..=n).map(|k| vertex_at(total * k as f64 / n as f64)).collect();
    bounds[0] = 0;
    bounds[n] = last;
    bounds.dedup();
    // The join at interior vertex `i`: the middle of the longer of its two segments, and that
    // segment's index.
    let join = |i: usize| {
        let seg = if cum[i] - cum[i - 1] >= cum[i + 1] - cum[i] { i - 1 } else { i };
        let (p, q) = (path[seg], path[seg + 1]);
        let w = Waypoint { lat: (p.0 + q.0) / 2.0, lon: (p.1 + q.1) / 2.0, through: false, heading: Some(bearing(p, q)) };
        (w, seg)
    };
    let k_last = bounds.len() - 2;
    (0..bounds.len() - 1)
        .map(|k| {
            let (a, b) = (bounds[k], bounds[k + 1]);
            let (leg_from, from_seg) = if k == 0 { (from, None) } else { (join(a).0, Some(join(a).1)) };
            let (to, to_seg) = if k == k_last { (last_to, None) } else { (join(b).0, Some(join(b).1)) };
            let mut vias: Vec<Waypoint> = Vec::new();
            let mut used: Option<usize> = None;
            for j in 1..=VIAS_PER_LEG {
                let d = cum[a] + (cum[b] - cum[a]) * j as f64 / (VIAS_PER_LEG + 1) as f64;
                let seg = cum.partition_point(|&c| c <= d).saturating_sub(1).clamp(a, b.saturating_sub(1));
                // A via on a join's segment would sit on top of the join.
                if used == Some(seg) || Some(seg) == from_seg || Some(seg) == to_seg {
                    continue;
                }
                used = Some(seg);
                let (p, q) = (path[seg], path[seg + 1]);
                vias.push(Waypoint { lat: (p.0 + q.0) / 2.0, lon: (p.1 + q.1) / 2.0, through: true, heading: Some(bearing(p, q)) });
            }
            Leg { from: leg_from, to, vias, span: (a, b) }
        })
        .collect()
}

/// A leg the server drives this much longer than the road map (×, plus metres) was refused
/// outright, not just shortcut between waypoints.
const REFUSAL_RATIO: f64 = 1.5;
const REFUSAL_SLACK_M: f64 = 1_000.0;
/// Times the road-map path may be re-planned around stretches the server refuses to drive.
const MAX_REPLANS: usize = 4;

/// The road-map search the guidance works from.
struct RoadMap<'g> {
    graph: &'g mut crate::roadnet::Graph,
    t: u32,
    /// `None`: camera-free only. `Some`: the fewest-camera fallback.
    penalty: Option<f64>,
    /// Cameras on or near the map area, to find which ones a path passes.
    cams: &'g [AvoidCamera],
    near_endpoint: &'g HashSet<String>,
}

impl RoadMap<'_> {
    /// A path from node `s`, and the cameras the route may pass (the path's own, which only
    /// the fewest-camera fallback has, plus those at the ends).
    fn path_from(&self, s: u32) -> Option<(crate::roadnet::GraphPath, HashSet<String>)> {
        let p = self.graph.path(s, self.t, self.penalty)?;
        let mut allowed = self.near_endpoint.clone();
        if self.penalty.is_some() {
            allowed.extend(cameras_on_route(&p.pts, self.cams, AVOID_RADIUS_M).into_iter().map(|c| c.camera.key));
        }
        Some((p, allowed))
    }
}

/// Drive a road-map path leg by leg. Each leg goes through waypoints on the path. If the
/// server strays onto a camera the path doesn't pass, that camera is excluded and the leg asked
/// again. If the server won't drive a stretch at all (a turn restriction, gate or closure the
/// map doesn't know about), that stretch is closed on the map and the rest re-planned from the
/// last leg that worked. Returns the route and the cameras it was allowed to pass; `Ok(None)`
/// when the path couldn't be driven within the budget.
#[allow(clippy::too_many_arguments)]
async fn guide<R, C>(
    router: &R,
    map: RoadMap<'_>,
    s: u32,
    start: LatLon,
    end: LatLon,
    cameras: &C,
    requests: &mut u32,
    step: &(dyn Fn(u32, String) + Sync),
    opts: &PlanOptions,
) -> AppResult<(Option<Evaluated>, HashSet<String>)>
where
    R: Router,
    C: Fn(&BBox) -> AppResult<Vec<AvoidCamera>>,
{
    let budget_end = *requests + opts.guided_requests;
    let mut driven: Vec<Candidate> = Vec::new();
    let mut allowed_all: HashSet<String> = map.near_endpoint.clone();
    let (mut from_node, mut from) = (s, Waypoint { heading: opts.start_heading, ..Waypoint::at(start) });
    for replan in 0..=MAX_REPLANS {
        let Some((path, allowed)) = map.path_from(from_node) else {
            log::warn!("road-map guidance: no path left after closing refused roads");
            return Ok((None, allowed_all));
        };
        allowed_all.extend(allowed.iter().cloned());
        let legs = plan_legs(&path.pts, from, end, opts.end_heading);
        let mut cum = vec![0.0];
        for w in path.pts.windows(2) {
            cum.push(cum.last().copied().unwrap_or(0.0) + haversine_m(w[0].0, w[0].1, w[1].0, w[1].1));
        }
        let mut refused: Option<usize> = None;
        for (k, leg) in legs.iter().enumerate() {
            let leg_m = cum[leg.span.1] - cum[leg.span.0];
            let mut exclude: Vec<AvoidCamera> = Vec::new();
            let mut best: Option<(usize, Candidate)> = None;
            let mut refusal = false;
            for _ in 0..LEG_ROUNDS {
                if *requests >= budget_end {
                    log::warn!("road-map guidance ran out of requests at leg {} of {}", k + 1, legs.len());
                    return Ok((None, allowed_all));
                }
                *requests += 1;
                step(*requests, format!("Following the road map (part {} of {})", k + 1, legs.len()));
                let mut locations = vec![leg.from];
                locations.extend(leg.vias.iter().copied());
                locations.push(leg.to);
                let req = RouteRequest {
                    locations,
                    exclude: exclude.iter().map(|c| LatLon { lat: c.lat, lon: c.lon }).collect(),
                    alternates: 0,
                };
                match router.route(&req).await {
                    Ok(cands) => {
                        let Some(c) = cands.into_iter().next() else { break };
                        let ev = evaluate(c, cameras)?;
                        let stray: Vec<AvoidCamera> =
                            ev.cameras.iter().filter(|rc| !allowed.contains(&rc.camera.key)).map(|rc| rc.camera.clone()).collect();
                        let n = stray.len();
                        if best.as_ref().map_or(true, |(bn, b)| n < *bn || (n == *bn && ev.candidate.duration_s < b.duration_s)) {
                            best = Some((n, ev.candidate.clone()));
                        }
                        if n == 0 {
                            break;
                        }
                        if ev.candidate.distance_m > leg_m * REFUSAL_RATIO + REFUSAL_SLACK_M {
                            // Not a shortcut between waypoints: it won't drive this stretch.
                            refusal = true;
                            break;
                        }
                        for c in stray {
                            if exclude.len() < MAX_EXCLUSIONS && !exclude.iter().any(|e| e.key == c.key) {
                                exclude.push(c);
                            }
                        }
                    }
                    // Waypoints it can't route through at all: refused.
                    Err(RouteError::NoRoute) => {
                        refusal = true;
                        break;
                    }
                    Err(RouteError::TooManyExclusions(_)) => break,
                    Err(RouteError::Api(e)) => return Err(e),
                }
            }
            let clear = best.as_ref().is_some_and(|(n, _)| *n == 0);
            if !clear && replan < MAX_REPLANS {
                if refusal {
                    log::info!("road-map guidance: the server won't drive part {} of {}", k + 1, legs.len());
                }
                refused = Some(k);
                break;
            }
            match best {
                Some((_, c)) => driven.push(c),
                None => return Ok((None, allowed_all)),
            }
        }
        let Some(k) = refused else { break };
        // Somewhere on this leg is a road the server won't take (or can't be kept on without
        // passing a camera). Where exactly can't be told from a detour that may loop back over
        // the same roads, so close the whole leg on the map and re-plan from its start.
        let leg = &legs[k];
        for i in leg.span.0..leg.span.1 {
            map.graph.close(path.nodes[i], path.nodes[i + 1]);
        }
        log::info!(
            "road-map guidance: closed part {} of {} ({} segments), re-planning ({} of {MAX_REPLANS})",
            k + 1,
            legs.len(),
            leg.span.1 - leg.span.0,
            replan + 1
        );
        from_node = path.nodes[leg.span.0];
        from = leg.from;
    }
    match stitch(driven) {
        Some(route) => Ok((Some(evaluate(route, cameras)?), allowed_all)),
        None => Ok((None, allowed_all)),
    }
}

/// What phase 2 found.
struct RoadMapResult {
    check: RoadCheck,
    route: Option<Evaluated>,
    info: Option<RoadMapInfo>,
    /// Cameras the road-map path itself passes (unavoidable), plus those at the ends.
    allowed: HashSet<String>,
    warning: Option<String>,
}

/// Phase 2: search the road map for a camera-free path (or, only when there is none, the one
/// with the fewest cameras) and have the server drive it.
#[allow(clippy::too_many_arguments)]
async fn road_map_search<R, S, C>(
    router: &R,
    roads: &S,
    start: LatLon,
    end: LatLon,
    near_endpoint: &HashSet<String>,
    cameras: &C,
    requests: &mut u32,
    step: &(dyn Fn(u32, String) + Sync),
    opts: &PlanOptions,
) -> AppResult<RoadMapResult>
where
    R: Router,
    S: RoadSource,
    C: Fn(&BBox) -> AppResult<Vec<AvoidCamera>>,
{
    let unavailable = |reason: String, info: Option<RoadMapInfo>| {
        log::warn!("road-map check unavailable: {reason}");
        RoadMapResult { check: RoadCheck::Unavailable { reason }, route: None, info, allowed: HashSet::new(), warning: None }
    };
    let trip = haversine_m(start.lat, start.lon, end.lat, end.lon);
    if trip > ROAD_MAP_MAX_TRIP_M {
        return Ok(unavailable(
            format!("the trip is too long to check the road map (over {:.0} km)", ROAD_MAP_MAX_TRIP_M / 1000.0),
            None,
        ));
    }
    let margin_m = ROAD_MAP_MIN_BUFFER_M.max(trip * ROAD_MAP_BUFFER_FRACTION);
    let area = grow(&bbox_of(&[(start.lat, start.lon), (end.lat, end.lon)]), margin_m);
    let map_area = crate::roadnet::Area { bbox: area, start: (start.lat, start.lon), end: (end.lat, end.lon), margin_m };
    let at = *requests;
    step(at, "Loading the road map".into());
    let loaded = match roads.load(&map_area, &|m| step(at, m)).await {
        Ok(l) => l,
        Err(e) => {
            let reason = match &e {
                AppError::Offline(_) => "the road map couldn't be downloaded because you're offline".to_string(),
                AppError::RateLimited(_) => "the road map server (Overpass) is too busy right now; try again in a few minutes".to_string(),
                other => format!("the road map couldn't be loaded ({other})"),
            };
            return Ok(unavailable(reason, None));
        }
    };
    let info = RoadMapInfo {
        tiles: loaded.tiles,
        downloaded_tiles: loaded.downloaded,
        cached_tiles: loaded.cached,
        missing_tiles: loaded.tiles.saturating_sub(loaded.cached + loaded.downloaded),
        downloaded_bytes: loaded.bytes,
        ways: loaded.ways.len(),
    };
    step(at, "Searching the road map for a camera-free route".into());
    let cams = cameras(&area)?;
    let mut graph = crate::roadnet::build_graph(&loaded.ways, &cams, near_endpoint, AVOID_RADIUS_M);
    let partial = loaded.partial;
    drop(loaded);
    let snapped = (graph.nearest((start.lat, start.lon), true), graph.nearest((end.lat, end.lon), false));
    let partly = "only part of the road map could be downloaded because Overpass is busy; try again in a few minutes";
    let (Some((s, ds)), Some((t, dt))) = snapped else {
        return Ok(unavailable(if partial { partly } else { "the road map has no roads here" }.into(), Some(info)));
    };
    if (ds > ROAD_MAP_MAX_SNAP_M || dt > ROAD_MAP_MAX_SNAP_M) && partial {
        // The tile holding it just didn't arrive.
        return Ok(unavailable(partly.into(), Some(info)));
    }
    if ds > ROAD_MAP_MAX_SNAP_M || dt > ROAD_MAP_MAX_SNAP_M {
        return Ok(unavailable("the start or destination is too far from a mapped road".into(), Some(info)));
    }
    let (check, path, penalty) = if let Some(p) = graph.path(s, t, None) {
        // Real even on a partial map: every road it uses is there.
        (RoadCheck::CameraFree, p, None)
    } else if partial {
        // Missing tiles might hold the way around; claim nothing.
        return Ok(unavailable(partly.into(), Some(info)));
    } else if let Some(p) = graph.path(s, t, Some(CAMERA_PENALTY_SECS)) {
        let fewest = cameras_on_route(&p.pts, &cams, AVOID_RADIUS_M)
            .iter()
            .filter(|c| !near_endpoint.contains(&c.camera.key))
            .count();
        (RoadCheck::NoneExists { fewest }, p, Some(CAMERA_PENALTY_SECS))
    } else {
        return Ok(unavailable("the road map doesn't connect the start and destination".into(), Some(info)));
    };
    log::info!(
        "road map: {} ways, {} segments ({} within {} m of a camera); {:?}, path {:.1} km",
        info.ways,
        graph.segments,
        graph.blocked_segments,
        AVOID_RADIUS_M,
        check,
        crate::geo_util::polyline_length_m(&path.pts) / 1000.0
    );
    drop(path);
    let map = RoadMap { graph: &mut graph, t, penalty, cams: &cams, near_endpoint };
    let (route, allowed, warning) = match guide(router, map, s, start, end, cameras, requests, step, opts).await {
        Ok((r, allowed)) => (r, allowed, None),
        Err(e) => {
            log::warn!("road-map guidance stopped: {e}");
            (None, near_endpoint.clone(), Some(server_stopped(&e)))
        }
    };
    Ok(RoadMapResult { check, route, info: Some(info), allowed, warning })
}

fn server_stopped(e: &AppError) -> String {
    format!(
        "The routing server stopped answering part-way ({}), so this is the best route found before that.",
        match e {
            AppError::RateLimited(_) => "rate limited".to_string(),
            AppError::Offline(_) => "connection lost".to_string(),
            other => other.to_string(),
        }
    )
}

/// Why phase 1 stopped trying to exclude a camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GiveUp {
    /// Excluding it left no route.
    NoRoute,
    /// Outside the corridor, or past the exclusion cap.
    Skipped,
}

/// How much a plan may do. Planning a trip uses the defaults; a reroute while driving is
/// quicker (see nav/reroute.rs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlanOptions {
    /// Direction of travel at the start (a moving car), degrees clockwise from north.
    pub start_heading: Option<f64>,
    /// Phase 1 requests, including the fastest route.
    pub max_requests: u32,
    /// Phase 2 requests.
    pub guided_requests: u32,
    /// Direction of travel at the destination (a stretch cut from the middle of a route).
    pub end_heading: Option<f64>,
    /// Requests for fixing a long trip stretch by stretch.
    pub repair_requests: u32,
    /// A long trip's stretch is rerouted only if that adds at most this much time per camera
    /// avoided, seconds (`None`: no limit).
    pub max_extra_secs_per_camera: Option<f64>,
}

impl Default for PlanOptions {
    fn default() -> Self {
        PlanOptions {
            start_heading: None,
            max_requests: MAX_REQUESTS,
            guided_requests: MAX_GUIDED_REQUESTS,
            end_heading: None,
            repair_requests: repair::REPAIR_REQUESTS,
            max_extra_secs_per_camera: Some(repair::DEFAULT_MAX_EXTRA_SECS_PER_CAMERA),
        }
    }
}

/// Plan the fastest and the camera-avoiding route. `cameras` returns the cameras inside a
/// box (local store only); `roads` loads the road map for phase 2; `progress` is told before
/// each request.
pub(crate) async fn plan<R, S, C, P>(
    router: &R,
    roads: &S,
    server: &str,
    start: LatLon,
    end: LatLon,
    cameras: C,
    progress: P,
) -> AppResult<RoutePlan>
where
    R: Router,
    S: RoadSource,
    C: Fn(&BBox) -> AppResult<Vec<AvoidCamera>>,
    P: Fn(Progress) + Sync,
{
    plan_with(router, roads, server, start, end, cameras, progress, PlanOptions::default()).await
}

/// `plan` within the given limits.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn plan_with<R, S, C, P>(
    router: &R,
    roads: &S,
    server: &str,
    start: LatLon,
    end: LatLon,
    cameras: C,
    progress: P,
    opts: PlanOptions,
) -> AppResult<RoutePlan>
where
    R: Router,
    S: RoadSource,
    C: Fn(&BBox) -> AppResult<Vec<AvoidCamera>>,
    P: Fn(Progress) + Sync,
{
    let max_requests = opts.max_requests.max(1);
    let between = |exclude: &[LatLon]| {
        let mut req = RouteRequest::between(start, end, exclude);
        req.locations[0].heading = opts.start_heading;
        req
    };
    if !valid_coord(start.lat, start.lon) || !valid_coord(end.lat, end.lon) {
        return Err(AppError::Invalid("invalid start or destination".into()));
    }
    if haversine_m(start.lat, start.lon, end.lat, end.lon) < MIN_TRIP_M {
        return Err(AppError::Invalid(
            "Start and destination are the same place, or too close together to route.".into(),
        ));
    }
    let step = |n: u32, message: String| progress(Progress { step: n, max: max_requests, message });

    // Cameras whose avoid zone holds the start or destination are counted, never excluded.
    let mut near_endpoint: HashSet<String> = HashSet::new();
    for p in [start, end] {
        let around = grow(&BBox::new(p.lat, p.lon, p.lat, p.lon), AVOID_RADIUS_M * 2.0);
        for c in cameras(&around)? {
            if haversine_m(c.lat, c.lon, p.lat, p.lon) <= AVOID_RADIUS_M {
                near_endpoint.insert(c.key);
            }
        }
    }
    let corridor = corridor(start, end);

    // ---- Phase 1: exclude the cameras the proposed routes pass. ----
    step(1, "Finding the fastest route".into());
    let first = match router.route(&between(&[])).await {
        Ok(c) => c,
        Err(RouteError::NoRoute) => {
            return Err(AppError::Invalid("No driving route was found between these points.".into()))
        }
        Err(RouteError::TooManyExclusions(_)) => {
            return Err(AppError::Other("the routing server rejected a request with no exclusions".into()))
        }
        Err(RouteError::Api(e)) => return Err(e),
    };
    let mut requests = 1u32;
    let mut evaluated = first.into_iter().map(|c| evaluate(c, &cameras)).collect::<AppResult<Vec<_>>>()?;
    let fastest = evaluated.remove(0);
    let fastest_rank = fastest.rank();
    // The best route other than the fastest so far (alternates count), and the exclusions
    // that produced it.
    let mut best: Option<Evaluated> = best_of(evaluated);
    let mut best_exclusions: HashSet<String> = HashSet::new();
    // The route the next exclusions are taken from: the fewest cameras in the latest answer.
    let mut current = match &best {
        Some(b) if b.better_than(&fastest) => b.clone(),
        _ => fastest.clone(),
    };

    let mut excluded: Vec<AvoidCamera> = Vec::new();
    let mut excluded_keys: HashSet<String> = HashSet::new();
    let mut given_up: HashMap<String, GiveUp> = HashMap::new();
    let mut last_batch: Vec<AvoidCamera> = Vec::new();
    let mut limits = Limits::default();
    // Retry the current exclusions (after giving one up) instead of adding a new batch.
    let mut retry = false;
    let mut warning = None;

    loop {
        if !retry {
            let mut batch = Vec::new();
            for rc in &current.cameras {
                let key = &rc.camera.key;
                if near_endpoint.contains(key) || excluded_keys.contains(key) || given_up.contains_key(key) {
                    continue;
                }
                if !corridor.contains(rc.camera.lat, rc.camera.lon) {
                    given_up.insert(key.clone(), GiveUp::Skipped);
                } else if excluded.len() + batch.len() >= MAX_EXCLUSIONS {
                    given_up.insert(key.clone(), GiveUp::Skipped);
                    if !limits.exclusion_cap {
                        log::warn!("avoidance search reached the {MAX_EXCLUSIONS}-exclusion limit of the routing server");
                    }
                    limits.exclusion_cap = true;
                } else {
                    batch.push(rc.camera.clone());
                }
            }
            if batch.is_empty() {
                break;
            }
            for c in &batch {
                excluded_keys.insert(c.key.clone());
            }
            excluded.extend(batch.iter().cloned());
            last_batch = batch;
        }
        if requests >= max_requests {
            // This batch is never asked about.
            for c in &last_batch {
                excluded_keys.remove(&c.key);
            }
            log::warn!("avoidance search used its {max_requests} requests with cameras still on the route");
            limits.request_budget = true;
            break;
        }
        let n = excluded.len();
        step(requests + 1, format!("Routing around {n} camera{}", if n == 1 { "" } else { "s" }));
        let points: Vec<LatLon> = excluded.iter().map(|c| LatLon { lat: c.lat, lon: c.lon }).collect();
        let answer = router.route(&between(&points)).await;
        requests += 1;
        match answer {
            Ok(cands) => {
                retry = false;
                let list = cands.into_iter().map(|c| evaluate(c, &cameras)).collect::<AppResult<Vec<_>>>()?;
                let top = best_of(list).expect("a routing answer has a trip");
                if best.as_ref().map_or(true, |b| top.better_than(b)) {
                    best = Some(top.clone());
                    best_exclusions = excluded_keys.clone();
                }
                current = top;
            }
            Err(RouteError::NoRoute) => {
                // Something in the last batch can't be driven around. Give up the camera
                // closest to either end first (a destination's only access road, say).
                let dist = |c: &AvoidCamera| {
                    haversine_m(c.lat, c.lon, start.lat, start.lon).min(haversine_m(c.lat, c.lon, end.lat, end.lon))
                };
                let Some((i, _)) = last_batch
                    .iter()
                    .enumerate()
                    .min_by(|a, b| dist(a.1).total_cmp(&dist(b.1)))
                else {
                    break;
                };
                let dropped = last_batch.remove(i);
                excluded.retain(|c| c.key != dropped.key);
                excluded_keys.remove(&dropped.key);
                given_up.insert(dropped.key, GiveUp::NoRoute);
                // An empty batch leaves the exclusions of `current`, which is already known.
                retry = !last_batch.is_empty();
            }
            Err(RouteError::TooManyExclusions(limit)) => {
                // A server with a lower limit than this app assumes: never silently.
                log::warn!("the routing server accepts fewer excluded cameras than sent ({n}; its limit: {limit:?})");
                for c in &last_batch {
                    excluded_keys.remove(&c.key);
                    given_up.insert(c.key.clone(), GiveUp::Skipped);
                }
                limits.exclusion_cap = true;
                break;
            }
            Err(RouteError::Api(e)) => {
                log::warn!("avoidance routing stopped early: {e}");
                // The last batch was never answered for.
                for c in &last_batch {
                    excluded_keys.remove(&c.key);
                }
                warning = Some(server_stopped(&e));
                break;
            }
        }
    }

    // The fastest route competes too: avoiding may not have helped.
    let (mut avoid, mut avoid_exclusions) = match best {
        Some(b) if b.better_than(&fastest) => (b, best_exclusions),
        _ => (fastest.clone(), HashSet::new()),
    };

    // ---- Phase 2: cameras left that aren't at the start or destination. ----
    let mut road_check = RoadCheck::NotNeeded;
    let mut road_map = None;
    let mut allowed = HashSet::new();
    let mut avoid_from_road_map = false;
    // Cameras a stretch's road map shows no way around (long trips).
    let mut unavoidable_keys: HashSet<String> = HashSet::new();
    // Cameras the only ways round are too slow for (long trips).
    let mut slow_keys: HashSet<String> = HashSet::new();
    let cameras_left = warning.is_none() && avoid.cameras.iter().any(|c| !near_endpoint.contains(&c.camera.key));
    let long_trip = haversine_m(start.lat, start.lon, end.lat, end.lon) > ROAD_MAP_MAX_TRIP_M;
    if cameras_left && long_trip && opts.repair_requests > 0 {
        // Too long for one road map: fix the stretches with cameras one at a time.
        let repair_step = |n: u32, message: String| {
            progress(Progress { step: n, max: max_requests + opts.repair_requests, message })
        };
        let r = repair::repair(router, roads, &avoid, start, end, &near_endpoint, &cameras, &mut requests, &repair_step, &opts).await?;
        road_check = RoadCheck::Stretches {
            fixed: r.fixed,
            total: r.stretches,
            too_slow: r.too_slow,
            limit_min: opts.max_extra_secs_per_camera.map(|s| (s / 60.0).round() as u32),
        };
        slow_keys = r.slow;
        road_map = r.road_map;
        warning = r.warning;
        unavoidable_keys = r.unavoidable;
        if let Some(route) = r.route {
            log::info!(
                "stretch repair: {} of {} stretches fixed; {} cameras, {:.0} min (phase 1 best: {} cameras)",
                r.fixed,
                r.stretches,
                route.cameras.len(),
                route.candidate.duration_s / 60.0,
                avoid.cameras.len()
            );
            if route.better_than(&avoid) {
                avoid = route;
                avoid_exclusions = HashSet::new();
                avoid_from_road_map = true;
            }
        }
    } else if cameras_left {
        let guided_step = |n: u32, message: String| {
            progress(Progress { step: n, max: max_requests + opts.guided_requests, message })
        };
        let found = road_map_search(router, roads, start, end, &near_endpoint, &cameras, &mut requests, &guided_step, &opts).await?;
        road_check = found.check;
        road_map = found.info;
        allowed = found.allowed;
        warning = found.warning;
        if let Some(route) = found.route {
            log::info!(
                "road-map route: {} cameras, {:.0} min (phase 1 best: {} cameras)",
                route.cameras.len(),
                route.candidate.duration_s / 60.0,
                avoid.cameras.len()
            );
            if route.better_than(&avoid) {
                avoid = route;
                avoid_exclusions = HashSet::new();
                avoid_from_road_map = true;
            }
        }
    }

    let same_route = avoid.candidate == fastest.candidate;
    let excluded_count = avoid_exclusions.len();
    let mut avoid = planned(avoid);
    let unavoidable = matches!(road_check, RoadCheck::NoneExists { .. });
    for rc in &mut avoid.cameras {
        let key = &rc.camera.key;
        rc.remaining = Some(if near_endpoint.contains(key) {
            Remaining::NearEndpoint
        } else if (unavoidable && allowed.contains(key)) || unavoidable_keys.contains(key) {
            Remaining::Unavoidable
        } else if slow_keys.contains(key) {
            Remaining::LongDetour
        } else if avoid_exclusions.contains(key) {
            Remaining::Nearby
        } else if !avoid_from_road_map
            && given_up.get(key) == Some(&GiveUp::NoRoute)
            && !matches!(road_check, RoadCheck::CameraFree)
        {
            Remaining::NoRoute
        } else {
            Remaining::SearchLimit
        });
    }
    let outcome = if avoid.cameras.is_empty() {
        Outcome::Clear
    } else if avoid.cameras.len() < fastest_rank.0 {
        Outcome::Reduced
    } else {
        Outcome::Unchanged
    };
    let fastest = planned(fastest);
    Ok(RoutePlan {
        long_detour: !same_route && is_long_detour(fastest.duration_s, avoid.duration_s),
        fastest,
        avoid,
        same_route,
        outcome,
        requests,
        excluded: excluded_count,
        warning,
        server: host_of(server),
        road_check,
        avoid_from_road_map,
        limits,
        road_map,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roadnet::{Loaded, OverpassRoads, Way};
    use std::cell::RefCell;

    fn encode6(points: &[(f64, f64)]) -> String {
        let mut out = String::new();
        let (mut plat, mut plon) = (0i64, 0i64);
        for &(lat, lon) in points {
            let (ilat, ilon) = ((lat * 1e6).round() as i64, (lon * 1e6).round() as i64);
            for mut v in [ilat - plat, ilon - plon] {
                v = if v < 0 { !(v << 1) } else { v << 1 };
                while v >= 0x20 {
                    out.push((((v & 0x1f) | 0x20) + 63) as u8 as char);
                    v >>= 5;
                }
                out.push((v + 63) as u8 as char);
            }
            plat = ilat;
            plon = ilon;
        }
        out
    }

    fn cam(key: &str, lat: f64, lon: f64) -> AvoidCamera {
        AvoidCamera {
            key: key.into(),
            lat,
            lon,
            category: "flock".into(),
            source: "osm".into(),
            direction: None,
            operator: None,
        }
    }

    fn candidate(points: &[(f64, f64)], duration_s: f64) -> Candidate {
        let shape = points.to_vec();
        Candidate {
            distance_m: crate::geo_util::polyline_length_m(&shape),
            shape,
            duration_s,
            maneuvers: Vec::new(),
        }
    }

    fn maneuver(kind: u32, instruction: &str, distance_m: f64) -> Maneuver {
        Maneuver { instruction: instruction.into(), kind, distance_m, duration_s: distance_m / 10.0, ..Default::default() }
    }

    #[test]
    fn decodes_polyline6() {
        let pts = vec![(39.7392, -104.9903), (39.74, -104.99), (39.6133, -105.0166)];
        let decoded = decode_polyline6(&encode6(&pts)).unwrap();
        assert_eq!(decoded.len(), 3);
        for (a, b) in pts.iter().zip(&decoded) {
            assert!((a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6);
        }
        assert!(decode_polyline6("_").is_err(), "truncated input is an error, not a panic");
        assert!(decode_polyline6("").unwrap().is_empty());
    }

    #[test]
    fn parses_trip_alternates_and_maneuvers() {
        let a = encode6(&[(39.0, -105.0), (39.001, -105.0), (39.002, -105.0)]);
        let b = encode6(&[(39.0, -105.0), (39.0, -105.001), (39.002, -105.0)]);
        let body = format!(
            r#"{{"trip":{{"legs":[{{"shape":"{a}","maneuvers":[
                {{"type":1,"instruction":"Drive north.","length":0.1,"time":10,"begin_shape_index":0}},
                {{"type":4,"instruction":"You have arrived.","length":0,"time":0,"begin_shape_index":2}}]}}],
              "summary":{{"length":0.222,"time":30.5}}}},
              "alternates":[{{"trip":{{"legs":[{{"shape":"{b}","maneuvers":[]}}],"summary":{{"length":0.3,"time":40}}}}}},
                            {{"trip":{{"legs":[]}}}}]}}"#
        );
        let c = parse_route_response(&body).unwrap();
        assert_eq!(c.len(), 2, "the malformed alternate is dropped");
        assert!((c[0].distance_m - 222.0).abs() < 1e-6);
        assert_eq!(c[0].duration_s, 30.5);
        assert_eq!(c[0].maneuvers.len(), 2);
        assert_eq!(c[0].maneuvers[1].kind, 4);
        assert!((c[0].maneuvers[1].lat - 39.002).abs() < 1e-9);
        assert!((c[0].maneuvers[0].distance_m - 100.0).abs() < 1e-6);
        assert!(parse_route_response("{}").is_err());
        assert!(parse_route_response("not json").is_err());
    }

    #[test]
    fn classifies_valhalla_errors() {
        let no_path = r#"{"error_code":442,"error":"No path could be found for input","status_code":400}"#;
        assert!(matches!(classify_error(400, no_path), RouteError::NoRoute));
        let no_edge = r#"{"error_code":171,"error":"No suitable edges near location"}"#;
        assert!(matches!(classify_error(400, no_edge), RouteError::Api(AppError::Invalid(m)) if m.contains("No road")));
        let islands = r#"{"error_code":170,"error":"Locations are in unconnected regions. Go check/edit the map at osm.org"}"#;
        assert!(matches!(classify_error(400, islands), RouteError::Api(AppError::Invalid(m)) if m.contains("don't join")));
        let far = r#"{"error_code":154,"error":"Path distance exceeds the max distance limit: 1500000 meters"}"#;
        assert!(matches!(classify_error(400, far), RouteError::Api(AppError::Invalid(m)) if m.ends_with("allows (1500 km).")));
        let cap = r#"{"error_code":157,"error":"Exceeded max avoid locations: 25"}"#;
        assert!(matches!(classify_error(400, cap), RouteError::TooManyExclusions(Some(25))));
        match classify_error(500, "<html>oops</html>") {
            RouteError::Api(AppError::Http { status, .. }) => assert_eq!(status, 500),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn request_body_carries_exclusions_with_radius() {
        let s = LatLon { lat: 1.0, lon: 2.0 };
        let body = request_body(&RouteRequest::between(s, s, &[]));
        assert!(body.get("exclude_locations").is_none());
        assert_eq!(body["alternates"], 2);
        let body = request_body(&RouteRequest::between(s, s, &[LatLon { lat: 3.0, lon: 4.0 }]));
        assert_eq!(body["exclude_locations"][0]["radius"], 30);
        assert_eq!(body["costing"], "auto");
        assert_eq!(body["locations"][0]["type"], "break");
    }

    #[test]
    fn guided_requests_pass_through_waypoints_with_headings() {
        let req = RouteRequest {
            locations: vec![
                Waypoint::at(LatLon { lat: 1.0, lon: 2.0 }),
                Waypoint { lat: 1.5, lon: 2.5, through: true, heading: Some(359.7) },
                Waypoint { lat: 2.0, lon: 3.0, through: false, heading: Some(-90.0) },
            ],
            exclude: Vec::new(),
            alternates: 0,
        };
        let body = request_body(&req);
        assert!(body.get("alternates").is_none(), "alternates aren't asked for on multi-point routes");
        assert_eq!(body["locations"][1]["type"], "through");
        assert_eq!(body["locations"][1]["heading"], 0, "359.7° rounds to north");
        assert_eq!(body["locations"][2]["heading"], 270);
        assert!(body["locations"][0].get("heading").is_none());
    }

    #[test]
    fn bearings() {
        assert!((bearing((39.0, -105.0), (39.01, -105.0)) - 0.0).abs() < 0.01);
        assert!((bearing((39.0, -105.0), (39.0, -104.99)) - 90.0).abs() < 0.1);
        assert!((bearing((39.0, -105.0), (38.99, -105.0)) - 180.0).abs() < 0.01);
        assert!((bearing((39.0, -105.0), (39.0, -105.01)) - 270.0).abs() < 0.1);
    }

    #[test]
    fn stitching_drops_the_stops_at_the_joins() {
        let mut a = candidate(&[(39.0, -105.0), (39.0, -104.99)], 60.0);
        a.maneuvers = vec![maneuver(1, "Drive east.", 500.0), maneuver(10, "Turn right.", 363.0), maneuver(4, "Arrive.", 0.0)];
        let mut b = candidate(&[(39.0, -104.99), (39.01, -104.99)], 90.0);
        b.maneuvers = vec![maneuver(2, "Drive north.", 200.0), maneuver(15, "Turn left.", 912.0), maneuver(5, "Arrive on the right.", 0.0)];
        let s = stitch(vec![a, b]).unwrap();
        assert_eq!(s.shape.len(), 3, "the shared point appears once");
        assert_eq!(s.duration_s, 150.0);
        let kinds: Vec<u32> = s.maneuvers.iter().map(|m| m.kind).collect();
        assert_eq!(kinds, [1, 10, 15, 5]);
        assert_eq!(s.maneuvers[1].distance_m, 563.0, "the dropped start's distance joins the step before it");
        assert!(stitch(Vec::new()).is_none());
    }

    #[test]
    fn legs_follow_the_path_between_the_real_ends() {
        // 10.8 km straight east with a vertex every 100 m.
        let path: Vec<(f64, f64)> = (0..=100).map(|i| (39.0, -105.0 + i as f64 * 0.00116)).collect();
        let start = LatLon { lat: 39.0003, lon: -105.0 };
        let end = LatLon { lat: 39.0003, lon: -104.884 };
        let legs = plan_legs(&path, Waypoint::at(start), end, None);
        assert_eq!(legs.len(), 3, "about 4 km a leg");
        assert_eq!(legs[0].span.0, 0);
        assert_eq!(legs[2].span.1, path.len() - 1);
        assert_eq!(legs[0].span.1, legs[1].span.0, "legs meet at a path vertex");
        assert_eq!((legs[0].from.lat, legs[0].from.lon), (start.lat, start.lon));
        assert_eq!((legs[2].to.lat, legs[2].to.lon), (end.lat, end.lon));
        // Joins are the same path vertex, entered and left heading east.
        assert_eq!((legs[0].to.lat, legs[0].to.lon), (legs[1].from.lat, legs[1].from.lon));
        assert!((legs[0].to.heading.unwrap() - 90.0).abs() < 1.0);
        assert!(!legs[1].from.through);
        for leg in &legs {
            assert!(!leg.vias.is_empty() && leg.vias.len() <= VIAS_PER_LEG);
            for v in &leg.vias {
                assert!(v.through);
                // Mid-segment: 50 m from a vertex, never on one.
                let off = (v.lon + 105.0) / 0.00116;
                assert!((off - off.floor() - 0.5).abs() < 1e-6, "{off}");
            }
        }
        assert_eq!(plan_legs(&path[..1], Waypoint::at(start), end, None).len(), 1);
    }

    #[test]
    fn finds_cameras_near_the_route_in_order() {
        // A straight east–west road at lat 39.
        let shape = [(39.0, -105.0), (39.0, -104.99), (39.0, -104.98)];
        let m_per_deg_lat = 111_320.0;
        let cams = vec![
            cam("node/2", 39.0 + 20.0 / m_per_deg_lat, -104.985), // 20 m off, second
            cam("node/1", 39.0 - 10.0 / m_per_deg_lat, -104.995), // 10 m off, first
            cam("node/3", 39.0 + 45.0 / m_per_deg_lat, -104.99),  // 45 m off: not passed
            cam("node/4", 39.0, -104.97),                         // beyond the end
        ];
        let hits = cameras_on_route(&shape, &cams, AVOID_RADIUS_M);
        let keys: Vec<_> = hits.iter().map(|h| h.camera.key.as_str()).collect();
        assert_eq!(keys, ["node/1", "node/2"]);
        assert!((hits[0].distance_m - 10.0).abs() < 0.5);
        assert!(hits[0].along_m < hits[1].along_m);
    }

    #[test]
    fn merges_submissions_without_duplicates() {
        let bbox = BBox::new(38.0, -106.0, 40.0, -104.0);
        let osm = vec![Camera {
            osm_type: "node".into(),
            osm_id: 5,
            lat: 39.0,
            lon: -105.0,
            category: "flock".into(),
            tags: [("direction".to_string(), "90".to_string())].into_iter().collect(),
            first_seen: 0,
            last_seen: 0,
            stale_since: None,
        }];
        let mut stale = osm[0].clone();
        stale.osm_id = 6;
        stale.lat = 39.5;
        stale.stale_since = Some(1);
        let sub = |id: i64, lat: f64| Submission {
            id,
            lat,
            lon: -105.0,
            category: "alpr".into(),
            direction: Some(180),
            mount: None,
            operator: None,
            notes: None,
            status: "local".into(),
            osm_element_id: None,
            created_at: 0,
            updated_at: 0,
        };
        let subs = vec![sub(1, 39.00005), sub(2, 39.2), sub(3, 45.0)];
        let merged = merge_cameras(vec![osm[0].clone(), stale], &subs, &bbox);
        let keys: Vec<_> = merged.iter().map(|c| c.key.as_str()).collect();
        // Stale camera dropped; submission 1 is the mapped camera; 3 is outside the box.
        assert_eq!(keys, ["node/5", "submission/2"]);
        assert_eq!(merged[0].direction.as_deref(), Some("90"));
        assert_eq!(merged[1].source, "submission");
        assert_eq!(merged[1].direction.as_deref(), Some("180"));
    }

    #[test]
    fn detour_thresholds() {
        assert!(!is_long_detour(1200.0, 1500.0));
        assert!(is_long_detour(1200.0, 1900.0), "more than 50% longer");
        assert!(is_long_detour(3600.0, 3600.0 + 21.0 * 60.0), "more than 20 min longer");
        assert!(!is_long_detour(3600.0, 3600.0 + 19.0 * 60.0));
    }

    #[test]
    fn endpoint_normalization() {
        assert_eq!(normalize_endpoint(""), DEFAULT_ENDPOINT);
        assert_eq!(normalize_endpoint(" https://x.org/ "), "https://x.org");
        assert_eq!(normalize_endpoint("https://x.org/route"), "https://x.org");
    }

    // -----------------------------------------------------------------------
    // Planner against fakes
    // -----------------------------------------------------------------------

    /// Two ways from west to east: a direct road along lat 39.00 and a detour along 39.01.
    /// The fake server avoids a road when an exclusion lies on it.
    struct TwoRoads {
        direct: Vec<(f64, f64)>,
        detour: Vec<(f64, f64)>,
        /// Excluding a point near here leaves no route at all.
        blocker: Option<(f64, f64)>,
        calls: RefCell<Vec<usize>>,
        fail_after: Option<usize>,
        /// Answer with every open road, not just the quickest.
        alternates: bool,
    }

    impl TwoRoads {
        fn new() -> Self {
            TwoRoads {
                direct: vec![(39.0, -105.0), (39.0, -104.95), (39.0, -104.9)],
                detour: vec![(39.0, -105.0), (39.01, -105.0), (39.01, -104.9), (39.0, -104.9)],
                blocker: None,
                calls: RefCell::new(Vec::new()),
                fail_after: None,
                alternates: false,
            }
        }
    }

    impl Router for TwoRoads {
        async fn route(&self, req: &RouteRequest) -> Result<Vec<Candidate>, RouteError> {
            let exclude = &req.exclude;
            self.calls.borrow_mut().push(exclude.len());
            if self.fail_after.is_some_and(|n| self.calls.borrow().len() > n) {
                return Err(RouteError::Api(AppError::RateLimited("fake".into())));
            }
            let on = |road: &[(f64, f64)]| {
                exclude.iter().any(|p| {
                    point_to_polyline(p.lat, p.lon, road).is_some_and(|h| h.distance_m <= AVOID_RADIUS_M)
                })
            };
            if let Some(b) = self.blocker {
                if exclude.iter().any(|p| haversine_m(p.lat, p.lon, b.0, b.1) < 5.0) {
                    return Err(RouteError::NoRoute);
                }
            }
            let mut out = Vec::new();
            if !on(&self.direct) {
                out.push(candidate(&self.direct, 600.0));
            }
            if !on(&self.detour) {
                out.push(candidate(&self.detour, 780.0));
            }
            if out.is_empty() {
                return Err(RouteError::NoRoute);
            }
            if !self.alternates {
                out.truncate(1);
            }
            Ok(out)
        }
    }

    /// No road map (offline): phase 2 reports itself unavailable.
    struct NoRoads;

    impl RoadSource for NoRoads {
        async fn load(&self, _: &crate::roadnet::Area, _: &(dyn Fn(String) + Sync)) -> AppResult<Loaded> {
            Err(AppError::Offline("fake".into()))
        }
    }

    /// A fixed road map.
    struct FixedRoads(Vec<Way>);

    impl RoadSource for FixedRoads {
        async fn load(&self, _: &crate::roadnet::Area, _: &(dyn Fn(String) + Sync)) -> AppResult<Loaded> {
            Ok(Loaded { ways: self.0.clone(), tiles: 1, downloaded: 1, bytes: 1234, partial: false, cached: 0 })
        }
    }

    const START: LatLon = LatLon { lat: 39.0, lon: -105.0 };
    const END: LatLon = LatLon { lat: 39.0, lon: -104.9 };

    fn run_with<R: Router, S: RoadSource>(router: &R, roads: &S, cams: Vec<AvoidCamera>) -> RoutePlan {
        let source = |b: &BBox| Ok(cams.iter().filter(|c| b.contains(c.lat, c.lon)).cloned().collect());
        let fut = plan(router, roads, DEFAULT_ENDPOINT, START, END, source, |_| {});
        tauri::async_runtime::block_on(fut).unwrap()
    }

    fn run<R: Router>(router: &R, cams: Vec<AvoidCamera>) -> RoutePlan {
        run_with(router, &NoRoads, cams)
    }

    #[test]
    fn avoids_cameras_on_the_direct_road() {
        let router = TwoRoads::new();
        let plan = run(&router, vec![cam("node/1", 39.0, -104.95), cam("node/2", 39.0001, -104.93)]);
        assert_eq!(plan.fastest.cameras.len(), 2);
        assert_eq!(plan.avoid.cameras.len(), 0);
        assert_eq!(plan.outcome, Outcome::Clear);
        assert!(!plan.same_route);
        assert_eq!(plan.excluded, 2);
        assert!(!plan.long_detour, "13 min vs 10 min is not a long detour");
        assert_eq!(plan.server, "valhalla1.openstreetmap.de");
        assert_eq!(*router.calls.borrow(), [0, 2]);
        assert_eq!(plan.road_check, RoadCheck::NotNeeded, "phase 1 was enough");
        assert!(!plan.avoid_from_road_map);
        assert_eq!(plan.limits, Limits::default());
    }

    #[test]
    fn a_camera_free_alternate_answers_in_one_request() {
        let mut router = TwoRoads::new();
        router.alternates = true;
        let plan = run(&router, vec![cam("node/1", 39.0, -104.95)]);
        assert_eq!(plan.outcome, Outcome::Clear);
        assert!(!plan.same_route);
        assert_eq!(plan.requests, 1);
        assert_eq!(plan.excluded, 0);
    }

    #[test]
    fn camera_free_fastest_route_needs_one_request() {
        let router = TwoRoads::new();
        let plan = run(&router, vec![cam("node/9", 39.01, -104.95)]);
        assert!(plan.same_route);
        assert_eq!(plan.outcome, Outcome::Clear);
        assert_eq!(plan.requests, 1);
    }

    #[test]
    fn a_camera_the_server_cant_route_around_says_so_when_the_map_is_unavailable() {
        // Cameras on both roads; no road map to settle whether a way around exists.
        let router = TwoRoads::new();
        let plan = run(
            &router,
            vec![cam("node/1", 39.0, -104.95), cam("node/2", 39.0, -104.94), cam("node/3", 39.01, -104.95)],
        );
        assert_eq!(plan.fastest.cameras.len(), 2);
        assert_eq!(plan.avoid.cameras.len(), 1, "the detour, with one camera");
        assert_eq!(plan.outcome, Outcome::Reduced);
        assert_eq!(plan.avoid.cameras[0].camera.key, "node/3");
        assert_eq!(plan.avoid.cameras[0].remaining, Some(Remaining::NoRoute));
        assert!(matches!(plan.road_check, RoadCheck::Unavailable { ref reason } if reason.contains("offline")));
        assert!(plan.requests <= MAX_REQUESTS);
    }

    #[test]
    fn blocking_camera_is_given_up_and_the_rest_avoided() {
        let mut router = TwoRoads::new();
        // node/1 sits in the corner where both roads leave the start, 35 m from it (outside
        // its avoid zone) but within 30 m of both. Excluding it leaves no route.
        let corner = (39.00025, -104.99975);
        router.blocker = Some(corner);
        let plan = run(&router, vec![cam("node/1", corner.0, corner.1), cam("node/2", 39.0, -104.95)]);
        // node/1 stays; node/2 is avoided.
        assert!(plan.avoid.cameras.iter().all(|c| c.camera.key != "node/2"));
        let n1 = plan.avoid.cameras.iter().find(|c| c.camera.key == "node/1").unwrap();
        assert_eq!(n1.remaining, Some(Remaining::NoRoute));
    }

    #[test]
    fn camera_at_the_destination_is_never_excluded() {
        let router = TwoRoads::new();
        let plan = run(&router, vec![cam("node/7", 39.0001, -104.9)]);
        assert_eq!(*router.calls.borrow(), [0], "no exclusion request for it");
        assert_eq!(plan.avoid.cameras[0].remaining, Some(Remaining::NearEndpoint));
        assert_eq!(plan.outcome, Outcome::Unchanged);
        assert_eq!(plan.road_check, RoadCheck::NotNeeded, "nothing the road map could help with");
    }

    #[test]
    fn a_failing_follow_up_request_keeps_the_best_so_far() {
        let mut router = TwoRoads::new();
        router.fail_after = Some(1);
        let plan = run(&router, vec![cam("node/1", 39.0, -104.95)]);
        assert!(plan.warning.as_deref().unwrap().contains("rate limited"));
        // Nothing better than the fastest route was found before the failure.
        assert!(plan.same_route);
        assert_eq!(plan.outcome, Outcome::Unchanged);
        assert_eq!(plan.avoid.cameras[0].remaining, Some(Remaining::SearchLimit));
    }

    #[test]
    fn too_close_is_rejected_before_any_request() {
        let router = TwoRoads::new();
        let fut = plan(&router, &NoRoads, DEFAULT_ENDPOINT, START, START, |_: &BBox| Ok(Vec::new()), |_| {});
        let err = tauri::async_runtime::block_on(fut).unwrap_err();
        assert!(matches!(err, AppError::Invalid(_)));
        assert!(router.calls.borrow().is_empty());
    }

    #[test]
    fn no_route_at_all_is_a_readable_error() {
        struct Nothing;
        impl Router for Nothing {
            async fn route(&self, _: &RouteRequest) -> Result<Vec<Candidate>, RouteError> {
                Err(RouteError::NoRoute)
            }
        }
        let fut = plan(&Nothing, &NoRoads, DEFAULT_ENDPOINT, START, END, |_: &BBox| Ok(Vec::new()), |_| {});
        let err = tauri::async_runtime::block_on(fut).unwrap_err();
        assert!(err.to_string().contains("No driving route"));
    }

    #[test]
    fn a_lower_exclusion_limit_on_the_server_is_reported_not_hidden() {
        struct LowCap(RefCell<u32>);
        impl Router for LowCap {
            async fn route(&self, req: &RouteRequest) -> Result<Vec<Candidate>, RouteError> {
                *self.0.borrow_mut() += 1;
                if !req.exclude.is_empty() {
                    return Err(RouteError::TooManyExclusions(Some(0)));
                }
                Ok(vec![candidate(&[(39.0, -105.0), (39.0, -104.95), (39.0, -104.9)], 600.0)])
            }
        }
        let plan = run(&LowCap(RefCell::new(0)), vec![cam("node/1", 39.0, -104.95)]);
        assert!(plan.limits.exclusion_cap);
        assert_eq!(plan.avoid.cameras[0].remaining, Some(Remaining::SearchLimit));
    }

    /// The failure this phase exists for: a server that keeps proposing the camera road
    /// however many cameras are excluded (in a dense metro, every new proposal passes new
    /// cameras). Asked to go through waypoints, it follows them.
    struct Stubborn {
        calls: RefCell<Vec<usize>>,
    }

    impl Router for Stubborn {
        async fn route(&self, req: &RouteRequest) -> Result<Vec<Candidate>, RouteError> {
            self.calls.borrow_mut().push(req.locations.len());
            if req.locations.len() == 2 && req.alternates > 0 {
                return Ok(vec![candidate(&[(39.0, -105.0), (39.0, -104.95), (39.0, -104.9)], 600.0)]);
            }
            let pts: Vec<(f64, f64)> = req.locations.iter().map(|w| (w.lat, w.lon)).collect();
            let mut c = candidate(&pts, 0.0);
            c.duration_s = c.distance_m / 13.0;
            c.maneuvers = vec![maneuver(1, "Drive.", c.distance_m), maneuver(4, "Arrive.", 0.0)];
            Ok(vec![c])
        }
    }

    fn ladder(parallel_oneway: i8) -> Vec<Way> {
        let w = |id: i64, oneway: i8, pts: &[(f64, f64)]| Way {
            id,
            oneway,
            kmh: 50,
            pts: pts.iter().map(|&(a, b)| ((a * 1e7).round() as i32, (b * 1e7).round() as i32)).collect(),
        };
        vec![
            w(1, 0, &[(39.0, -105.0), (39.0, -104.95), (39.0, -104.9)]),
            w(2, 0, &[(39.0, -105.0), (39.005, -105.0), (39.01, -105.0)]),
            w(3, parallel_oneway, &[(39.01, -105.0), (39.01, -104.975), (39.01, -104.95), (39.01, -104.925), (39.01, -104.9)]),
            w(4, 0, &[(39.01, -104.9), (39.005, -104.9), (39.0, -104.9)]),
        ]
    }

    #[test]
    fn the_road_map_finds_the_camera_free_route_phase_1_missed() {
        let router = Stubborn { calls: RefCell::new(Vec::new()) };
        let plan = run_with(&router, &FixedRoads(ladder(0)), vec![cam("node/1", 39.0001, -104.95)]);
        assert_eq!(plan.fastest.cameras.len(), 1);
        assert_eq!(plan.road_check, RoadCheck::CameraFree);
        assert!(plan.avoid_from_road_map);
        assert_eq!(plan.outcome, Outcome::Clear);
        assert!(plan.avoid.cameras.is_empty());
        // The count shown is the rule applied to the geometry shown.
        let shape: Vec<(f64, f64)> = plan.avoid.shape.iter().map(|p| (p[0], p[1])).collect();
        assert!(cameras_on_route(&shape, &[cam("node/1", 39.0001, -104.95)], AVOID_RADIUS_M).is_empty());
        assert!(plan.avoid.shape.iter().any(|p| p[0] > 39.009), "along the parallel road");
        let guided = router.calls.borrow().iter().filter(|&&n| n > 2).count();
        assert!(guided >= 2, "driven in legs through waypoints");
        assert!(plan.road_map.as_ref().is_some_and(|m| m.ways == 4));
        assert_eq!(plan.avoid.maneuvers.iter().filter(|m| (4..=6).contains(&m.kind)).count(), 1, "one arrival");
    }

    /// Like `Stubborn`, but it won't drive the northern detour (a turn restriction the road map
    /// doesn't know about): any waypoint up there gets no route.
    struct RefusesNorth {
        calls: RefCell<u32>,
    }

    impl Router for RefusesNorth {
        async fn route(&self, req: &RouteRequest) -> Result<Vec<Candidate>, RouteError> {
            *self.calls.borrow_mut() += 1;
            if req.locations.len() > 2 && req.locations.iter().any(|w| w.lat > 39.004) {
                return Err(RouteError::NoRoute);
            }
            Stubborn { calls: RefCell::new(Vec::new()) }.route(req).await
        }
    }

    #[test]
    fn a_stretch_the_server_refuses_is_closed_and_the_route_replanned() {
        let mut ways = ladder(0);
        let w = |id: i64, pts: &[(f64, f64)]| Way {
            id,
            oneway: 0,
            kmh: 50,
            pts: pts.iter().map(|&(a, b)| ((a * 1e7).round() as i32, (b * 1e7).round() as i32)).collect(),
        };
        // A second, slightly longer detour to the south.
        ways.push(w(5, &[(39.0, -105.0), (38.994, -105.0), (38.988, -105.0)]));
        ways.push(w(6, &[(38.988, -105.0), (38.988, -104.975), (38.988, -104.95), (38.988, -104.925), (38.988, -104.9)]));
        ways.push(w(7, &[(38.988, -104.9), (38.994, -104.9), (39.0, -104.9)]));
        let router = RefusesNorth { calls: RefCell::new(0) };
        let plan = run_with(&router, &FixedRoads(ways), vec![cam("node/1", 39.0001, -104.95)]);
        assert_eq!(plan.road_check, RoadCheck::CameraFree);
        assert!(plan.avoid_from_road_map);
        assert_eq!(plan.outcome, Outcome::Clear);
        assert!(plan.avoid.shape.iter().any(|p| p[0] < 38.99), "re-planned onto the southern detour");
        assert!(plan.avoid.shape.iter().all(|p| p[0] < 39.004), "nothing from the refused attempt");
    }

    #[test]
    fn when_the_road_map_has_no_way_around_the_camera_is_called_unavoidable() {
        // The parallel road is one-way the wrong way: every route passes the camera.
        let router = Stubborn { calls: RefCell::new(Vec::new()) };
        let plan = run_with(&router, &FixedRoads(ladder(-1)), vec![cam("node/1", 39.0001, -104.95)]);
        assert_eq!(plan.road_check, RoadCheck::NoneExists { fewest: 1 });
        assert_eq!(plan.avoid.cameras.len(), 1);
        assert_eq!(plan.avoid.cameras[0].remaining, Some(Remaining::Unavoidable));
        assert_eq!(plan.outcome, Outcome::Unchanged);
    }

    #[test]
    fn without_a_road_map_the_leftover_camera_is_a_search_limit_not_a_dead_end() {
        let router = Stubborn { calls: RefCell::new(Vec::new()) };
        let plan = run_with(&router, &NoRoads, vec![cam("node/1", 39.0001, -104.95)]);
        assert!(matches!(plan.road_check, RoadCheck::Unavailable { .. }));
        assert_eq!(plan.avoid.cameras[0].remaining, Some(Remaining::SearchLimit));
    }

    // -----------------------------------------------------------------------
    // Dense metro cases, from recorded answers (no live service)
    // -----------------------------------------------------------------------
    //
    // Fixtures in `fixtures/routing/`: `<case>.cameras.json.gz` (the cameras, fixed),
    // `<case>.roads.bin.gz` (the road map the planner loaded, roadnet format) and
    // `<case>.valhalla.json.gz` (every routing request the planner made, with the server's
    // answer). The planner is deterministic, so a replay makes the same requests. After a
    // change to the planner's requests, re-record with
    // `cargo test --lib record_dense_metro_fixtures -- --ignored --nocapture`.

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/routing");

    /// (case, start, destination)
    const CASES: [(&str, (f64, f64), (f64, f64)); 3] = [
        ("buckhead-sandy-springs", (33.8480, -84.3730), (33.9304, -84.3733)),
        ("decatur-marietta", (33.7748, -84.2963), (33.9526, -84.5499)),
        ("atlanta-midtown-airport", (33.7810, -84.3830), (33.6407, -84.4277)),
    ];

    fn gunzip(path: &str) -> Option<Vec<u8>> {
        let gz = std::fs::read(path).ok()?;
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&gz[..]), &mut out).ok()?;
        Some(out)
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        std::io::Write::write_all(&mut e, bytes).unwrap();
        e.finish().unwrap()
    }

    fn fixture_cameras(case: &str) -> Vec<AvoidCamera> {
        #[derive(Deserialize)]
        struct C {
            key: String,
            lat: f64,
            lon: f64,
            category: String,
        }
        let raw = gunzip(&format!("{FIXTURES}/{case}.cameras.json.gz")).expect("camera fixture");
        serde_json::from_slice::<Vec<C>>(&raw)
            .unwrap()
            .into_iter()
            .map(|c| AvoidCamera { category: c.category, ..cam(&c.key, c.lat, c.lon) })
            .collect()
    }

    #[derive(Serialize, Deserialize, Clone)]
    struct Recorded {
        request: String,
        #[serde(default)]
        ok: Option<String>,
        #[serde(default)]
        status: Option<u16>,
        #[serde(default)]
        body: Option<String>,
    }

    struct Replay {
        answers: HashMap<String, Recorded>,
        count: RefCell<usize>,
    }

    impl Router for Replay {
        async fn route(&self, req: &RouteRequest) -> Result<Vec<Candidate>, RouteError> {
            let key = request_body(req).to_string();
            let rec = self
                .answers
                .get(&key)
                .unwrap_or_else(|| panic!("no recorded answer for {key} (re-record the fixtures if the planner changed)"));
            *self.count.borrow_mut() += 1;
            interpret(match (&rec.ok, rec.status) {
                (Some(text), _) => Ok(text.clone()),
                (None, status) => Err(AppError::Http {
                    status: status.unwrap_or(400),
                    endpoint: "the routing server".into(),
                    body: rec.body.clone().unwrap_or_default(),
                }),
            })
        }
    }

    struct Recorder<'a> {
        live: Valhalla<'a>,
        log: RefCell<Vec<Recorded>>,
    }

    impl Router for Recorder<'_> {
        async fn route(&self, req: &RouteRequest) -> Result<Vec<Candidate>, RouteError> {
            let request = request_body(req).to_string();
            let answer = self.live.fetch(req).await;
            let rec = match &answer {
                Ok(text) => Recorded { request, ok: Some(text.clone()), status: None, body: None },
                Err(AppError::Http { status, body, .. }) => Recorded { request, ok: None, status: Some(*status), body: Some(body.clone()) },
                Err(e) => panic!("live request failed while recording: {e}"),
            };
            self.log.borrow_mut().push(rec);
            interpret(answer)
        }
    }

    struct RecordingRoads<S: RoadSource> {
        inner: S,
        got: RefCell<Option<Vec<Way>>>,
    }

    /// A road map from a saved Overpass answer in `out body` form (ways with node ids, then
    /// `out skel` nodes), read with the app's own road rules. For recording when Overpass is
    /// too busy to download: `FLOCKFINDER_RECORD_ROADS=<file>`.
    struct FileRoads(Vec<Way>);

    impl FileRoads {
        fn read(path: &str) -> Self {
            let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).expect("road file")).unwrap();
            let elements = v["elements"].as_array().expect("elements");
            let nodes: HashMap<i64, (i32, i32)> = elements
                .iter()
                .filter(|e| e["type"] == "node")
                .map(|e| {
                    let at = |k: &str| (e[k].as_f64().unwrap() * 1e7).round() as i32;
                    (e["id"].as_i64().unwrap(), (at("lat"), at("lon")))
                })
                .collect();
            let mut ways: Vec<Way> = elements
                .iter()
                .filter(|e| e["type"] == "way")
                .filter_map(|e| {
                    let tags: HashMap<String, String> = e["tags"]
                        .as_object()?
                        .iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                        .collect();
                    let pts = e["nodes"].as_array()?.iter().filter_map(|n| nodes.get(&n.as_i64()?).copied()).collect();
                    crate::roadnet::way_from(e["id"].as_i64()?, &tags, pts)
                })
                .collect();
            ways.sort_by_key(|w| w.id);
            FileRoads(ways)
        }
    }

    impl RoadSource for FileRoads {
        async fn load(&self, area: &crate::roadnet::Area, _: &(dyn Fn(String) + Sync)) -> AppResult<Loaded> {
            let ways: Vec<Way> = self.0.iter().filter(|w| crate::roadnet::intersects(&w.bbox(), &area.bbox)).cloned().collect();
            Ok(Loaded { ways, tiles: 0, downloaded: 0, bytes: 0, partial: false, cached: 0 })
        }
    }

    /// Either road source, for the recorder.
    enum RecordSource<'a> {
        Live(OverpassRoads<'a>),
        File(FileRoads),
        Fixed(FixedRoads),
    }

    impl RoadSource for RecordSource<'_> {
        async fn load(&self, area: &crate::roadnet::Area, progress: &(dyn Fn(String) + Sync)) -> AppResult<Loaded> {
            match self {
                RecordSource::Live(r) => r.load(area, progress).await,
                RecordSource::File(r) => r.load(area, progress).await,
                RecordSource::Fixed(r) => r.load(area, progress).await,
            }
        }
    }

    impl<S: RoadSource> RoadSource for RecordingRoads<S> {
        async fn load(&self, area: &crate::roadnet::Area, progress: &(dyn Fn(String) + Sync)) -> AppResult<Loaded> {
            let loaded = self.inner.load(area, progress).await?;
            assert!(!loaded.partial, "Overpass was too busy to record a complete road map; try again later");
            *self.got.borrow_mut() = Some(loaded.ways.clone());
            Ok(loaded)
        }
    }

    fn source_for(cams: &[AvoidCamera]) -> impl Fn(&BBox) -> AppResult<Vec<AvoidCamera>> + '_ {
        move |b: &BBox| Ok(cams.iter().filter(|c| b.contains(c.lat, c.lon)).cloned().collect())
    }

    fn replay(case: &str) -> (RoutePlan, Vec<AvoidCamera>, usize) {
        let (_, a, b) = CASES.iter().find(|c| c.0 == case).copied().unwrap();
        let cams = fixture_cameras(case);
        let raw = gunzip(&format!("{FIXTURES}/{case}.valhalla.json.gz")).expect("recorded answers");
        let recorded: Vec<Recorded> = serde_json::from_slice(&raw).unwrap();
        let router = Replay { answers: recorded.into_iter().map(|r| (r.request.clone(), r)).collect(), count: RefCell::new(0) };
        let roads = match std::fs::read(format!("{FIXTURES}/{case}.roads.bin.gz")) {
            Ok(data) => FixedRoads(crate::roadnet::decode(&data).unwrap()),
            Err(_) => FixedRoads(Vec::new()),
        };
        let fut = plan(
            &router,
            &roads,
            DEFAULT_ENDPOINT,
            LatLon { lat: a.0, lon: a.1 },
            LatLon { lat: b.0, lon: b.1 },
            source_for(&cams),
            |_| {},
        );
        let plan = tauri::async_runtime::block_on(fut).unwrap();
        let used = *router.count.borrow();
        (plan, cams, used)
    }

    fn recount(route: &PlannedRoute, cams: &[AvoidCamera]) -> usize {
        let shape: Vec<(f64, f64)> = route.shape.iter().map(|p| (p[0], p[1])).collect();
        cameras_on_route(&shape, cams, AVOID_RADIUS_M).len()
    }

    #[test]
    fn dense_metro_buckhead_to_sandy_springs_finds_the_camera_free_route() {
        // Before the road map: 2 cameras (fastest 17), reported as "no way around".
        let (plan, cams, used) = replay("buckhead-sandy-springs");
        assert_eq!(plan.fastest.cameras.len(), 17);
        assert!(plan.limits.request_budget || plan.limits.exclusion_cap, "phase 1 alone ran out");
        assert_eq!(plan.road_check, RoadCheck::CameraFree);
        assert!(plan.avoid_from_road_map);
        assert_eq!(plan.outcome, Outcome::Clear);
        assert_eq!(plan.avoid.cameras.len(), 0);
        assert_eq!(recount(&plan.avoid, &cams), 0, "count matches the geometry");
        assert_eq!(recount(&plan.fastest, &cams), plan.fastest.cameras.len());
        assert!(used > MAX_REQUESTS as usize);
    }

    #[test]
    fn dense_metro_decatur_to_marietta_finds_the_camera_free_route() {
        // Before the road map: 12 cameras (fastest 14), reported as "no way around".
        let (plan, cams, _) = replay("decatur-marietta");
        assert_eq!(plan.fastest.cameras.len(), 14);
        assert_eq!(plan.road_check, RoadCheck::CameraFree);
        assert!(plan.avoid_from_road_map);
        assert_eq!(plan.outcome, Outcome::Clear);
        assert_eq!(recount(&plan.avoid, &cams), 0, "count matches the geometry");
        assert!(plan.long_detour, "flagged: it is a long way round");
    }

    #[test]
    fn an_already_good_route_is_unchanged_and_needs_no_road_map() {
        let (plan, cams, used) = replay("atlanta-midtown-airport");
        assert_eq!(plan.fastest.cameras.len(), 7);
        assert_eq!(plan.outcome, Outcome::Clear);
        assert_eq!(plan.road_check, RoadCheck::NotNeeded);
        assert!(!plan.avoid_from_road_map);
        assert_eq!(used, 2, "the same two requests as before the road map existed");
        assert_eq!(recount(&plan.avoid, &cams), 0);
    }

    /// Records the fixtures above from the live services (Valhalla and Overpass). Opt-in:
    /// `cargo test --lib record_dense_metro_fixtures -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn record_dense_metro_fixtures() {
        let http = HttpClient::new().unwrap();
        let db = std::sync::Mutex::new(crate::db::test_conn());
        // FLOCKFINDER_RECORD_CASE=<case> records just that one.
        let only = std::env::var("FLOCKFINDER_RECORD_CASE").ok();
        for (case, a, b) in CASES.into_iter().filter(|c| only.as_deref().map_or(true, |o| o == c.0)) {
            let cams = fixture_cameras(case);
            let router = Recorder { live: Valhalla { http: &http, endpoint: DEFAULT_ENDPOINT.into() }, log: RefCell::new(Vec::new()) };
            // FLOCKFINDER_RECORD_ROADS=<saved Overpass answer> takes the road map from a file;
            // otherwise the road map already recorded for the case is reused (only the routing
            // answers are recorded again), unless FLOCKFINDER_RECORD_FRESH_ROADS is set;
            // FLOCKFINDER_RECORD_OVERPASS=<interpreter URL> downloads it from another server.
            let saved = std::fs::read(format!("{FIXTURES}/{case}.roads.bin.gz")).ok().filter(|_| std::env::var("FLOCKFINDER_RECORD_FRESH_ROADS").is_err());
            let inner = match (std::env::var("FLOCKFINDER_RECORD_ROADS"), saved) {
                (Ok(path), _) => RecordSource::File(FileRoads::read(&path)),
                (Err(_), Some(data)) => RecordSource::Fixed(FixedRoads(crate::roadnet::decode(&data).unwrap())),
                (Err(_), None) => RecordSource::Live(OverpassRoads {
                    http: &http,
                    endpoint: std::env::var("FLOCKFINDER_RECORD_OVERPASS").unwrap_or_else(|_| crate::overpass::DEFAULT_ENDPOINT.into()),
                    db: &db,
                    budget: None,
                }),
            };
            let roads = RecordingRoads { inner, got: RefCell::new(None) };
            let fut = plan(
                &router,
                &roads,
                DEFAULT_ENDPOINT,
                LatLon { lat: a.0, lon: a.1 },
                LatLon { lat: b.0, lon: b.1 },
                source_for(&cams),
                |p| eprintln!("  [{}/{}] {}", p.step, p.max, p.message),
            );
            let p = tauri::async_runtime::block_on(fut).unwrap();
            let log = router.log.into_inner();
            std::fs::write(format!("{FIXTURES}/{case}.valhalla.json.gz"), gzip(&serde_json::to_vec(&log).unwrap())).unwrap();
            let roads_file = format!("{FIXTURES}/{case}.roads.bin.gz");
            match roads.got.into_inner() {
                Some(ways) => std::fs::write(&roads_file, crate::roadnet::encode(&ways)).unwrap(),
                None => {
                    let _ = std::fs::remove_file(&roads_file);
                }
            }
            eprintln!(
                "{case}: fastest {} cams {:.0} min | avoid {} cams {:.1} km {:.0} min | {:?} from road map: {} | {} requests | limits {:?}",
                p.fastest.cameras.len(),
                p.fastest.duration_s / 60.0,
                p.avoid.cameras.len(),
                p.avoid.distance_m / 1000.0,
                p.avoid.duration_s / 60.0,
                p.road_check,
                p.avoid_from_road_map,
                log.len(),
                p.limits
            );
        }
    }
}
