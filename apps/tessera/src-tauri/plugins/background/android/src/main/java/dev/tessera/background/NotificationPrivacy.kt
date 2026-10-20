package dev.tessera.background

/** Every user-visible text field at the Android OS payload boundary. */
internal data class OsText(
  val title: String,
  val body: String,
  val publicBody: String,
  val subtitle: String? = null,
  val lines: List<String> = emptyList(),
  val chooser: String? = null
)

internal object NotificationPrivacy {
  val policyLock = Any()

  /** Rust writes don't share this lock yet. Re-check just before OS dispatch. */
  fun <T> dispatch(previews: () -> Boolean, build: (Boolean) -> T, notify: (T) -> Unit) {
    synchronized(policyLock) {
      val initial = previews()
      var payload = build(initial)
      val current = previews()
      if (initial && !current) payload = build(false)
      notify(payload)
    }
  }
  private const val GENERIC = "New activity. Open Tessera for details."

  fun message(previews: Boolean, private: Boolean, title: String, body: String, publicBody: String, subtitle: String?): OsText =
    if (private || !previews) OsText("Tessera", GENERIC, GENERIC)
    else OsText(title, body, publicBody, subtitle)

  fun summary(previews: Boolean, private: Boolean, subtitle: String?, titles: List<String>): OsText =
    if (private || !previews) OsText("Tessera", "New messages", "New messages", lines = listOf("New messages"))
    else OsText(subtitle ?: "Tessera", "${titles.size} messages", "${titles.size} messages", lines = titles.takeLast(5).asReversed())

  fun download(previews: Boolean, name: String): OsText =
    OsText(if (previews) name else "Tessera", "Download finished", "Download finished", chooser = if (previews) name else "Open download")
}
