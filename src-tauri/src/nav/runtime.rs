//! Runs a navigation session inside the app: feeds it fixes and ticks (from the phone or the
//! simulator), carries out its effects (speech, reroutes, the notification), and publishes its
//! state to the screen as `nav:state` / `nav:route` events.
//!
//! The session lives here, in the Rust process, not in the WebView: with the screen off or
//! the app in the background the WebView's JavaScript is throttled, but the foreground service
//! keeps this process (and so the session) running. When the WebView comes back it asks for
//! the current state (`nav_state`).

use super::platform::{EventSink, NotificationContent, Platform, PlatformEvent, SpeechKind};
use super::reroute::{reroute, Rerouting};
use super::route::{Mode, NavRoute};
use super::session::{Destination, Effect, NavState, Session, Snapshot};
use super::sim::{SimParams, Simulator};
use crate::gpx::TimedPoint;
use crate::db;
use crate::grid::BBox;
use crate::routing::{Maneuver, RouteCamera};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;

/// The trip being navigated, saved while a session runs so that after the system kills the
/// app it can offer to resume (never silently). One record, deleted when the trip ends.
const ACTIVE_TRIP_KEY: &str = "nav_active_trip";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActiveTrip {
    pub destination: Destination,
    pub mode: Mode,
    pub started_ms: i64,
}

