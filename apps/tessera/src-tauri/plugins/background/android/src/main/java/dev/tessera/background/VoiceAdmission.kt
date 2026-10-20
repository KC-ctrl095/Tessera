package dev.tessera.background

/** A permission reply is not admission: cancel/background can win before UI dispatch. */
internal class VoiceAdmission<T : Any> {
  data class Ticket<T : Any>(val invoke: T, val generation: Long)
  private var generation = 0L
  private var pending: Ticket<T>? = null
  private var owner: String? = null
  private val canceled = LinkedHashSet<String>()

  @Synchronized fun request(invoke: T, operation: String? = null): Boolean {
    if (operation != null && operation in canceled) return false
    owner = operation
    pending = Ticket(invoke, ++generation)
    return true
  }

  @Synchronized fun pending(invoke: T): Ticket<T>? = pending?.takeIf { it.invoke === invoke }

  @Synchronized fun cancel(operation: String? = null): Boolean {
    // IPC callbacks can be reordered; an End that arrives first still wins.
    (operation ?: owner)?.let { canceled.add(it) }
    while (canceled.size > 64) canceled.remove(canceled.first())
    if (operation != null && operation != owner) return false
    generation++
    pending = null
    return true
  }

  /** Recheck at the queued End/Quiet action as a replacement can arrive meanwhile. */
  @Synchronized fun act(operation: String?, action: () -> Unit): Boolean {
    if (operation != null && operation != owner) return false
    action()
    return true
  }

  /** Called in the actual UI action, not when it is queued. */
  @Synchronized fun start(ticket: Ticket<T>, allowed: () -> Boolean, start: () -> Unit): Boolean {
    if (pending !== ticket || ticket.generation != generation) return false
    pending = null
    if (!allowed()) return false
    start()
    return true
  }
}
