package org.flockfinder.app.nav

import android.Manifest
import android.annotation.SuppressLint
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.location.Location
import android.location.LocationManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.text.format.DateFormat
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import androidx.core.location.LocationManagerCompat
import app.tauri.plugin.Channel
import app.tauri.plugin.JSObject
import org.flockfinder.app.MainActivity
import org.flockfinder.app.R
import java.util.Date

/**
 * Keeps navigation running with the screen off or the app in the background: a foreground
 * service (type "location") that delivers fixes to the Rust session, speaks its prompts, and
 * shows the current maneuver in an ongoing notification (tap: back to the app; "End
 * navigation": stop). It owns no guidance logic of its own.
 *
 * The Activity (and its WebView) may be destroyed while this runs, so the session's calls
 * that keep guidance going (the notification, speech, stopping) come straight from Rust to
 * the `bridge*` functions below, not through the Tauri plugin, which needs an Activity.
 *
 * If the system kills the process the service is not restarted (START_NOT_STICKY): the app
 * offers to resume the trip when next opened instead of silently carrying on.
 */
class NavigationService : Service() {
  private val main = Handler(Looper.getMainLooper())
  private var location: LocationSource? = null
  private var voice: VoiceGuide? = null
  private var wakeLock: PowerManager.WakeLock? = null
  private var receiver: BroadcastReceiver? = null
  private var title = "Navigating"
  private var stopping = false
  /** A session is running (started and not yet stopping). */
  private var active = false
  /** Counts sessions, so a stop that finishes late never ends a newer one. */
  private var generation = 0

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onCreate() {
    super.onCreate()
    instance = this
    createChannel()
  }

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    if (intent?.action == ACTION_END) {
      android.util.Log.i("FlockFinderNav", "End navigation tapped in the notification (active: $active)")
      if (active) {
        // Tell the session (it cleans up its state and tells the screen), and stop here at
        // once as well, so ending never depends on the rest of the app answering.
        send(JSObject().put("type", "end"))
        sink = null
        finish(false)
      } else if (!stopping) {
        // A late tap (the notification was already going) started this service: nothing to end.
        stopSelf(startId)
      }
      return START_NOT_STICKY
    }
    stopping = false
    active = true
    generation++
    val simulated = intent?.getBooleanExtra(EXTRA_SIMULATED, false) ?: false
    title = intent?.getStringExtra(EXTRA_TITLE) ?: "Navigating"
    val type = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) ServiceInfo.FOREGROUND_SERVICE_TYPE_LOCATION else 0
    ServiceCompat.startForeground(this, NOTIFICATION_ID, build("depart", title, "Finding your location…", null), type)

    if (voice == null) {
      voice = VoiceGuide(this) { send(JSObject().put("type", "voice_unavailable")) }
    }
    if (wakeLock == null) {
      val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
      wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "flockfinder:navigation").apply {
        setReferenceCounted(false)
        acquire(12 * 60 * 60 * 1000L)
      }
    }
    watchSystem()
    // A new session: its own location feed (none when simulated).
    location?.stop()
    location = null
    if (!simulated) {
      location = LocationSource.create(this).also { it.start(::onLocation) }
    }
    send(JSObject().put("type", "throttled").put("on", NavigationPlugin.powerSaveLimitsGps(this)))
    return START_NOT_STICKY
  }

  private fun onLocation(l: Location) {
    if (ContextCompat.checkSelfPermission(this, Manifest.permission.ACCESS_COARSE_LOCATION) != PackageManager.PERMISSION_GRANTED) {
      send(JSObject().put("type", "permission_lost"))
      return
    }
    val fix = JSObject()
    fix.put("type", "fix")
    fix.put("lat", l.latitude)
    fix.put("lon", l.longitude)
    fix.put("accuracy_m", if (l.hasAccuracy()) l.accuracy.toDouble() else 100.0)
    if (l.hasSpeed()) fix.put("speed_mps", l.speed.toDouble())
    if (l.hasBearing() && (!l.hasSpeed() || l.speed > 1f)) fix.put("course_deg", l.bearing.toDouble())
    // The time it arrived, on the same clock as the session's ticks.
    fix.put("time_ms", System.currentTimeMillis())
    send(fix)
  }

  /** Location services turned off, battery saver turned on or off. */
  private fun watchSystem() {
    if (receiver != null) return
    val r = object : BroadcastReceiver() {
      override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
          LocationManager.PROVIDERS_CHANGED_ACTION -> {
            val lm = getSystemService(Context.LOCATION_SERVICE) as LocationManager
            if (!LocationManagerCompat.isLocationEnabled(lm) && location != null) {
              android.util.Log.i("FlockFinderNav", "location services turned off")
              send(JSObject().put("type", "location_off"))
            }
          }
          PowerManager.ACTION_POWER_SAVE_MODE_CHANGED ->
            send(JSObject().put("type", "throttled").put("on", NavigationPlugin.powerSaveLimitsGps(context)))
        }
      }
    }
    val filter = IntentFilter().apply {
      addAction(LocationManager.PROVIDERS_CHANGED_ACTION)
      addAction(PowerManager.ACTION_POWER_SAVE_MODE_CHANGED)
    }
    ContextCompat.registerReceiver(this, r, filter, ContextCompat.RECEIVER_NOT_EXPORTED)
    receiver = r
  }

  private fun createChannel() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
    val channel = NotificationChannel(CHANNEL_ID, "Navigation", NotificationManager.IMPORTANCE_LOW).apply {
      description = "The current turn while navigating"
      setShowBadge(false)
    }
    (getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).createNotificationChannel(channel)
  }

  private fun iconFor(name: String): Int {
    val id = resources.getIdentifier("ic_nav_$name", "drawable", packageName)
    return if (id != 0) id else R.drawable.ic_nav_straight
  }

  private fun build(icon: String, title: String, text: String, etaMs: Long?) = run {
    // The same intent as the launcher icon: brings the app's task back as it is (the
    // navigation screen), or recreates its activity if Android destroyed it.
    val launch = Intent(this, MainActivity::class.java)
      .setAction(Intent.ACTION_MAIN)
      .addCategory(Intent.CATEGORY_LAUNCHER)
      .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    val open = PendingIntent.getActivity(this, 0, launch, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
    val end = PendingIntent.getService(
      this, 1,
      Intent(this, NavigationService::class.java).setAction(ACTION_END),
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
    )
    val eta = etaMs?.let { " · ETA " + DateFormat.getTimeFormat(this).format(Date(it)) } ?: ""
    NotificationCompat.Builder(this, CHANNEL_ID)
      .setSmallIcon(iconFor(icon))
      .setContentTitle(title)
      .setContentText(text + eta)
      .setOngoing(true)
      .setOnlyAlertOnce(true)
      .setSilent(true)
      .setCategory(NotificationCompat.CATEGORY_NAVIGATION)
      .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
      .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
      .setContentIntent(open)
      .addAction(R.drawable.ic_nav_arrive, "End navigation", end)
      .build()
  }

  @SuppressLint("MissingPermission") // without the permission the update is simply not shown
  private fun show(icon: String, title: String, text: String, etaMs: Long?) {
    if (stopping) return
    val nm = NotificationManagerCompat.from(this)
    if (nm.areNotificationsEnabled()) {
      nm.notify(NOTIFICATION_ID, build(icon, title, text, etaMs))
    }
  }

  private fun finish(afterSpeech: Boolean) {
    if (stopping) return
    stopping = true
    active = false
    location?.stop()
    location = null
    receiver?.let { unregisterReceiver(it) }
    receiver = null
    val v = voice
    voice = null
    val stoppingGeneration = generation
    val done = {
      if (generation == stoppingGeneration) {
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        NotificationManagerCompat.from(this).cancel(NOTIFICATION_ID)
        stopSelf()
      }
    }
    if (v != null) v.shutdown(afterSpeech) { main.post(done) } else done()
  }

  override fun onDestroy() {
    location?.stop()
    receiver?.let { unregisterReceiver(it) }
    voice?.shutdown(false) {}
    wakeLock?.let { if (it.isHeld) it.release() }
    if (instance === this) instance = null
    super.onDestroy()
  }

  companion object {
    private const val CHANNEL_ID = "navigation"
    private const val NOTIFICATION_ID = 7201
    private const val ACTION_END = "org.flockfinder.app.nav.END"
    private const val EXTRA_SIMULATED = "simulated"
    private const val EXTRA_TITLE = "title"

    @Volatile private var instance: NavigationService? = null
    /** Where events for the Rust session go (the running session's channel). */
    @Volatile private var sink: Channel? = null
    /** For stopping the service when it has no instance yet (outlives any Activity). */
    @Volatile private var appContext: Context? = null

    private fun send(event: JSObject) {
      try {
        sink?.send(event)
      } catch (t: Throwable) {
        android.util.Log.w("FlockFinderNav", "event not delivered: $t")
      }
    }

    fun start(context: Context, events: Channel, simulated: Boolean, title: String) {
      sink = events
      appContext = context.applicationContext
      val intent = Intent(context, NavigationService::class.java)
        .putExtra(EXTRA_SIMULATED, simulated)
        .putExtra(EXTRA_TITLE, title)
      ContextCompat.startForegroundService(context, intent)
    }

    fun update(icon: String, title: String, text: String, etaMs: Long?) {
      val s = instance ?: return
      s.main.post { s.show(icon, title, text, etaMs) }
    }

    fun speak(text: String, camera: Boolean) {
      val s = instance ?: return
      s.main.post { s.voice?.speak(text, camera) }
    }

    fun stop(context: Context, afterSpeech: Boolean) {
      sink = null
      val s = instance
      if (s != null) {
        s.main.post { s.finish(afterSpeech) }
      } else {
        context.stopService(Intent(context, NavigationService::class.java))
      }
    }

    // Called from Rust over JNI (src-tauri/src/nav/platform.rs), on its own threads, with or
    // without an Activity.

    /** [update], with `etaMs` < 0 for no ETA. */
    @JvmStatic
    fun bridgeUpdate(icon: String, title: String, text: String, etaMs: Long) =
      update(icon, title, text, if (etaMs < 0) null else etaMs)

    @JvmStatic
    fun bridgeSpeak(text: String, camera: Boolean) = speak(text, camera)

    @JvmStatic
    fun bridgeStop(afterSpeech: Boolean) {
      val context = appContext
      if (context != null) {
        stop(context, afterSpeech)
      } else {
        sink = null
        instance?.let { s -> s.main.post { s.finish(afterSpeech) } }
      }
    }
  }
}
