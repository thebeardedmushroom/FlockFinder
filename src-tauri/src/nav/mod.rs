//! Turn-by-turn navigation (Android): live guidance along a planned route, with voice prompts,
//! camera alerts, and rerouting that keeps avoiding cameras.
//!
//! - `route`: the route prepared for guidance.
//! - `snap`: matching positions to it.
//! - `guidance`: when to say what.
//! - `cameras`: camera alerts along it.
//! - `session`: the state machine that ties these together (no I/O; tested with GPX drives).
//! - `reroute`: new routes from where the car is, in the same mode.
//! - `sim`: the simulated location provider.
//! - `platform`: the phone's side (location, service, notification, speech).
//! - `runtime`: runs a session in the app.
//! - `commands`: what the screen calls.

pub mod cameras;
pub mod commands;
pub mod guidance;
pub mod platform;
pub mod reroute;
pub mod route;
pub mod runtime;
pub mod session;
pub mod sim;
pub mod snap;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Manager, Wry};

/// The `navigation` plugin: registers the Android side and manages the [`runtime::NavManager`].
pub fn init() -> TauriPlugin<Wry> {
    Builder::new("navigation")
        .setup(|app, _api| {
            #[cfg(target_os = "android")]
            let platform: Arc<dyn platform::Platform> = {
                let handle = _api.register_android_plugin("org.flockfinder.app.nav", "NavigationPlugin")?;
                let android = Arc::new(platform::android::Android(handle));
                app.manage(android.clone());
                android
            };
            #[cfg(not(target_os = "android"))]
            let platform: Arc<dyn platform::Platform> = Arc::new(platform::Desktop);
            app.manage(Arc::new(runtime::NavManager::new(platform)));
            Ok(())
        })
        .build()
}
