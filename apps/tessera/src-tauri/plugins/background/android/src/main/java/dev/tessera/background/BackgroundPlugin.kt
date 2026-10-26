package dev.tessera.background

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.app.KeyguardManager
import android.content.ActivityNotFoundException
import android.content.ContentValues
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Rect
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.os.PowerManager
import android.provider.MediaStore
import android.provider.Settings
import android.util.Base64
import android.view.PixelCopy
import android.webkit.WebView
import androidx.activity.result.ActivityResult
import androidx.appcompat.app.AppCompatActivity
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import app.tauri.PermissionState
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Channel
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.ByteArrayOutputStream
import java.io.File
import kotlin.math.ceil
import kotlin.math.floor
import kotlin.math.roundToInt

@InvokeArg
class PostArgs {
  lateinit var tag: String
  var notificationId: Long? = null
  var sessionId: String? = null
  var urgent: Boolean = false
  var title: String = ""
  var body: String = ""
  var publicBody: String = ""
  var agent: Boolean = false
  var silent: Boolean = false
  var private: Boolean = false
  var sub: String? = null
  var `when`: Long = 0
}

@InvokeArg
class PullArgs {
  var dir: String? = null
  var enabled: Boolean = false
}

@InvokeArg
class WakeArgs {
  var at: Long = 0
}

@InvokeArg
class SaveArgs {
  lateinit var path: String
  lateinit var name: String
  var mime: String = "application/octet-stream"
}

@InvokeArg
class OpenArgs {
  lateinit var uri: String
  var mime: String = "*/*"
}

@InvokeArg
class WithdrawArgs {
  var tag: String? = null
}

@InvokeArg
class KeepAliveArgs {
  var run: Boolean = false
  var eager: Boolean = false
}

/** A rect of the web view in CSS pixels, in a viewport this big. */
@InvokeArg
class CaptureArgs {
  var x: Double = 0.0
  var y: Double = 0.0
  var width: Double = 0.0
  var height: Double = 0.0
  var viewportWidth: Double = 0.0
  var viewportHeight: Double = 0.0
  var maxSide: Int = 1600
}

@InvokeArg
class VoiceListenArgs {
  var operation: String? = null
  var language: String = "en-US"
  lateinit var events: Channel
}

@InvokeArg
class VoiceEndArgs { var operation: String? = null }

@InvokeArg
class VoiceSayArgs {
  var operation: String? = null
  var language: String = "en-US"
  var text: String = ""
}
/**
 * Called only by the app's Rust side (the web UI has no permission for any
 * of this). Rust decides what to show, when to keep the connection and what
 * to capture; this class only talks to Android.
 */
@TauriPlugin(
  permissions = [
    Permission(strings = [Manifest.permission.POST_NOTIFICATIONS], alias = "notifications"),
    Permission(strings = [Manifest.permission.RECORD_AUDIO], alias = "microphone")
  ]
)
class BackgroundPlugin(private val activity: Activity) : Plugin(activity) {
  companion object {
    @Volatile
    var instance: BackgroundPlugin? = null
      private set

    /** Who starts the assistant for the system's own gestures (AOSP, Samsung), known by name
     *  because Android 11+ may hide their details from this app. */
    private val SYSTEM_ASSIST = setOf("android", "com.android.systemui", "com.samsung.android.app.sidekey", "com.sec.android.app.launcher")

    /** The app is on screen: its own connection brings everything. */
    @Volatile
    var inFront = false
      private set
  }

  /** Rust wants the connection kept alive once the app leaves the screen. */
  private var wanted = false
  private var eager = false
  /** A tap that arrived before Rust was listening. */
  private var pendingOpen: String? = null
  /** The app's web view (for `capture`). */
  private var webView: WebView? = null
  /** Android 9 and older: a download waiting for the user to pick where. */
  private var pendingSave: SaveArgs? = null
  private var voiceEngine: Voice? = null
  private fun voice(): Voice = voiceEngine ?: Voice(activity).also { voiceEngine = it }
  private val voiceAdmission = VoiceAdmission<Invoke>()
  private val speechAdmission = VoiceAdmission<Invoke>()
  private val assistantEntry = AssistantEntry()
  private fun unlocked() = !(activity.getSystemService(Activity.KEYGUARD_SERVICE) as KeyguardManager).isKeyguardLocked

