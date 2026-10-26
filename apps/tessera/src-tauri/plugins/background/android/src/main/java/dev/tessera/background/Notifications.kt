package dev.tessera.background

import android.annotation.SuppressLint
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.util.Base64
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import java.security.MessageDigest
import java.security.SecureRandom
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

/**
 * Building notifications. Text is always plain, the lock screen only ever
 * sees a generic public version, and every PendingIntent is immutable and
 * targets this app's own launcher activity, carrying nothing but a session
 * id signed with a key that never leaves the device. Agents' messages from
 * one chat are grouped under it.
 */
internal object Notifications {
  const val CHANNEL_PAGES = "tessera.pages"
  const val CHANNEL_DIGEST = "tessera.digest"
  const val CHANNEL_APPROVALS = "tessera.approvals"
  const val CHANNEL_ACTIVITY = "tessera.activity"
  const val CHANNEL_CONNECTION = "tessera.connection"
  const val CHANNEL_AGENTS = "tessera.agents"
  const val CHANNEL_QUIET = "tessera.quiet"
  const val CHANNEL_DOWNLOADS = "tessera.downloads"

  /** Per-session notifications share this id; the session is the tag. */
  const val ID_SESSION = 7200
  const val ID_KEEP_ALIVE = 7201
  /** Agents' messages: one each, tagged `n<id>`. */
  const val ID_AGENT = 7202
  /** Saved downloads, tagged with the file name. */
  const val ID_DOWNLOAD = 7203
  /** A chat's agents' messages, grouped: their summary, tagged with the chat. */
  const val ID_CHAT = 7204

  /** The chat an agent's message opens (to remove it with the chat's). */
  private const val EXTRA_FOR = "dev.tessera.background.for"
  private const val EXTRA_REVISION = "dev.tessera.background.revision"

  private const val ACTION_OPEN = "dev.tessera.background.OPEN"
  private const val EXTRA_SESSION = "dev.tessera.background.session"
  private const val EXTRA_SIG = "dev.tessera.background.sig"
  private val ID = Regex("^[A-Za-z0-9_-]{1,64}$")

  fun validId(s: String?): Boolean = s != null && ID.matches(s)

  fun canPost(ctx: Context): Boolean = NotificationManagerCompat.from(ctx).areNotificationsEnabled()

  fun ensureChannels(ctx: Context) {
    if (Build.VERSION.SDK_INT < 26) return
    val nm = ctx.getSystemService(NotificationManager::class.java) ?: return
    val approvals = NotificationChannel(
      CHANNEL_APPROVALS, "Approvals", NotificationManager.IMPORTANCE_HIGH
    ).apply {
      description = "When Tessera needs your go-ahead to continue"
      lockscreenVisibility = Notification.VISIBILITY_PRIVATE
    }
    val activity = NotificationChannel(
      CHANNEL_ACTIVITY, "Task updates", NotificationManager.IMPORTANCE_DEFAULT
    ).apply {
      description = "Finished tasks, subagent reports, scheduled jobs and errors"
      lockscreenVisibility = Notification.VISIBILITY_PRIVATE
    }
    val connection = NotificationChannel(
      CHANNEL_CONNECTION, "Background connection", NotificationManager.IMPORTANCE_LOW
    ).apply {
      description = "Shown while Tessera stays connected in the background"
      setShowBadge(false)
    }
    val agents = NotificationChannel(
      CHANNEL_AGENTS, "Messages from agents", NotificationManager.IMPORTANCE_HIGH
    ).apply {
      description = "What you asked an agent to tell you, and reminders"
      lockscreenVisibility = Notification.VISIBILITY_PRIVATE
    }
    val quiet = NotificationChannel(
      CHANNEL_QUIET, "During quiet hours", NotificationManager.IMPORTANCE_LOW
    ).apply {
      description = "Everything that arrives during your quiet hours, without sound"
      lockscreenVisibility = Notification.VISIBILITY_PRIVATE
    }
    val downloads = NotificationChannel(
      CHANNEL_DOWNLOADS, "Downloads", NotificationManager.IMPORTANCE_LOW
    ).apply {
      description = "Files saved to Downloads from a chat"
      setShowBadge(false)
    }
    val pages = NotificationChannel(CHANNEL_PAGES, "Page", NotificationManager.IMPORTANCE_HIGH).apply {
      description = "Action needed, urgent news and replies you asked for"
      lockscreenVisibility = Notification.VISIBILITY_PRIVATE
    }
    val digest = NotificationChannel(CHANNEL_DIGEST, "Digest", NotificationManager.IMPORTANCE_LOW).apply {
      description = "Grouped updates without sound or vibration"
      setSound(null, null)
      enableVibration(false)
      lockscreenVisibility = Notification.VISIBILITY_PRIVATE
    }
    nm.createNotificationChannels(listOf(pages, digest, approvals, activity, connection, agents, quiet, downloads))
  }

