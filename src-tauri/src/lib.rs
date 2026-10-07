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
mod places;
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
    // Android builds rustls without a default crypto provider (see Cargo.toml). Our own client
    // brings its TLS config, but in dev builds Tauri proxies the dev server with a plain reqwest
    // client, which panics at startup unless a process default is installed.
    #[cfg(target_os = "android")]
    let _ = rustls::crypto::ring::default_provider().install_default();
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
            #[cfg(target_os = "android")]
            android_window::init(app.handle());
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
            commands::reverse_geocode,
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
            commands::list_saved_places,
            commands::save_saved_place,
            commands::delete_saved_place,
            commands::reorder_saved_places,
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
            // Android may destroy the activity while the process lives on: navigation's
            // foreground service keeps it alive with the app in the background, swiped from
            // recents, or under "Don't keep activities". Tauri would then exit (so keep the
            // process while a session runs), and the activity Android creates when the app comes
            // back gets no webview (tauri-apps/tauri#15671): a blank white screen. See
            // `android_window` for how the window is built again.
            #[cfg(target_os = "android")]
            match _event {
                tauri::RunEvent::WindowEvent { event: tauri::WindowEvent::Destroyed, .. } => {
                    android_window::lost();
                }
                tauri::RunEvent::ExitRequested { api, .. } => {
                    let navigating = _app
                        .try_state::<std::sync::Arc<nav::runtime::NavManager>>()
                        .is_some_and(|m| m.is_running());
                    if navigating {
                        api.prevent_exit();
                    }
                }
                _ => {}
            }
        });
}

/// Android: building the window again for an activity Android recreated (tauri#15671).
///
/// Tauri doesn't report the new activity (its Resumed event only goes to existing windows), and
/// the event loop can't tell it from the one being destroyed (which stays registered for a
/// moment, and a window built for it fails yet still takes the label). So the activity itself
/// says when it is resumed without a webview (`MainActivity.windowNeeded`), and the window is
/// built then, only if the old one is gone. The screen then picks up a running session.
#[cfg(target_os = "android")]
mod android_window {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    use tao::platform::android::prelude::{JNIEnv, JObject};
    use tauri::{AppHandle, Manager};

    static APP: OnceLock<AppHandle> = OnceLock::new();
    /// The window was destroyed with its activity.
    static LOST: AtomicBool = AtomicBool::new(false);

    pub fn init(app: &AppHandle) {
        let _ = APP.set(app.clone());
    }

    pub fn lost() {
        LOST.store(true, Ordering::Relaxed);
    }

    fn rebuild(app: &AppHandle) {
        if !LOST.load(Ordering::Relaxed) || !app.webview_windows().is_empty() {
            return;
        }
        if tao::platform::android::prelude::next_available_activity().is_none() {
            return;
        }
        LOST.store(false, Ordering::Relaxed);
        log::info!("the activity came back without a window (tauri#15671): building it again");
        let built = tauri::WebviewWindowBuilder::from_config(app, &app.config().app.windows[0]).and_then(|b| b.build());
        if let Err(e) = built {
            log::warn!("could not rebuild the window: {e}");
        }
    }

    /// `MainActivity.windowNeeded()`: resumed without a webview.
    #[no_mangle]
    pub extern "system" fn Java_org_flockfinder_app_MainActivity_windowNeeded(_env: JNIEnv, _activity: JObject) {
        let Some(app) = APP.get() else { return };
        if !LOST.load(Ordering::Relaxed) {
            return;
        }
        let app = app.clone();
        // tao's Android event loop can miss the wake-up for a task posted while the activity's
        // own lifecycle events arrive (it takes one per poll), leaving the task queued until
        // something else happens: post it again until the window is built (it runs once).
        std::thread::spawn(move || {
            for _ in 0..30 {
                let handle = app.clone();
                if let Err(e) = app.run_on_main_thread(move || rebuild(&handle)) {
                    log::warn!("could not schedule the window rebuild: {e}");
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
                if !LOST.load(Ordering::Relaxed) {
                    return;
                }
            }
        });
    }
}
