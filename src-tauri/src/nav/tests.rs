//! Whole drives through the session, from GPX tracks in `fixtures/nav/`.
//!
//! The route is a real one: the recorded Midtown Atlanta → airport answer from
//! `fixtures/routing/` (with its real maneuvers and the 7 mapped cameras on it). The tracks
//! were driven along it by the simulator (GPS noise included) and saved as GPX, so these tests
//! replay fixed recordings. To re-record after changing the route fixture:
//! `cargo test --lib record_nav_tracks -- --ignored`.

use super::cameras::AlertStage;
use super::guidance::{Stage, FAR_HIGHWAY_M, FAR_SURFACE_M, NEAR_M};
use super::route::{is_arrival, is_start, Mode, NavRoute};
use super::session::*;
use super::sim::{offset, to_gpx, SimParams, Simulator};
use crate::gpx::parse_points;
use crate::routing::{cameras_on_route, parse_route_response, AvoidCamera, PlannedRoute, AVOID_RADIUS_M};
use std::collections::HashMap;

const ROUTING: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/routing");
const TRACKS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/nav");

fn gunzip(path: &str) -> Vec<u8> {
    let gz = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&gz[..]), &mut out).unwrap();
    out
}

/// The fastest Midtown → airport route, with its cameras.
fn fixture_route(mode: Mode) -> NavRoute {
    let recorded: Vec<serde_json::Value> =
        serde_json::from_slice(&gunzip(&format!("{ROUTING}/atlanta-midtown-airport.valhalla.json.gz"))).unwrap();
    let answer = recorded[0]["ok"].as_str().expect("the first request was answered");
    let c = parse_route_response(answer).unwrap().remove(0);
    #[derive(serde::Deserialize)]
    struct C {
        key: String,
        lat: f64,
        lon: f64,
        category: String,
    }
    let cams: Vec<AvoidCamera> = serde_json::from_slice::<Vec<C>>(&gunzip(&format!("{ROUTING}/atlanta-midtown-airport.cameras.json.gz")))
        .unwrap()
        .into_iter()
        .map(|c| AvoidCamera { key: c.key, lat: c.lat, lon: c.lon, category: c.category, source: "osm".into(), direction: None, operator: None })
        .collect();
    let cameras = cameras_on_route(&c.shape, &cams, AVOID_RADIUS_M);
    let planned = PlannedRoute {
        shape: c.shape.iter().map(|&(a, b)| [a, b]).collect(),
        distance_m: c.distance_m,
        duration_s: c.duration_s,
        cameras,
        maneuvers: c.maneuvers,
        bbox: crate::grid::BBox::new(0.0, 0.0, 0.0, 0.0),
    };
    NavRoute::new(mode, &planned).unwrap()
}

fn destination() -> Destination {
    Destination { lat: 33.6407, lon: -84.4277, label: "Airport".into() }
}

const T0: i64 = 1_790_000_000_000;

fn track(name: &str) -> Vec<Fix> {
    let pts = parse_points(&std::fs::read_to_string(format!("{TRACKS}/{name}.gpx")).unwrap_or_else(|e| panic!("{name}.gpx: {e}"))).unwrap();
    // Replayed as a phone would deliver them (speed and course worked out between points).
    let mut sim = Simulator::replay(pts, SimParams::default(), T0);
    let mut out = Vec::new();
    if let Some(f) = sim.step(0) {
        out.push(f);
    }
    while !sim.finished() {
        if let Some(f) = sim.step(1000) {
            out.push(f);
        }
    }
    out
}

/// What happened on a drive.
#[derive(Default)]
struct Log {
    /// (time, effect, snapshot after)
    effects: Vec<(i64, Effect, Snapshot)>,
    /// (time, state) at every change.
    states: Vec<(i64, NavState)>,
    /// (time, progress) every second.
    progress: Vec<(i64, f64)>,
}

impl Log {
    fn prompts(&self) -> Vec<(i64, &super::guidance::Prompt, f64)> {
        self.effects
            .iter()
            .filter_map(|(t, e, s)| match e {
                Effect::Speak(p) => Some((*t, p, s.progress_m)),
                _ => None,
            })
            .collect()
    }

    fn reroutes(&self) -> Vec<(i64, &RerouteRequest)> {
        self.effects
            .iter()
            .filter_map(|(t, e, _)| match e {
                Effect::Reroute(r) => Some((*t, r)),
                _ => None,
            })
            .collect()
    }
}