  private fun assist(intent: Intent?) {
    if (intent == null) return
    // Reopened from Recents after Android stopped the app: the old gesture, not a new one.
    if (intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY != 0) return
    // TauriActivity forwards singleTask onNewIntent to this plugin. Ignore every
    // extra, URI and clip, including text supplied by another app.
    if (assistantEntry.request(intent.action, SystemClock.elapsedRealtime(), fromSystem(intent))) {
      intent.action = null // An activity recreation must not replay it.
      trigger("assistant", JSObject())
    }
  }

  /**
   * The system's assist gesture (side key, corner swipe, headset button), not another
   * app: only then does the microphone start by itself; otherwise the chat opens with
   * the microphone one tap away. Android reports the launching package itself, but an
   * app may claim another with the referrer extras, so those disqualify it. Read during
   * onNewIntent, while Android points the referrer at the new intent.
   */
  private fun fromSystem(intent: Intent): Boolean {
    if (intent.hasExtra(Intent.EXTRA_REFERRER) || intent.hasExtra(Intent.EXTRA_REFERRER_NAME)) return false
    val referrer = activity.referrer ?: return false
    val pkg = referrer.host?.takeIf { referrer.scheme == "android-app" } ?: return false
    if (pkg == activity.packageName) return false
    if (pkg in SYSTEM_ASSIST) return true
    return try {
      @Suppress("DEPRECATION")
      activity.packageManager.getApplicationInfo(pkg, 0).flags and android.content.pm.ApplicationInfo.FLAG_SYSTEM != 0
    } catch (_: PackageManager.NameNotFoundException) {
      false // Not visible to this app (Android 11+): not known to be the system.
    }
  }

  @Command
  fun assistantTakePending(invoke: Invoke) {
    activity.runOnUiThread {
      val taken = assistantEntry.take(SystemClock.elapsedRealtime(), inFront, !unlocked())
      val out = JSObject()
      out.put("pending", taken != AssistantEntry.NONE)
      out.put("listen", taken == AssistantEntry.LISTEN)
      invoke.resolve(out)
    }
  }

  @Command
  fun voiceStatus(invoke: Invoke) { val args = invoke.parseArgs(VoiceSayArgs::class.java); activity.runOnUiThread { if (!inFront || !unlocked()) invoke.reject("Voice only runs while the app is on screen and unlocked") else invoke.resolve(voice().status(args.language)) } }

  @Command
  fun voiceListen(invoke: Invoke) {
    if (!voiceAdmission.request(invoke, invoke.parseArgs(VoiceListenArgs::class.java).operation)) { invoke.reject("Microphone start was canceled."); return }
    if (getPermissionState("microphone") != PermissionState.GRANTED) requestPermissionForAlias("microphone", invoke, "voiceAllowed")
    else voiceAllowed(invoke)
  }

  @PermissionCallback
  private fun voiceAllowed(invoke: Invoke) {
    val ticket = voiceAdmission.pending(invoke)
    if (ticket == null) { invoke.reject("Microphone start was canceled. Tap voice again when the app is on screen."); return }
    val args = invoke.parseArgs(VoiceListenArgs::class.java)
    activity.runOnUiThread {
      // Permission can finish before this queued action. End/background must
      // still win at the actual microphone start, not just before dispatch.
      val started = voiceAdmission.start(ticket, {
        inFront && unlocked() && getPermissionState("microphone") == PermissionState.GRANTED
      }) { voice().start(invoke, args) }
      if (!started) invoke.reject("Microphone start was canceled. Tap voice again when the app is on screen.")
    }
  }

