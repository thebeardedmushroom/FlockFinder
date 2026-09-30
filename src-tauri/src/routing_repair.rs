//! Long trips: fix the stretches with cameras one at a time.
//!
//! On a long trip the cameras the first search leaves are bunched: in town centres and at
//! interchanges, with long clean stretches between. Searching the road map for the whole trip
//! doesn't scale (the download grows with the trip, and so do the requests), and the first
//! search can't clear them either: every request routes the whole trip, each answer passes new
//! cameras somewhere, and the server takes at most 50 excluded cameras a request.
//!
//! So the route is cut into **stretches** around each group of cameras (a few kilometres
//! before the first to a few kilometres after the last), and each stretch is fixed on its own:
//! 1. route through it with just its cameras excluded (a handful, well under the cap), up to
//!    [`LOCAL_REQUESTS`] requests;
//! 2. if cameras are still left, search the road map for that stretch alone (phase 2, on a
//!    small area) and have the server drive it.
//!
//! Each stretch's route is spliced in where it was cut out. Work and downloads grow with the
//! number of camera groups, not with the length of the trip. Stretches with the most cameras
//! go first, so if the budget runs out, what is left is the least.

use super::*;
use std::time::{Duration, Instant};

/// Cameras closer than this along the route form one group (one stretch).
pub const GROUP_GAP_M: f64 = 8_000.0;
/// A stretch starts this far before its first camera and ends this far after its last…
pub const STRETCH_MARGIN_M: f64 = 4_000.0;
/// …moved further out until its ends are at least this far from any camera on the route.
const CLEAR_M: f64 = 150.0;
/// Requests for the quick fix (excluding the stretch's cameras) of one stretch.
pub const LOCAL_REQUESTS: u32 = 3;
/// Requests for all the stretches together, by default.
pub const REPAIR_REQUESTS: u32 = 40;
/// A stretch's fix may add at most this much driving time per camera it avoids, unless the
/// user set another limit (Settings → Directions, `PlanOptions::max_extra_secs_per_camera`); a
/// slower one is not used (the stretch keeps the original road and its cameras).
pub const DEFAULT_MAX_EXTRA_SECS_PER_CAMERA: f64 = 300.0;
/// Road maps are only loaded for stretches this long (straight line) or shorter…
const STRETCH_MAP_MAX_M: f64 = 40_000.0;
/// …and only in this much time from the start of the repair (downloads can be slow).
const MAP_TIME: Duration = Duration::from_secs(240);

/// A route with distances along it, for cutting it up.
pub(crate) struct Line<'a> {
    pub c: &'a Candidate,
    cum: Vec<f64>,
    /// Distance along the route of each maneuver.
    at: Vec<f64>,
}

impl<'a> Line<'a> {
    pub fn new(c: &'a Candidate) -> Self {
        let mut cum = vec![0.0];
        for w in c.shape.windows(2) {
            cum.push(cum.last().copied().unwrap_or(0.0) + haversine_m(w[0].0, w[0].1, w[1].0, w[1].1));
        }
        let last = c.shape.len().saturating_sub(1);
        let at = c.maneuvers.iter().map(|m| cum[m.shape_index.min(last)]).collect();
        Line { c, cum, at }
    }

    pub fn length(&self) -> f64 {
        self.cum.last().copied().unwrap_or(0.0)
    }

    /// Index of the segment holding the point `d` metres along.
    fn segment(&self, d: f64) -> usize {
        self.cum.partition_point(|&c| c <= d).saturating_sub(1).min(self.c.shape.len().saturating_sub(2))
    }

