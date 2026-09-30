//! Navigation commands for the screen.

use super::platform::Readiness;
use super::route::{Mode, NavRoute};
use super::runtime::{NavManager, NavStatus, RouteView, SessionView, SimCommand, SimSource};
use super::session::Destination;
use super::sim::SimParams;
use crate::error::{AppError, AppResult};
use crate::routing::PlannedRoute;
use serde::Deserialize;
use std::sync::Arc;
use tauri::{AppHandle, State};

#[derive(Debug, Deserialize)]
pub struct StartArgs {
    pub mode: Mode,
    /// The plan's route for that mode (from `plan_route`).
    pub route: PlannedRoute,
    pub destination: Destination,
    /// Drive with the simulator instead of the phone's location (debug builds).
    pub simulate: Option<SimParams>,
    /// With `simulate`: replay a GPX track (picked now) instead of driving the route.
    #[serde(default)]
    pub replay_gpx: bool,
}

/// Start guidance along a planned route, from wherever the phone is (a start far from the
/// route's own start reroutes at once). `None` when a GPX pick was cancelled.
#[tauri::command]
pub async fn nav_start(app: AppHandle, manager: State<'_, Arc<NavManager>>, args: StartArgs) -> AppResult<Option<SessionView>> {
    if args.simulate.is_some() && !cfg!(debug_assertions) {
        return Err(AppError::Invalid("The simulator is only available in debug builds.".into()));
    }
    let route = NavRoute::new(args.mode, &args.route).ok_or_else(|| AppError::Invalid("The route is empty.".into()))?;
    let source = match args.simulate {
        None => None,
        Some(p) if !args.replay_gpx => Some(SimSource::Drive(p)),
        Some(p) => {
            use tauri_plugin_dialog::DialogExt;
            let builder = app.dialog().file().set_title("Replay a GPX track").add_filter("GPX", &["gpx", "xml"]);
            let picked = tokio::task::spawn_blocking(move || builder.blocking_pick_file())
                .await
                .map_err(|e| AppError::Other(format!("dialog task failed: {e}")))?;
            let Some(fp) = picked else { return Ok(None) };
            let xml = String::from_utf8_lossy(&crate::commands::read_picked(&app, &fp)?).into_owned();
            Some(SimSource::Replay(crate::gpx::parse_points(&xml)?, p))
        }
    };
    manager.inner().start(app, route, args.destination, source).map(Some).map_err(AppError::Invalid)
}

#[tauri::command]
pub fn nav_stop(app: AppHandle, manager: State<'_, Arc<NavManager>>) {
    manager.stop(&app);
}

/// The running session (if any), a trip to offer resuming, and why the last one ended.
#[tauri::command]
pub fn nav_status(app: AppHandle, manager: State<'_, Arc<NavManager>>) -> NavStatus {
    manager.status(&app)
}

#[tauri::command]
pub fn nav_route(manager: State<'_, Arc<NavManager>>) -> Option<RouteView> {
    manager.route()
}

/// Cameras the session alerts for; the map's proximity alerts skip these.
#[tauri::command]
pub fn nav_covered_cameras(manager: State<'_, Arc<NavManager>>) -> Vec<String> {
    manager.covered()
}

#[tauri::command]
pub fn nav_set_muted(manager: State<'_, Arc<NavManager>>, muted: bool) {
    manager.set_muted(muted);
}

#[tauri::command]
pub fn nav_sim(manager: State<'_, Arc<NavManager>>, command: SimCommand) {
    manager.sim(command);
}

/// Decline the offer to resume a trip.
#[tauri::command]
pub fn nav_forget_resume(app: AppHandle, manager: State<'_, Arc<NavManager>>) {
    manager.forget_resume(&app);
}

/// Permissions and settings navigation needs, as they stand.
#[tauri::command]
pub async fn nav_readiness(manager: State<'_, Arc<NavManager>>) -> AppResult<Readiness> {
    Ok(manager.platform.readiness())
}

/// Ask for something navigation needs: `request_location` (the permission prompt),
/// `enable_location` (the system dialog that turns location on), `request_notifications`,
/// `open_app_settings`. Returns the readiness afterwards.
#[tauri::command]
pub async fn nav_request(_app: AppHandle, manager: State<'_, Arc<NavManager>>, what: String) -> AppResult<Readiness> {
    if !["request_location", "enable_location", "request_notifications", "open_app_settings"].contains(&what.as_str()) {
        return Err(AppError::Invalid(format!("unknown request {what}")));
    }
    #[cfg(target_os = "android")]
    {
        use tauri::Manager;
        let android = _app.state::<Arc<super::platform::android::Android>>();
        return android.request(&what).await.map_err(AppError::Other);
    }
    #[cfg(not(target_os = "android"))]
    Ok(manager.platform.readiness())
}

/// Keep the screen on while the navigation screen shows (Android).
#[tauri::command]
pub fn nav_keep_screen_on(_app: AppHandle, on: bool) {
    #[cfg(target_os = "android")]
    {
        use tauri::Manager;
        _app.state::<Arc<super::platform::android::Android>>().keep_screen_on(on);
    }
    let _ = on;
}