  @Command
  fun voiceFinish(invoke: Invoke) { val operation = invoke.parseArgs(VoiceEndArgs::class.java).operation; if (voiceAdmission.cancel(operation)) activity.runOnUiThread { voiceAdmission.act(operation) { voiceEngine?.finish() }; invoke.resolve() } else invoke.resolve() }
  @Command
  fun voiceCancel(invoke: Invoke) { val operation = invoke.parseArgs(VoiceEndArgs::class.java).operation; if (voiceAdmission.cancel(operation)) activity.runOnUiThread { voiceAdmission.act(operation) { voiceEngine?.cancel() }; invoke.resolve() } else invoke.resolve() }
  @Command
  fun voiceQuiet(invoke: Invoke) { val operation = invoke.parseArgs(VoiceEndArgs::class.java).operation; if (speechAdmission.cancel(operation)) activity.runOnUiThread { speechAdmission.act(operation) { voiceEngine?.quiet() }; invoke.resolve() } else invoke.resolve() }
  @Command
  fun voiceSay(invoke: Invoke) {
    val args = invoke.parseArgs(VoiceSayArgs::class.java)
    if (!speechAdmission.request(invoke, args.operation)) { invoke.reject("Speech was canceled."); return }
    val ticket = speechAdmission.pending(invoke) ?: run { invoke.reject("Speech was canceled."); return }
    activity.runOnUiThread {
      if (!speechAdmission.start(ticket, { inFront && unlocked() }) { voice().say(invoke, args) }) invoke.reject("Speech was canceled.")
    }
  }

  init {
    instance = this
  }

  override fun load(webView: WebView) {
    this.webView = webView
    // On cold WebView startup load can follow the first Activity onResume.
    inFront = (activity as? AppCompatActivity)?.lifecycle?.currentState?.isAtLeast(androidx.lifecycle.Lifecycle.State.RESUMED) == true
    Notifications.ensureChannels(activity)
    opened(activity.intent)
    assist(activity.intent)
  }

  override fun onNewIntent(intent: Intent) {
    opened(intent)
    assist(intent)
  }

  // Still visible at this point, so Android lets the service start; later,
  // from the background, it may refuse.
  override fun onPause(activity: AppCompatActivity) {
    inFront = false
    voiceAdmission.cancel()
    speechAdmission.cancel()
    voiceEngine?.pause()
    if (wanted && !KeepAliveService.running) KeepAliveService.start(this.activity, eager)
  }

  override fun onResume(activity: AppCompatActivity) {
    inFront = true
    // A request received while locked remains inert until the activity is visible
    // and unlocked. The UI pulls the flag again here, never the intent's extras.
    trigger("assistant", JSObject())
  }

  override fun onDestroy(activity: AppCompatActivity) {
    voiceAdmission.cancel()
    speechAdmission.cancel()
    voiceEngine?.destroy(); voiceEngine = null
    // Without its activity the app can't reach the Rust side: let it go.
    if (!activity.isChangingConfigurations) {
      wanted = false
      KeepAliveService.stop(activity)
    }
  }

  private fun opened(intent: Intent?) {
    val session = Notifications.openedSession(activity, intent) ?: return
    if (hasListener("open")) {
      val payload = JSObject()
      payload.put("sessionId", session)
      trigger("open", payload)
    } else {
      pendingOpen = session
    }
  }

  fun disconnectRequested() {
    wanted = false
    trigger("disconnect", JSObject())
  }

  @Command
  fun post(invoke: Invoke) {
    val out = JSObject()
    out.put("accepted", Notifications.post(activity, invoke.parseArgs(PostArgs::class.java)))
    invoke.resolve(out)
  }

  @Command
  fun withdraw(invoke: Invoke) {
    Notifications.withdraw(activity, invoke.parseArgs(WithdrawArgs::class.java).tag)
    invoke.resolve()
  }

  @Command
  fun status(invoke: Invoke) {
    invoke.resolve(status())
  }

  @Command
  fun requestNotifications(invoke: Invoke) {
    if (Build.VERSION.SDK_INT < 33 || notificationPermission() != "prompt") {
      invoke.resolve(status())
      return
    }
    requestPermissionForAlias("notifications", invoke, "notificationsAnswered")
  }

  @PermissionCallback
  private fun notificationsAnswered(invoke: Invoke) {
    invoke.resolve(status())
  }