    pub fn point(&self, d: f64) -> (f64, f64) {
        let d = d.clamp(0.0, self.length());
        let i = self.segment(d);
        let len = self.cum[i + 1] - self.cum[i];
        let t = if len > 0.0 { (d - self.cum[i]) / len } else { 0.0 };
        let (a, b) = (self.c.shape[i], self.c.shape[i + 1]);
        (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
    }

    /// The middle of the segment holding `d` (a cut there is never on an intersection), and
    /// its distance along the route.
    pub fn mid_segment(&self, d: f64) -> f64 {
        let i = self.segment(d);
        (self.cum[i] + self.cum[i + 1]) / 2.0
    }

    /// A break waypoint at `d`, heading the route's way.
    pub fn waypoint(&self, d: f64) -> Waypoint {
        let i = self.segment(d);
        let p = self.point(d);
        Waypoint { lat: p.0, lon: p.1, through: false, heading: Some(bearing(self.c.shape[i], self.c.shape[i + 1])) }
    }

    /// Share of maneuver `k`'s step (distance and time) between `from` and `to`.
    fn share(&self, k: usize, from: f64, to: f64) -> f64 {
        let start = self.at[k];
        let end = self.at.get(k + 1).copied().unwrap_or(self.length()).max(start);
        let span = end - start;
        if span <= 0.0 {
            return 0.0;
        }
        ((to.min(end) - from.max(start)) / span).clamp(0.0, 1.0)
    }

    /// The route from `from` to `to` metres along it, as a leg to `stitch` with others: it
    /// starts with a "start" maneuver (dropped by `stitch` unless it's the route's own start)
    /// carrying what is left of the step it cuts into, and ends with an "arrive" maneuver
    /// (dropped unless it's the route's own end).
    pub fn slice(&self, from: f64, to: f64) -> Candidate {
        let len = self.length();
        let (from, to) = (from.clamp(0.0, len), to.clamp(0.0, len));
        let (i, j) = (self.segment(from), self.segment(to));
        let mut shape = vec![self.point(from)];
        for p in &self.c.shape[i + 1..=j] {
            if shape.last() != Some(p) {
                shape.push(*p);
            }
        }
        let end = self.point(to);
        if shape.last() != Some(&end) {
            shape.push(end);
        }
        // Original shape index `m` (> i) is `m - i` here.
        let index = |m: usize| m.saturating_sub(i).min(shape.len() - 1);
        let mut maneuvers: Vec<Maneuver> = Vec::new();
        let first = self.at.iter().rposition(|&a| a <= from + 0.5).unwrap_or(0);
        for k in first..self.c.maneuvers.len() {
            let m = &self.c.maneuvers[k];
            let at = self.at[k];
            let own_arrival = to >= len - 0.5 && (4..=6).contains(&m.kind);
            if at > to + 0.5 || (at >= to - 0.5 && !own_arrival) {
                break;
            }
            let share = self.share(k, from, to);
            let mut m = m.clone();
            if k == first && at < from - 0.5 {
                // Cutting into this step: it becomes the slice's start.
                m = Maneuver {
                    instruction: m.instruction,
                    kind: 1,
                    distance_m: m.distance_m,
                    duration_s: m.duration_s,
                    lat: shape[0].0,
                    lon: shape[0].1,
                    ..Default::default()
                };
            }
            m.shape_index = if k == first { 0 } else { index(m.shape_index) };
            m.distance_m *= share;
            m.duration_s *= share;
            if (4..=6).contains(&m.kind) {
                m.shape_index = shape.len() - 1;
            }
            maneuvers.push(m);
        }
        if !maneuvers.last().is_some_and(|m| (4..=6).contains(&m.kind)) {
            maneuvers.push(Maneuver { instruction: "Arrive.".into(), kind: 4, shape_index: shape.len() - 1, lat: end.0, lon: end.1, ..Default::default() });
        }
        Candidate {
            distance_m: maneuvers.iter().map(|m| m.distance_m).sum(),
            duration_s: maneuvers.iter().map(|m| m.duration_s).sum(),
            maneuvers,
            shape,
        }
    }
}

/// A stretch of the route to fix: from `from` to `to` metres along it, and the cameras on it.
#[derive(Debug, Clone, PartialEq)]
pub struct Stretch {
    pub from: f64,
    pub to: f64,
    pub cameras: Vec<String>,
}

/// Stretches around the groups of `cameras` (on `line`, in driving order) not in `skip`.
pub(crate) fn stretches(line: &Line, cameras: &[RouteCamera], skip: &HashSet<String>) -> Vec<Stretch> {
    let len = line.length();
    let on_route: Vec<f64> = cameras.iter().map(|c| c.along_m).collect();
    let clear = |d: f64| on_route.iter().all(|&a| (a - d).abs() >= CLEAR_M);
    let mut out: Vec<Stretch> = Vec::new();
    for c in cameras.iter().filter(|c| !skip.contains(&c.camera.key)) {
        match out.last_mut() {
            Some(s) if c.along_m - s.to <= GROUP_GAP_M => {
                s.to = c.along_m;
                s.cameras.push(c.camera.key.clone());
            }
            _ => out.push(Stretch { from: c.along_m, to: c.along_m, cameras: vec![c.camera.key.clone()] }),
        }
    }
    for s in &mut out {
        // Room to go round, ends clear of cameras and off intersections (or the trip's own ends).
        let mut from = (s.from - STRETCH_MARGIN_M).max(0.0);
        while from > 0.0 && !clear(from) {
            from = (from - CLEAR_M).max(0.0);
        }
        let mut to = (s.to + STRETCH_MARGIN_M).min(len);
        while to < len && !clear(to) {
            to = (to + CLEAR_M).min(len);
        }
        s.from = if from > 0.0 { line.mid_segment(from).min(s.from - CLEAR_M).max(0.0) } else { 0.0 };
        s.to = if to < len { line.mid_segment(to).max(s.to + CLEAR_M).min(len) } else { len };
    }
    // Overlapping stretches become one.
    let mut merged: Vec<Stretch> = Vec::new();
    for s in out {
        match merged.last_mut() {
            Some(m) if s.from <= m.to => {
                m.to = m.to.max(s.to);
                m.cameras.extend(s.cameras);
            }
            _ => merged.push(s),
        }
    }
    merged
}

/// Route from `from` to `to`, excluding the cameras each answer passes (except those in
/// `allowed` or at either end), in up to `requests` requests. The answer with the fewest
/// cameras (then the quickest), with its cameras.
pub(crate) async fn route_around<R, C>(
    router: &R,
    from: Waypoint,
    to: Waypoint,
    cameras: &C,
    allowed: &HashSet<String>,
    requests: u32,
    used: &mut u32,
) -> AppResult<Option<Evaluated>>
where
    R: Router,
    C: Fn(&BBox) -> AppResult<Vec<AvoidCamera>>,
{
    let at_ends = |c: &AvoidCamera| {
        haversine_m(c.lat, c.lon, from.lat, from.lon) <= AVOID_RADIUS_M || haversine_m(c.lat, c.lon, to.lat, to.lon) <= AVOID_RADIUS_M
    };
    let counted = |e: &Evaluated| e.cameras.iter().filter(|c| !allowed.contains(&c.camera.key) && !at_ends(&c.camera)).count();
    let mut exclude: Vec<AvoidCamera> = Vec::new();
    let mut best: Option<(usize, Evaluated)> = None;
    for _ in 0..requests {
        let req = RouteRequest {
            locations: vec![from, to],
            exclude: exclude.iter().map(|c| LatLon { lat: c.lat, lon: c.lon }).collect(),
            alternates: 0,
        };
        *used += 1;
        let cand = match router.route(&req).await {
            Ok(c) => c.into_iter().next(),
            Err(RouteError::NoRoute) | Err(RouteError::TooManyExclusions(_)) => break,
            Err(RouteError::Api(e)) => return Err(e),
        };
        let Some(cand) = cand else { break };
        let ev = evaluate(cand, cameras)?;
        let n = counted(&ev);
        let stray: Vec<AvoidCamera> =
            ev.cameras.iter().map(|c| c.camera.clone()).filter(|c| !allowed.contains(&c.key) && !at_ends(c)).collect();
        if best.as_ref().map_or(true, |(bn, b)| n < *bn || (n == *bn && ev.candidate.duration_s < b.candidate.duration_s)) {
            best = Some((n, ev));
        }
        if stray.is_empty() {
            break;
        }
        let before = exclude.len();
        for c in stray {
            if exclude.len() < MAX_EXCLUSIONS && !exclude.iter().any(|e| e.key == c.key) {
                exclude.push(c);
            }
        }
        if exclude.len() == before {
            break;
        }
    }
    Ok(best.map(|(_, e)| e))
}

/// What the repair did.
pub(crate) struct Repaired {
    /// The route with the fixed stretches spliced in, when anything was fixed.
    pub route: Option<Evaluated>,
    pub stretches: usize,
    pub fixed: usize,
    /// Stretches whose only ways around cost more than the time limit.
    pub too_slow: usize,
    /// Their cameras.
    pub slow: HashSet<String>,
    /// Cameras whose stretch's road map shows no way around.
    pub unavoidable: HashSet<String>,
    pub road_map: Option<RoadMapInfo>,
    pub warning: Option<String>,
}

/// Fix the stretches of `route` that pass cameras (other than those in `near_endpoint`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn repair<R, S, C>(
    router: &R,
    roads: &S,
    route: &Evaluated,
    start: LatLon,
    end: LatLon,
    near_endpoint: &HashSet<String>,
    cameras: &C,
    requests: &mut u32,
    step: &(dyn Fn(u32, String) + Sync),
    opts: &PlanOptions,
) -> AppResult<Repaired>
where
    R: Router,
    S: RoadSource,
    C: Fn(&BBox) -> AppResult<Vec<AvoidCamera>>,
{
    let line = Line::new(&route.candidate);
    let len = line.length();
    let list = stretches(&line, &route.cameras, near_endpoint);
    let mut out = Repaired { route: None, stretches: list.len(), fixed: 0, too_slow: 0, slow: HashSet::new(), unavoidable: HashSet::new(), road_map: None, warning: None };
    let budget_end = *requests + opts.repair_requests;
    let started = Instant::now();
    // Worst first.
    let mut order: Vec<usize> = (0..list.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(list[i].cameras.len()));
    let mut fixes: Vec<Option<Candidate>> = vec![None; list.len()];
    for (n, &i) in order.iter().enumerate() {
        if *requests >= budget_end {
            log::warn!("stretch repair ran out of requests after {n} of {} stretches", list.len());
            break;
        }
        let s = &list[i];
        let from = if s.from <= 0.0 { Waypoint { heading: opts.start_heading, ..Waypoint::at(start) } } else { line.waypoint(s.from) };
        let to = if s.to >= len { Waypoint::at(end) } else { line.waypoint(s.to) };
        let before = s.cameras.len();
        step(*requests + 1, format!("Routing around the cameras on part {} of {} of the route", n + 1, list.len()));
        let local = match route_around(router, from, to, cameras, near_endpoint, LOCAL_REQUESTS.min(budget_end - *requests), requests).await {
            Ok(r) => r,
            Err(e) => {
                out.warning = Some(server_stopped(&e));
                break;
            }
        };
        let count = |e: &Evaluated| e.cameras.iter().filter(|c| !near_endpoint.contains(&c.camera.key)).count();
        // A way round is worth it when it avoids cameras for at most the time limit each.
        let base = line.slice(s.from, s.to).duration_s;
        let worth = |n: usize, e: &Evaluated| {
            let avoided = before.saturating_sub(n);
            avoided > 0 && opts.max_extra_secs_per_camera.map_or(true, |max| e.candidate.duration_s - base <= max * avoided as f64)
        };
        let mut options: Vec<(usize, Evaluated)> = local.into_iter().map(|e| (count(&e), e)).collect();
        // No camera-free way round in the time limit yet: the road map for this stretch alone
        // (its path may also be quicker than the one the exclusions gave).
        let straight = haversine_m(from.lat, from.lon, to.lat, to.lon);
        if !options.iter().any(|(n, e)| *n == 0 && worth(*n, e))
            && *requests < budget_end
            && straight <= STRETCH_MAP_MAX_M
            && started.elapsed() < MAP_TIME
        {
            let map_opts = PlanOptions {
                start_heading: from.heading,
                end_heading: to.heading,
                guided_requests: (budget_end - *requests).min(opts.guided_requests),
                ..*opts
            };
            let a = LatLon { lat: from.lat, lon: from.lon };
            let b = LatLon { lat: to.lat, lon: to.lon };
            let found = road_map_search(router, roads, a, b, near_endpoint, cameras, requests, step, &map_opts).await?;
            if let Some(info) = &found.info {
                let m = out.road_map.get_or_insert_with(RoadMapInfo::default);
                m.tiles += info.tiles;
                m.downloaded_tiles += info.downloaded_tiles;
                m.cached_tiles += info.cached_tiles;
                m.missing_tiles += info.missing_tiles;
                m.downloaded_bytes += info.downloaded_bytes;
                m.ways += info.ways;
            }
            if matches!(found.check, RoadCheck::NoneExists { .. }) {
                out.unavoidable.extend(found.allowed.iter().filter(|k| s.cameras.contains(k)).cloned());
            }
            if let Some(r) = found.route {
                options.push((count(&r), r));
            }
            if found.warning.is_some() {
                out.warning = found.warning;
                break;
            }
        }
        let fewer = options.iter().any(|(n, _)| *n < before);
        let best = options
            .into_iter()
            .filter(|(n, e)| worth(*n, e))
            .min_by(|(an, a), (bn, b)| an.cmp(bn).then(a.candidate.duration_s.total_cmp(&b.candidate.duration_s)));
        match best {
            Some((n, e)) => {
                log::info!(
                    "stretch {:.1}–{:.1} km: {before} → {n} cameras, {:+.1} min",
                    s.from / 1000.0,
                    s.to / 1000.0,
                    (e.candidate.duration_s - base) / 60.0
                );
                fixes[i] = Some(e.candidate);
                out.fixed += 1;
            }
            None if fewer => {
                log::info!("stretch {:.1}–{:.1} km: every way round its {before} cameras is too slow", s.from / 1000.0, s.to / 1000.0);
                out.too_slow += 1;
                out.slow.extend(s.cameras.iter().cloned());
            }
            None => log::info!("stretch {:.1}–{:.1} km: no way round its {before} cameras found", s.from / 1000.0, s.to / 1000.0),
        }
    }
    if out.fixed == 0 {
        return Ok(out);
    }
    // Splice: the original route between the stretches, the fixes in them.
    let mut pieces: Vec<Candidate> = Vec::new();
    let mut at = 0.0;
    for (s, fix) in list.iter().zip(fixes) {
        let Some(fix) = fix else { continue };
        if s.from > at {
            pieces.push(line.slice(at, s.from));
        }
        pieces.push(fix);
        at = s.to;
    }
    if at < len {
        pieces.push(line.slice(at, len));
    }
    if let Some(joined) = stitch(pieces) {
        out.route = Some(evaluate(joined, cameras)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roadnet::Loaded;

    /// Points along lat 39 from `lon0` to `lon1`, every ~0.001°.
    fn east(lon0: f64, lon1: f64) -> Vec<(f64, f64)> {
        let n = ((lon1 - lon0) / 0.001).round() as usize;
        (0..=n).map(|i| (39.0, lon0 + (lon1 - lon0) * i as f64 / n as f64)).collect()
    }

    /// A candidate along `shape` with a start, a turn at each corner index, and an arrival.
    fn cand(shape: Vec<(f64, f64)>, turns: &[usize]) -> Candidate {
        let cum: Vec<f64> = std::iter::once(0.0)
            .chain(shape.windows(2).scan(0.0, |d, w| {
                *d += haversine_m(w[0].0, w[0].1, w[1].0, w[1].1);
                Some(*d)
            }))
            .collect();
        let mut idx = vec![0];
        idx.extend_from_slice(turns);
        idx.push(shape.len() - 1);
        let maneuvers = idx
            .iter()
            .enumerate()
            .map(|(k, &i)| {
                let next = idx.get(k + 1).map(|&j| cum[j]).unwrap_or(cum[i]);
                let kind = if k == 0 { 1 } else if k == idx.len() - 1 { 4 } else { 15 };
                Maneuver { instruction: format!("m{k}"), kind, shape_index: i, distance_m: next - cum[i], duration_s: (next - cum[i]) / 20.0, ..Default::default() }
            })
            .collect::<Vec<_>>();
        Candidate { distance_m: *cum.last().unwrap(), duration_s: cum.last().unwrap() / 20.0, maneuvers, shape }
    }

    fn cam(key: &str, lat: f64, lon: f64) -> AvoidCamera {
        AvoidCamera { key: key.into(), lat, lon, category: "flock".into(), source: "osm".into(), direction: None, operator: None }
    }

    #[test]
    fn a_route_cut_into_slices_joins_back_into_the_same_route() {
        let shape = east(-105.0, -104.9);
        let c = cand(shape, &[30, 60]);
        let line = Line::new(&c);
        let (a, b) = (line.mid_segment(2_000.0), line.mid_segment(6_000.0));
        let joined = stitch(vec![line.slice(0.0, a), line.slice(a, b), line.slice(b, line.length())]).unwrap();
        assert!((joined.distance_m - c.distance_m).abs() < 1.0, "{} vs {}", joined.distance_m, c.distance_m);
        assert!((joined.duration_s - c.duration_s).abs() < 0.1);
        let kinds: Vec<u32> = joined.maneuvers.iter().map(|m| m.kind).collect();
        assert_eq!(kinds, vec![1, 15, 15, 4], "the cuts add and drop nothing");
        let j = Line::new(&joined);
        for (m, &at) in joined.maneuvers.iter().zip(&j.at) {
            let orig = c.maneuvers.iter().position(|o| o.instruction == m.instruction).unwrap();
            assert!((at - line.at[orig]).abs() < 1.0, "{} moved", m.instruction);
        }
    }

    #[test]
    fn cameras_are_grouped_into_stretches_with_clear_ends() {
        let c = cand(east(-105.0, -104.0), &[]);
        let line = Line::new(&c);
        let rc = |key: &str, along: f64| {
            let p = line.point(along);
            RouteCamera { camera: cam(key, p.0, p.1), along_m: along, distance_m: 0.0, remaining: None }
        };
        let cams = vec![rc("a", 20_000.0), rc("b", 25_000.0), rc("c", 60_000.0), rc("start", 50.0)];
        let skip: HashSet<String> = ["start".to_string()].into();
        let s = stretches(&line, &cams, &skip);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].cameras, vec!["a", "b"]);
        assert!(s[0].from < 20_000.0 - 3_000.0 && s[0].to > 25_000.0 + 3_000.0);
        assert_eq!(s[1].cameras, vec!["c"]);
        for st in &s {
            for c in &cams {
                assert!((c.along_m - st.from).abs() >= CLEAR_M && (c.along_m - st.to).abs() >= CLEAR_M);
            }
        }
    }

    /// Routes straight along lat 39, ignoring exclusions on long requests (the whole trip can't
    /// be cleared) but going round an excluded camera on short ones (a stretch can), by a road
    /// `north` degrees to the north.
    struct Corridor {
        north: f64,
    }

    const CAMERA_LON: f64 = -104.6;

    impl Router for Corridor {
        async fn route(&self, req: &RouteRequest) -> Result<Vec<Candidate>, RouteError> {
            let (a, b) = (req.locations[0], req.locations[req.locations.len() - 1]);
            let short = haversine_m(a.lat, a.lon, b.lat, b.lon) < 40_000.0;
            let excluded = req.exclude.iter().any(|p| (p.lon - CAMERA_LON).abs() < 0.001);
            if short && excluded {
                // Round the block to the north.
                let mut shape = east(a.lon, CAMERA_LON - 0.01);
                let i = shape.len() - 1;
                shape.push((39.0 + self.north, CAMERA_LON - 0.01));
                shape.push((39.0 + self.north, CAMERA_LON + 0.01));
                let j = shape.len();
                shape.extend(east(CAMERA_LON + 0.01, b.lon));
                return Ok(vec![cand(shape, &[i, i + 1, i + 2, j])]);
            }
            Ok(vec![cand(east(a.lon, b.lon), &[])])
        }
    }

    struct NoRoads;
    impl RoadSource for NoRoads {
        async fn load(&self, _: &crate::roadnet::Area, _: &(dyn Fn(String) + Sync)) -> AppResult<Loaded> {
            Err(AppError::Offline("no road map in this test".into()))
        }
    }

    #[test]
    fn a_long_trip_is_cleared_stretch_by_stretch() {
        let cams = vec![cam("node/1", 39.0, CAMERA_LON)];
        let source = |b: &BBox| Ok(cams.iter().filter(|c| b.contains(c.lat, c.lon)).cloned().collect());
        let start = LatLon { lat: 39.0, lon: -105.0 };
        let end = LatLon { lat: 39.0, lon: -104.0 };
        let plan = tauri::async_runtime::block_on(plan(&Corridor { north: 0.005 }, &NoRoads, "test", start, end, source, |_| {})).unwrap();
        assert_eq!(plan.fastest.cameras.len(), 1);
        assert_eq!(plan.road_check, RoadCheck::Stretches { fixed: 1, total: 1, too_slow: 0, limit_min: Some(5) });
        assert_eq!(plan.outcome, Outcome::Clear);
        assert!(plan.avoid.cameras.is_empty());
        // One start, one arrival, the detour's turns in between; the route is continuous.
        let kinds: Vec<u32> = plan.avoid.maneuvers.iter().map(|m| m.kind).collect();
        assert_eq!(kinds.iter().filter(|&&k| (1..=3).contains(&k)).count(), 1, "{kinds:?}");
        assert_eq!(kinds.iter().filter(|&&k| (4..=6).contains(&k)).count(), 1, "{kinds:?}");
        assert_eq!(kinds.iter().filter(|&&k| k == 15).count(), 4, "{kinds:?}");
        for w in plan.avoid.shape.windows(2) {
            assert!(haversine_m(w[0][0], w[0][1], w[1][0], w[1][1]) < 2_000.0, "a gap in the route at {w:?}");
        }
        let extra = plan.avoid.distance_m - plan.fastest.distance_m;
        assert!((extra - 2.0 * 556.0).abs() < 60.0, "the detour adds only the block: {extra:.0} m");
        assert!(plan.requests <= MAX_REQUESTS + LOCAL_REQUESTS);
    }

    #[test]
    fn a_way_round_that_costs_too_much_time_is_not_taken() {
        // The only way round goes 17 km north and back: about half an hour for one camera.
        let cams = vec![cam("node/1", 39.0, CAMERA_LON)];
        let source = |b: &BBox| Ok(cams.iter().filter(|c| b.contains(c.lat, c.lon)).cloned().collect());
        let start = LatLon { lat: 39.0, lon: -105.0 };
        let end = LatLon { lat: 39.0, lon: -104.0 };
        let plan = tauri::async_runtime::block_on(plan(&Corridor { north: 0.15 }, &NoRoads, "test", start, end, source, |_| {})).unwrap();
        assert_eq!(plan.road_check, RoadCheck::Stretches { fixed: 0, total: 1, too_slow: 1, limit_min: Some(5) });
        assert_eq!(plan.avoid.cameras.len(), 1, "keeps the road and its camera");
        assert_eq!(plan.avoid.cameras[0].remaining, Some(Remaining::LongDetour));
        assert!((plan.avoid.duration_s - plan.fastest.duration_s).abs() < 1.0);

        // With no limit, the half hour is worth it.
        let opts = PlanOptions { max_extra_secs_per_camera: None, ..PlanOptions::default() };
        let source = |b: &BBox| Ok(cams.iter().filter(|c| b.contains(c.lat, c.lon)).cloned().collect());
        let fut = plan_with(&Corridor { north: 0.15 }, &NoRoads, "test", start, end, source, |_| {}, opts);
        let plan = tauri::async_runtime::block_on(fut).unwrap();
        assert_eq!(plan.road_check, RoadCheck::Stretches { fixed: 1, total: 1, too_slow: 0, limit_min: None });
        assert!(plan.avoid.cameras.is_empty());
    }
}
