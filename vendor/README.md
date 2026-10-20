# Vendored crates

## wry 0.57.0 (`vendor/wry`)

Unmodified upstream wry 0.57.0 (MIT OR Apache-2.0, see its LICENSE files) except
the changes below. When updating wry, re-apply them or check whether
upstream has fixed them.

### Android: the IPC bridge only in the app's own frame

In `src/android/kotlin/RustWebView.kt`:

Upstream injects every initialization script with
`addDocumentStartJavaScript(..., setOf("*"))`. On Android that ignores
`for_main_frame_only`, so Tauri's IPC bridge, including its invoke key, is
injected into **every frame** of the app's WebView. Tessera's whiteboards show
untrusted content (model-written pages, remote sites, local web apps) in
sandboxed iframes; with the upstream behaviour, that content could call the
app's commands. The patch limits injection to the app's own origin
(`http(s)://tauri.localhost`), so only the trusted main frame gets the bridge.

Desktop platforms are unaffected: WebKitGTK, WKWebView and WebView2 honour
`for_main_frame_only`.

### Android: old web views get the IPC bridge only in the app's pages

In `src/android/mod.rs` (the custom protocol responder, marked `TESSERA`):

Where the phone's WebView lacks `addDocumentStartJavaScript` (old Android
System WebView versions), upstream falls back to writing the initialization
scripts, Tauri's IPC bridge and its key among them, into **every HTML
response of every custom protocol**, Tessera's `tessera-board` whiteboard
pages included. The patch writes them only into the app's own pages (Tauri's
`tauri` protocol, served as `http://tauri.localhost`).

`scripts/android.sh` refuses to build, and refuses to ship an APK, if the
upstream `setOf("*")` comes back (for example after updating wry without
re-applying the patch), or if the fallback below loses its check.

### Navigation guards on Linux, Windows and Android

The navigation handler is a pre-request allowlist, including subframes.
HTML boards and previews use only the app's board routes. Local web apps
render in the remote browser, or open outside the app through their scoped
loopback proxy. Other sites never load in the shell's frames.

- `src/webkitgtk/mod.rs` checks every `NavigationAction` immediately, including
  script-driven iframe navigations. The former deferred response check was
  too late to prevent a request and skipped subframes entirely.
- `src/webview2/mod.rs` registers both `NavigationStarting` and
  `FrameNavigationStarting` with the same guard. The first event alone covers
  only the main frame.
- `src/android/kotlin/RustWebViewClient.kt` passes all
  `shouldOverrideUrlLoading` requests to the guard, not just main-frame or
  user-gesture requests. Keep that behavior when updating wry.

These sites are marked `TESSERA`. The app's navigation guard must remain
side-effect-free. A refused navigation must not open a system browser and
thereby send the refused request anyway. Trusted UI link handlers call the
scoped opener themselves.

The parent document also pins `frame-src` to the exact board origin and its
`/board/` and `/preview/` routes before loading untrusted content. This is the
cross-platform backstop where a WebView omits a navigation callback. The
browser UI uses the same policy. Do not replace it with a load-event check,
which would only run after a request had already been sent.

### Android: images from the keyboard reach the app

In `src/android/kotlin/RustWebView.kt` (`onCreateInputConnection`, marked
`TESSERA`):

Keyboards insert images (Gboard's or Samsung's clipboard, GIF and sticker
keyboards) with `InputConnection.commitContent`, and only offer to when the
text field lists image types in its `EditorInfo`. Android's web view lists
none for a `<textarea>`, so pasting an image from the keyboard's clipboard into
the composer failed with "Tessera does not support image pasting here".

The patch only bridges the platform's receive-content API: when the app has set
an `OnReceiveContentListener` on the web view (the app's `MainActivity` does,
for PNG, JPEG, GIF and WebP), the input connection advertises the listener's
types and is wrapped with `InputConnectionCompat.createWrapper`, which hands
`commitContent` to that listener. Without a listener nothing changes. The
listener reads the image and passes it to the page (a `tessera:content` event,
see `apps/tessera/ui/src/lib/clipboard.ts`).

If the patch is lost, keyboard image pastes fail again with that message;
long-press → Paste still works (the web view hands those images to the page
itself).

### Android: "Take photo" keeps nothing

In `src/android/kotlin/RustWebChromeClient.kt` (the image-capture path of
`onShowFileChooser`, marked `TESSERA`):

The composer's "Take photo" is a hidden `<input type="file" accept="image/*"
capture>`; wry's file chooser answers it by opening the camera app with
`MediaStore.ACTION_IMAGE_CAPTURE`. Upstream (copied from Capacitor) has the
camera app write into the app's permanent picture folder and never removes the
file, so every photo taken would stay on the phone. The patch:
- writes the photo to `cache/camera/` (`apps/tessera/src-tauri/gen/android/app/src/main/res/xml/file_paths.xml`
  lists only that folder), deletes photos older than 10 minutes each time the
  camera opens and all of them when the app starts, and deletes the file of a
  cancelled or empty shot;
- answers the page (`onReceiveValue(null)`) if the camera app can't be launched,
  and falls back to the normal picker, so the input can't get stuck.

The app's manifest also declares the `IMAGE_CAPTURE` action under `<queries>`.
Without it Android 11+ hides the camera app from `resolveActivity`, wry decides
there's no camera, and "Take photo" would open the file picker instead.

If the patch is lost, photos pile up in the app's picture folder; taking one
still works.

### Build warnings

`Cargo.toml` also allows upstream's use of WebKitGTK's deprecated
`run_javascript` and declares the `v2_42` feature its code checks for, so
builds of the untouched upstream code stay quiet.
