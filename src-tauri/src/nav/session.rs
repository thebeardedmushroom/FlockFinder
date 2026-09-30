//! The navigation session: a state machine fed with position fixes and clock ticks.
//!
//! ```text
//!             ┌──────────── back on the route ────────────┐
//!             ▼                                            │
//! start ─▶ Navigating ──(>40 m off for 5 s, moving)──▶ OffRoute ──▶ Rerouting ──(new route)──▶ Navigating
//!             │  ▲                                         ▲            │
//!             │  └── fix returns ── LostSignal ──(30 s)──▶ Paused       └──(failed: retry later)
//!             │         (no fix for 4 s; dead reckoning along the route)
//!             └──(within 30 m of the destination, or past it)──▶ Arrived
//! ```
//!
//! Everything here is plain logic with no I/O: it returns [`Effect`]s (say this, reroute
//! from here, …) for the runtime (nav/runtime.rs) to carry out, and time comes only from the
//! fixes and ticks it is given. That keeps it deterministic, so the tests replay recorded or
//! simulated drives through it.

use super::cameras::{CameraAlert, CameraAlerts};
use super::guidance::{Prompt, Prompter, Stage, THEN_WITHIN_M};
use super::route::{is_arrival, maneuver_icon, Mode, NavRoute};
use super::snap::{angle_diff, snap, Snap, Window};
use crate::geo_util::haversine_m;
use crate::routing::bearing;
use serde::{Deserialize, Serialize};

