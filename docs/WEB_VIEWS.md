# Web views: sign-ins and homework

A web view is a real Chrome tab, not a preview of the website. It uses
Tessera's own browser profile on the server, or on the computer named in the
tile. Your devices see that same page and sign-in; your personal browser's
profile, passwords and extensions are separate.

## Like ordinary Chrome

- Password/code sign-ins, redirects and “Sign in with …” windows can use
  their real opener, including messages between different websites.
- Cookies, “remember me”, local storage and IndexedDB stay in that computer's
  profile when its browser or Tessera restarts. Chrome's usual cookie and
  storage rules apply, including third-party cookies/storage, SameSite and
  site partitioning. Tessera does not turn off third-party cookies or add
  a stricter cookie policy. A site's own rules and Chrome's changes still apply.
- On Linux, virtual web views now run ordinary desktop Chrome on a private
  in-memory display. They report Chrome's own identity, not a rewritten
  “HeadlessChrome” identity or a pretend Android phone. There is no display
  server to install and no access to your desktop or clipboard. Narrow
  tiles still have touch controls and a phone-sized layout. Pages get WebGL
  (maps, charts, some sign-in checks) from Chrome's own software renderer,
  as in its headless mode, since there is no graphics card to use. Chrome's
  toolbar and address-bar pop-ups, which nobody sees in a web view, are drawn
  natively instead of as web pages: about 100 MB less memory, nothing a page
  can notice.
- Ordinary new-tab links have their own saved page and tab in the tile. Switch
  with the strip, or the phone's tab-count button; use **New web tab** to add
  a page. Explicit sign-in windows still stack over their opener; Back returns
  to it and the site's opener messages keep working. Chrome's default
  `target=_blank` no-opener protection remains.
- Click a website's upload field, then **Choose files** in Tessera. Only the
  files you pick on your current phone/computer go to that website, even when
  the page runs on the server or another computer. Up to **8 files totaling
  200 MiB** can be sent, in small encrypted chunks with progress and Cancel.
  No arbitrary server/machine path can be selected.
- Downloads go to Tessera's downloads folder on the computer running the
  page. Server downloads also appear as downloadable files in the tile/chat.
- Phones get mobile viewport and touch metrics without pretending to be
  Android Chrome. **Desktop site** switches back to desktop layout. Tap, drag,
  pinch and long-press control the actual page; swipe right from the left edge
  to go Back. The keyboard button raises the shared page keyboard; **Paste**
  uses your device clipboard, or a normal paste box if clipboard access is
  unavailable. Long-press/right-click can copy selected text or a link.
- **Find in page** (Ctrl/Cmd+F), local history suggestions, bookmarks and
  **Share link** are in the web-view controls. Bookmarks store saved-page IDs,
  not passwords or page contents. They are scoped to this server; private-chat
  bookmarks and groups stay only in memory and clear on close or disconnect.
  Ctrl/Cmd+T, W and Tab work while the page
  keyboard has focus (a browser-hosted UI may reserve these for itself).
- **Save page as PDF** reads the page in small chunks, up to 16 MiB, and exports
  it into the existing download bar. Larger pages should be printed in your browser.
- If the browser stops, the tile and the chat using it say so. Tabs keep
  their handles and last addresses, but unsent forms and unfinished popups
  cannot be restored. The first three restarts wait 0.5, 1 and 2 seconds.
  Repeated stops less than ten quiet minutes apart pause automatic recovery;
  every caller (including an agent) must wait 60 seconds before trying again.
  Then reload or use the existing tab, rather than opening duplicates.

## Still different

- A web view is not your personal browser session. Signing in on the server
  does not sign you in on a different machine. A session cookie without an
  expiry is governed by Chrome's session rules; persistence does not force a
  site to keep you signed in forever.
- Explicit popups stack over one tile (up to four). Ordinary tabs have saved
  boards and a strip (up to sixteen grouped pages); the group and addresses
  return after a restart, not an unsaved form or unfinished popup. Address
  saves are debounced; session-only cookies still follow Chrome's rules.
- Security keys and passkeys are unavailable. Choose the site's password,
  code or another supported sign-in method. Stored passwords still need
  the normal approval and matching website.
- File names that look like paths must be renamed first. Transfers over
  4 MiB use 256 KiB chunks, bounded memory and picker/device-scoped tokens.
  Private upload copies are capped at 256 MiB per browser run and cleared at
  its next launch. Transfers keep at least 20 GiB of disk free and are refused if
  the free space cannot be checked. Abandoned pending batches expire after an hour. Accepted
  files stay until restart, since a site may read them later; Cancel stops an
  in-progress transfer, but cannot recall bytes the site already received.
- Copy/link context actions work in same-origin frames and open shadow roots.
  Cross-origin embedded pages remain isolated; use your own browser for their
  copy/link actions. Closed shadow roots are not inspected.
- On a phone this is desktop Chrome at a narrow size, not Android Chrome.
  Sites that explicitly require an Android browser may behave differently.
- Windows and macOS keep their existing Chrome headless launch mode; there
  is no fingerprint/UA rewrite there either. Their native-display identity
  and real Windows runtime are not qualified by this Linux change.
- Camera, microphone, browser extensions, your personal keyring and desktop
  integrations are not provided by this remote picture of a page.
- **There is no streamed sound.** The browser remains muted, so lectures and
  fullscreen video send pictures, not audio. **Audio / video in your browser**
  opens the address on your device to play it there (that separate profile
  may need its own sign-in). See [the audio design](WEBVIEW_AUDIO_DESIGN.md).
- Fullscreen inside the remote page does not turn this tile into an OS video
  player. Inline PDFs depend on Chrome's built-in viewer; downloading/opening
  the PDF on your device is the dependable alternative.
- Automation is not concealed. This is **not a way around CAPTCHA,
  Cloudflare, school device policies or other verification checks**. On a
  supported machine, use **Show on <computer>'s screen** and finish the check
  yourself in an ordinary visible window. A server without a desktop cannot
  show one; use your own browser instead. Agents must hand checks to you.

Chrome's sandbox and site isolation remain on. Pages cannot reach Tessera's
own port, read arbitrary host files, change its browser settings or inspect
cookies through the agent tools. Machine access, approval, pause/revocation
and Stop rules remain in force.

These behaviors were tested with throwaway local TLS websites and profiles,
not university accounts. Real Canvas/school SSO still needs verification
with your own account after installation.
