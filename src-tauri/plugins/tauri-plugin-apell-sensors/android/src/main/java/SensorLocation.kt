package dev.apell.sensors

import android.annotation.SuppressLint
import android.content.Context
import android.location.Location
import android.location.LocationManager
import android.os.Handler
import android.os.Looper
import androidx.core.content.ContextCompat
import androidx.core.location.LocationManagerCompat
import androidx.core.os.CancellationSignal

internal class SensorLocation(private val context: Context, private val deliver: (Location?) -> Unit) {
  private val main = Handler(Looper.getMainLooper())
  private val signals = mutableListOf<CancellationSignal>()
  private val deadline = Runnable { finish(null) }
  private var stopped = false

  @SuppressLint("MissingPermission") // SensorsPlugin checks coarse/fine after the Android prompt.
  fun request(precise: Boolean) {
    val manager = context.getSystemService(LocationManager::class.java)
      ?: error("Location service is unavailable.")
    check(LocationManagerCompat.isLocationEnabled(manager))
    val enabled = manager.getProviders(true)
    val providers = if (LocationManager.FUSED_PROVIDER in enabled) {
      listOf(LocationManager.FUSED_PROVIDER)
    } else {
      // On older devices, network covers approximate permission and indoor fixes.
      listOf(LocationManager.NETWORK_PROVIDER, LocationManager.GPS_PROVIDER).filter {
        it in enabled && (precise || it != LocationManager.GPS_PROVIDER)
      }
    }
    check(providers.isNotEmpty())
    var remaining = providers.size
    fun update(location: Location?) {
      if (stopped) return
      remaining -= 1
      if (location != null && location.hasAccuracy() && location.accuracy.isFinite() && location.accuracy >= 0 &&
        location.latitude in -90.0..90.0 && location.longitude in -180.0..180.0 && location.time > 0
      ) {
        finish(location)
      } else if (remaining == 0) {
        finish(null)
      }
    }

    val executor = ContextCompat.getMainExecutor(context)
    main.postDelayed(deadline, 30_000L)
    for (provider in providers) {
      val signal = CancellationSignal()
      signals.add(signal)
      try {
        LocationManagerCompat.getCurrentLocation(manager, provider, signal, executor, ::update)
      } catch (_: Exception) {
        update(null)
      }
    }
  }

  fun stop() {
    if (stopped) return
    stopped = true
    main.removeCallbacks(deadline)
    signals.forEach { runCatching { it.cancel() } }
    signals.clear()
  }

  private fun finish(location: Location?) {
    if (stopped) return
    stop()
    deliver(location)
  }
}