/// Where simulated fixes come from.
pub enum SimSource {
    /// Driving the route.
    Drive(SimParams),
    /// A recorded GPX track, at its own times.
    Replay(Vec<TimedPoint>, SimParams),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SimCommand {
    Deviate { meters: f64 },
    LoseSignal { secs: f64 },
    Speed { mps: f64 },
    Noise { meters: f64 },
    Rate { rate: f64 },
}

enum Event {
    Platform(PlatformEvent),
    Tick(i64),
    Rerouted(u64, Result<NavRoute, String>),
    Mute(bool),
    Sim(SimCommand),
    Stop,
}

/// The route as drawn on the map.
#[derive(Debug, Clone, Serialize)]
pub struct RouteView {
    pub version: u32,
    pub mode: Mode,
    pub shape: Vec<[f64; 2]>,
    pub cameras: Vec<RouteCamera>,
    pub maneuvers: Vec<Maneuver>,
}

/// What the screen gets.
#[derive(Debug, Clone, Serialize)]
pub struct SessionView {
    #[serde(flatten)]
    pub snapshot: Snapshot,
    pub simulated: bool,
    /// Speech isn't available (no TTS engine or voice): shown once.
    pub voice_unavailable: bool,
    /// Battery saver is limiting location with the screen off.
    pub throttled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NavStatus {
    pub session: Option<SessionView>,
    /// A trip that was running when the app was last killed.
    pub resume: Option<ActiveTrip>,
    /// Why the last session ended, if not by the user (location turned off, …); given once.
    pub ended: Option<String>,
}

struct Shared {
    view: Mutex<SessionView>,
    route: Mutex<RouteView>,
    covered: Mutex<Vec<String>>,
}

struct Running {
    id: u64,
    tx: mpsc::UnboundedSender<Event>,
    shared: Arc<Shared>,
}

pub struct NavManager {
    running: Mutex<Option<Running>>,
    ended: Mutex<Option<String>>,
    next_id: std::sync::atomic::AtomicU64,
    pub platform: Arc<dyn Platform>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// "500 ft" under 0.1 mile, "0.3 mi" above.
pub fn format_distance(m: f64) -> String {
    let mi = m / crate::routing::METERS_PER_MILE;
    if mi < 0.1 {
        format!("{:.0} ft", ((m * 3.28084 / 50.0).round() * 50.0).max(50.0))
    } else if mi < 10.0 {
        format!("{mi:.1} mi")
    } else {
        format!("{mi:.0} mi")
    }
}

fn notification_for(s: &Snapshot) -> NotificationContent {
    let (icon, title, text) = match (s.state, &s.step) {
        (NavState::Arrived, _) => ("arrive".into(), "You have arrived".to_string(), s.destination.label.clone()),
        (NavState::OffRoute | NavState::Rerouting, _) => ("straight".into(), "Rerouting…".into(), s.destination.label.clone()),
        (NavState::Paused, _) => ("straight".into(), "Waiting for GPS signal".into(), "Guidance resumes when your position is found".into()),
        _ if s.acquiring => ("depart".into(), "Finding your location…".into(), s.destination.label.clone()),
        (_, Some(step)) => {
            let mut title = step.instruction.clone();
            if title.is_empty() {
                title = step.street.clone().unwrap_or_default();
            }
            (step.icon.to_string(), title, format_distance(step.distance_m))
        }
        (_, None) => ("straight".into(), "Navigating".into(), s.destination.label.clone()),
    };
    NotificationContent { icon, title, text, eta_ms: s.eta_ms }
}

fn route_view(session: &Session) -> RouteView {
    let r = session.route();
    RouteView {
        version: session.route_version(),
        mode: r.mode,
        shape: r.shape.iter().map(|&(a, b)| [a, b]).collect(),
        cameras: r.cameras.clone(),
        maneuvers: r.maneuvers.clone(),
    }
}

impl NavManager {
    pub fn new(platform: Arc<dyn Platform>) -> Self {
        NavManager { running: Mutex::new(None), ended: Mutex::new(None), next_id: std::sync::atomic::AtomicU64::new(1), platform }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Running>> {
        self.running.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn status(&self, app: &AppHandle) -> NavStatus {
        let session = self.lock().as_ref().map(|r| r.shared.view.lock().unwrap_or_else(|p| p.into_inner()).clone());
        let resume = if session.is_none() {
            let state = app.state::<AppState>();
            let conn = state.conn();
            db::get_json::<ActiveTrip>(&conn, ACTIVE_TRIP_KEY).ok().flatten()
        } else {
            None
        };
        let ended = self.ended.lock().unwrap_or_else(|p| p.into_inner()).take();
        NavStatus { session, resume, ended }
    }

    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub fn is_running(&self) -> bool {
        self.lock().is_some()
    }

    pub fn route(&self) -> Option<RouteView> {
        self.lock().as_ref().map(|r| r.shared.route.lock().unwrap_or_else(|p| p.into_inner()).clone())
    }

    /// Keys of the cameras the running session alerts for.
    pub fn covered(&self) -> Vec<String> {
        self.lock().as_ref().map(|r| r.shared.covered.lock().unwrap_or_else(|p| p.into_inner()).clone()).unwrap_or_default()
    }

    pub fn forget_resume(&self, app: &AppHandle) {
        let state = app.state::<AppState>();
        let _ = db::delete_setting(&state.conn(), ACTIVE_TRIP_KEY);
    }

    fn send(&self, e: Event) -> bool {
        self.lock().as_ref().is_some_and(|r| r.tx.send(e).is_ok())
    }

    /// End the running session (the user tapped End, or dismissed the arrival summary). The
    /// service stops and the saved trip is deleted here, before this returns, so a new session
    /// can start straight after.
    pub fn stop(&self, app: &AppHandle) {
        if let Some(r) = self.lock().take() {
            let _ = r.tx.send(Event::Stop);
            self.platform.stop(false);
            self.forget_resume(app);
        }
    }

    pub fn set_muted(&self, muted: bool) {
        self.send(Event::Mute(muted));
    }

    pub fn sim(&self, c: SimCommand) {
        self.send(Event::Sim(c));
    }

    pub fn start(
        self: &Arc<Self>,
        app: AppHandle,
        route: NavRoute,
        destination: Destination,
        simulate: Option<SimSource>,
    ) -> Result<SessionView, String> {
        // A new trip replaces a running one.
        self.stop(&app);
        *self.ended.lock().unwrap_or_else(|p| p.into_inner()) = None;
        let now = now_ms();
        let session = Session::new(route, destination.clone(), now);
        let view = SessionView { snapshot: session.snapshot(), simulated: simulate.is_some(), voice_unavailable: false, throttled: false };
        let shared = Arc::new(Shared {
            view: Mutex::new(view.clone()),
            route: Mutex::new(route_view(&session)),
            covered: Mutex::new(session.covered_cameras()),
        });
        let (tx, rx) = mpsc::unbounded_channel();

        let sink_tx = tx.clone();
        let sink: EventSink = Arc::new(move |e| {
            let _ = sink_tx.send(Event::Platform(e));
        });
        self.platform.start(sink, simulate.is_some(), &format!("Navigating to {}", destination.label))?;

        {
            let state = app.state::<AppState>();
            let trip = ActiveTrip { destination, mode: session.mode(), started_ms: now };
            let _ = db::set_json(&state.conn(), ACTIVE_TRIP_KEY, &trip);
        }

        let sim = simulate.map(|s| {
            Arc::new(Mutex::new(match s {
                SimSource::Drive(p) => Simulator::drive(session.route().shape.clone(), 0.0, p, now),
                SimSource::Replay(points, p) => Simulator::replay(points, p, now),
            }))
        });
        match &sim {
            Some(sim) => spawn_simulator(sim.clone(), tx.clone()),
            None => spawn_clock(tx.clone()),
        }
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *self.lock() = Some(Running { id, tx: tx.clone(), shared: shared.clone() });
        let me = self.clone();
        let task = tauri::async_runtime::spawn(drive(me, id, app.clone(), session, rx, tx, shared, sim));
        // If the session task dies (a panic), end the session properly rather than leave it
        // "running" with nothing listening: the service would keep going and the notification's
        // End would do nothing.
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = task.await {
                log::error!("navigation: the session failed: {e}");
                me.finish(id, &app, Some("Navigation stopped because of an error.".into()));
            }
        });
        Ok(view)
    }

