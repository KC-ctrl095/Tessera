package dev.tessera.background

import org.junit.Assert.*
import org.junit.Test

class VoiceAdmissionTest {
  private fun queuedStart(gate: VoiceAdmission<Any>, invoke: Any, allowed: () -> Boolean, start: () -> Unit): () -> Boolean {
    val ticket = checkNotNull(gate.pending(invoke))
    return { gate.start(ticket, allowed, start) }
  }

  @Test fun cancelBetweenPermissionAndUiActionNeverStarts() {
    val gate = VoiceAdmission<Any>()
    val invoke = Any()
    gate.request(invoke)
    var starts = 0
    val queued = queuedStart(gate, invoke, { true }) { starts++ }
    gate.cancel()
    assertFalse(queued())
    assertEquals(0, starts)
  }

  @Test fun backgroundBetweenPermissionAndUiActionNeverStarts() {
    val gate = VoiceAdmission<Any>()
    val invoke = Any()
    gate.request(invoke)
    var foreground = true
    var starts = 0
    val queued = queuedStart(gate, invoke, { foreground }) { starts++ }
    foreground = false
    assertFalse(queued())
    assertNull(gate.pending(invoke))
    assertEquals(0, starts)
  }

  @Test fun permissionRevokedAfterQueueNeverStarts() {
    val gate = VoiceAdmission<Any>()
    val invoke = Any()
    gate.request(invoke)
    var permission = true
    var starts = 0
    val queued = queuedStart(gate, invoke, { permission }) { starts++ }
    permission = false
    assertFalse(queued())
    assertEquals(0, starts)
  }

  @Test fun oldActionCannotConsumeNewInvocation() {
    val gate = VoiceAdmission<Any>()
    val first = Any()
    val second = Any()
    gate.request(first)
    var starts = 0
    val old = queuedStart(gate, first, { true }) { starts++ }
    gate.request(second)
    val current = queuedStart(gate, second, { true }) { starts++ }
    assertFalse(old())
    assertTrue(current())
    assertEquals(1, starts)
  }

  @Test fun cancelGenerationWinsEvenIfInvocationIdentityIsReused() {
    val gate = VoiceAdmission<Any>()
    val invoke = Any()
    gate.request(invoke)
    var starts = 0
    val old = queuedStart(gate, invoke, { true }) { starts++ }
    gate.cancel()
    gate.request(invoke)
    val current = queuedStart(gate, invoke, { true }) { starts++ }
    assertFalse(old())
    assertTrue(current())
    assertEquals(1, starts)
  }

  @Test fun foregroundPermittedStartIsAdmittedOnce() {
    val gate = VoiceAdmission<Any>()
    val invoke = Any()
    gate.request(invoke)
    var starts = 0
    val queued = queuedStart(gate, invoke, { true }) { starts++ }
    assertTrue(queued())
    assertFalse(queued())
    assertNull(gate.pending(invoke))
    assertEquals(1, starts)
  }

  @Test fun staleOwnerCannotEndTheReplacementAtReceiptOrUiDispatch() {
    val gate = VoiceAdmission<Any>()
    val first = Any(); val second = Any()
    gate.request(first, "first")
    assertTrue(gate.cancel("first"))
    gate.request(second, "second")
    var ended = 0
    assertFalse(gate.act("first") { ended++ })
    assertFalse(gate.cancel("first"))
    assertTrue(gate.act("second") { ended++ })
    assertEquals(1, ended)
    assertNotNull(gate.pending(second))
  }

  @Test fun cancelArrivingBeforeStartStillWinsWithoutReplacingCurrentOwner() {
    val gate = VoiceAdmission<Any>()
    val current = Any(); val stale = Any()
    gate.request(current, "current")
    assertFalse(gate.cancel("stale"))
    assertFalse(gate.request(stale, "stale"))
    assertNotNull(gate.pending(current))
    assertNull(gate.pending(stale))
  }
}
