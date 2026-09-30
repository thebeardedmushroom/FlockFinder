//! A new route from where the car is, in the same mode as the one it left.
//!
//! **Fastest**: one request to the destination.
//!
//! **Avoidance**, cheapest first (a reroute has to be quick, and the routing server is shared):
//! 1. *Rejoin*: route back onto the avoidance route about 1.5 km ahead (past any camera near
//!    that point), excluding cameras the way back passes, in up to [`REJOIN_REQUESTS`]
//!    requests. Used when it adds no cameras.
//! 2. *Replan*: the planner's first phase from here (at most [`REPLAN_REQUESTS`] requests),
//!    then its road-map phase on road tiles already cached (no downloads), capped at
//!    [`REPLAN_GUIDED_REQUESTS`].
//!
//! The route with the fewest cameras (then the quickest) wins. Every request goes through the
//! shared HTTP client, which spaces routing requests at least a second apart.

use super::route::{Mode, NavRoute};
use crate::error::{AppError, AppResult};
use crate::grid::BBox;
use crate::roadnet::RoadSource;
use crate::routing::repair::{route_around, Line};
use crate::routing::{planned, plan_with, stitch, AvoidCamera, Candidate, LatLon, PlanOptions, PlannedRoute, Router, Waypoint};
use std::collections::HashSet;

/// How far ahead on the old route to rejoin it.
pub const REJOIN_AHEAD_M: f64 = 1_500.0;
/// The rejoin point keeps at least this far from any camera on the route.
const REJOIN_CLEAR_M: f64 = 100.0;
/// Closer than this to the destination, route straight there.
const REJOIN_MIN_LEFT_M: f64 = 2_500.0;
pub const REJOIN_REQUESTS: u32 = 3;
pub const REPLAN_REQUESTS: u32 = 6;
pub const REPLAN_GUIDED_REQUESTS: u32 = 12;
/// A long trip's replan fixes at most this many requests' worth of stretches.
pub const REPLAN_REPAIR_REQUESTS: u32 = 12;

pub struct Rerouting {
    pub from: (f64, f64),
    pub heading: Option<f64>,
    pub mode: Mode,
    pub destination: (f64, f64),
    pub old: NavRoute,
    pub progress_m: f64,
    /// The detour limit from Settings (see `PlanOptions`).
    pub max_extra_secs_per_camera: Option<f64>,
}

/// The old route as the planner's candidate (for cutting it with `Line`).
fn candidate(old: &NavRoute) -> Candidate {
    Candidate { shape: old.shape.clone(), distance_m: old.length_m(), duration_s: old.duration_s, maneuvers: old.maneuvers.clone() }
}

/// Where to rejoin the old route: about `REJOIN_AHEAD_M` on, moved further on until clear of
/// cameras. `None` when that is too near the end (route to the destination instead).
fn rejoin_point(old: &NavRoute, progress: f64) -> Option<f64> {
    let end = old.length_m();
    if end - progress < REJOIN_MIN_LEFT_M {
        return None;
    }
    let mut at = progress + REJOIN_AHEAD_M;
    while let Some(c) = old.cameras.iter().find(|c| (c.along_m - at).abs() < REJOIN_CLEAR_M) {
        at = c.along_m + REJOIN_CLEAR_M;
    }
    (at < end - 500.0).then_some(at)
}

/// Better: fewer cameras, then quicker.
fn better(a: &PlannedRoute, b: &PlannedRoute) -> bool {
    a.cameras.len() < b.cameras.len() || (a.cameras.len() == b.cameras.len() && a.duration_s < b.duration_s)
}

