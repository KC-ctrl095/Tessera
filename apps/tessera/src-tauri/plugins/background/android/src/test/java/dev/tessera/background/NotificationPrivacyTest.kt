package dev.tessera.background

import org.junit.Assert.*
import org.junit.Test

class NotificationPrivacyTest {
  private fun allFields(payload: OsText) = listOf(payload.title, payload.body, payload.publicBody, payload.subtitle, payload.chooser).joinToString("|") + payload.lines.joinToString("|")

  @Test fun everyLivePullReplayExplicitAndUnknownKindUsesCurrentPolicy() {
    for (path in listOf("live", "pull", "replay", "explicit", "agent", "unknown-kind")) {
      for (private in listOf(false, true)) {
        val payload = NotificationPrivacy.message(false, private, "SECRET_${path}_TITLE", "SECRET_BODY", "SECRET_PUBLIC", "SECRET_SENDER")
        assertFalse(allFields(payload).contains("SECRET"))
        assertEquals("Tessera", payload.title)
        assertEquals(payload.body, payload.publicBody)
        assertNull(payload.subtitle)
      }
    }
  }

  @Test fun queuedWithPreviewsOnIsGenericWhenPostedAfterToggle() {
    val title = "SECRET_QUEUED_TITLE"
    assertEquals(title, NotificationPrivacy.message(true, false, title, "SECRET_BODY", "Public", "SECRET_SENDER").title)
    assertFalse(allFields(NotificationPrivacy.message(false, false, title, "SECRET_BODY", "Public", "SECRET_SENDER")).contains("SECRET"))
    assertFalse(allFields(NotificationPrivacy.message(true, true, title, "SECRET_BODY", "Public", "SECRET_SENDER")).contains("SECRET"))
  }

  @Test fun summaryCannotReexportOldRichTitlesOrSenderWhenPreviewsGoOff() {
    val payload = NotificationPrivacy.summary(false, false, "SECRET_SENDER", listOf("SECRET_OLD_TITLE", "SECRET_NEW_TITLE"))
    assertFalse(allFields(payload).contains("SECRET"))
    assertEquals(listOf("New messages"), payload.lines)
    assertEquals("New messages", payload.body)
    assertEquals(payload.body, payload.publicBody)
    assertFalse(allFields(NotificationPrivacy.summary(true, true, "SECRET_SENDER", listOf("SECRET_TITLE"))).contains("SECRET"))
  }

  @Test fun downloadedFileAndChooserAreGenericWithPreviewsOff() {
    val payload = NotificationPrivacy.download(false, "SECRET_FILENAME.pdf")
    assertFalse(allFields(payload).contains("SECRET"))
    assertEquals("Tessera", payload.title)
    assertEquals("Download finished", payload.body)
    assertEquals("Open download", payload.chooser)
  }

  @Test fun finalDispatchRechecksUnderTheSamePolicyLockAndRebuildsEveryPayload() {
    val builds = listOf<(Boolean) -> OsText>(
      { NotificationPrivacy.message(it, false, "SECRET_ALERT", "SECRET_BODY", "SECRET_PUBLIC", "SECRET_SENDER") },
      { NotificationPrivacy.message(it, false, "SECRET_PULL", "SECRET_BODY", "SECRET_PUBLIC", "SECRET_SENDER") },
      { NotificationPrivacy.summary(it, false, "SECRET_SENDER", listOf("SECRET_TITLE", "SECRET_OLD")) },
      { NotificationPrivacy.download(it, "SECRET_DOWNLOAD.pdf") }
    )
    for (build in builds) {
      var previews = true
      var reads = 0
      var posted = false
      NotificationPrivacy.dispatch({
        assertTrue(Thread.holdsLock(NotificationPrivacy.policyLock))
        reads++
        previews
      }, { policy ->
        assertTrue(Thread.holdsLock(NotificationPrivacy.policyLock))
        val payload = build(policy)
        if (policy) previews = false // off commits after the rich build
        payload
      }, { payload ->
        assertTrue(Thread.holdsLock(NotificationPrivacy.policyLock))
        assertEquals(2, reads)
        assertFalse(allFields(payload).contains("SECRET"))
        posted = true
      })
      assertTrue(posted)
    }
  }
}
