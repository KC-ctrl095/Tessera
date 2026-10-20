package dev.tessera.app

import android.graphics.Color
import android.os.Bundle
import android.util.Base64
import android.view.View
import android.webkit.WebView
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ContentInfoCompat
import androidx.core.view.OnReceiveContentListener
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import org.json.JSONArray
import org.json.JSONObject

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    // Dark chrome with light system-bar icons to match the app.
    enableEdgeToEdge(
      statusBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
      navigationBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
    )
    super.onCreate(savedInstanceState)
    window.decorView.setBackgroundColor(Color.parseColor("#0b0b0c"))
    // Edge-to-edge windows no longer resize for the keyboard: pad the content
    // by the system bars, display cutout and IME so the composer stays visible.
    val content = findViewById<android.view.View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { v, insets ->
      val types = WindowInsetsCompat.Type.systemBars() or
        WindowInsetsCompat.Type.displayCutout() or
        WindowInsetsCompat.Type.ime()
      val i = insets.getInsets(types)
      v.setPadding(i.left, i.top, i.right, i.bottom)
      WindowInsetsCompat.CONSUMED
    }
  }

  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    // Keyboards insert images (their clipboard, GIFs, stickers) through the
    // platform's receive-content API, which the web view turns down in a text
    // box ("Tessera does not support image pasting here"). Take them here and
    // hand them to the page: the composer adds them like picked files.
    ViewCompat.setOnReceiveContentListener(webView, KeyboardImages.TYPES, KeyboardImages)
  }
}

/** Images a keyboard inserts, to the page as a `tessera:content` event (lib/clipboard.ts). */
private object KeyboardImages : OnReceiveContentListener {
  val TYPES = arrayOf("image/png", "image/jpeg", "image/gif", "image/webp")
  /** The server takes files up to 25 MB. */
  private const val MAX_BYTES = 25 shl 20

  override fun onReceiveContent(view: View, payload: ContentInfoCompat): ContentInfoCompat? {
    // Pastes and drops reach the page through the web view itself.
    if (payload.source != ContentInfoCompat.SOURCE_INPUT_METHOD) return payload
    val split = payload.partition { it.uri != null }
    val images = split.first ?: return split.second
    val clip = images.clip
    val uris = (0 until clip.itemCount).mapNotNull { clip.getItemAt(it).uri }
    val resolver = view.context.applicationContext.contentResolver
    Thread {
      val items = JSONArray()
      for (uri in uris) {
        try {
          val mime = resolver.getType(uri) ?: clip.description.getMimeType(0)
          if (mime !in TYPES) continue
          val bytes = resolver.openInputStream(uri)?.use { s ->
            val out = java.io.ByteArrayOutputStream()
            val buf = ByteArray(64 shl 10)
            while (true) {
              val n = s.read(buf)
              if (n < 0) break
              out.write(buf, 0, n)
              if (out.size() > MAX_BYTES) return@use null
            }
            out.toByteArray()
          } ?: continue
          items.put(JSONObject().put("mime", mime).put("data", Base64.encodeToString(bytes, Base64.NO_WRAP)))
        } catch (e: Exception) {
          android.util.Log.w("Tessera", "couldn't read an inserted image: $e")
        }
      }
      if (items.length() == 0) return@Thread
      val detail = JSONObject().put("items", items).toString()
      view.post {
        (view as WebView).evaluateJavascript(
          "window.dispatchEvent(new CustomEvent('tessera:content',{detail:$detail}))",
          null,
        )
      }
    }.start()
    return split.second
  }
}
