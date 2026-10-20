package dev.tessera.background

import android.app.Activity
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import java.util.Locale

/** Never creates a network recognizer or uses a network TTS voice. */
class Voice(private val activity: Activity) {
  private val main = Handler(Looper.getMainLooper())
  private var recognizer: SpeechRecognizer? = null
  private var events: Channel? = null
  private var language = "en-US"
  private var listening = false
  private var generation = 0
  private var tts: TextToSpeech? = null
  private var ready = false
  private var initialized = false
  private var pending: Invoke? = null
  private var utterance = 0
  init {
    main.post {
      tts = TextToSpeech(activity) { result -> ready = result == TextToSpeech.SUCCESS; initialized = true }
    }
  }
  fun status(language: String): JSObject {
    val locale = Locale.forLanguageTag(language)
    val result = JSObject()
    result.put("initializing", !initialized)
    result.put("listen", Build.VERSION.SDK_INT >= 31 && SpeechRecognizer.isOnDeviceRecognitionAvailable(activity))
    result.put("speak", ready && tts?.voices?.any { !it.isNetworkConnectionRequired && it.locale.language == locale.language } == true)
    return result
  }
  fun start(invoke: Invoke, args: VoiceListenArgs) {
    if (Build.VERSION.SDK_INT < 31 || !SpeechRecognizer.isOnDeviceRecognitionAvailable(activity)) { invoke.reject("No on-device recognizer is installed. Choose Automatic or On my server."); return }
    cancel()
    language = args.language
    events = args.events
    listening = true
    val gen = ++generation
    recognizer = SpeechRecognizer.createOnDeviceSpeechRecognizer(activity)
    recognizer?.setRecognitionListener(object : RecognitionListener {
      override fun onReadyForSpeech(params: Bundle?) {}
      override fun onBeginningOfSpeech() {}
      override fun onRmsChanged(rms: Float) {}
      override fun onBufferReceived(buffer: ByteArray?) {}
      override fun onEndOfSpeech() {}
      override fun onEvent(event: Int, params: Bundle?) {}
      override fun onPartialResults(results: Bundle?) { words(results, false, gen) }
      override fun onResults(results: Bundle?) { words(results, true, gen); restart(gen) }
      override fun onError(error: Int) {
        if (gen != generation) return
        if (error == SpeechRecognizer.ERROR_NO_MATCH || error == SpeechRecognizer.ERROR_SPEECH_TIMEOUT) restart(gen)
        else { listening = false; val result = JSObject(); result.put("error", "On-device recognition stopped ($error). Try On my server."); events?.send(result) }
      }
    })
    try { recognize() } catch (_: Exception) { cancel(); invoke.reject("Could not start on-device recognition. Try On my server."); return }
    invoke.resolve()
  }
  private fun recognize() {
    val intent = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH)
    intent.putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
    intent.putExtra(RecognizerIntent.EXTRA_LANGUAGE, language)
    intent.putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
    intent.putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true)
    recognizer?.startListening(intent)
  }
  private fun restart(gen: Int) { main.postDelayed({ if (listening && gen == generation) { try { recognize() } catch (_: Exception) { listening = false; val result = JSObject(); result.put("error", "Could not restart on-device recognition. Try On my server."); events?.send(result) } } }, 250) }
  private fun words(results: Bundle?, final: Boolean, gen: Int) {
    if (gen != generation) return
    val text = results?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull() ?: return
    val result = JSObject(); result.put("text", text); result.put("final", final); events?.send(result)
  }
  fun finish() { listening = false; recognizer?.stopListening() }
  fun cancel() { generation++; listening = false; recognizer?.cancel(); recognizer?.destroy(); recognizer = null; events = null }
  fun say(invoke: Invoke, args: VoiceSayArgs) {
    quiet()
    val engine = tts
    val locale = Locale.forLanguageTag(args.language)
    val voice = engine?.voices?.firstOrNull { !it.isNetworkConnectionRequired && it.locale.language == locale.language }
    if (!ready || voice == null || engine == null) { invoke.reject("No offline voice for this language is installed. Choose On my server."); return }
    engine.voice = voice
    val id = (++utterance).toString()
    pending = invoke
    engine.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
      override fun onStart(utteranceId: String?) {}
      override fun onDone(utteranceId: String?) { main.post { if (utteranceId == id && id == utterance.toString()) { pending?.resolve(); pending = null } } }
      @Deprecated("Android callback")
      override fun onError(utteranceId: String?) { main.post { if (utteranceId == id && id == utterance.toString()) { pending?.reject("Offline speech failed"); pending = null } } }
    })
    main.postDelayed({ if (pending === invoke && id == utterance.toString()) { pending = null; engine.stop(); invoke.reject("Offline speech took too long. Try On my server.") } }, 90000)
    if (engine.speak(args.text, TextToSpeech.QUEUE_FLUSH, null, id) == TextToSpeech.ERROR) { pending = null; invoke.reject("Offline speech failed") }
  }
  fun quiet() { utterance++; tts?.stop(); pending?.resolve(); pending = null }
  fun pause() { cancel(); quiet() }
  fun destroy() { pause(); tts?.shutdown(); tts = null; ready = false }
}
