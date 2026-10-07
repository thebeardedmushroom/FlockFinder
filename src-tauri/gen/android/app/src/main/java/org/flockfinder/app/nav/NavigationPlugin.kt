package org.flockfinder.app.nav

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.location.LocationManager
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import android.view.WindowManager
import androidx.activity.result.ActivityResult
import androidx.activity.result.IntentSenderRequest
import androidx.core.app.ActivityCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.core.location.LocationManagerCompat
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import com.google.android.gms.common.api.ResolvableApiException
import com.google.android.gms.location.LocationServices
import com.google.android.gms.location.LocationSettingsRequest
import org.flockfinder.app.MainActivity

@InvokeArg
class StartArgs {
  lateinit var events: Channel
  var simulated: Boolean = false
  var title: String = ""
}

@InvokeArg
class ScreenArgs {
  var on: Boolean = false
}

/**
 * The Android side of turn-by-turn navigation (Rust: src-tauri/src/nav/platform.rs). The
 * session itself runs in Rust; this starts and stops [NavigationService] (location, speech,
 * the notification) and answers permission and settings questions.
 */
@TauriPlugin(
  permissions = [
    Permission(strings = [Manifest.permission.ACCESS_FINE_LOCATION, Manifest.permission.ACCESS_COARSE_LOCATION], alias = "location"),
    Permission(strings = [Manifest.permission.POST_NOTIFICATIONS], alias = "notifications"),
  ]
)
class NavigationPlugin(private val activity: Activity) : Plugin(activity) {

  private fun granted(permission: String) =
    ContextCompat.checkSelfPermission(activity, permission) == PackageManager.PERMISSION_GRANTED

  /** What navigation needs, as it stands (field names match the Rust `Readiness`). */
  private fun readinessObject(): JSObject {
    val lm = activity.getSystemService(Context.LOCATION_SERVICE) as LocationManager
    val precise = granted(Manifest.permission.ACCESS_FINE_LOCATION)
    val approximate = granted(Manifest.permission.ACCESS_COARSE_LOCATION)
    // Asked before and refused with "don't ask again": the system won't show the prompt.
    val deniedPermanently = !precise && asked() &&
      !ActivityCompat.shouldShowRequestPermissionRationale(activity, Manifest.permission.ACCESS_FINE_LOCATION)
    val notifications = NotificationManagerCompat.from(activity).areNotificationsEnabled()
    val o = JSObject()
    o.put("platform", "android")
    o.put("device_location", true)
    o.put("precise", precise)
    o.put("approximate", approximate)
    o.put("denied_permanently", deniedPermanently)
    o.put("location_enabled", LocationManagerCompat.isLocationEnabled(lm))
    o.put("notifications", notifications)
    o.put("play_services", LocationSource.hasPlayServices(activity))
    o.put("power_save_gps_off", powerSaveLimitsGps(activity))
    return o
  }

  private fun prefs() = activity.getSharedPreferences("navigation", Context.MODE_PRIVATE)
  private fun asked() = prefs().getBoolean("asked_location", false)

  @Command
  fun readiness(invoke: Invoke) {
    invoke.resolve(readinessObject())
  }

  @Command
  fun requestLocation(invoke: Invoke) {
    if (granted(Manifest.permission.ACCESS_FINE_LOCATION)) {
      invoke.resolve(readinessObject())
      return
    }
    prefs().edit().putBoolean("asked_location", true).apply()
    requestPermissionForAlias("location", invoke, "permissionAnswered")
  }

  @Command
  fun requestNotifications(invoke: Invoke) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU || granted(Manifest.permission.POST_NOTIFICATIONS)) {
      invoke.resolve(readinessObject())
      return
    }
    requestPermissionForAlias("notifications", invoke, "permissionAnswered")
  }

  @PermissionCallback
  private fun permissionAnswered(invoke: Invoke) {
    invoke.resolve(readinessObject())
  }

  /** The system dialog that turns location on (Play services), else the Location settings page. */
  @Command
  fun enableLocation(invoke: Invoke) {
    if (!LocationSource.hasPlayServices(activity)) {
      startActivityForResult(invoke, Intent(Settings.ACTION_LOCATION_SOURCE_SETTINGS), "settingsClosed")
      return
    }
    val request = LocationSettingsRequest.Builder().addLocationRequest(LocationSource.request()).setAlwaysShow(true).build()
    LocationServices.getSettingsClient(activity).checkLocationSettings(request)
      .addOnSuccessListener { invoke.resolve(readinessObject()) }
      .addOnFailureListener { e ->
        if (e is ResolvableApiException) {
          try {
            startIntentSenderForResult(invoke, IntentSenderRequest.Builder(e.resolution).build(), "settingsClosed")
          } catch (t: Throwable) {
            startActivityForResult(invoke, Intent(Settings.ACTION_LOCATION_SOURCE_SETTINGS), "settingsClosed")
          }
        } else {
          startActivityForResult(invoke, Intent(Settings.ACTION_LOCATION_SOURCE_SETTINGS), "settingsClosed")
        }
      }
  }

  @Command
  fun openAppSettings(invoke: Invoke) {
    val intent = Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", activity.packageName, null))
    startActivityForResult(invoke, intent, "settingsClosed")
  }

  @ActivityCallback
  private fun settingsClosed(invoke: Invoke, @Suppress("UNUSED_PARAMETER") result: ActivityResult) {
    invoke.resolve(readinessObject())
  }

  @Command
  fun start(invoke: Invoke) {
    val args = invoke.parseArgs(StartArgs::class.java)
    if (!granted(Manifest.permission.ACCESS_FINE_LOCATION) && !granted(Manifest.permission.ACCESS_COARSE_LOCATION)) {
      invoke.reject("Location permission is needed to navigate.")
      return
    }
    NavigationService.start(activity, args.events, args.simulated, args.title)
    invoke.resolve()
  }

  // The notification, speech and stopping are called from Rust straight on NavigationService
  // (its bridge* functions): they must work while there is no activity.

  @Command
  fun keepScreenOn(invoke: Invoke) {
    val args = invoke.parseArgs(ScreenArgs::class.java)
    // The activity showing the screen now (this plugin's may have been destroyed since).
    val target = MainActivity.current() ?: activity
    target.runOnUiThread {
      if (args.on) {
        target.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
      } else {
        target.window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
      }
    }
    invoke.resolve()
  }

  companion object {
    /** Battery saver set to turn GPS off (or throttle it) while the screen is off. */
    fun powerSaveLimitsGps(context: Context): Boolean {
      val pm = context.getSystemService(Context.POWER_SERVICE) as PowerManager
      if (!pm.isPowerSaveMode) return false
      // Before Android 9 there is no finer setting: battery saver may stop GPS with the screen.
      if (Build.VERSION.SDK_INT < Build.VERSION_CODES.P) return true
      return when (pm.locationPowerSaveMode) {
        PowerManager.LOCATION_MODE_GPS_DISABLED_WHEN_SCREEN_OFF,
        PowerManager.LOCATION_MODE_ALL_DISABLED_WHEN_SCREEN_OFF,
        PowerManager.LOCATION_MODE_THROTTLE_REQUESTS_WHEN_SCREEN_OFF -> true
        else -> false
      }
    }
  }
}
