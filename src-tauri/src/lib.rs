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
mod nominatim;
mod osm;
pub mod overpass;
mod points;
mod scheduler;
mod snapshot;
mod state;
mod submissions;
mod sync;
pub mod wifi;

use state::AppState;
use tauri::Manager;
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
        .setup(|app| {
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Flock Finder");
}
