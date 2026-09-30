package org.flockfinder.app.nav

import android.annotation.SuppressLint
import android.content.Context
import android.location.Location
import android.location.LocationListener
import android.location.LocationManager
import android.os.Bundle
import android.os.Looper
import com.google.android.gms.common.ConnectionResult
import com.google.android.gms.common.GoogleApiAvailability
import com.google.android.gms.location.FusedLocationProviderClient
import com.google.android.gms.location.LocationCallback
import com.google.android.gms.location.LocationRequest
import com.google.android.gms.location.LocationResult
import com.google.android.gms.location.LocationServices
import com.google.android.gms.location.Priority

/**
 * Position fixes about once a second while navigating: Google Play services' fused provider
 * where it's available, the platform GPS provider otherwise (phones without Play services).
 * Mock locations from Android's developer options come through either one unchanged.
 */
interface LocationSource {
  fun start(onFix: (Location) -> Unit)
  fun stop()

  companion object {
    const val INTERVAL_MS = 1000L

    fun hasPlayServices(context: Context): Boolean =
      try {
        GoogleApiAvailability.getInstance().isGooglePlayServicesAvailable(context) == ConnectionResult.SUCCESS
      } catch (t: Throwable) {
        false
      }

    fun request(): LocationRequest =
      LocationRequest.Builder(Priority.PRIORITY_HIGH_ACCURACY, INTERVAL_MS)
        .setMinUpdateIntervalMillis(INTERVAL_MS / 2)
        .setWaitForAccurateLocation(false)
        .build()

    fun create(context: Context): LocationSource =
      if (hasPlayServices(context)) FusedSource(context) else PlatformSource(context)
  }
}

class FusedSource(context: Context) : LocationSource {
  private val client: FusedLocationProviderClient = LocationServices.getFusedLocationProviderClient(context)
  private var callback: LocationCallback? = null

  @SuppressLint("MissingPermission") // checked before the service starts
  override fun start(onFix: (Location) -> Unit) {
    val cb = object : LocationCallback() {
      override fun onLocationResult(result: LocationResult) {
        result.locations.forEach(onFix)
      }
    }
    callback = cb
    client.requestLocationUpdates(LocationSource.request(), cb, Looper.getMainLooper())
  }

  override fun stop() {
    callback?.let { client.removeLocationUpdates(it) }
    callback = null
  }
}

class PlatformSource(context: Context) : LocationSource {
  private val manager = context.getSystemService(Context.LOCATION_SERVICE) as LocationManager
  private var listener: LocationListener? = null

  @SuppressLint("MissingPermission")
  override fun start(onFix: (Location) -> Unit) {
    val l = object : LocationListener {
      override fun onLocationChanged(location: Location) = onFix(location)
      @Deprecated("Deprecated in Java")
      override fun onStatusChanged(provider: String?, status: Int, extras: Bundle?) {}
      override fun onProviderEnabled(provider: String) {}
      override fun onProviderDisabled(provider: String) {}
    }
    listener = l
    val provider = if (manager.isProviderEnabled(LocationManager.GPS_PROVIDER)) LocationManager.GPS_PROVIDER else LocationManager.NETWORK_PROVIDER
    manager.requestLocationUpdates(provider, LocationSource.INTERVAL_MS, 0f, l, Looper.getMainLooper())
  }

  override fun stop() {
    listener?.let { manager.removeUpdates(it) }
    listener = null
  }
}