pub(crate) async fn reroute<R, S, C>(router: &R, roads: &S, server: &str, req: &Rerouting, cameras: C) -> AppResult<NavRoute>
where
    R: Router,
    S: RoadSource,
    C: Fn(&BBox) -> AppResult<Vec<AvoidCamera>>,
{
    let from = Waypoint { lat: req.from.0, lon: req.from.1, through: false, heading: req.heading };
    let dest = Waypoint::at(LatLon { lat: req.destination.0, lon: req.destination.1 });
    let into_nav = |p: PlannedRoute| NavRoute::new(req.mode, &p).ok_or_else(|| AppError::Parse("the new route is empty".into()));
    let none = HashSet::new();
    let mut used = 0;

    if req.mode == Mode::Fastest {
        return match route_around(router, from, dest, &cameras, &none, 1, &mut used).await? {
            Some(e) => into_nav(planned(e)),
            None => Err(AppError::Invalid("no route from here".into())),
        };
    }

    // 1. Back onto the avoidance route ahead.
    let mut best: Option<PlannedRoute> = None;
    if let Some(at) = rejoin_point(&req.old, req.progress_m) {
        let old = candidate(&req.old);
        let line = Line::new(&old);
        let to = line.waypoint(at);
        if let Some(back) = route_around(router, from, to, &cameras, &none, REJOIN_REQUESTS, &mut used).await? {
            if let Some(joined) = stitch(vec![back.candidate, line.slice(at, line.length())]) {
                let spliced = planned(crate::routing::evaluate(joined, &cameras)?);
                let old_ahead = req.old.cameras_ahead(req.progress_m);
                log::info!("reroute: rejoin {:.0} m ahead passes {} cameras (old route ahead: {old_ahead})", at - req.progress_m, spliced.cameras.len());
                if spliced.cameras.len() <= old_ahead {
                    // Adds nothing: good enough, no need to replan.
                    return into_nav(spliced);
                }
                best = Some(spliced);
            }
        }
    }

    // 2. Replan from here, quickly, with cached road tiles only.
    let opts = PlanOptions {
        start_heading: req.heading,
        max_requests: REPLAN_REQUESTS,
        guided_requests: REPLAN_GUIDED_REQUESTS,
        repair_requests: REPLAN_REPAIR_REQUESTS,
        max_extra_secs_per_camera: req.max_extra_secs_per_camera,
        ..PlanOptions::default()
    };
    let start = LatLon { lat: req.from.0, lon: req.from.1 };
    let end = LatLon { lat: req.destination.0, lon: req.destination.1 };
    match plan_with(router, roads, server, start, end, &cameras, |_| {}, opts).await {
        Ok(plan) => {
            log::info!("reroute: replan passes {} cameras ({} requests)", plan.avoid.cameras.len(), plan.requests);
            if best.as_ref().map_or(true, |b| better(&plan.avoid, b)) {
                best = Some(plan.avoid);
            }
        }
        Err(e) if best.is_some() => log::warn!("reroute: replan failed ({e}); using the rejoin"),
        Err(e) => return Err(e),
    }
    best.map(into_nav).unwrap_or_else(|| Err(AppError::Invalid("no route from here".into())))
}

#[cfg(test)]
mod tests {
    use super::super::route::testing::route_through;
    use super::*;
    use crate::roadnet::Loaded;
    use crate::geo_util::haversine_m;
    use crate::routing::{Maneuver, RouteError, RouteRequest};
    use crate::routing::RouteCamera;
    use std::cell::RefCell;

    struct Fake {
        /// Answers in order.
        answers: RefCell<Vec<Result<Vec<Candidate>, RouteError>>>,
        asked: RefCell<Vec<RouteRequest>>,
    }

    impl Router for Fake {
        async fn route(&self, req: &RouteRequest) -> Result<Vec<Candidate>, RouteError> {
            self.asked.borrow_mut().push(req.clone());
            let mut a = self.answers.borrow_mut();
            if a.is_empty() {
                return Err(RouteError::NoRoute);
            }
            a.remove(0)
        }
    }

    struct NoRoads;
    impl RoadSource for NoRoads {
        async fn load(&self, _: &crate::roadnet::Area, _: &(dyn Fn(String) + Sync)) -> AppResult<Loaded> {
            Err(AppError::Offline("no tiles cached".into()))
        }
    }

