//! What navigation needs from the phone: location, the foreground service and its
//! notification, and speech. On Android this is the Kotlin side of the `navigation` plugin
//! (gen/android/app/src/main/java/org/flockfinder/app/nav/); on the desktop only the simulator
//! can drive a session, and nothing is spoken.

use super::session::Fix;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Something the phone reports during a session.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlatformEvent {
    Fix(Fix),
    /// "End navigation" in the notification.
    End,
    /// Location services were turned off.
    LocationOff,
    /// Location permission is gone.
    PermissionLost,
    /// No text-to-speech engine or US English voice: visual guidance only.
    VoiceUnavailable,
    /// Battery saver is limiting location while the screen is off (or no longer is).
    Throttled { on: bool },
}

pub type EventSink = Arc<dyn Fn(PlatformEvent) + Send + Sync>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationContent {
    /// Icon name (see `route::maneuver_icon`).
    pub icon: String,
    pub title: String,
    pub text: String,
    pub eta_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeechKind {
    Guidance,
    /// Preceded by the camera chime.
    Camera,
}

/// What stands between the user and starting navigation (checked when they tap Start).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Readiness {
    /// `android` or `desktop`.
    pub platform: String,
    /// Real location can drive a session here (Android); otherwise only the simulator.
    pub device_location: bool,
    pub precise: bool,
    pub approximate: bool,
    /// The user said "don't ask again": only Settings can grant it now.
    #[serde(default)]
    pub denied_permanently: bool,
    pub location_enabled: bool,
    pub notifications: bool,
    /// Google Play services' location (else the platform GPS provider is used).
    #[serde(default)]
    pub play_services: bool,
    /// Battery saver turns GPS off with the screen.
    #[serde(default)]
    pub power_save_gps_off: bool,
}

pub trait Platform: Send + Sync {
    /// Start the foreground service and (unless simulated) location updates to `events`.
    fn start(&self, events: EventSink, simulated: bool, title: &str) -> Result<(), String>;
    fn update(&self, n: &NotificationContent);
    fn speak(&self, text: &str, kind: SpeechKind);
    /// Stop location and the service (after anything being said, when `after_speech`).
    fn stop(&self, after_speech: bool);
    fn readiness(&self) -> Readiness;
}

/// Desktop: the simulator only, silent.
pub struct Desktop;

impl Platform for Desktop {
    fn start(&self, _events: EventSink, simulated: bool, _title: &str) -> Result<(), String> {
        if simulated {
            Ok(())
        } else {
            Err("Turn-by-turn navigation uses the phone's location and is available on Android. On the desktop, only the simulator can drive it.".into())
        }
    }

    fn update(&self, _n: &NotificationContent) {}

    fn speak(&self, text: &str, kind: SpeechKind) {
        log::debug!("nav voice ({kind:?}): {text}");
    }

    fn stop(&self, _after_speech: bool) {}

    fn readiness(&self) -> Readiness {
        Readiness { platform: "desktop".into(), location_enabled: true, notifications: true, ..Default::default() }
    }
}

#[cfg(target_os = "android")]
pub mod android {
    use super::*;
    use tauri::ipc::{Channel, InvokeResponseBody};
    use tauri::plugin::PluginHandle;
    use tauri::Wry;

    pub struct Android(pub PluginHandle<Wry>);

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct StartArgs {
        events: Channel<serde_json::Value>,
        simulated: bool,
        title: String,
    }

    impl Android {
        fn call(&self, command: &str, payload: impl Serialize) -> Result<serde_json::Value, String> {
            self.0.run_mobile_plugin::<serde_json::Value>(command, payload).map_err(|e| e.to_string())
        }

        pub fn call_readiness(&self, command: &str) -> Result<Readiness, String> {
            let v = self.call(command, serde_json::json!({}))?;
            serde_json::from_value(v).map_err(|e| e.to_string())
        }

        /// A request that waits on the user (a permission prompt, a settings dialog).
        pub async fn request(&self, command: &str) -> Result<Readiness, String> {
            let command = match command {
                "request_location" => "requestLocation",
                "enable_location" => "enableLocation",
                "request_notifications" => "requestNotifications",
                "open_app_settings" => "openAppSettings",
                other => other,
            };
            self.0
                .run_mobile_plugin_async::<Readiness>(command, serde_json::json!({}))
                .await
                .map_err(|e| e.to_string())
        }

        pub fn keep_screen_on(&self, on: bool) {
            if let Err(e) = self.call("keepScreenOn", serde_json::json!({ "on": on })) {
                log::warn!("navigation: keep-screen-on failed: {e}");
            }
        }
    }

    impl Platform for Android {
        fn start(&self, events: EventSink, simulated: bool, title: &str) -> Result<(), String> {
            let channel = Channel::new(move |body| {
                let parsed = match body {
                    InvokeResponseBody::Json(s) => serde_json::from_str::<PlatformEvent>(&s),
                    InvokeResponseBody::Raw(b) => serde_json::from_slice::<PlatformEvent>(&b),
                };
                match parsed {
                    Ok(e) => events(e),
                    Err(e) => log::warn!("navigation: unreadable event from Android: {e}"),
                }
                Ok(())
            });
            self.call("start", StartArgs { events: channel, simulated, title: title.into() }).map(|_| ())
        }

        fn update(&self, n: &NotificationContent) {
            if let Err(e) = self.call("update", n) {
                log::warn!("navigation: notification update failed: {e}");
            }
        }

        fn speak(&self, text: &str, kind: SpeechKind) {
            if let Err(e) = self.call("speak", serde_json::json!({ "text": text, "kind": kind })) {
                log::warn!("navigation: speech failed: {e}");
            }
        }

        fn stop(&self, after_speech: bool) {
            if let Err(e) = self.call("stop", serde_json::json!({ "afterSpeech": after_speech })) {
                log::warn!("navigation: stopping the service failed: {e}");
            }
        }

        fn readiness(&self) -> Readiness {
            self.call_readiness("readiness").unwrap_or_else(|e| {
                log::warn!("navigation: readiness check failed: {e}");
                Readiness { platform: "android".into(), device_location: true, ..Default::default() }
            })
        }
    }
}
