package dev.tessera.background

/**
 * One coalesced assist request. There is deliberately no intent content to retain,
 * only when it came and whether the system (not another app) sent it.
 */
internal class AssistantEntry {
  private var at: Long? = null
  private var fromSystem = false

  @Synchronized fun request(action: String?, now: Long, system: Boolean): Boolean {
    if (action != "android.intent.action.ASSIST" && action != "android.intent.action.VOICE_COMMAND") return false
    // A burst keeps a waiting system request trusted; an app's alone never is.
    val waiting = at?.let { now - it in 0..WINDOW_MS } == true
    fromSystem = (waiting && fromSystem) || system
    at = now
    return true
  }

  /** NONE, OPEN (show the chat, the microphone one tap away) or LISTEN (start listening). */
  @Synchronized fun take(now: Long, visible: Boolean, locked: Boolean): Int {
    val requested = at ?: return NONE
    if (now - requested !in 0..WINDOW_MS) { at = null; return NONE }
    if (!visible || locked) return NONE
    at = null
    return if (fromSystem) LISTEN else OPEN
  }

  companion object {
    const val NONE = 0
    const val OPEN = 1
    const val LISTEN = 2
    /** Long enough for a cold start and connecting; a request made while locked doesn't wait long. */
    const val WINDOW_MS = 20_000L
  }
}