    /// Session `id` ended on its own (End in the notification, location turned off). Nothing
    /// happens if it was already stopped or replaced.
    fn finish(&self, id: u64, app: &AppHandle, reason: Option<String>) {
        let mut running = self.lock();
        if !running.as_ref().is_some_and(|r| r.id == id) {
            return;
        }
        *running = None;
        drop(running);
        self.platform.stop(false);
        self.forget_resume(app);
        *self.ended.lock().unwrap_or_else(|p| p.into_inner()) = reason.clone();
        let _ = app.emit("nav:ended", serde_json::json!({ "reason": reason }));
    }
}

fn spawn_clock(tx: mpsc::UnboundedSender<Event>) {
    tauri::async_runtime::spawn(async move {
        let mut every = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            every.tick().await;
            if tx.send(Event::Tick(now_ms())).is_err() {
                break;
            }
        }
    });
}

fn spawn_simulator(sim: Arc<Mutex<Simulator>>, tx: mpsc::UnboundedSender<Event>) {
    tauri::async_runtime::spawn(async move {
        loop {
            let rate = sim.lock().map(|s| s.params.rate).unwrap_or(1.0).clamp(0.25, 20.0);
            tokio::time::sleep(std::time::Duration::from_millis((1000.0 / rate) as u64)).await;
            let (fix, now) = {
                let mut s = sim.lock().unwrap_or_else(|p| p.into_inner());
                (s.step(1000), s.now_ms())
            };
            if let Some(f) = fix {
                if tx.send(Event::Platform(PlatformEvent::Fix(f))).is_err() {
                    break;
                }
            }
            if tx.send(Event::Tick(now)).is_err() {
                break;
            }
        }
    });
}

fn spawn_reroute(app: AppHandle, req: super::session::RerouteRequest, tx: mpsc::UnboundedSender<Event>) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let (endpoint, overpass, subs, max_extra) = {
            let conn = state.conn();
            let settings = db::load_settings(&conn);
            let subs = crate::submissions::list(&conn);
            match (settings, subs) {
                (Ok(s), Ok(subs)) => (
                    crate::routing::normalize_endpoint(&s.routing_endpoint),
                    s.overpass_endpoint.clone(),
                    subs,
                    s.max_extra_secs_per_camera(),
                ),
                (Err(e), _) | (_, Err(e)) => {
                    let _ = tx.send(Event::Rerouted(req.id, Err(e.to_string())));
                    return;
                }
            }
        };
        let router = crate::routing::Valhalla { http: &state.http, endpoint: endpoint.clone() };
        // Cached road tiles only: no downloads while driving.
        let roads = crate::roadnet::OverpassRoads {
            http: &state.http,
            endpoint: overpass,
            db: &state.db,
            budget: Some(std::time::Duration::ZERO),
        };
        let cameras = |bbox: &BBox| {
            let osm = {
                let conn = state.conn();
                db::cameras_in_bbox(&conn, bbox)?
            };
            Ok(crate::routing::merge_cameras(osm, &subs, bbox))
        };
        let r = Rerouting {
            from: req.from,
            heading: req.heading,
            mode: req.mode,
            destination: req.destination,
            old: req.old,
            progress_m: req.progress_m,
            max_extra_secs_per_camera: max_extra,
        };
        let started = std::time::Instant::now();
        let result = reroute(&router, &roads, &endpoint, &r, cameras).await;
        log::info!("reroute {} ({:?}) took {:.1} s: {}", req.id, req.mode, started.elapsed().as_secs_f64(), if result.is_ok() { "ok" } else { "failed" });
        let _ = tx.send(Event::Rerouted(
            req.id,
            result.map_err(|e| match e {
                crate::error::AppError::Offline(_) => "no connection".to_string(),
                crate::error::AppError::RateLimited(_) => "the routing server is busy".to_string(),
                other => other.to_string(),
            }),
        ));
    });
}

