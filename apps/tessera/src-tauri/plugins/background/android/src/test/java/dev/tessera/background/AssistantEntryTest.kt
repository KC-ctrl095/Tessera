package dev.tessera.background

import org.junit.Assert.*
import org.junit.Test

class AssistantEntryTest {
  private val assist = "android.intent.action.ASSIST"

  @Test fun onlyFixedActionsAreAccepted() {
    val entry = AssistantEntry()
    assertFalse(entry.request(null, 10, true))
    assertFalse(entry.request("android.intent.action.SEND", 10, true))
    assertEquals(AssistantEntry.NONE, entry.take(10, true, false))
    assertTrue(entry.request(assist, 10, true))
    assertEquals(AssistantEntry.LISTEN, entry.take(10, true, false))
    assertEquals(AssistantEntry.NONE, entry.take(10, true, false))
    assertTrue(entry.request("android.intent.action.VOICE_COMMAND", 10, true))
  }

  @Test fun anotherAppsRequestOnlyOpensTheChat() {
    val entry = AssistantEntry()
    entry.request(assist, 10, false)
    assertEquals(AssistantEntry.OPEN, entry.take(10, true, false))
    // A burst keeps a waiting system request trusted, never the other way round.
    entry.request(assist, 20, true)
    entry.request(assist, 21, false)
    assertEquals(AssistantEntry.LISTEN, entry.take(21, true, false))
    entry.request(assist, 100_000, true)
    entry.request(assist, 200_000, false)
    assertEquals(AssistantEntry.OPEN, entry.take(200_000, true, false))
  }

  @Test fun lockedOrHiddenNeverConsumesTheEntry() {
    val entry = AssistantEntry()
    entry.request(assist, 10, true)
    assertEquals(AssistantEntry.NONE, entry.take(20, true, true))
    assertEquals(AssistantEntry.NONE, entry.take(20, false, false))
    assertEquals(AssistantEntry.LISTEN, entry.take(20, true, false))
  }

  @Test fun staleEntriesExpireAndBurstsCoalesce() {
    val entry = AssistantEntry()
    entry.request(assist, 10, true)
    assertEquals(AssistantEntry.NONE, entry.take(20_011, true, false))
    entry.request(assist, 40_000, true)
    entry.request(assist, 40_001, true)
    assertEquals(AssistantEntry.LISTEN, entry.take(40_001, true, false))
    assertEquals(AssistantEntry.NONE, entry.take(40_001, true, false))
  }
}
