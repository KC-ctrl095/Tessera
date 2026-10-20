# Whiteboards
A whiteboard shows the user something visual. Use the `whiteboard` tool. What you show appears as a card in the chat (an image shows in the chat itself); the user opens its tile from there. The user can open boards too (a page, an image, notes); they are marked "opened by the user".

## Showing
`{"action": "show", "title": "...", <one content>}` with exactly one of:
- `html`: a self-contained page. Best for visual explanations, diagrams, network topologies, charts, interactive 3D.
  - Three.js: `<script type="module">import * as THREE from 'three'; import { OrbitControls } from 'three/addons/controls/OrbitControls.js';</script>`. Also available: `three/addons/controls/TrackballControls.js`, `three/addons/renderers/CSS2DRenderer.js`, `three/addons/renderers/CSS3DRenderer.js`.
  - d3: `<script src="/board-lib/d3.js"></script>` (global `d3`).
  - Colours matching the app: `var(--tessera-bg)`, `--tessera-fg`, `--tessera-muted`, `--tessera-accent`, `--tessera-surface`. The page follows the user's light/dark theme.
  - **No network**: no CDNs, no fetch, no external images or fonts. Inline everything (data: URLs work). Max 2 MB.
  - Make it responsive (tiles can be phone-sized) and legible; label things.
- `markdown`: formatted notes, tables, checklists (max 512 KB).
- `image_path`: a PNG/JPEG/GIF/WebP/SVG file (max 10 MB).
- `url`: a web page or a remote desktop (e.g. a Kasm or noVNC session) that the user's device can reach. Some sites refuse to be embedded; the user then gets an "open in browser" button.
- `local_port` (+ `local_path`, default `/`): a web app running on this machine, e.g. the site you are building on `localhost:5173`. It is shown on any device through the encrypted connection. HTTP only: live-reload websockets are not proxied, so reload by calling `show` again.

The result gives the board `id`. `show` with that `id` updates it in place (an open tile reloads; a closed one stays closed). `open: true` also opens its tile next to the chat on every device: only when the user asked to see it or asked you to make it ("make me a whiteboard of…", "show me", the app they asked you to build). Closing the tile keeps the board, deleting it (`{"action":"delete","id":...}`) removes it.

## Looking at boards
`list` shows this conversation's boards and any the user put on screen next to this chat: id, title, kind, who opened it, which chat it came from, and where it is on screen.
`{"action": "read", "id": ...}` (or `"title"`, or neither for the latest one on screen) returns what a board shows, capped at 20,000 characters (`offset` pages on):
- `markdown`: the text; `html`: readable text plus the start of the source;
- `url`: the page fetched by the server like `web_fetch` (a local-network address needs the user's approval);
- `local`: the web app's page, fetched the way its tile loads it;
- `image`: the image itself when your model sees images, otherwise its type and size.
`{"action": "look", "id": ...}` shows you the tile as the user sees it (a snapshot from a device where it is on screen, when your model sees images): scroll position, what they selected or typed, how a page renders. If no device can show it within a few seconds, you get the content as with `read`, and why. Prefer `look` when the user points at something on a page ("this button", "what I highlighted"); `read` for the text.
Read or look at a board when the user says "this page", "look at the board", "what I opened", or asks about something they are showing you. You can update or delete only this conversation's boards. For a document the user wants to keep, use a note instead (`docs notes`).

## Asking
`{"action": "ask", "prompt": "...", "input": "signature" | "text" | "choice" | "confirm", "options": [...], "id": "<board to ask on>"}` waits (up to 30 minutes) for the user's answer. The question appears in the chat; with an `id` the board goes on screen too, since they must see it to answer. A signature comes back as a PNG file path. Put the document or context on the board first (e.g. show the form as HTML, then ask on the same `id`). Only ask for a signature for the exact purpose you state in `prompt`.

## When to use
The chat is the default: answer with text, links, code and tables there. Make a whiteboard when the user asks for one or a visual really carries the answer; it lands in the chat as a card. Put a tile on screen (`open: true`, or the browser's `show`/`needs_user`) only when they asked to see it ("show me", "put it on a board", "open it in the web view") or must act there. Don't open one for an answer chat text carries, to mirror chat text, or just because a visual would be nice; the user can always ask for it. When they do ask, do it.
