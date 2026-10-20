package dev.tessera.background

import android.app.AlarmManager
import android.app.PendingIntent
import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log
import org.json.JSONObject

/**
 * The background check for agents' messages while Tessera is closed or
 * asleep. No third-party push service: about every 15 minutes (Android's
 * shortest period, batched with other apps' work and paused in deep sleep),
 * and when a scheduled message is due, this job asks the app's Rust side
 * (through JNI, [Native.pull]) to connect to the server, fetch what this
 * device missed and say what to show. It runs only with a network and
 * never while the app is on screen (its own connection brings everything).
 */
class PullJob : JobService() {
  companion object {
    private const val TAG = "TesseraPull"
    private const val PERIODIC = 7300
    private const val NOW = 7301
    private const val PERIOD_MS = 15 * 60 * 1000L
    private const val PREFS = "dev.tessera.background.pull"

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    fun previews(ctx: Context): Boolean = synchronized(NotificationPrivacy.policyLock) {
      try {
        val dir = prefs(ctx).getString("dir", null)
        dir != null && JSONObject(java.io.File(dir, "notifications.json").readText()).optBoolean("previews", false)
      } catch (_: Exception) { false }
    }

    /** Turn the check on (with the app's data directory) or off. */
    fun configure(ctx: Context, dir: String?, enabled: Boolean) {
      val on = enabled && !dir.isNullOrEmpty()
      prefs(ctx).edit().putString("dir", dir).putBoolean("enabled", on).apply()
      val js = ctx.getSystemService(JobScheduler::class.java) ?: return
      if (!on) {
        js.cancel(PERIODIC)
        js.cancel(NOW)
        alarms(ctx)?.cancel(wakeIntent(ctx))
        return
      }
      if (js.allPendingJobs.any { it.id == PERIODIC }) return
      val job = JobInfo.Builder(PERIODIC, ComponentName(ctx, PullJob::class.java))
        .setPeriodic(PERIOD_MS)
        .setRequiredNetworkType(JobInfo.NETWORK_TYPE_ANY)
        .setPersisted(true)
        .build()
      if (js.schedule(job) != JobScheduler.RESULT_SUCCESS) Log.w(TAG, "periodic check not scheduled")
    }

    /** Check right away, as soon as there is a network. */
    fun runSoon(ctx: Context) {
      if (!prefs(ctx).getBoolean("enabled", false)) return
      val js = ctx.getSystemService(JobScheduler::class.java) ?: return
      val job = JobInfo.Builder(NOW, ComponentName(ctx, PullJob::class.java))
        .setRequiredNetworkType(JobInfo.NETWORK_TYPE_ANY)
        .setOverrideDeadline(60_000)
        .build()
      js.schedule(job)
    }

    /**
     * Wake up at `at` (unix ms) for a scheduled message, even in Doze (an
     * exact alarm where the user allows them, otherwise within minutes).
     * The earliest pending time wins.
     */
    fun wakeAt(ctx: Context, at: Long) {
      val p = prefs(ctx)
      if (!p.getBoolean("enabled", false) || at <= 0) return
      val now = System.currentTimeMillis()
      val current = p.getLong("wakeAt", 0)
      if (current in (now + 1) until at) return
      p.edit().putLong("wakeAt", at).apply()
      val am = alarms(ctx) ?: return
      // A few seconds late, so the server has sent it.
      val time = maxOf(at, now) + 3_000
      val exact = Build.VERSION.SDK_INT < 31 || am.canScheduleExactAlarms()
      try {
        if (exact) {
          am.setExactAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, time, wakeIntent(ctx))
        } else {
          am.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, time, wakeIntent(ctx))
        }
      } catch (e: SecurityException) {
        am.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, time, wakeIntent(ctx))
      }
    }

    private fun alarms(ctx: Context) = ctx.getSystemService(AlarmManager::class.java)

    private fun wakeIntent(ctx: Context): PendingIntent = PendingIntent.getBroadcast(
      ctx,
      0,
      Intent(ctx, WakeReceiver::class.java),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )

    /** Do what the Rust side decided. */
    fun apply(ctx: Context, json: String?) {
      if (json.isNullOrEmpty()) return
      val o = try {
        JSONObject(json)
      } catch (e: Exception) {
        return
      }
      o.optString("error").takeIf { it.isNotEmpty() }?.let { Log.i(TAG, "check failed: $it") }
      o.optJSONArray("withdraw")?.let { a ->
        for (i in 0 until a.length()) Notifications.withdraw(ctx, a.optString(i))
      }
      val accepted = org.json.JSONArray()
      o.optJSONArray("suppressed")?.let { a -> for (i in 0 until a.length()) accepted.put(a.getLong(i)) }
      synchronized(NotificationPrivacy.policyLock) {
        o.optJSONArray("posts")?.let { a ->
          for (i in 0 until a.length()) {
            val p = a.optJSONObject(i) ?: continue
            val args = PostArgs()
            args.tag = p.optString("tag")
            args.sessionId = p.optString("sessionId").takeIf { it.isNotEmpty() && !p.isNull("sessionId") }
            args.urgent = p.optBoolean("urgent")
            args.title = p.optString("title")
            args.body = p.optString("body")
            args.publicBody = p.optString("publicBody")
            args.agent = p.optBoolean("agent")
            args.silent = p.optBoolean("silent")
            args.private = p.optBoolean("private")
            args.sub = p.optString("sub").takeIf { it.isNotEmpty() }
            args.`when` = p.optLong("when", 0)
            if (Notifications.post(ctx, args) && p.has("notificationId")) {
              accepted.put(p.getLong("notificationId"))
            }
          }
        }
      }
      val dir = prefs(ctx).getString("dir", null)
      if (dir != null && accepted.length() > 0 && o.has("ackServer") && o.has("ackKey")) {
        Native.ack(dir, JSONObject().put("server", o.getString("ackServer")).put("key", o.getString("ackKey")).put("ids", accepted).toString())
      }
      val wake = o.optLong("wakeAt", 0)
      if (wake > 0) wakeAt(ctx, wake)
    }
  }

  @Volatile
  private var worker: Thread? = null

  override fun onStartJob(params: JobParameters): Boolean {
    val p = prefs(this)
    val dir = p.getString("dir", null)
    if (BackgroundPlugin.inFront || dir.isNullOrEmpty() || !p.getBoolean("enabled", false)) {
      return false
    }
    if (params.jobId == NOW) p.edit().remove("wakeAt").apply()
    val ctx = applicationContext
    worker = Thread {
      val json = Native.pull(dir)
      apply(ctx, json)
      jobFinished(params, false)
    }.also { it.start() }
    return true
  }

  // Stopped early (network gone, time up): try again later.
  override fun onStopJob(params: JobParameters): Boolean = true
}

/** The alarm for a scheduled message: check as soon as there is a network. */
class WakeReceiver : BroadcastReceiver() {
  override fun onReceive(ctx: Context, intent: Intent) {
    PullJob.runSoon(ctx)
  }
}

/** The app's Rust side (`notify/pull.rs`), loaded on first use. */
internal object Native {
  @Volatile
  private var loaded = false

  fun pull(dir: String): String? {
    return try {
      if (!loaded) {
        System.loadLibrary("tessera_app_lib")
        loaded = true
      }
      pullNative(dir)
    } catch (e: Throwable) {
      Log.w("TesseraPull", "check unavailable: $e")
      null
    }
  }

  fun ack(dir: String, json: String) {
    try { if (loaded) ackNative(dir, json) } catch (_: Throwable) { }
  }

  @JvmStatic
  private external fun ackNative(dir: String, json: String)

  @JvmStatic
  private external fun pullNative(dir: String): String
}