  @SuppressLint("MissingPermission") // canPost() checks it
  fun post(ctx: Context, args: PostArgs): Boolean {
    val session = args.sessionId
    if (!validId(args.tag) || (session != null && !validId(session)) || !canPost(ctx)) return false
    ensureChannels(ctx)
    val channel = if (args.silent) CHANNEL_DIGEST else CHANNEL_PAGES
    val loud = !args.silent
    NotificationPrivacy.dispatch({ PullJob.previews(ctx) }, { previews ->
      val text = NotificationPrivacy.message(previews, args.private, args.title, args.body, args.publicBody, args.sub)
      val public = NotificationCompat.Builder(ctx, channel)
        .setSmallIcon(R.drawable.tessera_notification)
        .setContentTitle("Tessera")
        .setContentText(text.publicBody)
        .build()
      val sameRevision = args.notificationId?.let { revision ->
        ctx.getSystemService(NotificationManager::class.java)?.activeNotifications.orEmpty().any {
          it.tag == args.tag && it.id == ID_SESSION && it.notification.extras.getLong(EXTRA_REVISION, -1L) == revision
        }
      } ?: true
      val b = NotificationCompat.Builder(ctx, channel)
        .setSmallIcon(R.drawable.tessera_notification)
        .setContentTitle(text.title)
        .setContentText(text.body)
        .setStyle(NotificationCompat.BigTextStyle().bigText(text.body))
        .setPriority(
          when {
            args.silent -> NotificationCompat.PRIORITY_LOW
            loud -> NotificationCompat.PRIORITY_HIGH
            else -> NotificationCompat.PRIORITY_DEFAULT
          }
        )
        .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
        .setPublicVersion(public)
        .setAutoCancel(true)
        .setSilent(args.silent)
        // The same message shown again (the app and a background check
        // both had it) replaces itself without a second alert.
        .setOnlyAlertOnce(args.silent || sameRevision)
        .setContentIntent(openIntent(ctx, session))
      args.notificationId?.let { revision -> b.addExtras(android.os.Bundle().apply { putLong(EXTRA_REVISION, revision) }) }
      text.subtitle?.takeIf { it.isNotBlank() }?.let { b.setSubText(it) }
      if (args.`when` > 0) b.setWhen(args.`when`).setShowWhen(true)
      if (session != null) {
        b.setCategory(NotificationCompat.CATEGORY_MESSAGE)
        b.addExtras(android.os.Bundle().apply { putString(EXTRA_FOR, session) })
        // One live card per chat or scheduled job, shared with PullJob.
        b.setGroup(group(args.tag)).setGroupSummary(true)
      }
      b.build()
    }, { notification ->
      NotificationManagerCompat.from(ctx).notify(args.tag, ID_SESSION, notification)
    })
    return true
  }

  private fun group(session: String) = "tessera.chat.$session"


  /**
   * A chat's summary goes with the last of its messages (`gone`: just
   * removed, maybe still listed). Removing a summary removes its group, so
   * only an empty one is removed.
   */
  private fun dropEmptySummaries(ctx: Context, gone: Set<String?>) {
    val active = ctx.getSystemService(NotificationManager::class.java)?.activeNotifications ?: return
    val left: Set<String?> = active
      .filter { it.id == ID_AGENT && it.tag !in gone }
      .mapNotNull { it.notification.extras.getString(EXTRA_FOR) }
      .toSet()
    for (s in active) {
      if (s.id == ID_CHAT && s.tag !in left) NotificationManagerCompat.from(ctx).cancel(s.tag, ID_CHAT)
    }
  }

  /**
   * One notification by tag: a session's (with its agents' messages and
   * their summary), or an agent's message (`n<id>`). Null: all of them
   * (never the keep-alive one).
   */
  fun withdraw(ctx: Context, tag: String?) {
    val nm = NotificationManagerCompat.from(ctx)
    val sys = ctx.getSystemService(NotificationManager::class.java)
    if (tag != null) {
      if (!validId(tag)) return
      nm.cancel(tag, ID_SESSION)
      nm.cancel(tag, ID_AGENT)
      nm.cancel(tag, ID_CHAT)
      val gone = mutableSetOf<String?>(tag)
      for (s in sys?.activeNotifications.orEmpty()) {
        if ((s.id == ID_AGENT || s.id == ID_SESSION) && s.notification.extras.getString(EXTRA_FOR) == tag) {
          nm.cancel(s.tag, s.id)
          gone.add(s.tag)
        }
      }
      dropEmptySummaries(ctx, gone)
      return
    }
    for (s in sys?.activeNotifications.orEmpty()) {
      if (s.id == ID_SESSION || s.id == ID_AGENT || s.id == ID_CHAT) nm.cancel(s.tag, s.id)
    }
  }

