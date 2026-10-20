package dev.tessera.background

import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.content.ContextCompat

/**
 * Keeps the app process (and so its server connection) out of the cached
 * state while work is in progress. It does no work of its own: the Rust side
 * holds the socket and decides when this runs.
 *
 * Type remoteMessaging: the connection relays a conversation with the agent
 * to this device. Unlike dataSync it has no daily time limit on Android 15+;
 * onTimeout still stops it cleanly if a future release adds one.
 */
class KeepAliveService : Service() {
  companion object {
    const val ACTION_DISCONNECT = "dev.tessera.background.DISCONNECT"
    private const val EXTRA_ALWAYS = "always"
    private const val TAG = "TesseraKeepAlive"

    @Volatile
    var running = false
      private set

    /** Null on success, else why Android refused (e.g. started from the background). */
    fun start(ctx: Context, always: Boolean): String? {
      return try {
        ContextCompat.startForegroundService(
          ctx,
          Intent(ctx, KeepAliveService::class.java).putExtra(EXTRA_ALWAYS, always)
        )
        null
      } catch (e: Exception) {
        Log.w(TAG, "not started: $e")
        e.message ?: e.javaClass.simpleName
      }
    }

    fun stop(ctx: Context) {
      ctx.stopService(Intent(ctx, KeepAliveService::class.java))
    }
  }

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    if (intent?.action == ACTION_DISCONNECT) {
      BackgroundPlugin.instance?.disconnectRequested()
      halt()
      return START_NOT_STICKY
    }
    val notification = Notifications.keepAlive(this, intent?.getBooleanExtra(EXTRA_ALWAYS, false) == true)
    try {
      if (Build.VERSION.SDK_INT >= 34) {
        startForeground(
          Notifications.ID_KEEP_ALIVE,
          notification,
          ServiceInfo.FOREGROUND_SERVICE_TYPE_REMOTE_MESSAGING
        )
      } else {
        startForeground(Notifications.ID_KEEP_ALIVE, notification)
      }
      running = true
    } catch (e: Exception) {
      // Not allowed right now (app in the background, type refused...).
      Log.w(TAG, "startForeground refused: $e")
      halt()
    }
    // Without the app's Rust side a restarted service would hold nothing.
    return START_NOT_STICKY
  }

  // Android 15+: a time-limited type ran out. Stop within the grace period.
  override fun onTimeout(startId: Int, fgsType: Int) {
    halt()
  }

  // Android 14: the same for shortService.
  @Deprecated("superseded by onTimeout(startId, fgsType) on Android 15")
  override fun onTimeout(startId: Int) {
    halt()
  }

  override fun onDestroy() {
    running = false
    super.onDestroy()
  }

  private fun halt() {
    running = false
    stopForeground(STOP_FOREGROUND_REMOVE)
    stopSelf()
  }
}
