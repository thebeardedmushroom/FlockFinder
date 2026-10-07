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
    // `update`, `speak` and `stop` are called during the session, also when the app's screen
    // (on Android, its activity) is gone: they must not need it.
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
    use std::sync::OnceLock;
    use tao::platform::android::prelude::jni::{
        self,
        objects::{GlobalRef, JClass, JObject, JValue},
        JNIEnv, JavaVM,
    };
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

    /// `NavigationService`'s `bridge*` functions, called over JNI.
    ///
    /// Tauri plugin calls are carried out by the activity, and with no activity (Android
    /// destroyed it while the app was in the background) they panic ("no available activity").
    /// The session keeps updating the notification, speaking and stopping then, so it reaches
    /// the service directly instead.
    struct Bridge {
        vm: JavaVM,
        service: GlobalRef,
    }

    static BRIDGE: OnceLock<Bridge> = OnceLock::new();

    const SERVICE_CLASS: &str = "org.flockfinder.app.nav.NavigationService";

    /// A failed call leaves a Java exception pending: log it and clear it.
    fn jni_error(env: &mut JNIEnv, e: jni::errors::Error) -> String {
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
        e.to_string()
    }

    /// Looked up once, through an activity's class loader (a native thread's own loader can't
    /// see the app's classes); the class stays valid for the life of the process.
    fn bridge() -> Result<&'static Bridge, String> {
        if let Some(b) = BRIDGE.get() {
            return Ok(b);
        }
        let ctx = tao::platform::android::prelude::main_android_context()
            .ok_or("no activity to find the navigation service through")?;
        let vm = unsafe { JavaVM::from_raw(ctx.java_vm.cast()) }.map_err(|e| e.to_string())?;
        let service = {
            let mut env = vm.attach_current_thread_as_daemon().map_err(|e| e.to_string())?;
            let found = env.with_local_frame(8, |env| -> jni::errors::Result<GlobalRef> {
                let activity = unsafe { JObject::from_raw(ctx.context_jobject.cast()) };
                let loader = env.call_method(&activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?.l()?;
                let name = env.new_string(SERVICE_CLASS)?;
                let class = env
                    .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])?
                    .l()?;
                env.new_global_ref(class)
            });
            found.map_err(|e| jni_error(&mut env, e))?
        };
        Ok(BRIDGE.get_or_init(|| Bridge { vm, service }))
    }

    impl Bridge {
        /// Calls the static `void` function `name`, from any thread, with `strings` as its
        /// first arguments and `rest` after them.
        fn call(&self, name: &str, sig: &str, strings: &[&str], rest: &[JValue]) -> Result<(), String> {
            let mut env = self.vm.attach_current_thread_as_daemon().map_err(|e| e.to_string())?;
            // (This thread may never return to Java, so its local references are freed here.)
            let result = env.with_local_frame(8, |env| -> jni::errors::Result<()> {
                let strings = strings.iter().map(|s| env.new_string(s).map(JObject::from)).collect::<Result<Vec<_>, _>>()?;
                let mut args: Vec<JValue> = strings.iter().map(JValue::Object).collect();
                args.extend_from_slice(rest);
                let class: &JClass = self.service.as_obj().into();
                env.call_static_method(class, name, sig, &args)?;
                Ok(())
            });
            result.map_err(|e| jni_error(&mut env, e))
        }
    }

    fn service_call(name: &str, sig: &str, strings: &[&str], rest: &[JValue]) {
        if let Err(e) = bridge().and_then(|b| b.call(name, sig, strings, rest)) {
            log::warn!("navigation: NavigationService.{name} failed: {e}");
        }
    }

    impl Android {
        /// A plugin call (made from the screen, so normally with an activity). The panic Tauri
        /// raises when there is none becomes an error.
        fn call(&self, command: &str, payload: impl Serialize) -> Result<serde_json::Value, String> {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.0.run_mobile_plugin::<serde_json::Value>(command, payload).map_err(|e| e.to_string())
            }))
            .unwrap_or_else(|_| Err(format!("{command}: the app's activity is gone")))
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
            // Find the service now, while the screen (an activity) is certainly there.
            bridge()?;
            self.call("start", StartArgs { events: channel, simulated, title: title.into() }).map(|_| ())
        }

        fn update(&self, n: &NotificationContent) {
            service_call(
                "bridgeUpdate",
                "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;J)V",
                &[&n.icon, &n.title, &n.text],
                &[JValue::Long(n.eta_ms.unwrap_or(-1))],
            );
        }

        fn speak(&self, text: &str, kind: SpeechKind) {
            service_call("bridgeSpeak", "(Ljava/lang/String;Z)V", &[text], &[JValue::Bool(u8::from(kind == SpeechKind::Camera))]);
        }

        fn stop(&self, after_speech: bool) {
            service_call("bridgeStop", "(Z)V", &[], &[JValue::Bool(u8::from(after_speech))]);
        }

        fn readiness(&self) -> Readiness {
            self.call_readiness("readiness").unwrap_or_else(|e| {
                log::warn!("navigation: readiness check failed: {e}");
                Readiness { platform: "android".into(), device_location: true, ..Default::default() }
            })
        }
    }
}