  // Only on the user's explicit "stay connected: always" choice.
  @SuppressLint("BatteryLife")
  @Command
  fun requestBatteryExemption(invoke: Invoke) {
    if (!batteryOptimized()) {
      invoke.resolve(status())
      return
    }
    val intent = Intent(
      Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
      Uri.parse("package:" + activity.packageName)
    )
    try {
      startActivityForResult(invoke, intent, "batteryAnswered")
    } catch (e: ActivityNotFoundException) {
      // Some vendors remove the dialog: open the list instead.
      try {
        activity.startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS))
      } catch (_: Exception) {
      }
      invoke.resolve(status())
    }
  }

  @ActivityCallback
  private fun batteryAnswered(invoke: Invoke, result: ActivityResult) {
    invoke.resolve(status())
  }

  @Command
  fun openSettings(invoke: Invoke) {
    val intent = if (Build.VERSION.SDK_INT >= 26) {
      Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
        .putExtra(Settings.EXTRA_APP_PACKAGE, activity.packageName)
    } else {
      Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:" + activity.packageName))
    }
    try {
      activity.startActivity(intent)
      invoke.resolve()
    } catch (e: Exception) {
      invoke.reject(e.message ?: "no settings screen")
    }
  }

  @Command
  fun keepAlive(invoke: Invoke) {
    val args = invoke.parseArgs(KeepAliveArgs::class.java)
    val changed = args.eager != eager
    wanted = args.run
    eager = args.eager
    val out = JSObject()
    if (!args.run) {
      KeepAliveService.stop(activity)
    } else if (KeepAliveService.running) {
      if (changed) refresh()
    } else if (args.eager || !onScreen()) {
      KeepAliveService.start(activity, args.eager)?.let { out.put("error", it) }
    }
    out.put("running", KeepAliveService.running)
    invoke.resolve(out)
  }

  /** The background check for agents' messages (see [PullJob]). */
  @Command
  fun configurePull(invoke: Invoke) {
    val args = invoke.parseArgs(PullArgs::class.java)
    PullJob.configure(activity, args.dir, args.enabled)
    invoke.resolve()
  }

  /** A scheduled message is due then: check for it, even with the app closed. */
  @Command
  fun wakeAt(invoke: Invoke) {
    PullJob.wakeAt(activity, invoke.parseArgs(WakeArgs::class.java).at)
    invoke.resolve()
  }

  /**
   * Move a finished download from the app's cache into Downloads (Android
   * 10+: straight there; older: the system's "save as" screen) and say so
   * with a notification. Resolves `{uri, name}`.
   */
  @Command
  fun saveDownload(invoke: Invoke) {
    val args = invoke.parseArgs(SaveArgs::class.java)
    val file = File(args.path)
    val cache = activity.cacheDir.canonicalFile
    if (!file.canonicalFile.startsWith(cache) || !file.isFile) {
      invoke.reject("not a finished download")
      return
    }
    if (Build.VERSION.SDK_INT >= 29) {
      Thread {
        try {
          invoke.resolve(saveToDownloads(file, args))
        } catch (e: Exception) {
          invoke.reject(e.message ?: "couldn't save it to Downloads")
        }
      }.start()
      return
    }
    pendingSave = args
    val intent = Intent(Intent.ACTION_CREATE_DOCUMENT)
      .addCategory(Intent.CATEGORY_OPENABLE)
      .setType(args.mime)
      .putExtra(Intent.EXTRA_TITLE, args.name)
    try {
      startActivityForResult(invoke, intent, "saveAnswered")
    } catch (e: ActivityNotFoundException) {
      pendingSave = null
      invoke.reject("this phone has no way to save files")
    }
  }

  @ActivityCallback
  private fun saveAnswered(invoke: Invoke, result: ActivityResult) {
    val args = pendingSave
    pendingSave = null
    val uri = result.data?.data
    if (args == null || result.resultCode != Activity.RESULT_OK || uri == null) {
      invoke.reject("cancelled")
      return
    }
    Thread {
      try {
        activity.contentResolver.openOutputStream(uri)?.use { out ->
          File(args.path).inputStream().use { it.copyTo(out, 1 shl 16) }
        } ?: throw IllegalStateException("couldn't write there")
        Notifications.downloaded(activity, uri, args.name, args.mime)
        val out = JSObject()
        out.put("uri", uri.toString())
        out.put("name", args.name)
        invoke.resolve(out)
      } catch (e: Exception) {
        invoke.reject(e.message ?: "couldn't save it")
      }
    }.start()
  }

  /** Android 10+: into Downloads through the media store (no permission). */
  @SuppressLint("NewApi")
  private fun saveToDownloads(file: File, args: SaveArgs): JSObject {
    val resolver = activity.contentResolver
    val values = ContentValues().apply {
      put(MediaStore.MediaColumns.DISPLAY_NAME, args.name)
      put(MediaStore.MediaColumns.MIME_TYPE, args.mime)
      put(MediaStore.MediaColumns.IS_PENDING, 1)
    }
    val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
      ?: throw IllegalStateException("Downloads isn't available")
    try {
      resolver.openOutputStream(uri)?.use { out ->
        file.inputStream().use { it.copyTo(out, 1 shl 16) }
      } ?: throw IllegalStateException("couldn't write to Downloads")
      values.clear()
      values.put(MediaStore.MediaColumns.IS_PENDING, 0)
      resolver.update(uri, values, null, null)
    } catch (e: Exception) {
      resolver.delete(uri, null, null)
      throw e
    }
    // Android renames a taken name ("report (1).pdf"): report the real one.
    var name = args.name
    resolver.query(uri, arrayOf(MediaStore.MediaColumns.DISPLAY_NAME), null, null, null)?.use {
      if (it.moveToFirst()) name = it.getString(0) ?: name
    }
    Notifications.downloaded(activity, uri, name, args.mime)
    val out = JSObject()
    out.put("uri", uri.toString())
    out.put("name", name)
    return out
  }

  /** Open a saved download with the app the user picks. */
  @Command
  fun openDownload(invoke: Invoke) {
    val args = invoke.parseArgs(OpenArgs::class.java)
    val uri = Uri.parse(args.uri)
    if (uri.scheme != "content") {
      invoke.reject("not a saved download")
      return
    }
    val view = Intent(Intent.ACTION_VIEW)
      .setDataAndType(uri, args.mime)
      .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    try {
      activity.startActivity(Intent.createChooser(view, null))
      invoke.resolve()
    } catch (e: ActivityNotFoundException) {
      invoke.reject("No app on this phone opens this kind of file.")
    }
  }

  @Command
  fun takeOpened(invoke: Invoke) {
    val out = JSObject()
    pendingOpen?.let { out.put("sessionId", it) }
    pendingOpen = null
    invoke.resolve(out)
  }

  /**
   * A picture of one rect of the web view: a whiteboard tile the user is
   * looking at, for an agent that asked to look (or "Ask about this"). Only
   * the app's Rust side calls this, with a rect the UI measured. Only that
   * rect of the window is copied, scaled down and returned; nothing is
   * stored. Refused while the app isn't on screen.
   */
  @Command
  fun capture(invoke: Invoke) {
    val args = invoke.parseArgs(CaptureArgs::class.java)
    activity.runOnUiThread {
      val wv = webView
      if (wv == null || wv.width <= 0 || wv.height <= 0 || !onScreen() ||
        !(args.viewportWidth >= 1.0 && args.viewportHeight >= 1.0)
      ) {
        invoke.reject("Tessera isn't on screen")
        return@runOnUiThread
      }
      val sx = wv.width / args.viewportWidth
      val sy = wv.height / args.viewportHeight
      // Edges inside a pixel round inwards: no pixel from outside the tile
      // (a little slack for float noise).
      val left = ceil(args.x * sx - 0.02).toInt().coerceIn(0, wv.width)
      val top = ceil(args.y * sy - 0.02).toInt().coerceIn(0, wv.height)
      val right = floor((args.x + args.width) * sx + 0.02).toInt().coerceIn(0, wv.width)
      val bottom = floor((args.y + args.height) * sy + 0.02).toInt().coerceIn(0, wv.height)
      if (right - left < 8 || bottom - top < 8) {
        invoke.reject("the tile isn't on screen")
        return@runOnUiThread
      }
      val bitmap = Bitmap.createBitmap(right - left, bottom - top, Bitmap.Config.ARGB_8888)
      if (Build.VERSION.SDK_INT >= 26) {
        // What is on screen, frames included, and nothing outside the rect.
        val at = IntArray(2)
        wv.getLocationInWindow(at)
        val src = Rect(at[0] + left, at[1] + top, at[0] + right, at[1] + bottom)
        PixelCopy.request(activity.window, src, bitmap, { result ->
          if (result == PixelCopy.SUCCESS) {
            encode(invoke, bitmap, args.maxSide)
          } else {
            bitmap.recycle()
            invoke.reject("couldn't copy the screen ($result)")
          }
        }, Handler(Looper.getMainLooper()))
      } else {
        val canvas = Canvas(bitmap)
        canvas.translate(-(left + wv.scrollX).toFloat(), -(top + wv.scrollY).toFloat())
        wv.draw(canvas)
        encode(invoke, bitmap, args.maxSide)
      }
    }
  }

  /** Scale to `maxSide` and encode off the main thread: PNG, or JPEG when big. */
  private fun encode(invoke: Invoke, bitmap: Bitmap, maxSide: Int) {
    Thread {
      try {
        val long = maxOf(bitmap.width, bitmap.height)
        val out = if (maxSide in 1 until long) {
          val k = maxSide.toDouble() / long
          Bitmap.createScaledBitmap(
            bitmap,
            maxOf(1, (bitmap.width * k).roundToInt()),
            maxOf(1, (bitmap.height * k).roundToInt()),
            true
          )
        } else {
          bitmap
        }
        val png = ByteArrayOutputStream()
        out.compress(Bitmap.CompressFormat.PNG, 100, png)
        var bytes = png.toByteArray()
        var mime = "image/png"
        if (bytes.size > 1_200_000) {
          val jpeg = ByteArrayOutputStream()
          out.compress(Bitmap.CompressFormat.JPEG, 85, jpeg)
          if (jpeg.size() < bytes.size) {
            bytes = jpeg.toByteArray()
            mime = "image/jpeg"
          }
        }
        // The server takes up to 4 MB.
        if (bytes.size > 3_800_000) {
          val small = ByteArrayOutputStream()
          out.compress(Bitmap.CompressFormat.JPEG, 60, small)
          bytes = small.toByteArray()
          mime = "image/jpeg"
        }
        val res = JSObject()
        res.put("mime", mime)
        res.put("data", Base64.encodeToString(bytes, Base64.NO_WRAP))
        res.put("width", out.width)
        res.put("height", out.height)
        if (out !== bitmap) out.recycle()
        bitmap.recycle()
        invoke.resolve(res)
      } catch (e: Throwable) {
        invoke.reject(e.message ?: "couldn't make the picture")
      }
    }.start()
  }

  /** New text on the running service's notification ("always" vs "while busy"). */
  @SuppressLint("MissingPermission") // canPost() checks it
  private fun refresh() {
    if (Notifications.canPost(activity)) {
      NotificationManagerCompat.from(activity)
        .notify(Notifications.ID_KEEP_ALIVE, Notifications.keepAlive(activity, eager))
    }
  }

  private fun onScreen(): Boolean =
    (activity as? LifecycleOwner)?.lifecycle?.currentState?.isAtLeast(Lifecycle.State.RESUMED) == true

  private fun status(): JSObject {
    val out = JSObject()
    out.put("permission", notificationPermission())
    out.put("batteryOptimized", batteryOptimized())
    return out
  }

  private fun notificationPermission(): String {
    val granted = Build.VERSION.SDK_INT < 33 ||
      ContextCompat.checkSelfPermission(activity, Manifest.permission.POST_NOTIFICATIONS) ==
      PackageManager.PERMISSION_GRANTED
    if (granted) return if (Notifications.canPost(activity)) "granted" else "denied"
    // Answered "don't allow" twice: Android won't show the prompt again.
    return if (getPermissionState("notifications") == PermissionState.DENIED) "denied" else "prompt"
  }

  private fun batteryOptimized(): Boolean {
    val pm = activity.getSystemService(PowerManager::class.java) ?: return false
    return !pm.isIgnoringBatteryOptimizations(activity.packageName)
  }
}