/// Farther than this from the route (after matching) is off it…
pub const OFF_ROUTE_M: f64 = 40.0;
/// …once that has lasted this long, while moving with a good fix.
pub const OFF_ROUTE_MS: i64 = 5_000;
/// Fixes vaguer than this never trigger off-route (and show the weak-signal indicator).
pub const MAX_ACCURACY_M: f64 = 50.0;
/// Slower than this is standing still (no off-route, no course).
pub const MOVING_MPS: f64 = 2.0;
/// Off the route, closer than this (heading the route's way) is back on it.
const BACK_ON_ROUTE_M: f64 = 25.0;
/// No fix for this long: signal lost.
pub const LOST_AFTER_MS: i64 = 4_000;
/// Dead reckoning along the route lasts this long, then guidance pauses.
pub const DEAD_RECKON_MS: i64 = 30_000;
/// "The new route passes more cameras" shows for this long.
const CAMERA_CHANGE_SHOW_MS: i64 = 90_000;
/// Arrived within this distance of the destination.
pub const ARRIVE_M: f64 = 30.0;
/// At most one reroute attempt per this long…
pub const REROUTE_MIN_MS: i64 = 10_000;
/// …and after failures, waits growing like this.
const REROUTE_BACKOFF_MS: [i64; 4] = [10_000, 20_000, 40_000, 60_000];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Fix {
    pub lat: f64,
    pub lon: f64,
    /// Horizontal accuracy (68% radius), metres.
    pub accuracy_m: f64,
    pub speed_mps: Option<f64>,
    /// Direction of travel, degrees clockwise from north.
    pub course_deg: Option<f64>,
    /// Milliseconds since the Unix epoch (or the simulator's clock).
    pub time_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NavState {
    Navigating,
    OffRoute,
    Rerouting,
    LostSignal,
    Paused,
    Arrived,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    pub lat: f64,
    pub lon: f64,
    pub label: String,
}

/// Ask for a new route from here.
#[derive(Debug, Clone)]
pub struct RerouteRequest {
    pub id: u64,
    pub from: (f64, f64),
    pub heading: Option<f64>,
    pub mode: Mode,
    pub destination: (f64, f64),
    /// The route being left and how far along it the car had got (for rejoining it).
    pub old: NavRoute,
    pub progress_m: f64,
}

#[derive(Debug, Clone)]
pub enum Effect {
    Speak(Prompt),
    CameraAlert(CameraAlert),
    Reroute(RerouteRequest),
    /// The route was replaced (the map should redraw it).
    RouteChanged,
    Arrived,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StepView {
    pub index: usize,
    pub kind: u32,
    /// Icon name (see `maneuver_icon`).
    pub icon: &'static str,
    pub instruction: String,
    pub street: Option<String>,
    pub distance_m: f64,
    pub exit_number: Option<String>,
    pub roundabout_exit_count: Option<u32>,
    pub bearing_before: Option<f64>,
    pub bearing_after: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CameraChange {
    /// Cameras ahead on the route that was replaced, and on its replacement.
    pub before: usize,
    pub after: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub elapsed_s: f64,
    pub distance_m: f64,
    pub cameras_passed: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PositionView {
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: f64,
    /// The position matched to the route (while on it).
    pub snapped: Option<[f64; 2]>,
    /// Direction to draw the car pointing (course when moving, the route's otherwise).
    pub bearing: f64,
    pub speed_mps: f64,
}

/// Everything the screen, the notification and the tests need to know.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Snapshot {
    pub state: NavState,
    pub mode: Mode,
    pub destination: Destination,
    pub route_version: u32,
    pub position: Option<PositionView>,
    pub progress_m: f64,
    pub remaining_m: f64,
    pub remaining_s: f64,
    pub eta_ms: Option<i64>,
    pub step: Option<StepView>,
    /// The maneuver right after `step`, when it follows within 150 m.
    pub then: Option<StepView>,
    pub cameras_ahead: usize,
    pub cameras_passed: usize,
    /// The latest camera alert, and when it was given.
    pub camera_alert: Option<CameraAlert>,
    pub camera_alert_ms: Option<i64>,
    pub weak_signal: bool,
    /// No fix yet.
    pub acquiring: bool,
    /// A message for the screen ("Couldn't reroute …").
    pub notice: Option<String>,
    pub camera_change: Option<CameraChange>,
    pub summary: Option<Summary>,
    pub muted: bool,
    pub now_ms: i64,
    pub started_ms: i64,
}

#[derive(Debug, Default, Clone)]
struct RerouteGate {
    last_ms: Option<i64>,
    failures: usize,
    in_flight: Option<u64>,
    next_id: u64,
}

impl RerouteGate {
    fn allowed(&self, now: i64) -> bool {
        if self.in_flight.is_some() {
            return false;
        }
        let wait = if self.failures == 0 {
            REROUTE_MIN_MS
        } else {
            REROUTE_BACKOFF_MS[(self.failures - 1).min(REROUTE_BACKOFF_MS.len() - 1)].max(REROUTE_MIN_MS)
        };
        self.last_ms.map_or(true, |t| now - t >= wait)
    }
}

pub struct Session {
    route: NavRoute,
    route_version: u32,
    destination: Destination,
    state: NavState,
    progress: f64,
    last_fix: Option<Fix>,
    last_snap: Option<Snap>,
    /// Speed and course from the latest fix (course only while moving).
    speed: f64,
    course: Option<f64>,
    /// Speed used for dead reckoning while the signal is lost.
    dr_speed: f64,
    /// Latest time seen (fix or tick).
    now: i64,
    last_tick: i64,
    started: i64,
    /// When the car first went off the route without coming back since.
    off_since: Option<i64>,
    /// Waiting for the first fix with good accuracy (to check the start is on the route).
    first_good_fix: bool,
    gate: RerouteGate,
    prompter: Prompter,
    alerts: CameraAlerts,
    last_alert: Option<(CameraAlert, i64)>,
    notice: Option<String>,
    /// And when (it is shown for a while).
    camera_change: Option<(CameraChange, i64)>,
    traveled: f64,
    summary: Option<Summary>,
    pub muted: bool,
}

impl Session {
    pub fn new(route: NavRoute, destination: Destination, now: i64) -> Self {
        Session {
            route,
            route_version: 1,
            destination,
            state: NavState::Navigating,
            progress: 0.0,
            last_fix: None,
            last_snap: None,
            speed: 0.0,
            course: None,
            dr_speed: 0.0,
            now,
            last_tick: now,
            started: now,
            off_since: None,
            first_good_fix: true,
            gate: RerouteGate::default(),
            prompter: Prompter::default(),
            alerts: CameraAlerts::default(),
            last_alert: None,
            notice: None,
            camera_change: None,
            traveled: 0.0,
            summary: None,
            muted: false,
        }
    }

    #[cfg(test)]
    pub fn state(&self) -> NavState {
        self.state
    }

    pub fn route(&self) -> &NavRoute {
        &self.route
    }

    pub fn route_version(&self) -> u32 {
        self.route_version
    }

    #[cfg(test)]
    pub fn progress(&self) -> f64 {
        self.progress
    }

    pub fn mode(&self) -> Mode {
        self.route.mode
    }

    fn guiding(&self) -> bool {
        matches!(self.state, NavState::Navigating | NavState::LostSignal)
    }

    /// Move the progress on to `to`, with the prompts and camera alerts that brings.
    fn advance(&mut self, to: f64, out: &mut Vec<Effect>) {
        let from = self.progress;
        self.progress = to.clamp(0.0, self.route.length_m());
        if !self.guiding() {
            return;
        }
        if let Some(p) = self.prompter.update(&self.route, self.progress, self.speed) {
            out.push(Effect::Speak(p));
        }
        if let Some(a) = self.alerts.update(&self.route, from, self.progress) {
            self.last_alert = Some((a.clone(), self.now));
            out.push(Effect::CameraAlert(a));
        }
    }

    fn request_reroute(&mut self, out: &mut Vec<Effect>) {
        let Some(fix) = self.last_fix else { return };
        if !self.gate.allowed(self.now) {
            return;
        }
        self.gate.next_id += 1;
        let id = self.gate.next_id;
        self.gate.in_flight = Some(id);
        self.gate.last_ms = Some(self.now);
        self.state = NavState::Rerouting;
        out.push(Effect::Reroute(RerouteRequest {
            id,
            from: (fix.lat, fix.lon),
            heading: self.course,
            mode: self.route.mode,
            destination: self.route.destination(),
            old: self.route.clone(),
            progress_m: self.progress,
        }));
    }

    fn arrive(&mut self, out: &mut Vec<Effect>) {
        self.state = NavState::Arrived;
        self.progress = self.route.length_m();
        self.summary = Some(Summary {
            elapsed_s: ((self.now - self.started) as f64 / 1000.0).max(0.0),
            distance_m: self.traveled,
            cameras_passed: self.alerts.passed(),
        });
        let text = self
            .route
            .maneuvers
            .iter()
            .rev()
            .find(|m| is_arrival(m.kind))
            .and_then(|m| m.verbal_pre.clone())
            .unwrap_or_else(|| "You have arrived at your destination.".into());
        let last = self.route.maneuvers.len().saturating_sub(1);
        out.push(Effect::Speak(Prompt { text, maneuver: last, stage: Stage::Now }));
        out.push(Effect::Arrived);
    }

    fn arrived_at(&self, fix: &Fix, s: &Snap) -> bool {
        let dest = self.route.destination();
        let d = haversine_m(fix.lat, fix.lon, dest.0, dest.1);
        if d <= ARRIVE_M {
            return true;
        }
        // Past it: matched to the very end of the route, still near it, and the progress had
        // got close to the end (not a jump from far back).
        let last_seg = self.route.shape.len() - 2;
        s.segment == last_seg && self.route.length_m() - s.along_m < 1.0 && d <= 150.0 && self.route.length_m() - self.progress < 200.0
    }

    pub fn on_fix(&mut self, fix: Fix) -> Vec<Effect> {
        let mut out = Vec::new();
        if self.state == NavState::Arrived {
            return out;
        }
        self.now = self.now.max(fix.time_ms);
        let prev = self.last_fix;
        // Speed and course: the fix's own, or worked out from the last fix.
        let moved = prev.map(|p| haversine_m(p.lat, p.lon, fix.lat, fix.lon)).unwrap_or(0.0);
        let dt = prev.map(|p| (fix.time_ms - p.time_ms) as f64 / 1000.0).unwrap_or(0.0);
        self.speed = fix.speed_mps.filter(|s| s.is_finite() && *s >= 0.0).unwrap_or(if dt > 0.0 { moved / dt } else { 0.0 });
        let moving = self.speed >= MOVING_MPS;
        self.course = if moving {
            fix.course_deg
                .filter(|c| c.is_finite())
                .or_else(|| prev.filter(|_| moved > 3.0).map(|p| bearing((p.lat, p.lon), (fix.lat, fix.lon))))
        } else {
            None
        };
        let good = fix.accuracy_m <= MAX_ACCURACY_M;
        if moving && good {
            self.traveled += moved;
        }
        let returning = matches!(self.state, NavState::LostSignal | NavState::Paused);
        let window = match (prev, self.state) {
            (None, _) | (_, NavState::Paused) => Window::Whole,
            (_, NavState::LostSignal) => Window::Around { along: self.progress, behind: 300.0, ahead: 2_000.0 },
            _ => Window::Around { along: self.progress, behind: 40.0, ahead: (self.speed * dt.max(1.0) * 2.0 + 100.0).max(150.0) },
        };
        let s = snap(&self.route, (fix.lat, fix.lon), self.course, window);
        self.last_fix = Some(fix);
        self.last_snap = Some(s);
        if returning {
            self.state = NavState::Navigating;
            self.off_since = None;
        }

        if self.arrived_at(&fix, &s) {
            self.advance(s.along_m, &mut out);
            self.arrive(&mut out);
            return out;
        }

        // The start: a first good fix far from the route reroutes at once.
        if self.first_good_fix && good {
            self.first_good_fix = false;
            let near_start = s.dist_m <= OFF_ROUTE_M;
            if near_start {
                self.progress = s.along_m;
                self.alerts.skip_behind(&self.route, s.along_m);
                if s.along_m < 50.0 {
                    if let Some(p) = self.prompter.opening(&self.route) {
                        out.push(Effect::Speak(p));
                    }
                }
                self.advance(s.along_m, &mut out);
            } else {
                self.state = NavState::OffRoute;
                self.request_reroute(&mut out);
            }
            return out;
        }

        match self.state {
            NavState::Navigating => {
                if s.dist_m <= OFF_ROUTE_M || !good {
                    if s.dist_m <= OFF_ROUTE_M {
                        self.off_since = None;
                    }
                    // Forward only: a match a little behind (jitter) holds the progress.
                    if good || s.dist_m <= fix.accuracy_m {
                        self.advance(s.along_m.max(self.progress), &mut out);
                    }
                } else if moving {
                    let since = *self.off_since.get_or_insert(fix.time_ms);
                    if fix.time_ms - since >= OFF_ROUTE_MS {
                        self.state = NavState::OffRoute;
                        self.request_reroute(&mut out);
                    }
                } else {
                    // Standing still off the route (a car park): wait until it moves.
                    self.off_since = None;
                }
            }
            NavState::OffRoute | NavState::Rerouting => {
                // Back on the route: here, or anywhere ahead heading its way.
                let back = if s.dist_m <= BACK_ON_ROUTE_M {
                    Some(s)
                } else {
                    let w = snap(&self.route, (fix.lat, fix.lon), self.course, Window::Whole);
                    let heading_ok = self.course.map_or(true, |c| angle_diff(c, w.bearing) <= 45.0);
                    (w.dist_m <= BACK_ON_ROUTE_M && heading_ok && w.along_m >= self.progress).then_some(w)
                };
                if let Some(b) = back.filter(|_| good) {
                    self.state = NavState::Navigating;
                    self.off_since = None;
                    // Whatever was asked for is no longer wanted.
                    self.gate.in_flight = None;
                    self.notice = None;
                    self.advance(b.along_m, &mut out);
                } else if self.state == NavState::OffRoute && good {
                    self.request_reroute(&mut out);
                }
            }
            _ => {}
        }
        out
    }

    pub fn on_tick(&mut self, now: i64) -> Vec<Effect> {
        let mut out = Vec::new();
        let dt = (now - self.last_tick).max(0) as f64 / 1000.0;
        self.last_tick = now;
        self.now = self.now.max(now);
        let Some(fix) = self.last_fix else { return out };
        let silent = now - fix.time_ms;
        match self.state {
            NavState::Navigating | NavState::OffRoute | NavState::Rerouting if silent >= LOST_AFTER_MS => {
                // No reroutes while the signal is lost; one in flight is dropped.
                let was_on_route = self.state == NavState::Navigating;
                self.gate.in_flight = None;
                self.off_since = None;
                self.state = NavState::LostSignal;
                // Dead reckoning only makes sense on the route.
                self.dr_speed = if was_on_route { self.speed } else { 0.0 };
                // Catch up on the time since the last fix.
                if was_on_route {
                    let gap = (now - fix.time_ms) as f64 / 1000.0;
                    self.advance(self.progress + self.dr_speed * gap, &mut out);
                }
            }
            NavState::LostSignal => {
                if silent >= LOST_AFTER_MS + DEAD_RECKON_MS {
                    self.state = NavState::Paused;
                } else if self.dr_speed > 0.0 {
                    // Dead reckoning: on along the route at the last speed.
                    self.advance(self.progress + self.dr_speed * dt, &mut out);
                }
            }
            NavState::OffRoute => {
                // Retry a failed reroute once its wait is over.
                if fix.accuracy_m <= MAX_ACCURACY_M {
                    self.request_reroute(&mut out);
                }
            }
            _ => {}
        }
        out
    }

    /// The answer to reroute request `id`.
    pub fn on_reroute(&mut self, id: u64, result: Result<NavRoute, String>) -> Vec<Effect> {
        let mut out = Vec::new();
        if self.gate.in_flight != Some(id) {
            return out;
        }
        self.gate.in_flight = None;
        match result {
            Ok(route) => {
                self.gate.failures = 0;
                let before = self.route.cameras_ahead(self.progress);
                self.route = route;
                self.route_version += 1;
                self.prompter.reset();
                let fix = self.last_fix.expect("a reroute starts from a fix");
                let s = snap(&self.route, (fix.lat, fix.lon), self.course, Window::Whole);
                self.last_snap = Some(s);
                self.progress = s.along_m;
                let after = self.route.cameras_ahead(self.progress);
                self.camera_change = (after > before).then_some((CameraChange { before, after }, self.now));
                self.state = NavState::Navigating;
                self.off_since = None;
                self.notice = None;
                out.push(Effect::RouteChanged);
                self.advance(s.along_m, &mut out);
            }
            Err(reason) => {
                self.gate.failures += 1;
                self.state = NavState::OffRoute;
                let wait = REROUTE_BACKOFF_MS[(self.gate.failures - 1).min(REROUTE_BACKOFF_MS.len() - 1)] / 1000;
                self.notice = Some(format!("Couldn't reroute ({reason}). Following the old route; trying again in {wait} s."));
            }
        }
        out
    }

    fn step_view(&self, i: usize) -> StepView {
        let m = &self.route.maneuvers[i];
        let names = if m.street_names.is_empty() { &m.begin_street_names } else { &m.street_names };
        StepView {
            index: i,
            kind: m.kind,
            icon: maneuver_icon(m.kind),
            instruction: m.instruction.clone(),
            street: (!names.is_empty()).then(|| names.join(" / ")),
            distance_m: (self.route.maneuver_at[i] - self.progress).max(0.0),
            exit_number: m.exit_number.clone(),
            roundabout_exit_count: m.roundabout_exit_count,
            bearing_before: m.bearing_before,
            bearing_after: m.bearing_after,
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let next = if self.state == NavState::Arrived { None } else { self.route.next_maneuver(self.progress) };
        let step = next.map(|i| self.step_view(i));
        let then = next.and_then(|i| {
            (i + 1 < self.route.maneuvers.len() && self.route.maneuver_at[i + 1] - self.route.maneuver_at[i] < THEN_WITHIN_M)
                .then(|| self.step_view(i + 1))
        });
        let remaining_s = if self.state == NavState::Arrived { 0.0 } else { self.route.remaining_s(self.progress) };
        let on_route = matches!(self.state, NavState::Navigating | NavState::LostSignal | NavState::Paused);
        let position = self.last_fix.map(|f| {
            let s = self.last_snap;
            let route_bearing = s.map(|s| s.bearing).unwrap_or(0.0);
            PositionView {
                lat: f.lat,
                lon: f.lon,
                accuracy_m: f.accuracy_m,
                snapped: s.filter(|s| on_route && s.dist_m <= OFF_ROUTE_M).map(|s| {
                    // While dead reckoning, the estimate along the route.
                    if self.state == NavState::LostSignal {
                        let p = self.route.point_at(self.progress);
                        [p.0, p.1]
                    } else {
                        [s.point.0, s.point.1]
                    }
                }),
                bearing: self.course.unwrap_or(route_bearing),
                speed_mps: self.speed,
            }
        });
        Snapshot {
            state: self.state,
            mode: self.route.mode,
            destination: self.destination.clone(),
            route_version: self.route_version,
            position,
            progress_m: self.progress,
            remaining_m: (self.route.length_m() - self.progress).max(0.0),
            remaining_s,
            eta_ms: (self.state != NavState::Arrived).then(|| self.now + (remaining_s * 1000.0) as i64),
            step,
            then,
            cameras_ahead: if self.state == NavState::Arrived { 0 } else { self.route.cameras_ahead(self.progress) },
            cameras_passed: self.alerts.passed(),
            camera_alert: self.last_alert.as_ref().map(|a| a.0.clone()),
            camera_alert_ms: self.last_alert.as_ref().map(|a| a.1),
            weak_signal: self.last_fix.is_some_and(|f| f.accuracy_m > MAX_ACCURACY_M),
            acquiring: self.last_fix.is_none(),
            notice: self.notice.clone(),
            camera_change: self.camera_change.as_ref().filter(|(_, at)| self.now - at < CAMERA_CHANGE_SHOW_MS).map(|(c, _)| c.clone()),
            summary: self.summary.clone(),
            muted: self.muted,
            now_ms: self.now,
            started_ms: self.started,
        }
    }

    /// Keys of the cameras the session alerts for (the map's proximity alerts skip them).
    pub fn covered_cameras(&self) -> Vec<String> {
        CameraAlerts::covered(&self.route)
    }
}