#[allow(clippy::too_many_arguments)]
async fn drive(
    manager: Arc<NavManager>,
    id: u64,
    app: AppHandle,
    mut session: Session,
    mut rx: mpsc::UnboundedReceiver<Event>,
    tx: mpsc::UnboundedSender<Event>,
    shared: Arc<Shared>,
    sim: Option<Arc<Mutex<Simulator>>>,
) {
    let platform = manager.platform.clone();
    let mut last_note: Option<(String, String, String)> = None;
    let mut voice_unavailable = false;
    let mut throttled = false;
    let mut finished = false;
    let mut end_reason: Option<String> = None;
    let mut last_route_version = session.route_version();
    let _ = app.emit("nav:route", route_view(&session));

    while let Some(event) = rx.recv().await {
        let effects = match event {
            Event::Platform(PlatformEvent::Fix(f)) if !finished => session.on_fix(f),
            Event::Tick(now) if !finished => session.on_tick(now),
            Event::Rerouted(id, result) if !finished => session.on_reroute(id, result),
            Event::Mute(m) => {
                session.muted = m;
                Vec::new()
            }
            Event::Sim(c) => {
                if let Some(sim) = &sim {
                    let mut s = sim.lock().unwrap_or_else(|p| p.into_inner());
                    match c {
                        SimCommand::Deviate { meters } => s.deviate(meters),
                        SimCommand::LoseSignal { secs } => s.lose_signal(secs),
                        SimCommand::Speed { mps } => s.set_speed(mps),
                        SimCommand::Noise { meters } => s.set_noise(meters),
                        SimCommand::Rate { rate } => s.params.rate = rate,
                    }
                }
                Vec::new()
            }
            Event::Platform(PlatformEvent::VoiceUnavailable) => {
                voice_unavailable = true;
                Vec::new()
            }
            Event::Platform(PlatformEvent::Throttled { on }) => {
                throttled = on;
                Vec::new()
            }
            // Stopped from the app: already cleaned up.
            Event::Stop => {
                log::info!("navigation: ended from the app");
                return;
            }
            Event::Platform(PlatformEvent::End) => {
                log::info!("navigation: ended from the notification");
                break;
            }
            // (A simulated drive doesn't use the phone's location.)
            Event::Platform(PlatformEvent::LocationOff) if !finished && sim.is_none() => {
                log::info!("navigation: ended, location services turned off");
                end_reason = Some("Navigation ended because location services were turned off.".into());
                break;
            }
            Event::Platform(PlatformEvent::PermissionLost) if !finished && sim.is_none() => {
                log::info!("navigation: ended, location permission gone");
                end_reason = Some("Navigation ended because location permission was turned off.".into());
                break;
            }
            _ => Vec::new(),
        };
        for effect in effects {
            match effect {
                Effect::Speak(p) => {
                    if !session.muted {
                        platform.speak(&p.text, SpeechKind::Guidance);
                    }
                }
                Effect::CameraAlert(a) => {
                    if !session.muted {
                        platform.speak(&a.text, SpeechKind::Camera);
                    }
                }
                Effect::Reroute(req) => spawn_reroute(app.clone(), req, tx.clone()),
                Effect::RouteChanged => {
                    if let Some(sim) = &sim {
                        sim.lock().unwrap_or_else(|p| p.into_inner()).follow(session.route().shape.clone());
                    }
                }
                Effect::Arrived => {
                    finished = true;
                    platform.stop(true);
                    manager.forget_resume(&app);
                }
            }
        }
        if session.route_version() != last_route_version {
            last_route_version = session.route_version();
            let view = route_view(&session);
            *shared.route.lock().unwrap_or_else(|p| p.into_inner()) = view.clone();
            *shared.covered.lock().unwrap_or_else(|p| p.into_inner()) = session.covered_cameras();
            let _ = app.emit("nav:route", view);
        }
        let snapshot = session.snapshot();
        // (Once arrived the service is stopping: no more updates.)
        if !finished {
            let n = notification_for(&snapshot);
            let key = (n.icon.clone(), n.title.clone(), n.text.clone());
            if last_note.as_ref() != Some(&key) {
                platform.update(&n);
                last_note = Some(key);
            }
        }
        let view = SessionView { snapshot, simulated: sim.is_some(), voice_unavailable, throttled };
        *shared.view.lock().unwrap_or_else(|p| p.into_inner()) = view.clone();
        let _ = app.emit("nav:state", view);
    }

    // Ended from the notification, or because location went away.
    manager.finish(id, &app, end_reason);
}