/// Feed `fixes` to the session with a tick every second in between (and `extra_s` seconds of
/// ticks after the last). `answer` is called for every reroute request.
fn drive(
    session: &mut Session,
    fixes: &[Fix],
    extra_s: i64,
    mut answer: impl FnMut(&Session, &RerouteRequest) -> Option<Result<NavRoute, String>>,
) -> Log {
    let mut log = Log::default();
    let mut last_state = session.state();
    log.states.push((T0, last_state));
    let mut clock = fixes.first().map(|f| f.time_ms).unwrap_or(T0);
    let end = fixes.last().map(|f| f.time_ms).unwrap_or(T0) + extra_s * 1000;
    let mut next_fix = 0;
    while clock <= end {
        let mut effects = Vec::new();
        while next_fix < fixes.len() && fixes[next_fix].time_ms <= clock {
            effects.extend(session.on_fix(fixes[next_fix]));
            next_fix += 1;
        }
        effects.extend(session.on_tick(clock));
        log.progress.push((clock, session.progress()));
        let mut i = 0;
        while i < effects.len() {
            if let Effect::Reroute(r) = &effects[i] {
                if let Some(result) = answer(session, r) {
                    let id = r.id;
                    effects.extend(session.on_reroute(id, result));
                }
            }
            i += 1;
        }
        for e in effects {
            log.effects.push((clock, e, session.snapshot()));
        }
        if session.state() != last_state {
            last_state = session.state();
            log.states.push((clock, last_state));
        }
        clock += 1000;
    }
    log
}

fn no_answer(_: &Session, _: &RerouteRequest) -> Option<Result<NavRoute, String>> {
    None
}

#[test]
fn a_full_drive_guides_to_the_destination() {
    let route = fixture_route(Mode::Fastest);
    assert_eq!(route.cameras.len(), 7, "the fixture route passes 7 cameras");
    let mut s = Session::new(route.clone(), destination(), T0);
    let fixes = track("atlanta-drive");
    let log = drive(&mut s, &fixes, 5, no_answer);

    // Never off the route, never rerouted, arrived once at the end.
    assert!(log.states.iter().all(|(_, st)| matches!(st, NavState::Navigating | NavState::Arrived)), "{:?}", log.states);
    assert!(log.reroutes().is_empty());
    assert_eq!(s.state(), NavState::Arrived);
    assert_eq!(log.effects.iter().filter(|(_, e, _)| matches!(e, Effect::Arrived)).count(), 1);

    // Progress only moves forward.
    for w in log.progress.windows(2) {
        assert!(w[1].1 >= w[0].1, "progress went back at {}: {} → {}", w[1].0, w[0].1, w[1].1);
    }

    let prompts = log.prompts();
    // The opening prompt first: the start maneuver's full text.
    assert_eq!(prompts[0].1.maneuver, 0);
    assert_eq!(Some(&prompts[0].1.text), route.maneuvers[0].verbal_pre.as_ref());
    // Every maneuver after the start gets its "now" prompt (the arrival is announced on
    // arrival), each stage at most once, each at the right distance.
    let mut seen: HashMap<(usize, Stage), usize> = HashMap::new();
    for (_, p, progress) in &prompts {
        *seen.entry((p.maneuver, p.stage)).or_default() += 1;
        let before = route.maneuver_at[p.maneuver] - progress;
        let highway = p.maneuver > 0 && route.maneuvers[p.maneuver - 1].highway;
        let slack = 25.0; // one fix at 20 m/s, plus noise
        match p.stage {
            Stage::Far => assert!(before <= if highway { FAR_HIGHWAY_M } else { FAR_SURFACE_M } + 1.0, "{p:?} at {before:.0} m"),
            Stage::Near => assert!(before <= NEAR_M + 1.0 && before >= NEAR_M - 60.0 - slack, "{p:?} at {before:.0} m"),
            Stage::Now => {
                if p.maneuver > 0 && !is_arrival(route.maneuvers[p.maneuver].kind) {
                    assert!((0.0..=250.0 + slack).contains(&before), "{p:?} at {before:.0} m");
                }
            }
            Stage::Post => {}
        }
    }
    assert!(seen.values().all(|&n| n == 1), "a prompt repeated: {seen:?}");
    for (i, m) in route.maneuvers.iter().enumerate() {
        if !is_start(m.kind) && !is_arrival(m.kind) {
            assert!(seen.contains_key(&(i, Stage::Now)), "no prompt at maneuver {i}: {}", m.instruction);
        }
    }
    // Arrival is announced last, in the server's words ("Your destination is on the right.").
    let arrival = route.maneuvers.last().unwrap();
    assert!(is_arrival(arrival.kind));
    assert_eq!(Some(&prompts.last().unwrap().1.text), arrival.verbal_pre.as_ref());

    // Camera alerts: each camera ahead gets one at a quarter mile and one at 500 ft, grouped
    // when close together; all 7 are passed.
    let mut alerted: HashMap<(String, AlertStage), usize> = HashMap::new();
    for (_, e, snap) in &log.effects {
        if let Effect::CameraAlert(a) = e {
            for c in &a.cameras {
                *alerted.entry((c.key.clone(), a.stage)).or_default() += 1;
                let limit = if a.stage == AlertStage::Far { super::cameras::FAR_M } else { super::cameras::NEAR_M };
                assert!(c.distance_m <= limit + 150.0 && c.distance_m > 0.0, "{c:?}");
                assert!(snap.progress_m < route.cameras.iter().find(|rc| rc.camera.key == c.key).unwrap().along_m, "alerted after passing it");
            }
        }
    }
    assert!(alerted.values().all(|&n| n == 1), "an alert repeated: {alerted:?}");
    for c in &route.cameras {
        assert!(alerted.contains_key(&(c.camera.key.clone(), AlertStage::Near)), "no 500 ft alert for {}", c.camera.key);
    }
    let summary = s.snapshot().summary.expect("summary on arrival");
    assert_eq!(summary.cameras_passed, 7);
    assert!((summary.distance_m - route.length_m()).abs() < route.length_m() * 0.1, "{summary:?}");
}