    fn line(a: (f64, f64), b: (f64, f64), n: usize) -> Vec<(f64, f64)> {
        (0..=n).map(|i| {
            let t = i as f64 / n as f64;
            (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
        }).collect()
    }

    fn cand(shape: Vec<(f64, f64)>) -> Candidate {
        let n = shape.len();
        Candidate {
            distance_m: crate::geo_util::polyline_length_m(&shape),
            duration_s: 300.0,
            maneuvers: vec![
                Maneuver { instruction: "Drive.".into(), kind: 1, ..Default::default() },
                Maneuver { instruction: "Turn.".into(), kind: 10, shape_index: n / 2, ..Default::default() },
                Maneuver { instruction: "Arrive.".into(), kind: 4, shape_index: n - 1, ..Default::default() },
            ],
            shape,
        }
    }

    fn cam(key: &str, p: (f64, f64)) -> AvoidCamera {
        AvoidCamera { key: key.into(), lat: p.0, lon: p.1, category: "flock".into(), source: "osm".into(), direction: None, operator: None }
    }

    #[test]
    fn an_avoidance_reroute_rejoins_the_old_route_when_that_adds_no_cameras() {
        // Old route: 8 km north. The car is 300 m east of it at 1 km.
        let old = route_through(&line((39.0, -105.0), (39.072, -105.0), 16), Mode::Avoid);
        let car = (39.009, -104.9965);
        let rejoin_at = 1000.0 + REJOIN_AHEAD_M;
        let back = cand(line(car, old.point_at(rejoin_at), 10));
        let router = Fake { answers: RefCell::new(vec![Ok(vec![back])]), asked: RefCell::new(Vec::new()) };
        let req = Rerouting { from: car, heading: Some(0.0), mode: Mode::Avoid, destination: old.destination(), old: old.clone(), progress_m: 1000.0, max_extra_secs_per_camera: None };
        let fut = reroute(&router, &NoRoads, "test", &req, |_: &BBox| Ok(Vec::new()));
        let r = tauri::async_runtime::block_on(fut).unwrap();
        assert_eq!(router.asked.borrow().len(), 1, "one request");
        assert_eq!(r.mode, Mode::Avoid);
        let asked = &router.asked.borrow()[0];
        assert_eq!(asked.locations[0].heading, Some(0.0), "leaves in the direction of travel");
        // The way back, then the old route to its end.
        assert!((r.length_m() - (haversine_m(car.0, car.1, old.point_at(rejoin_at).0, old.point_at(rejoin_at).1) + old.length_m() - rejoin_at)).abs() < 20.0);
        assert_eq!(r.destination(), old.destination());
        // Maneuvers: the way back's start and turn, then the old route's remaining turns and arrival.
        let kinds: Vec<u32> = r.maneuvers.iter().map(|m| m.kind).collect();
        assert_eq!(kinds.first(), Some(&1));
        assert_eq!(kinds.iter().filter(|&&k| k == 1).count(), 1, "the join's start is dropped: {kinds:?}");
        assert_eq!(kinds.last(), Some(&4));
        for w in r.maneuver_at.windows(2) {
            assert!(w[0] <= w[1], "maneuvers in order");
        }
    }

    #[test]
    fn the_rejoin_point_keeps_clear_of_cameras() {
        let mut old = route_through(&line((39.0, -105.0), (39.072, -105.0), 16), Mode::Avoid);
        old.cameras = vec![RouteCamera { camera: cam("node/1", old.point_at(2500.0)), along_m: 2500.0, distance_m: 0.0, remaining: None }];
        assert_eq!(rejoin_point(&old, 1000.0), Some(2600.0));
        assert_eq!(rejoin_point(&old, old.length_m() - 1000.0), None, "near the end: straight to the destination");
    }

    #[test]
    fn a_fastest_reroute_is_one_request_to_the_destination() {
        let old = route_through(&line((39.0, -105.0), (39.072, -105.0), 16), Mode::Fastest);
        let car = (39.009, -104.9965);
        let router = Fake { answers: RefCell::new(vec![Ok(vec![cand(line(car, old.destination(), 20))])]), asked: RefCell::new(Vec::new()) };
        let req = Rerouting { from: car, heading: None, mode: Mode::Fastest, destination: old.destination(), old, progress_m: 1000.0, max_extra_secs_per_camera: None };
        let r = tauri::async_runtime::block_on(reroute(&router, &NoRoads, "test", &req, |_: &BBox| Ok(Vec::new()))).unwrap();
        assert_eq!(router.asked.borrow().len(), 1);
        assert_eq!(r.mode, Mode::Fastest);
    }

    #[test]
    fn a_rejoin_past_a_camera_falls_back_to_replanning() {
        let old = route_through(&line((39.0, -105.0), (39.072, -105.0), 16), Mode::Avoid);
        let car = (39.009, -104.9965);
        let back_path = line(car, old.point_at(2500.0), 10);
        let camera = cam("node/9", back_path[5]);
        // Rejoin: passes the camera, then (excluded) no route. Replan: a clear route.
        let clear = cand(line(car, old.destination(), 30));
        let router = Fake {
            answers: RefCell::new(vec![Ok(vec![cand(back_path)]), Err(RouteError::NoRoute), Ok(vec![clear])]),
            asked: RefCell::new(Vec::new()),
        };
        let req = Rerouting { from: car, heading: Some(0.0), mode: Mode::Avoid, destination: old.destination(), old, progress_m: 1000.0, max_extra_secs_per_camera: None };
        let cams = vec![camera];
        let source = |b: &BBox| Ok(cams.iter().filter(|c| b.contains(c.lat, c.lon)).cloned().collect());
        let r = tauri::async_runtime::block_on(reroute(&router, &NoRoads, "test", &req, source)).unwrap();
        assert_eq!(r.cameras.len(), 0, "the replanned route");
        assert_eq!(router.asked.borrow().len(), 3);
        assert_eq!(router.asked.borrow()[2].locations[0].heading, Some(0.0), "the replan leaves in the direction of travel too");
    }
}