  /** "Saved to Downloads": tap to open it with an app the user picks. */
  @SuppressLint("MissingPermission") // canPost() checks it
  fun downloaded(ctx: Context, uri: Uri, name: String, mime: String) {
    if (!canPost(ctx)) return
    ensureChannels(ctx)
    NotificationPrivacy.dispatch({ PullJob.previews(ctx) }, { previews ->
      val text = NotificationPrivacy.download(previews, name)
      val view = Intent(Intent.ACTION_VIEW)
        .setDataAndType(uri, mime)
        .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
      val open = PendingIntent.getActivity(
        ctx,
        uri.hashCode(),
        Intent.createChooser(view, text.chooser).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
      )
      NotificationCompat.Builder(ctx, CHANNEL_DOWNLOADS)
        .setSmallIcon(android.R.drawable.stat_sys_download_done)
        .setContentTitle(text.title)
        .setContentText(text.body)
        .setPriority(NotificationCompat.PRIORITY_LOW)
        .setSilent(true)
        .setAutoCancel(true)
        .setContentIntent(open)
        .build()
    }, { notification -> NotificationManagerCompat.from(ctx).notify(uri.toString().hashCode().toString(), ID_DOWNLOAD, notification) })
  }

  fun keepAlive(ctx: Context, always: Boolean): Notification {
    ensureChannels(ctx)
    val disconnect = PendingIntent.getService(
      ctx,
      1,
      Intent(ctx, KeepAliveService::class.java).setAction(KeepAliveService.ACTION_DISCONNECT),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
    return NotificationCompat.Builder(ctx, CHANNEL_CONNECTION)
      .setSmallIcon(R.drawable.tessera_notification)
      .setContentTitle("Tessera is connected")
      .setContentText(
        if (always) "Staying connected in the background"
        else "Staying connected until the current work is done"
      )
      .setOngoing(true)
      .setSilent(true)
      .setShowWhen(false)
      .setCategory(NotificationCompat.CATEGORY_SERVICE)
      .setPriority(NotificationCompat.PRIORITY_LOW)
      .setContentIntent(openIntent(ctx, null))
      .addAction(0, "Disconnect", disconnect)
      .build()
  }

  private fun openIntent(ctx: Context, session: String?): PendingIntent {
    // The launcher activity, named explicitly: never anything resolved.
    val intent = ctx.packageManager.getLaunchIntentForPackage(ctx.packageName)
      ?: Intent(Intent.ACTION_MAIN)
    intent.setPackage(ctx.packageName)
    intent.flags = Intent.FLAG_ACTIVITY_NEW_TASK or
      Intent.FLAG_ACTIVITY_SINGLE_TOP or
      Intent.FLAG_ACTIVITY_CLEAR_TOP
    if (session != null) {
      intent.action = ACTION_OPEN
      intent.putExtra(EXTRA_SESSION, session)
      intent.putExtra(EXTRA_SIG, sign(ctx, session))
    }
    return PendingIntent.getActivity(
      ctx,
      session?.hashCode() ?: 0,
      intent,
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
  }

  /**
   * The session a tapped notification asks to open, or null. The activity is
   * exported (it is the launcher), so any app could send this action: only a
   * valid id with our own signature counts.
   */
  fun openedSession(ctx: Context, intent: Intent?): String? {
    if (intent == null || intent.action != ACTION_OPEN) return null
    return try {
      val session = intent.getStringExtra(EXTRA_SESSION)
      val sig = intent.getStringExtra(EXTRA_SIG)
      // Handled once: a recreated activity must not open it again.
      intent.removeExtra(EXTRA_SESSION)
      intent.removeExtra(EXTRA_SIG)
      if (session != null && sig != null && validId(session) &&
        MessageDigest.isEqual(sign(ctx, session).toByteArray(), sig.toByteArray())
      ) session else null
    } catch (e: Exception) {
      null // extras another app made unreadable on purpose
    }
  }

  private fun sign(ctx: Context, session: String): String {
    val mac = Mac.getInstance("HmacSHA256")
    mac.init(SecretKeySpec(secret(ctx), "HmacSHA256"))
    return Base64.encodeToString(mac.doFinal(session.toByteArray()), Base64.NO_WRAP)
  }

  /** Random per install; app-private and excluded from backups. */
  private fun secret(ctx: Context): ByteArray {
    val prefs = ctx.getSharedPreferences("dev.tessera.background", Context.MODE_PRIVATE)
    prefs.getString("key", null)?.let { return Base64.decode(it, Base64.NO_WRAP) }
    val key = ByteArray(32).also { SecureRandom().nextBytes(it) }
    prefs.edit().putString("key", Base64.encodeToString(key, Base64.NO_WRAP)).commit()
    return key
  }
}