#[test]
fn leaving_the_route_reroutes_once_within_the_thresholds_and_keeps_avoidance_mode() {
    let route = fixture_route(Mode::Avoid);
    let mut s = Session::new(route.clone(), destination(), T0);
    let fixes = track("atlanta-detour");
    let log = drive(&mut s, &fixes, 0, no_answer);
    let reroutes = log.reroutes();
    assert_eq!(reroutes.len(), 1, "one request while it is outstanding");
    let (t, req) = reroutes[0];
    assert_eq!(req.mode, Mode::Avoid);
    // When did the car first get more than 40 m off (by the fixes)?
    let first_off = fixes
        .iter()
        .find(|f| crate::geo_util::point_to_polyline(f.lat, f.lon, &route.shape).unwrap().distance_m > OFF_ROUTE_M + 10.0)
        .unwrap()
        .time_ms;
    let after = t - first_off;
    assert!((OFF_ROUTE_MS - 2_000..=OFF_ROUTE_MS + 3_000).contains(&after), "rerouted {after} ms after leaving");
    // The car came back: guidance picked up again on the old route.
    assert!(log.states.iter().any(|(_, st)| *st == NavState::Rerouting));
    assert_eq!(log.states.last().unwrap().1, NavState::Arrived);
}

#[test]
fn an_answered_reroute_replaces_the_route_and_reports_more_cameras() {
    let route = fixture_route(Mode::Avoid);
    let mut s = Session::new(route.clone(), destination(), T0);
    let fixes = track("atlanta-detour");
    let mut answered = 0;
    let log = drive(&mut s, &fixes, 0, |session, req| {
        answered += 1;
        // A route from the car straight back to the old one 1 km on, then the rest of it, with
        // an extra camera on the way back.
        let join = session.route().point_at(req.progress_m + 1000.0);
        let mut pts = vec![req.from, join];
        let from_seg = session.route().segment_at(req.progress_m + 1000.0);
        pts.extend_from_slice(&session.route().shape[from_seg + 1..]);
        let mut r = super::route::testing::route_through(&pts, Mode::Avoid);
        r.cameras = session.route().cameras.clone();
        for c in &mut r.cameras {
            c.along_m = c.along_m - req.progress_m - 1000.0 + crate::geo_util::haversine_m(req.from.0, req.from.1, join.0, join.1);
        }
        let mut extra = r.cameras[r.cameras.len() - 1].clone();
        extra.camera.key = "node/extra".into();
        extra.along_m = 50.0;
        r.cameras.insert(0, extra);
        r.cameras.retain(|c| c.along_m > 0.0);
        Some(Ok(r))
    });
    // (The recorded car carries on with its detour, so it leaves each new route too; each
    // leaving is its own reroute, never sooner than the limit.)
    assert!(answered >= 1);
    let times: Vec<i64> = log.reroutes().iter().map(|(t, _)| *t).collect();
    assert!(times.windows(2).all(|w| w[1] - w[0] >= REROUTE_MIN_MS), "{times:?}");
    assert!(log.effects.iter().any(|(_, e, _)| matches!(e, Effect::RouteChanged)));
    let (_, _, after) = log.effects.iter().find(|(_, e, _)| matches!(e, Effect::RouteChanged)).unwrap();
    assert_eq!(after.route_version, 2);
    assert_eq!(after.mode, Mode::Avoid);
    assert_eq!(after.state, NavState::Navigating);
    let change = after.camera_change.as_ref().expect("more cameras is reported");
    assert!(change.after > change.before, "{change:?}");
}

