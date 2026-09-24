//! Background work: the worldwide camera sync whenever it is due, and the alert refresh
//! once shortly after launch, then whenever its configured interval has elapsed. In
//! "manual only" mode the alert loop never touches the network.

use crate::alerts;
use crate::db;
use crate::state::AppState;
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// Delay before the launch refresh so the window is up first.
const LAUNCH_DELAY: Duration = Duration::from_secs(8);
/// Delay before the first sync check; short, because a fresh install has an empty map.
const SYNC_LAUNCH_DELAY: Duration = Duration::from_secs(3);
/// How often the loops re-read settings and check whether work is due.
const TICK: Duration = Duration::from_secs(60);
/// A launch refresh is skipped if the previous refresh was this recent.
const LAUNCH_MIN_AGE_SECS: i64 = 3600;

pub fn start(app: AppHandle) {
    let sync_app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(SYNC_LAUNCH_DELAY).await;
        loop {
            crate::sync::maybe_run(&sync_app).await;
            tokio::time::sleep(TICK).await;
        }
    });
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(LAUNCH_DELAY).await;
        maybe_run(&app, true).await;
        loop {
            tokio::time::sleep(TICK).await;
            maybe_run(&app, false).await;
        }
    });
}

async fn maybe_run(app: &AppHandle, is_launch: bool) {
    let state = app.state::<AppState>();
    let (interval_hours, last, has_targets) = {
        let conn = state.conn();
        let settings = match db::load_settings(&conn) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("scheduler could not read settings: {e}");
                return;
            }
        };
        let last: Option<i64> = db::get_json(&conn, "last_alert_refresh").ok().flatten();
        let has_targets = alerts::list_areas(&conn).map(|a| !a.is_empty()).unwrap_or(false)
            || alerts::list_routes(&conn).map(|r| !r.is_empty()).unwrap_or(false);
        (settings.refresh_interval_hours, last, has_targets)
    };

    if interval_hours == 0 {
        // Manual only: zero background network requests.
        return;
    }
    if !has_targets {
        return;
    }
    let now = db::now();
    let min_age = if is_launch {
        LAUNCH_MIN_AGE_SECS.min(interval_hours as i64 * 3600)
    } else {
        interval_hours as i64 * 3600
    };
    let due = last.map_or(true, |t| now - t >= min_age);
    if !due {
        return;
    }
    log::info!("scheduled alert refresh starting (launch={is_launch})");
    match alerts::run_refresh(app, true).await {
        Ok(outcome) if outcome.offline => log::info!("scheduled refresh skipped: offline"),
        Ok(outcome) => log::info!(
            "scheduled refresh done: {} requests, {} targets",
            outcome.requests,
            outcome.targets.len()
        ),
        Err(e) => log::warn!("scheduled refresh failed: {e}"),
    }
}
