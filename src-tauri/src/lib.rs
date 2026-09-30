//! Flock Finder — Tauri backend entry point.

mod alerts;
mod commands;
pub mod db;
pub mod error;
mod fetcher;
pub mod geo_util;
mod gpx;
pub mod grid;
pub mod http;
mod nav;
mod nominatim;
mod osm;
pub mod overpass;
mod points;
mod roadnet;
mod routing;
mod scheduler;
mod snapshot;
mod state;
mod submissions;
mod sync;
pub mod wifi;

use state::AppState;
use tauri::Manager;

/// The OS light/dark setting at launch (desktop only), read before the window is forced dark.
/// The map theme defaults to it; see `AppInfo::system_theme`.
pub static SYSTEM_THEME: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
use tauri_plugin_deep_link::DeepLinkExt;

#[cfg(desktop)]
fn focus_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// WebView2 answers a page's location request with its own "allow?" bubble, which renders as
/// an empty grey box in a Tauri window, so the request silently times out. The page only asks
/// after the user presses the locate button, so grant it here; Windows' location privacy
/// setting still decides whether a position is available. Other permission kinds keep
/// WebView2's default handling.
#[cfg(windows)]
fn allow_geolocation_requests(
    controller: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller,
) -> Result<(), String> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION,
        COREWEBVIEW2_PERMISSION_STATE_ALLOW,
    };
    use webview2_com::PermissionRequestedEventHandler;

    let mut token: i64 = 0;
    unsafe {
        let webview = controller.CoreWebView2().map_err(|e| e.to_string())?;
        webview
            .add_PermissionRequested(
                &PermissionRequestedEventHandler::create(Box::new(|_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                    args.PermissionKind(&mut kind)?;
                    if kind == COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION {
                        args.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW)?;
                    }
                    Ok(())
                })),
                &mut token,
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // Desktop only, and must be registered first: a second launch (e.g. from a deep link)
    // forwards its arguments to this instance instead of opening another window. Android
    // already keeps a single instance and delivers deep links to it.
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        focus_main(app);
    }));
    builder
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .level_for("sqlx", log::LevelFilter::Warn)
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        // Reads/writes files the user picks; on Android those are content:// URIs.
        .plugin(tauri_plugin_fs::init())
        .plugin(nav::init())
        .setup(|app| {
            // The window follows the OS theme until here. Note which one that is, then keep the
            // title bar dark to match the app chrome. (A theme forced in tauri.conf.json would
            // also force the webview's prefers-color-scheme, hiding the OS setting from the page.)
            #[cfg(desktop)]
            if let Some(window) = app.get_webview_window("main") {
                if let Ok(theme) = window.theme() {
                    let _ = SYSTEM_THEME.set(if matches!(theme, tauri::Theme::Light) { "light" } else { "dark" });
                }
                if let Err(e) = window.set_theme(Some(tauri::Theme::Dark)) {
                    log::warn!("could not set the window theme: {e}");
                }
            }

            let data_dir = app.path().app_data_dir()?;
            let db_path = data_dir.join("flockfinder.sqlite");
            let mut conn = db::open(&db_path)?;
            db::migrate(&mut conn)?;
            db::purge_stale(&conn, db::now())?;
            db::sync_runs_mark_interrupted(&conn)?;
            log::info!("database ready at {}", db_path.display());

            let http = http::HttpClient::new()?;
            app.manage(AppState::new(conn, http));

            #[cfg(windows)]
            if let Some(window) = app.get_webview_window("main") {
                window.with_webview(|webview| {
                    if let Err(e) = allow_geolocation_requests(&webview.controller()) {
                        log::warn!("could not install the WebView2 location permission handler: {e}");
                    }
                })?;
            }

            // Register the flockfinder:// scheme at runtime for dev builds on
            // Linux/Windows (bundled installers register it themselves).
            #[cfg(any(windows, target_os = "linux"))]
            {
                if let Err(e) = app.deep_link().register_all() {
                    log::warn!("could not register deep link scheme: {e}");
                }
            }
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    let h = handle.clone();
                    tauri::async_runtime::spawn(async move {
                        commands::handle_deep_link(h, url).await;
                    });
                }
            });

            scheduler::start(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_app_info,
            commands::get_settings,
            commands::save_settings,
            commands::mark_first_run_done,
            commands::get_initial_view,
            commands::save_view,
            commands::get_camera_points,
            commands::get_sync_status,
            commands::sync_now,
            commands::get_cached_cameras,
            commands::cameras_near,
            commands::get_cache_stats,
            commands::clear_cache,
            commands::load_fixture,
            commands::geocode,
            commands::plan_route,
            commands::check_location,
            commands::list_submissions,
            commands::create_submission,
            commands::update_submission,
            commands::delete_submission,
            commands::check_submission_proximity,
            commands::export_josm,
            commands::create_watch_area,
            commands::list_watch_areas,
            commands::rename_watch_area,
            commands::delete_watch_area,
            commands::create_route,
            commands::list_routes,
            commands::delete_route,
            commands::import_gpx,
            commands::get_alert_state,
            commands::get_alert_history,
            commands::get_target_cameras,
            commands::get_cameras_by_keys,
            commands::acknowledge_alerts,
            commands::run_alert_refresh,
            commands::get_route_report,
            commands::export_route_report,
            commands::osm_auth_status,
            commands::osm_sign_in,
            commands::osm_complete_auth,
            commands::osm_sign_out,
            commands::preview_submission_tags,
            commands::osm_upload_submission,
            commands::open_external,
            commands::notify,
            commands::frontend_log,
            commands::wifi_dataset_status,
            commands::wifi_download_dataset,
            commands::wifi_import_wigle,
            commands::wifi_clear,
            commands::get_wifi_sightings,
            commands::wifi_oui_list,
            nav::commands::nav_start,
            nav::commands::nav_stop,
            nav::commands::nav_status,
            nav::commands::nav_route,
            nav::commands::nav_covered_cameras,
            nav::commands::nav_set_muted,
            nav::commands::nav_sim,
            nav::commands::nav_forget_resume,
            nav::commands::nav_readiness,
            nav::commands::nav_request,
            nav::commands::nav_keep_screen_on,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Flock Finder")
        .run(|_app, _event| {
            // Android: navigation's foreground service keeps the process alive after the app is
            // swiped away from recents. Tauri would exit when the activity goes (so keep the
            // process while a session runs), and a relaunched activity gets no webview
            // (tauri-apps/tauri#15671), so build the window again when the app comes back.
            #[cfg(target_os = "android")]
            match _event {
                tauri::RunEvent::ExitRequested { api, .. } => {
                    let navigating = _app
                        .try_state::<std::sync::Arc<nav::runtime::NavManager>>()
                        .is_some_and(|m| m.is_running());
                    if navigating {
                        api.prevent_exit();
                    }
                }
                tauri::RunEvent::Resumed => {
                    if _app.webview_windows().is_empty() {
                        log::info!("no window after resuming (tauri#15671): building it again");
                        if let Err(e) = tauri::WebviewWindowBuilder::new(_app, "main", tauri::WebviewUrl::default()).build() {
                            log::warn!("could not rebuild the window: {e}");
                        }
                    }
                }
                _ => {}
            }
        });
}