#[test]
fn reroute_attempts_never_come_faster_than_the_rate_limit() {
    let route = fixture_route(Mode::Avoid);
    let mut s = Session::new(route.clone(), destination(), T0);
    // Drive 3 km on the route, then straight off it (east) for 3 minutes; every reroute fails.
    let mut fixes: Vec<Fix> = track("atlanta-drive").into_iter().take(150).collect();
    let last = *fixes.last().unwrap();
    for i in 1..=180 {
        let p = offset((last.lat, last.lon), 90.0, 15.0 * i as f64);
        fixes.push(Fix { lat: p.0, lon: p.1, accuracy_m: 5.0, speed_mps: Some(15.0), course_deg: Some(90.0), time_ms: last.time_ms + i * 1000 });
    }
    let log = drive(&mut s, &fixes, 0, |_, _| Some(Err("offline".into())));
    let times: Vec<i64> = log.reroutes().iter().map(|(t, _)| *t).collect();
    assert!(times.len() >= 3, "{times:?}");
    let gaps: Vec<i64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(gaps.iter().all(|&g| g >= REROUTE_MIN_MS), "{gaps:?}");
    // Backing off: 10, 20, 40, 60 s.
    assert_eq!(&gaps[..3.min(gaps.len())], &[10_000, 20_000, 40_000][..3.min(gaps.len())]);
    assert!(s.snapshot().notice.unwrap().contains("Couldn't reroute"));
}

#[test]
fn losing_the_signal_dead_reckons_then_pauses_without_rerouting() {
    let route = fixture_route(Mode::Fastest);
    let mut s = Session::new(route.clone(), destination(), T0);
    let fixes = track("atlanta-tunnel");
    // Find the gap.
    let (gap_at, gap_ms) = fixes.windows(2).map(|w| (w[0].time_ms, w[1].time_ms - w[0].time_ms)).max_by_key(|g| g.1).unwrap();
    assert!(gap_ms >= 40_000, "the track has a 40 s gap");
    let log = drive(&mut s, &fixes, 0, no_answer);
    let lost = log.states.iter().find(|(_, st)| *st == NavState::LostSignal).expect("signal lost").0;
    assert!((LOST_AFTER_MS..=LOST_AFTER_MS + 1000).contains(&(lost - gap_at)), "lost after {} ms", lost - gap_at);
    let paused = log.states.iter().find(|(_, st)| *st == NavState::Paused).expect("paused").0;
    assert_eq!(paused - gap_at, LOST_AFTER_MS + DEAD_RECKON_MS);
    // Dead reckoning moved the progress on at about the last speed (20 m/s) until the pause,
    // and not after.
    let at = |t: i64| log.progress.iter().find(|(pt, _)| *pt == t).map(|p| p.1).unwrap();
    let reckoned = at(paused) - at(gap_at);
    let expected = 20.0 * (paused - gap_at) as f64 / 1000.0;
    assert!((reckoned - expected).abs() < expected * 0.15, "dead reckoning: {reckoned:.0} m, expected about {expected:.0}");
    assert_eq!(at(paused + 3000), at(paused), "paused: no more dead reckoning");
    // No reroute during the gap, and guidance picked up again afterwards.
    assert!(log.reroutes().is_empty());
    let back = log.states.iter().find(|(t, st)| *t > paused && *st == NavState::Navigating);
    assert!(back.is_some(), "{:?}", log.states);
    assert_eq!(s.state(), NavState::Arrived);
}

#[test]
fn a_weak_fix_far_off_the_route_never_triggers_off_route() {
    let route = fixture_route(Mode::Fastest);
    let mut s = Session::new(route.clone(), destination(), T0);
    let mut fixes: Vec<Fix> = track("atlanta-drive").into_iter().take(60).collect();
    let last = *fixes.last().unwrap();
    for i in 1..=30 {
        let p = offset(route.point_at(s.progress() + 20.0 * 60.0 + 20.0 * i as f64), 90.0, 80.0);
        fixes.push(Fix { lat: p.0, lon: p.1, accuracy_m: 90.0, speed_mps: Some(20.0), course_deg: None, time_ms: last.time_ms + i * 1000 });
    }
    let log = drive(&mut s, &fixes, 0, no_answer);
    assert!(log.reroutes().is_empty());
    assert!(!log.states.iter().any(|(_, st)| *st == NavState::OffRoute));
    assert!(s.snapshot().weak_signal);
}

#[test]
fn a_long_stop_repeats_nothing_and_never_reroutes() {
    let route = fixture_route(Mode::Fastest);
    let mut s = Session::new(route.clone(), destination(), T0);
    let mut fixes: Vec<Fix> = track("atlanta-drive").into_iter().take(40).collect();
    let last = *fixes.last().unwrap();
    // Five minutes at a light, with GPS wander of up to 15 m.
    for i in 1..=300 {
        let p = offset((last.lat, last.lon), (i * 37 % 360) as f64, (i % 15) as f64);
        fixes.push(Fix { lat: p.0, lon: p.1, accuracy_m: 8.0, speed_mps: Some(0.0), course_deg: None, time_ms: last.time_ms + i * 1000 });
    }
    let log = drive(&mut s, &fixes, 0, no_answer);
    let during: Vec<_> = log.effects.iter().filter(|(t, _, _)| *t > last.time_ms).collect();
    assert!(during.iter().all(|(_, e, _)| !matches!(e, Effect::Reroute(_) | Effect::Speak(_))), "{:?}", during.len());
}

#[test]
fn starting_far_from_the_route_reroutes_at_once() {
    let route = fixture_route(Mode::Avoid);
    let mut s = Session::new(route.clone(), destination(), T0);
    let p = offset(route.shape[0], 180.0, 600.0);
    let effects = s.on_fix(Fix { lat: p.0, lon: p.1, accuracy_m: 6.0, speed_mps: Some(0.0), course_deg: None, time_ms: T0 });
    assert!(effects.iter().any(|e| matches!(e, Effect::Reroute(r) if r.mode == Mode::Avoid)));
    assert_eq!(s.state(), NavState::Rerouting);
}

#[test]
fn driving_past_the_destination_arrives() {
    let route = fixture_route(Mode::Fastest);
    let mut s = Session::new(route.clone(), destination(), T0);
    let n = route.shape.len();
    let dir = crate::routing::bearing(route.shape[n - 2], route.shape[n - 1]);
    let mut fixes = Vec::new();
    let mut t = T0;
    // Along the last 300 m, then on past the end without a fix within 30 m of it.
    let mut along = route.length_m() - 300.0;
    while along < route.length_m() - 40.0 {
        let p = route.point_at(along);
        fixes.push(Fix { lat: p.0, lon: p.1, accuracy_m: 5.0, speed_mps: Some(20.0), course_deg: Some(dir), time_ms: t });
        along += 20.0;
        t += 1000;
    }
    let past = offset(route.destination(), dir, 45.0);
    fixes.push(Fix { lat: past.0, lon: past.1, accuracy_m: 5.0, speed_mps: Some(20.0), course_deg: Some(dir), time_ms: t });
    let mut arrived = false;
    for f in fixes {
        arrived |= s.on_fix(f).iter().any(|e| matches!(e, Effect::Arrived));
    }
    assert!(arrived);
}

/// Records the GPX tracks above with the simulator. Opt-in:
/// `cargo test --lib record_nav_tracks -- --ignored`
#[test]
#[ignore]
fn record_nav_tracks() {
    let route = fixture_route(Mode::Fastest);
    std::fs::create_dir_all(TRACKS).unwrap();
    let params = SimParams { speed_mps: 20.0, noise_m: 4.0, rate: 1.0, seed: 42 };
    let record = |name: &str, act: &dyn Fn(usize, &mut Simulator)| {
        let mut sim = Simulator::drive(route.shape.clone(), 0.0, params, T0);
        let mut fixes = Vec::new();
        let mut i = 0;
        // On to the end, and a few seconds standing there.
        let mut after_end = 0;
        while after_end < 5 {
            act(i, &mut sim);
            if let Some(f) = sim.step(1000) {
                fixes.push(f);
            }
            if sim.finished() {
                after_end += 1;
            }
            i += 1;
        }
        std::fs::write(format!("{TRACKS}/{name}.gpx"), to_gpx(&fixes)).unwrap();
        eprintln!("{name}: {} fixes", fixes.len());
    };
    record("atlanta-drive", &|_, _| {});
    // 3 km in, a right turn off the route for 400 m, and back.
    record("atlanta-detour", &|i, sim| {
        if i == 150 {
            sim.deviate(400.0);
        }
    });
    // 6 km in, 40 s without a signal.
    record("atlanta-tunnel", &|i, sim| {
        if i == 300 {
            sim.lose_signal(40.0);
        }
    });
}
