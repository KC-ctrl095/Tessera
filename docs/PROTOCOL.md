# Tessera client protocol

All first-party clients (TUI, desktop, Android, web) speak the same protocol.
The Rust types in `crates/proto/src/lib.rs` are the source of truth; TypeScript
bindings are generated into `apps/tessera/ui/src/proto/` with:

```sh
TS_RS_EXPORT_DIR="$PWD/apps/tessera/ui/src/proto" cargo test -p tessera-proto --features ts
```

## Lightweight session catalog

`hello.session_list_pages` is true on servers supporting `list_session_page`.
If absent or false, use legacy `list_sessions` unchanged. Old clients still
receive complete SessionInfo rows, including task ledgers and idle job runs.

`list_session_page` takes `archived` (false by default), `limit` (100 by default,
clamped to 1–200), `before` (nullable SessionListCursor), and `include_ids` (open
tiles, first 128 IDs). It returns `{sessions, next_cursor}`. The cursor is
`{updated_at, id}` with a strict descending timestamp/ID keyset. A null cursor
means the catalog is complete. Cursors are not offsets or a frozen snapshot.
Live mutations arrive through existing session events. Do not replace newer
event metadata with an older in-flight page.

Rows retain SessionInfo list fields, including preview, model, usage, context,
bot/channel identity, unread sequence, pin, archive, revision and status. The
durable `task` ledger is absent. List rows must not erase a loaded chat's ledger.
Open the chat for full metadata and its newest history page. First pages also
include pinned, running/waiting, authorized private and explicitly open sessions,
even outside the recent window. Extras do not move the cursor. Later pages
contain ordinary chats/channels only. Idle helpers/jobs are not in the catalog.
Open chats supply children and the jobs view fetches run history on demand.
Explicit IDs obey the same private-device visibility rule. Private summaries
are memory-only.

The UI paints the first page and restored tiles before fetching the remaining
lightweight catalog in yielding pages. This preserves older-chat sidebar search,
bot profiles and the launcher without thousands of idle job runs. Archived
pages are requested on demand. Reconnect keeps rendered tiles, resyncs revision
cursors and refetches cheap metadata, not full ledgers.

`sessions.summary` and `sessions.list_kind` are maintained by triggers on every
persistent `info` write. Initialization backfills missing summaries. Private
sessions never enter this projection. The legacy full-row query stays intact.

## HTTP endpoints (server)

| Method | Path              | Auth | Purpose |
|--------|-------------------|------|---------|
| GET    | `/api/health`     | none | `{ "name": "tessera", "version": "…", "protocol": 1, "e2ee": true, "local": bool }`: used for endpoint probing; `local` says whether this request counts as made directly on the server's computer (see below; `tessera remote` uses it to catch proxies that don't say who connects). Reveals nothing else |
| POST   | `/api/pair/start` | none | Encrypted pairing, step 1 (see below) |
| POST   | `/api/pair/finish`| none | Encrypted pairing, step 2 |
| GET    | `/api/secure`     | inside | WebSocket for end-to-end encrypted sessions (apps, CLI) |
| POST   | `/api/pair`       | none | **Plain** pairing for the browser UI: `PairRequest { code, device_name }` → `PairResponse { token, device_id, server }` |
| GET    | `/api/ws`         | token | **Plain** WebSocket for the browser UI. Token via `Authorization: Bearer` or `?token=` |
| GET    | `/*`              | none | The web UI from `--web <dir>` (for a browser on the server itself). |
| GET    | `/web/…`          | none | "Use Tessera in a web browser" (off by default): the web app (`--web <dir>`, else the one built in), its API below. **404 for everything under `/web` while off.** |
| GET    | `/web/api/health` | none | As `/api/health`, for the web app |
| POST   | `/web/api/pair/start`, `/web/api/pair/finish` | none | Encrypted pairing for web browsers (see below) |
| GET    | `/web/api/secure` | inside | WebSocket for web browsers' end-to-end encrypted sessions |

Pairing codes are 8 digits, single-use, valid for 10 minutes, and only one is
active at a time. Plain endpoints are refused (403) for anything that did not
come directly from this machine, unless `[security] require_e2ee = false`.
"Directly" means a loopback peer, no forwarding headers (`Forwarded`,
`X-Forwarded-*`, `X-Real-IP`, `CF-Connecting-IP`, `CF-Ray`, `Via`), and a
loopback `Host`/`Origin`. A tunnel daemon on the same host therefore does not
count.

Custom headers configured on a client (for example Cloudflare Access
`CF-Access-Client-Id` / `CF-Access-Client-Secret`) are sent on every HTTP
request and WebSocket upgrade to an **`https`** address, never to a plain
`http` one (a home address, which on another network can be anyone's). The
server ignores unknown headers. Clients follow a redirect only to the same
host (http → https allowed, never the reverse), so those headers are never
sent anywhere else. A client tries its addresses in preference order and
moves on from one that answers but fails the handshake (its key doesn't
match) or refuses the device; it reports a failure only when every address
that answered failed (fatal only if all of them failed for good). A login
without a pinned key (from before end-to-end encryption) is only used with a
loopback address.

Every response carries `X-Content-Type-Options: nosniff`,
`X-Frame-Options: DENY` and `Referrer-Policy: no-referrer`. The web UI's HTML
also gets a `Content-Security-Policy`: scripts from the server itself plus its
own inline scripts (by SHA-256 hash), images only from the server, `data:` or
`blob:` (so model output can't leak data through an image URL), no plugins,
`<base>` or form submissions, and frames only from the server itself
(`frame-src 'self'`: its board and preview pages). At most 64 `/api/secure`
sockets may be in their unauthenticated handshake at once, 8 from one address
(for a loopback peer, the client a tunnel or proxy there names in
`CF-Connecting-IP`, else the last `X-Forwarded-For` entry, counts); the first
handshake message must arrive within 5 s and the whole handshake within 15 s;
more get 503. Request headers must arrive within 10 s (a kept-alive
connection idle that long closes), and the server keeps at most 512
connections open at once, 64 from one network address (loopback counts only
toward the total). Server errors say only "internal server error" (details go
to the log).

**`/web` ("Use Tessera in a web browser",** `[server] web_access`, Settings →
Connection → Web browsers): off by default, and while off every path under
`/web` answers 404 with no body, and paired browsers' connections are closed
(at the moment it is switched off) and refused. While on, it answers only over
https, as a tunnel or proxy on the server's own computer says (`X-Forwarded-Proto:
https` or `Cf-Visitor: {"scheme":"https"}` from a loopback peer; anything else
on the network could claim it), or to a browser on the server itself; plain
http from elsewhere gets 403 (a page that came over plain http could have been
changed on the way). Its API answers only requests from the app's own page:
`Origin` must be the host asked for (`Host`, or `X-Forwarded-Host` from a
loopback peer); browsers always send it with these. Everything under `/web`
also gets `Cross-Origin-Opener-Policy: same-origin`,
`Cross-Origin-Resource-Policy: same-origin`, a `Permissions-Policy` (camera
and microphone for the app only; no location, payment, USB, serial, HID,
Bluetooth, screen capture or security keys), `X-Robots-Tag: noindex`, HSTS
(over https) and `Cache-Control: no-store` on the API. The page's policy is
stricter than the local web UI's: `default-src 'none'`; scripts from the
server and the page's own inline script (by hash); `connect-src 'self'` plus
the page's own WebSocket address; no frames in or out (`frame-src 'none'`,
`frame-ancestors 'none'`); no workers, plugins, `<base>` or forms. Visual
whiteboards and HTML previews aren't shown there (their pages are served only
on the server's own computer, outside the end-to-end encryption). `/web`
(no slash) redirects to `/web/` while on. A server built with
`TESSERA_WEB_APP=<built web app>` (the installer and release script set it)
carries the app itself; `RemoteStatus.web.available` says whether there is
one.

Pairing limits cut both ways: while a code is active, anyone who can reach the
server can use up its 3 attempts or the network's 10-failure / 60-attempt
window and so delay pairing (never break into it). Pairing from the server's
own computer (a direct local request) has a window of its own, so it keeps
working. Existing devices are unaffected. Create a
new code, or restart the server to reset the window; put Cloudflare Access in
front of a public tunnel so strangers can't reach the endpoints at all.

## Rust client transport and request admission

Desktop/Android shells and the CLI use the Rust client. Its reader runs while
frames are being written. No inbound activity for 40 seconds closes the link
(regardless of outgoing progress). Foreground probes have a fixed 2.5-second
ceiling starting only after their Ping is flushed, or reconnect immediately
after more than 20 seconds without input. Ping/Pong controls pass between
complete upload frames on both plaintext and Noise links.
Only inbound activity renews read liveness. The independent write watchdog
closes after 40 seconds without successful nonzero socket writes below TLS.
Close interrupts both sides. These are local policies, not wire-format changes.

The client admits at most **128 outstanding requests** and **16 MiB of serialized
ordinary request bytes**, plus **one larger request** queued or in flight. The
per-request ceiling is derived from the server's shared **50 MiB raw attachment
limit** (25 MiB per file), base64 expansion plus 1 MiB for JSON/metadata, about
68 MiB serialized. The same derived ceiling bounds server plaintext and Noise
request reassembly. Large requests acquire their exclusive lease before a
shared count slot, separate from the ordinary 16 MiB pool, so waiting large
producers cannot exhaust ordinary count admission. Retained admitted request
data is bounded by that pool plus one maximum plaintext message. Noise encrypts
only the next chunk as its frame is sent, never a full second ciphertext copy.
The send adds at most one 65,535-byte ciphertext frame and a 65,519-byte temporary
plaintext scratch during sealing (about 128 KiB, plus socket/TLS buffers).
Caller-owned input still exists during JSON encoding and while admission waits,
so this is not a total-process peak-memory guarantee. A second large RPC waits
within its existing timeout, a second large post returns Busy.
One shared lease covers queue, disconnected work, active frame and an RPC
awaiting its reply. It is not counted twice on reconnect.
RPC admission backpressure, connection wait and reply share the caller's one
timeout. Cancelled or expired queued RPCs release their JSON and permits without
waiting for a blocked writer. Aborting an active partial frame drops the socket
rather than reusing corrupted framing. Shutdown clears unsent work and wakes
blocked admission. Caller-owned requests waiting to be admitted are not copied
into a second encoded client buffer. This bounds client-owned requests, not all
memory held by arbitrary external callers or protocol/TLS frame overhead.

Client Plain message/frame reception and Noise reassembly use that same request
ceiling plus a 4 MiB response-envelope allowance (75,147,948 bytes). Plain frame
reception matches the server's outgoing single-message frame; Noise remains
chunked. A maximum admitted content entry therefore fits as an echoed event.
Attachments that remain inline images are echoed as image content; ordinary
files have saved-file notes and downloadable file offers instead of base64 echoes.
Snapshots are measured before JSON encoding. When several large bodies would
exceed the response budget, older bodies become explicit "too large to show here"
display stubs saying the full message is kept on the server. The newest fitting
entry stays complete, queued display text is bounded too, and exceptionally long
histories keep a bounded tail. Sequence identities and durable server history
are unchanged, private content is never exported to disk by stubbing, and huge
session/approval metadata returns an API error instead of disconnecting clients.
Stubbed bodies cannot currently be fetched separately read-only (including private chats).
Bounded entry/range retrieval is a follow-up, not part of this protocol.

`Client::post` does not wait for space. It returns `Busy` if the count, ordinary
byte budget or exclusive large-message slot is full, `BadRequest` for a request
over the raw/serialized server limits and an error after Close.
Rejection means **not queued**. Callers must surface the error, never silently
drop a mutating request. An admitted post waits through reconnects until first
send, is never replayed after a partial/complete send, and has no delivery
acknowledgment; use an RPC when an answer or timeout is required. Shell login
retirement reports admission errors and keeps its token for a later admission.
## Work state and context (Claude/native parity)

`SessionInfo.revision` is the monotonic revision of the whole public/internal
session snapshot. `session_updated { session }` carries it in the session,
as do RPC info/snapshot responses. Older servers omit it (treat as 0), so
its TypeScript binding is optional. A server conditionally persists info and
internal state together and skips publishing an accepted older write once
the live revision has passed it. Revision watermarks belong to a connection
epoch: a new connection's authoritative bootstrap replaces cached info and
sets its baseline (0 when omitted), including a rollback to a legacy server.
Within that connection, clients ignore lower revisions in delayed events/RPC
responses; equal revisions apply. Prior-connection responses are rejected by
the generation guard. A stale helper-settings write changes nothing,
publishes nothing and returns `conflict` with a plain reload-and-retry message.

`SessionInfo.context` adds optional `source: "reported" | "estimated"`.
It labels **current context**, not cumulative billable usage. Older payloads
without `source` still decode (displayed as estimated). Claude's reported count
comes only from the documented correlated `get_context_usage` response
(`totalTokens`, `maxTokens`); unsupported/timed-out controls use an estimate.

`EntryBody::Compaction` adds optional/default-false `external`. `true` records
an engine-reported opaque boundary: `summary` is empty, `first_kept_seq` is 0
(no local history is hidden), and an unavailable token count is 0, not a claimed
exact reduction. The UI uses the same card/status as native but explicitly says
Claude's private summary is unavailable. Native entries retain `external=false`.

Claude links persist a default-empty ledger version (hash of the complete native
`TaskLedger`); changed/cleared work state is sent as user context, never by
rewriting a frozen system prompt. The task tool, `/goal`, criteria/evidence,
constraints and plan use the same protocol and storage as native. Legacy CLI
`TodoWrite`/`Task*` entries remain history only. No migration deletes old data.
The internal JSON key is `confirmed_ledger_version`: a successful input-channel
send is pending, not proof of receipt. A main-chat `isReplay` acceptance must
match the caller-supplied input UUID, exact role/content, and FIFO pending head
before both ledger version and resume-entry boundary advance. Synthetic, child,
foreign and duplicate replays do not advance them. Output after an unaccepted
input cannot move the resume boundary past it. Failed/untaken sends persist an
empty delivery version, so continue/restart sends the full current ledger plus
unseen work. Pre-fix `ledger_version` JSON values are deliberately not trusted;
old links retain their CLI session/context, but get one full ledger resend.
UUIDs stay in stdin/replay transport metadata, not prompt text. The SDK contract
supports them; real-CLI acceptance remains a parent-only live test.
Unchanged Claude Code work state is a one-line hash/version marker. Native
in-place checkpoint nudges do the same when their cached checkpoint already
holds that ledger; a replacement checkpoint still carries one bounded full copy.
`TaskLedger.archived` is optional (`{section: {count, artifact}}`), defaulting to
empty for older clients/ledgers. New ledger updates cap each live text section at
2,048 bytes. Whole older/pruned decisions/artifacts/notes and settled plan/criteria
items go to session-owned recall artifacts; oversized active requirements cause
an update error, never automatic removal/truncation. Legacy active state is kept
until explicitly revised. Archive JSON bundles contain `section`, `items` and
`previous_artifact`. No entries or
artifacts are deleted by pruning. Archive chains follow the existing recall
ownership/private-chat rules and are removed with their owning session.

## End-to-end encryption

Apps and the CLI encrypt everything to the server itself, above TLS, so a
proxy that terminates TLS (a Cloudflare tunnel, a reverse proxy, an
inspection box) only relays ciphertext. It can still see timing, message
sizes, and your Access headers. Implementation: `crates/secure`; types:
`tessera_proto::e2e`.

**Pairing** uses SPAKE2 (Ed25519 group, identities `tessera device` /
`tessera server`) with the 8 digits of the pairing code as the password:

1. `POST /api/pair/start { pake }`: the device's SPAKE2 message (base64).
   The server answers `{ pairing, pake }` for the active code, whether or not
   the device used the right one, and the same way when no code is active
   (the finish then fails as a wrong code).
2. Both sides derive `HKDF-SHA256(salt "tessera pairing v1")` →
   `device to server` / `server to device` keys (ChaCha20-Poly1305, random
   nonce prefix).
3. `POST /api/pair/finish { pairing, sealed }` where `sealed` = `PairHello {
   device_name, machine? }`. Opening it proves the device knew the code. The
   server replies `{ sealed }` = `PairGrant { token, device_id, server_key,
   server, machine? }`, and opening that proves the server knew it too.
   `machine: true` (sent by `tessera node enable '<link>'`) makes the token a
   machine login (see Machines), and the grant says so; servers from before
   ignore it and make an ordinary device.

**Web browsers** pair the same way through `/web/api/pair/start` and
`/web/api/pair/finish` (an attempt finishes through the door it started at),
with `PairHello.device_key`: the public half (base64, an uncompressed P-256
point, 65 bytes) of a key the browser made with Web Crypto as
non-extractable, kept in IndexedDB as a key object (no script can read it
out). Without one the finish is refused (400). The device is kept as a
browser (`DeviceInfo.browser`); its token works only at `/web/api/secure`,
together with a proof of the key (below), never at `/api/secure` or
`/api/ws`, and the apps' tokens don't work at `/web/api/secure`. The web door
has a budget of its own (10 failures / 60 attempts per 10 minutes) and one per
address (5 failures / 10 attempts), so strangers there can't hold up the apps'
pairing or each other.

Someone in the middle learns nothing from a transcript and gets exactly one
guess per attempt. The server cancels a code after 3 answers that failed
(each is counted before it is checked), and allows 10 failures and 60
attempts per 10 minutes, apart for direct local requests and the network. The device pins
`server_key` (X25519, 32 bytes). Its fingerprint (first 10 bytes of
SHA-256, hex) is printed by `tessera pair` / `tessera serve` and shown in
the app.

**Sessions** use Noise `NK_25519_ChaChaPoly_BLAKE2s`, prologue
`tessera/e2e/1`, against the pinned key over `GET /api/secure`:

1. Device → server: handshake message 1 (binary frame).
2. Server → device: handshake message 2. If the server can't read message 1
   (usually because the device pinned another key), it closes with code
   **4001**.
3. From then on every WebSocket binary frame is one Noise transport message
   (≤ 65 535 bytes). Its plaintext is `flag ‖ data`, where flag `1` ends a
   message and `0` means more follows. Reassembled messages are capped at
   64 MiB. Nonces are implicit counters, so any tampering, reordering or
   replay ends the connection.
4. The first message is `SessionAuth { token, proof? }`, sent after the
   handshake so the token has forward secrecy. A web browser adds `proof`:
   its key's signature (ECDSA P-256 with SHA-256, `r ‖ s`, base64) over
   `tessera browser login v1` (`e2e::BROWSER_PROOF`) followed by the Noise
   handshake hash, so a login works only with the key and only for that
   session. The reply is `SessionAuthReply { ok, error? }` (a browser's login
   without a valid proof gets the same refusal as an unknown token; while web
   access is off, every login at `/web` is refused with "Web browser access
   is turned off on this server.", and the page keeps its pairing). After
   that the connection carries the JSON messages below.

A client with a pinned key never falls back to plaintext. Text frames on an
encrypted connection end it.

## WebSocket framing

Text frames containing JSON.

Client → server (`ClientMessage`):

```json
{ "id": 12, "method": "send", "params": { "session_id": "…", "content": [{ "type": "text", "text": "hi" }] } }
```

Server → client (`ServerMessage`), one of:

```json
{ "id": 12, "result": null }
{ "id": 12, "error": { "code": "not_found", "message": "…" } }
{ "event": { "type": "entry_added", "session_id": "…", "entry": { … } } }
```

The first request on a connection should be `hello`. Every authenticated
connection receives **all** events (the harness is single-owner); clients filter
by `session_id`. A machine's connection (`tessera node`) receives none of them.

`stream_delta` events carry what arrived within about 40 ms of each other
(text or reasoning of one part, or one tool call's further arguments) in one
event; the first piece of a tool call, `stream_ended` and errors come at once,
and the order never changes. Clients append `text`/`arguments` as they come,
whatever their length.

A connection whose device reads slowly (a phone asleep with its connection
open, a slow link) is never waited for: once more than about 1 MB of events
waits to be sent to it, it misses events until it has caught up, and then
gets `resync` (see below). Replies to its requests always arrive.

`memory_report` → the server's account of its memory (the shape is the
server's own, read by `tessera doctor`): the process's private and resident
memory, the allocator's bytes in use and freed-but-held, events waiting per
connection, and the sizes of its caches and background commands.

## Resynchronisation

Persisted history (`Entry`) carries a per-session `seq`. Streaming deltas are
not persisted. After any reconnect a client calls
`get_session { session_id, since_seq: <last seq it has> }` for each open
session; the snapshot contains missed entries, the in-progress `streaming`
message (if a turn is running), pending `approvals`, the `queued` messages
and `children`: the subagent sessions the chat started, as they are now
(subagents aren't in `list_sessions`; older servers leave it out). Clients
replace what they know of the chat's subagents with it, so every device counts
the same ones, in their current state (one stopped by a restart isn't shown
running). Replacing local streaming state with the snapshot's is always correct.

Snapshots also include `history_revision` (default `0` when absent). A
conversation rewind increments it atomically with the history cut. Sequence
numbers can then be reused, so a reconnecting client must compare the revision
with its last **full** history snapshot, not just compare `last_seq`. If it
changed, discard incremental history and history-derived caches, and fetch
`get_session` without `since_seq`. Keep local drafts and pending sends. Older
clients ignore this additive field; older servers remain usable with revision
`0` and the existing shorter-history fallback.

### Bounded display history

New clients may open a large chat with
`get_session_page { session_id, before_seq?, limit? }` instead. This opt-in
request returns the same `SessionSnapshot` metadata and live state, but only
the newest display page strictly before `before_seq` (or the newest page
when it is absent). Entries are always in ascending sequence order. `limit`
defaults to 120 and is clamped to 1–200. Stored bodies have a soft 512 KiB
budget per page. At least one entry is returned when one exists, even if
that entry alone is larger. The existing overall response limit still applies.

A paged snapshot includes optional `history_before` when older rows remain.
Use it as the next exclusive `before_seq`, then prepend that page. The cursor
is based on stored rows, including hidden protocol entries, so an empty
visible page can still have a continuation. Stop only when the cursor is
absent. Never calculate the next cursor by subtracting a page length.
Concurrent appends do not change any earlier page boundary.

Compare each page's `history_revision` with the installed baseline. If it
changed, discard the page and open a new newest page. Ignore responses from
an earlier connection or baseline. Older pages only supply history and must
not replace current streaming, approvals, queues or session status. Ordinary
reconnects still use `get_session { since_seq }` to preserve the installed
prefix and retrieve all missed newer entries. A changed revision starts a
new paged baseline.

`get_session` is unchanged for older clients. `history_before` has a serde
default and is omitted on full/incremental snapshots. Older servers reject
the new request as an unknown method/variant. New clients then fall back to
`get_session` and treat the absent cursor as complete history. Paging only
bounds display work. Durable history, recall, forks and model context are
not shortened or rewritten.

## Session lifecycle

`idle → running → (awaiting_approval ↔ running) → idle`. `compacting` may occur
before or during a turn. If the server restarts mid-turn the session becomes
`interrupted`; sending a new message resumes it and any tool call whose outcome
is unknown is reported to the model as uncertain rather than silently retried.

## Tauri bridge (desktop + Android app)

The web UI inside the Tauri app does **not** open sockets itself. The Rust side
(`tessera-client`) owns connections so it can attach custom headers, switch
between LAN and remote endpoints, and reconnect in the background.

Commands (`invoke`):

| Command          | Args                                   | Returns |
|------------------|----------------------------------------|---------|
| `servers_get`    | –                                      | `ClientConfig` |
| `servers_save`   | `{ config: ClientConfig }`             | `null` |
| `server_pair`    | `{ endpoints: string[], headers: Record<string,string>, code: string, deviceName: string, name: string }` | `SavedServer` |
| `server_pair_link` | `{ link: string, deviceName: string }` | `SavedServer` (from a scanned `tessera://pair` QR code) |
| `server_connect` | `{ id: string }`                       | `null` (status arrives as events) |
| `server_disconnect` | –                                   | `null` |
| `rpc`            | `{ request: Request }`                 | result JSON or rejects with `ApiError` |
| `app_resumed`    | –                                      | `null`: check the connection now (call on `visibilitychange`, `focus`, `online`) |
| `device_report`  | –                                      | `{ computer, host, manufacturer, model }` (each `string \| null`): computers give their key (`tessera_secure::computer_key`) and name, phones their maker and model (Android system properties), for `device_hello` and the name suggested when pairing |
| `local_server_detect` | – (desktop only)                  | `{ available, running, url, saved }` |
| `local_server_start` | `{ autostart: boolean }` (desktop only) | server id: starts `tessera serve` (or installs the login service), gets a device token from the local store and connects |
| `notifications_get` | – | `NotificationsState` |
| `notifications_set` | `{ settings: NotificationSettings }` | `NotificationsState`; asks the OS permission when notifications are turned on (or never asked yet), offers the battery exemption when `keepAlive` becomes `"always"` (Android) |
| `notifications_request_permission` | – | `NotificationsState` (shows the OS prompt if it still can) |
| `notifications_test` | – | `NotificationsState`, after showing a sample notification; rejects if notifications are blocked |
| `notifications_seen` | `{ sessionId: string }` | `null`: the UI shows this session, withdraw its notification |
| `notifications_take_opened` | – | `string \| null`: session of a notification tap in the last minute (call once at startup) |
| `notifications_open_settings` | – (Android only) | `null`: the system notification settings for the app |
| `file_download` | `{ id: string, name: string, mime: string }` | `{ name, location, place }`: fetch a shared file (`read_shared_file`, 1 MiB pieces, 4 in flight; progress as `tessera://download { id, received, size }`) and save it: Android into Downloads (media store; Android 9 and older: the system "save as" screen), Linux behind the GTK/portal save dialog (rejects `cancelled`), other desktops into Downloads |
| `file_open` | `{ location: string, mime?: string, reveal?: boolean }` | `null`: open a file `file_download` saved (only those), or show it in its folder (desktop) |
| `battery_optimization_request` | – (Android only) | `NotificationsState`, after the system "run in background" dialog |
| `app_update_available` | – (desktop) | `bool`: this shell supports the installer flow (Windows x64 only) |
| `app_update_check` | – (desktop) | `{ serverId, build: AppUpdate \| null } \| null`: manual check through the current pinned/encrypted server; unavailable elsewhere |
| `app_update_install` | `{ serverId, id, sha256 }` (desktop) | `bool`: false if native consent is cancelled, true only once setup opens (not installation success). Confirms same offer/server, validates chunk/size/saved SHA-256 and PE/NSIS; no path/URL supplied by the UI |
| `local_server_restart` | – (desktop only) | `null` once the server on this computer is back (reinstalls the login service, or restarts the server this app started) |
| `local_server_keep_running` | – (desktop only) | `{ supported, installed, running, system }` from `tessera service status --json`: whether the server on this computer starts when the user signs in ("Keep Tessera running"; `system`: a Linux system service only root changes). An older `tessera`: all false |
| `local_server_set_keep_running` | `{ on: boolean }` (desktop only) | as `local_server_keep_running`, once the server answers again: `tessera service install --replace` (Linux: `--no-linger` too) or `uninstall` followed by starting the server for this session (it runs until the computer restarts). The server restarts once (cleanly: chats carry on) and the app reconnects |
| `board_frame_url` | `{ id: string, version: number }` | URL of an HTML whiteboard on its own origin: the `tessera-board` scheme on phones, a token-guarded loopback server on desktops (add `?theme=light` for light; keep the query) |
| `board_local_url` | `{ id: string, external?: boolean }` | `http://127.0.0.1:<port>/…?__tessera=<token>`: this device's proxy to a `local` board's web app; with `external` (opening it in the browser) the host is the proxy's own `<random>.localhost`, so it shares no cookies with other services on 127.0.0.1. The proxy answers only to those two names |
| `this_computer_status` | – (desktop only) | `{ node: NodeStatus \| null, appServer: string \| null, serverHere: boolean }`: this computer as a machine (`tessera node status`; `node` null without the `tessera` program). Starts the app's own `tessera node run` if it is on and nothing runs it |
| `this_computer_enable` | `{ on: boolean }` (desktop only) | as `this_computer_status`: `tessera node enable --yes` (without a `tessera connect` login, the node gets this app's pairing over stdin) or `disable` |
| `this_computer_scope` | `{ scope: MachineScope }` (desktop only) | as `this_computer_status`: `tessera node scope --set … --yes` (the page confirms widening first) |
| `this_computer_service` | `{ on: boolean }` (desktop only) | as `this_computer_status`: `tessera node service install --no-linger` / `uninstall` ("keep it on in the background") |
| `this_computer_log` | `{ limit: number }` (desktop only) | `NodeLogEntry[]`, newest first (`tessera node log`) |
| `this_computer_admin_password` | `{ password: string \| null }` (desktop only) | as `this_computer_status`: `tessera node admin password --stdin` (checked with `sudo`, kept on this computer only, owner-only; passed on stdin, never in arguments) or, with `null`, `tessera node admin forget`. `NodeStatus.admin_password` says whether one is saved |
| `this_computer_stop` | `{ stopped: boolean }` (desktop only) | as `this_computer_status`: `tessera node stop` (what the Stop bar does) or `resume` (only on this computer). `NodeStatus.stop_bar` (Windows, desktop control on): the Stop key, or why the bar couldn't start |
| `this_computer_name` | `{ name: string }` (desktop only) | as `this_computer_status`: `tessera node name` (what agents and approvals call this computer; 1 to 60 characters, one line) |
| `this_computer_list_dir` | `{ path: string }` (desktop only) | `DirEntry[]` of a folder on this computer (the folder picker for This computer) |
| `this_computer_home` | – (desktop only) | this computer's home folder |
| `clipboard_image` | – (desktop only) | `{ mime, data, width, height } \| null`: the clipboard's image as PNG (base64), read from GTK's clipboard on Linux (WebKitGTK gives pasted images to no page). The composer asks only while handling a paste |
| `clipboard_files` | – (desktop only) | `{ files: [{ name, mime, data }], skipped: [{ name, reason }] }`: the files copied in a file manager, read on Linux (WebKitGTK hands the page neither the files nor their paths): the local `file://` links of the clipboard's `text/uri-list`, else of `x-special/gnome-copied-files`; regular files only, at most 10 of up to 25 MB each (`data` base64). `reason` for those left out: `folder`, `too large`, `unreadable`, `limit`. Empty when the clipboard names no files. The composer asks only while handling a paste, before `clipboard_image` |
| `dropped_files` | – (desktop only) | as `clipboard_files`, for files dragged in from another app and dropped on the composer (WebKitGTK gives the page only their links; the shell notes the local links of the last drag over the window, for two minutes). Each drop is read once |

Events (`listen`):

| Event             | Payload |
|-------------------|---------|
| `tessera://event` | `Event` |
| `tessera://status`| `ConnectionStatus` |
| `tessera://open-session` | `{ sessionId: string }`: a notification was tapped |
| `tessera://servers` | –: the shell changed the saved servers itself (a server's `endpoints`/`headers` after `get_server_route`); read them again with `servers_get` |

Android also dispatches `tessera:content` on the page's `window` (a DOM
`CustomEvent`, not a Tauri event) with `detail: { items: [{ mime, data }] }`
(base64) when a keyboard inserts images (its clipboard, GIF or sticker
keyboards); the composer being typed in adds them as attachments. See
`vendor/README.md` for the web view patch this needs.

```ts
type SavedServer = {
  id: string; name: string;
  endpoints: string[];               // tried in order, e.g. ["http://192.168.1.10:7878", "https://tessera.example.com"]
  headers: Record<string, string>;   // sent with every request
  token: string;                     // always "" in the UI (kept on the Rust side)
  serverKey: string;                 // always "" in the UI (pinned on the Rust side)
  fingerprint?: string;              // e.g. "3f9a 1c2b 77e0 5d41 a0c9" (display only)
  deviceName: string;
  routeRev?: number;                 // times the server changed endpoints/headers; round-trip it as is:
                                     // a servers_save carrying an older count keeps the shell's addresses
};
type ClientConfig = { servers: SavedServer[]; active: string | null };
type ConnectionStatus = {
  state: "disconnected" | "connecting" | "connected" | "offline";
  serverId: string | null;
  endpoint: string | null;           // which endpoint is in use
  error: string | null;
  encrypted: boolean;                // end-to-end encrypted to the pinned key
};
```
```ts
type NotificationSettings = {
  enabled: boolean;                  // default true (the OS is only asked on first need)
  approvals: boolean; finished: boolean; subagents: boolean; jobs: boolean; errors: boolean;
  previews: boolean;                 // default false: no message or tool text
  keepAlive: "while_busy" | "always" | "off";   // Android; default "while_busy"
};
type NotificationsState = NotificationSettings & {
  permission: "granted" | "denied" | "prompt";
  batteryOptimized: boolean | null;  // Android only
  keepAliveAvailable: boolean;       // true on Android
};
```

**Notifications** are decided on the Rust side from the connection's events
and only posted while the app isn't in front (window focus on desktop, the
activity's pause/resume on Android): an approval is needed, a turn of 20 s
or more finished (after 2 s without a restart), subagents reported back, a
scheduled job finished or failed, an error. At most one per session every
10 s (a newer one replaces it). Text is generic unless `previews` is on;
private chats only ever show "Private chat"; on Android the lock screen gets
a generic public version. Session ids are checked (`[A-Za-z0-9_-]{1,64}`)
before they reach a notification or `tessera://open-session`, and Android
taps carry an HMAC-signed id in an immutable PendingIntent. On Android a
`remoteMessaging` foreground service keeps the connection while work runs
(and 2 minutes after), always, or never (`keepAlive`). Settings live in
`notifications.json` next to `servers.json` (0600).

`servers_save` can't change tokens or pinned keys: they are always taken from
the stored copy. For a server without a pinned key (paired before encryption)
changing its endpoints also drops the token, since it would otherwise be sent
to the new address.

The app's CSP allows no remote content (`img-src 'self' data: blob:`,
`connect-src` IPC only). The main window can't be navigated away from the app:
http(s) links open in the system browser (the opener capability is scoped to
`http`, `https` and `mailto`), anything else is refused. `servers.json` is
written owner-only (0600, directory 0700). On Android, backups and device
transfers are off (`allowBackup="false"` plus `data_extraction_rules.xml`), so
device tokens never leave the phone.

**Reconnecting.** The client probes every endpoint at once and takes the
first that answers; a more preferred one (listed earlier, e.g. the LAN
address) still wins if it answers within 400 ms. It pings every 15 s and
treats 40 s of silence as a dead link. When the app resumes (the Tauri
`Resumed` event or `app_resumed`), a link that has been silent for over
20 s is replaced at once, since the app was suspended. Otherwise it is pinged
and must answer within 2.5 s. A client waiting to retry reconnects
immediately instead of finishing its backoff.

**Running on this computer.** `local_server_start` only ever hands a token to
a loopback address. The token comes from `tessera device-token`, which reads
the local store directly: no pairing code, and nothing crosses the network.

In a plain browser (web UI served by `tessera serve`) the UI uses a direct
WebSocket to `/api/ws?token=…` on the same origin and stores the token in
`localStorage`. That is not end-to-end encrypted, so by default it only works
on the server itself. A browser can't be protected from the proxy that serves
it its code anyway: use the app when connecting over the network.

## Steering, queueing and goals

`send` with `delivery: steer` is read at the agent's next step (including
while it is writing its final answer: the turn takes one more step).
`follow_up` runs after the reply. `auto` starts a turn when idle and queues
the message (as a follow-up) otherwise. `interrupt` ("send now") stops the
running turn and starts the next one with the message, in one step: the
interrupted reply is kept and marked stopped (as with `abort`), the chat
stays busy in between, steering is read at the new turn's first step and
follow-ups stay queued. When idle it is `auto`; a subagent is steered
instead. `edit_queued { session_id, index, text }` replaces what the user
typed in a queued message, keeping its attachments (`text` may be empty when
it has some), `steer_queued { session_id, index }` turns it
into steering, and `interrupt_queued { session_id, index }` sends it now as
`interrupt` does. Waiting messages appear in `queued` (snapshot and
`queue_changed`) as `QueuedMessage { text, steer, interrupt?, attachments }`
(`text`: everything typed, in full; `attachments`: how many images and files
it carries; servers before batch 9 sent a one-line preview and no count), in the
order they reach the agent: being sent now (`interrupt: true`, until the
next turn has it), steering, then follow-ups. `dequeue { session_id, index }`
withdraws one. Each message leaves `queued` (a `queue_changed`) as it is
delivered. `abort` keeps the queue; what was being sent now goes first in
it. Messages sent during `compact` run once it is done. `set_goal`
stores the goal in the task ledger (kept verbatim through compaction) and
tells the agent: immediately when idle (a user entry with author `goal`),
otherwise at the next step.

Harness notes are user entries with author `harness` (clients may show them
as system messages): budget warnings and the `[Harness restarted]` recovery
note. Completion checks and acknowledgements are internal protocol: the
native check uses a `[Completion check]` user entry; Claude Code receives
it through Stop `additionalContext`. A text-only acknowledgement is stored
with assistant `author: "harness:completion"`. Neither check nor ack is sent
in snapshots or `entry_added`; they still consume sequence numbers, so gaps
are normal. Streaming holds acknowledgement-shaped prefixes until they
become real content. Clients also recognize the marker and a narrow legacy
fallback (bare ✓/✔/✅, or Done/Checked/OK with a mark), never a substantive
answer such as “✓ Tests passed”. Reports, previews, notifications, recall
and checkpoint summaries omit protocol too.

Ordinarily there is at most one check per turn. An acknowledged unchanged
open-state digest skips another check, but an own-helper result or restart
interruption of the chat/its child invalidates that acknowledgement. A value-free
persisted outstanding-Claude-dependency marker also expires an idle parent's mark
on restart when its background Bash has no child session. Unaffected idle chats
keep theirs. A tracked Claude task completion renews the next main Stop check;
an own native helper report does so only after exact main-chat UUID/content/FIFO
input acceptance, not saved/enqueued input. Duplicate source reports, invalid
acceptance echoes and duplicate/unknown notifications cannot renew it. Unaccepted
input still forces full ledger redelivery without advancing the confirmed resume
boundary. Only explicitly blocked plan items are exempt as waiting:
unchecked/unmet criteria are independent, and merely having a running helper does
not block other in-progress work. Rewinding also clears the saved digest.
No extra UI row or setting: the task ledger shows actual state changes.
`TaskLedger.criteria` holds a `CriterionState { status:
unchecked|met|unmet|unverifiable, evidence }` per `done_criteria` item (same
index), set by the agent's `task` `verify`.

The task tool accepts replacement `decisions`, `artifacts`, `notes` lists and
`remove: {decisions|artifacts|notes: [1-based indexes]}`; both archive removed
items. `[Environment update]` (author `harness`) appends current date/model
facts only on change, rather than editing the frozen prompt; Claude Code's
resumed input carries the same facts. Environment shown-state is invalidated
on rewind so a removed note cannot leave a stale model/date.

After a restart, sessions that were running are marked `interrupted` with a
notice. The serving process continues unattended ones by itself (kind `job`,
and chats in `auto` permission; config `agent.resume_on_restart`), telling
the agent which tool calls completed, are uncertain or never ran; others
wait for a message.

## Models, fallbacks and cost

Wherever a request takes a `ModelRef` (`create_session`, `update_session`,
`save_bot`, `save_job`, `set_default_model`) it may be a role, written
`@fast` (on the wire: `{"provider": "@", "model": "fast"}`). Sessions store
the concrete model the role named when they started; bots and jobs keep the
role. When a model still fails after retries and `[models.fallback]` names a
chain, the next model answers, a warning notice says so, and that turn's
assistant entries carry the fallback's `model`. `Usage.cost_usd` (optional)
is the estimated US-dollar cost billed for an entry, and for a session in
`SessionInfo.usage`: present only when the model is paid per token (an API
key, or a local model the user priced) and its price is known; requests
through a subscription (ChatGPT sign-in, Claude Code) have none. What they
would cost at API prices is in the usage stats (`api_cost_usd`).

Roles: `get_model_roles` → `ModelRoles { roles: [ModelRole] }`, with
`ModelRole { name, label, description, value?, resolved?, builtin }`.
Tessera's own roles come first: `default` (new chats), `utility` (titles,
summaries, page extracts; also called `fast`) and `memory` (the background
review that saves memories and skills; unset, it uses `@utility`, then the
chat's own model). The user's `[models]` roles follow. `value` is what the
role is set to (`provider/model` or `@other-role`); `resolved` is the model
it names now, fallbacks included (absent when it falls back to each chat's
own model). `set_model_role { role, model? }` sets a role, or clears it when
`model` is absent or empty, saves `config.toml` and returns `ModelRoles`
(`fast` sets `utility`; unknown providers and cycles are refused).

Fast mode: `ModelInfo.fast` says a model offers priority processing: by
default models of `openai_responses` and `openai_codex` providers
(`fast = true/false` on a `[[provider.models]]` entry overrides it).
`update_session { fast }` turns it on for a session (`SessionInfo.fast`;
subagents inherit it). Requests then carry `service_tier: "priority"`, and on
the ChatGPT backend also `x-codex-routing-hint: model=<model>;tier=priority`.
Other models ignore it. It uses more of the plan's credits or costs more per
token; `cost_usd` and `api_cost_usd` are still estimated at standard prices.

Daybreak Blue: `set_session_daybreak { session_id, enabled }` returns
`SessionInfo`. Its optional `daybreak` field defaults to false and is persisted
with the chat. Enabling requires current credential-scoped OpenAI discovery for
the chat's model. `ModelInfo.cyber_access_programs` is an optional array of known
selections from OpenAI's caller-specific `available_access_programs.cyber`
catalog metadata. Missing or empty means unknown or unavailable, never eligible.
The UI offers Blue only when both `standard` and `daybreak_blue` are listed and
the provider is authenticated. Red alone does not enable the Blue choice.

Native OpenAI Responses and ChatGPT Codex requests keep the selected model and
send `access_programs: { cyber: "daybreak_blue" }` on, or `standard` off when
advertised. Unknown catalogs preserve the existing request shape, matching
Codex. Opted-in requests recheck discovery and fail closed if support cannot be
confirmed. No request fields named `daybreakEnabled` or `cyberAccessProgram`
are forwarded to Responses. No Daybreak-specific header is added. The program
still requires OpenAI approval and server enforcement. Daybreak turns do not
automatically fall back to another account, model or provider on errors. If a
saved opt-in loses confirmed access, the Daybreak toggle stays hidden while a
separate **Use standard safeguards** action can clear the preference without
retrying any stopped request.

Only the exact official OpenAI Responses and ChatGPT Codex endpoints support
this feature. API-key program discovery follows the official Codex client by
querying the ChatGPT Codex model catalog with the API key and any documented
OpenAI project/organization routing headers. Arbitrary custom provider headers
are not forwarded to that other origin. Program evidence is scoped to the active
credentials and project configuration in memory and is not restored from saved
model lists.
Changing to an unsupported model clears the chat preference. Native helpers
inherit their parent's preference, but internal utility jobs start off.

Small model mode: `ModelInfo.small_mode` (`SmallMode`: `auto` | `on` |
`off`; absent = `auto`) is the user's setting for a model, and
`ModelInfo.small` (absent = false) whether new chats on it start in small
model mode now (`auto` worked out: on for models on the user's own computer or
network unless known to be large, for known small models, and for a model that
failed the one-time tool check; never for Claude Code). In it a chat gets a
short prompt, the core tools with flat `edit`/`task` arguments, worked
examples as its first messages and stricter loop limits; the agent turns the
other tools on with `docs` topic `tools` (see `docs/SMALL_MODELS.md`).
`set_model_small { provider, model, mode }` → null saves it as
`[tuning."provider/model"] small` (`auto` removes it) and broadcasts
`providers_changed`; chats already open keep their mode until their next
context checkpoint. A model that failed the tool check gets a `Warning`
notice in the chat that ran it.

Several subscriptions (batch 17): `add_account { kind, name }` → `ProviderInfo`
adds another ChatGPT (`openai_codex`) or Claude (`claude_code`) account with
the user's `name` (shown as "ChatGPT (Work)" / "Claude (Work)" unless it
names the service; ids `openai-codex-<name>`, `claude-code-<name>`, never
another's; 8 per kind). A ChatGPT account then signs in with `start_oauth
{ provider: <its id> }` like the usual one (its own login and models; a
notice warns when it is the same ChatGPT account as another entry). A Claude
account is signed in by the user through Claude Code itself, in its own
configuration folder (Claude Code's documented `CLAUDE_CONFIG_DIR`, a folder
per account next to Tessera's data, `<data dir>-claude/<id>`, made empty and
private by Tessera, which never reads or writes in it); `claude_code_status
{ refresh?, provider? }` → `ClaudeCodeInfo` for that account, with `provider`
and `sign_in` (the command to run on the server, e.g. `CLAUDE_CONFIG_DIR='…'
claude`); `start_oauth` for a Claude account answers with that hint as an
error. Turns, titles and usage of a Claude account run `claude` with its
folder; its conversations and model list are its own (`Link.provider`).
`rename_provider { id, name }` → `ProviderInfo` renames any provider (a
subscription's name says which service it is). A chat uses an account by its
model (`provider/model`); usage, `subscriptions` and plan limits are per
account. `get_accounts` / `set_accounts { continue_on_next }` →
`AccountSettings { continue_on_next }` (`[accounts]` in config.toml, off by
default): a chat whose account answers that its usage limit is reached moves
for good to the same model on the next account of that kind (in the user's
order) that is signed in to its plan (not a Claude account on an API key),
offers that model and hasn't reported its own limit reached, with a warning
notice. Older servers don't know these requests.

Local model servers that are off: `ProviderInfo.offline` (optional, absent
when false) marks an OpenAI-compatible server on this computer or the local
network (llama.cpp, Ollama, LM Studio…) that didn't answer the last time it
was asked (connection refused, no route, no answer within the time). Its
`models` are the ones it listed last (kept across restarts in
`model-lists.json`); clients show them marked "Offline — <host:port> isn't
answering" and don't offer them for a chat. The mark is set by model
discovery and by a turn that can't reach the server (its error then reads
`<name> isn't answering at <host:port>`, and the turn fails with "— start it,
or pick another model for this chat"), and cleared by any answer.
`list_providers { refresh: false }` while a server is offline makes the server
ask it again in the background (at most every 10 s); while clients are
connected it also asks every minute. When one answers, `providers_changed`
follows.

Context window of local models (batch 27): for a model on a server of the
user's own, `ModelInfo.context_window` is what the server runs it with, not
what it was trained for. llama.cpp: `/props` of a model already loaded (never
of one that isn't; a router would load it), else the router's `--ctx-size`.
Ollama: `/api/ps` (`context_length` of loaded models) and `/api/show` (a
model's own `num_ctx`). LM Studio: `/api/v1/models` (`loaded_instances`) or
`/api/v0/models` (`loaded_context_length`). Nothing here loads a model; hosted
APIs are not asked. A model that isn't loaded and has no `num_ctx` of its own
keeps Tessera's usual figure and gets no warning: Ollama's start-up value
depends on the computer's video memory and on `OLLAMA_CONTEXT_LENGTH`, which no
API shows, so it is not guessed. The figure is read again after a reply (at most
once a minute per model, beside the turn; `providers_changed` follows a
change), and what was read stays when the model is unloaded.
`ModelInfo.context_warning` (optional, absent when there is nothing to say) is
set when the window the server says is under 16,384 tokens: plain words that say
what the server runs and how to raise it in that server.
Not set when the user gave the model a `context_window` in `config.toml`. A
chat with such a model also gets one `Warning` notice with the same text
after its first answer.

Router safety: before every targeted llama.cpp `GET /props` Tessera refreshes
`/models` and requires that exact model to be loaded (explicit router states
must say `loaded`). The request also sets `autoload=false` to prevent a load
if status changes after the check. Primary-source proof, pinned at llama.cpp
[`8bb3cf0d216a5ae48360179f2931daf284be4d01`](https://github.com/ggml-org/llama.cpp/blob/8bb3cf0d216a5ae48360179f2931daf284be4d01/tools/server/server-models.cpp#L1923-L2034):
`get_router_props`, lines1995–2019, delegates model-targeted props to
`proxy_get`; lines1942–1948 parse the query's `autoload`, and1935–1937 reject
not-running models when false; lines2022–2033 call `ensure_model_ready` only
when true. `proxy_request` itself (1607–1614) rejects a model no longer running.
The [README lines1980–1986](https://github.com/ggml-org/llama.cpp/blob/8bb3cf0d216a5ae48360179f2931daf284be4d01/tools/server/README.md#L1980-L1986)
also defines per-request `autoload=false`. No real router was used to qualify
this: only primary-source inspection and a mock unloading A after its reply.

Provider HTTP traffic never follows redirects automatically, including discovery,
model lists, `/props`, context-window reads and completions. A redirect is refused
with its HTTP status and a request to set the provider's final address. Optional
reads log that error and keep the window unknown. Redirect URLs are not shown
because they may contain credentials.

Retrying a failing model server: a chat the user watches retries
`Transient`/`RateLimited` failures after 2, 4, 8, 16, 32 s (hosted APIs: 5
tries; a server on the user's network: 10, none paused longer than 60 s; with
a fallback model: 2) and says "retrying in Ns (attempt n/m)". Work nobody
watches keeps trying for 30 minutes with jittered pauses of 1–2 minutes: scheduled
jobs, helpers (within their own budget: a helper is stopped at its hard
deadline, 3 minutes after its time budget), bots' work and bot-to-bot
conversations, harness helpers, chats in YOLO mode, and chats whose user
wrote at least 15 minutes ago. User-driven rooms (`Channel` without `bot_id`)
are always watched, even in YOLO mode or after 15 minutes, so one failing
member does not hold the others up. Bot-to-bot channels stay unattended. A `Retry-After`
(seconds or a date, at most 10 minutes) is a floor; one longer than the patience
that is left ends it at once. One `Warning` notice when it starts, one every ten
minutes, a notification to the user's devices after five (chats only; jobs and
helpers have none, as for other long work), an `Info` notice when the server
answers again, and an `Error` ("kept trying for 30 min") if it never does. A
server on the user's own network that answers the same HTTP error (other than
503, 529 or 429, or a response reporting loading progress or overload) five times
in a row ends it too. Loading failures (such as "error loading model: out of
memory"), not just "model is loading", count towards that guard. A queued
message wakes one retry even if it arrived before the error response/pause;
Stop ends a wait at once.

Exception for explicit subscription-plan resets: only a 429 body with
`type: usage_limit_reached` and a reset timestamp/duration can use the separate
long wait (over 2 minutes through 6 hours, at most 3 waits, each reset plus 5 seconds). A
plain 429/Retry-After header, even 600/3600 seconds, uses the common patience
budget above and never claims a plan reset. Plan-reset pauses also wake on a
new queued message and cancel on Stop; helper hard deadlines still apply.

## Images

Models with `vision` get images: attachments, the `read` tool on image files,
board snapshots and image boards. On the OpenAI Responses API and the ChatGPT
backend they are sent as `input_image` data URLs (as the Codex CLI does), and
a ChatGPT model's vision follows its `input_modalities` in the model list
(images unless it says otherwise).

## Usage stats

The server records every model response: chat turns, subagents, the memory
review, titles, compaction summaries, scheduled jobs and helper calls such as
page extracts, with its time, session, model, kind and tokens (and estimated
cost when the price is known). Private chats are never recorded. Rows are kept
when their chat is deleted. On first start with this table, usage already
stored in chats is counted once (titles and summaries weren't stored, so only
turns).

`get_usage { from?, to?, utc_offsets? }` (unix ms; default the last 30 days
up to now; `utc_offsets: [UtcOffset { since, minutes }]` is the asking
device's time zone over the period, its offset from UTC in minutes east from
each instant on, oldest first, the first also standing for everything before
it: at most 400, ±18 h, in order, else `bad_request`) →
`UsageReport { from, to, total, by_model, by_kind, by_day, unpriced,
subscriptions }`:
- `UsageTotals { requests, input, output, cache_read, cache_write, cost_usd?,
  api_cost_usd? }`: `cost_usd` is what was billed per token (API keys, and
  local models the user priced), `api_cost_usd` what the same requests cost
  or would have cost at pay-per-token API prices, subscriptions included.
  Each sums the requests whose price is known and is absent when none is;
- `by_model: [ModelUsage { provider, model, totals, billing?, price?,
  price_source? }]`, largest first. `billing` is `api` (per token),
  `subscription` (a flat-fee plan: `openai-codex`, `claude-code`, or any
  provider given a plan) or `local` (an OpenAI-compatible server on this
  machine or network, or one without a key); absent when the provider was
  removed. `price` is `Price { input, output, cache_read?, cache_write? }`
  in US dollars per million tokens (cache prices default to the input
  price) and `price_source` is `custom` (set by the user), `builtin` or
  `catalog` (only in `by_model`);
- `by_kind: [KindUsage { kind, totals }]`, `kind` one of `chat`, `subagent`,
  `memory`, `title`, `compaction`, `job`, `other`;
- `by_day: [DayUsage { day: "YYYY-MM-DD", totals, models }]`, oldest first,
  only days with usage (`models` carry `billing` only). Days are the asking
  device's (by `utc_offsets`, summer time included: the apps send their zone
  since batch 17), else the server's own time zone (older apps);
- `unpriced`: `provider/model` of hosted models used in the period whose
  price is unknown (their requests count in no cost). Local servers are
  never listed: they cost nothing unless the user sets a price;
- `subscriptions: [SubscriptionInfo { provider, label, plan?, monthly_usd?,
  period_usd?, requests, api_cost_usd?, limits }]`: providers billed by plan that
  are signed in (Claude Code: it reports a sign-in), used in the period,
  have a plan set, or reported limits that haven't reset yet. `period_usd` is the monthly fee prorated over the period (months
  of 30.44 days); `api_cost_usd` is what the period's requests through it
  would have cost at API prices, so `api_cost_usd - period_usd` is what the
  plan saved. `limits` (absent from older servers) are the plan's usage
  limits as the provider last reported them along with a reply, never
  fetched on their own: `PlanLimit { window, label, used_percent?, status?,
  resets_at?, updated_at }`. `window` is `five_hour`, `weekly`, a model's
  own weekly limit (`weekly_opus`, `weekly_sonnet`…), `extra_usage`, or
  `<minutes>m` for other lengths; `label` says it in words ("5-hour limit",
  "Weekly limit, Opus"); `used_percent` is the share used (may pass 100;
  absent when only a status was given); `status` is `ok`, `warning` (close
  to it) or `reached`; `resets_at` and `updated_at` (when it was reported)
  are unix ms. The 5-hour and weekly limits stay listed after they reset
  (their `resets_at` then lies in the past: the numbers are from before);
  others only until they reset. ChatGPT reports them in the Codex
  backend's `x-codex-primary-*` and `x-codex-secondary-*` response headers,
  Claude Code in its `rate_limit_event` messages (`unifiedWindows` and the
  window limiting now). They are kept in `plan-limits.json` in the data
  directory.

Costs are computed when the report is made, from the recorded tokens and
today's prices, so a price set now applies to past requests too. A model's
price is, in order: `[prices]."provider/model"` (what `set_model_price`
writes), the model's `price` in `[[provider.models]]`, other `[prices]` keys
(model ids or `*` patterns), Tessera's built-in list of list prices (hosted
models only), then the public catalog (hosted models only). The catalog
(`[pricing] catalog`, default on; models.dev's `api.json`) is a plain
download that sends nothing about the user, fetched at most once a day and
only when a report has unpriced or catalog-priced models; its prices of a
few first-party providers are kept in `price-catalog.json` in the data
directory and used offline. When it is fetched for unpriced models,
`get_usage` waits up to about 4 seconds for it. A provider that reports its
own API-equivalent cost (Claude Code's `total_cost_usd`) is counted with
that figure unless the user set a price for the model; what Claude Code bills
per token (only when it runs on an API key) is the `cost_usd` it recorded.

`set_subscription { provider, plan?, monthly_usd? }` → null: the plan a
provider is billed by and its monthly fee in US dollars (0–100000; `plan` at
most 80 characters), saved as `[pricing.subscriptions.<provider>]`. On a
pay-per-token provider it marks it as billed by plan (a "coding plan" used
with an API key); both absent clears it. `set_model_price { provider, model,
price? }` → null: the user's price for a model (`Price`, every rate 0–100000),
saved as `[prices]."provider/model"`; absent clears it.

## Notes

The notes library holds markdown documents the user keeps, written by them or
by an agent (`note` tool). `Note { id, title, content, version, created_at,
updated_at, session_id?, by_user, agent_edit, edit_session?, all_bots, keep_from_agents }`:
`by_user` is who made it, `agent_edit` whether the latest change was an
agent's and `edit_session` the chat it was made in (never a private one).
`NoteInfo` is the note without `agent_edit`/`edit_session`, with a one-line
`preview` (≤ 140 characters) instead of `content`.
- `list_notes` → `[NoteInfo]`, newest first; `get_note { id }` → `Note`.
- Every agent and bot (also a bot with its own memory) reads every note,
  unless the user keeps it from agents (`keep_from_agents`): such a note is
  listed, read or changed only by the agent of a chat the user brought it
  into (a `[[Title]]` or `[[id]]` link, or a note's id, in a message the
  user sent there: "Ask about this" puts the id in the message box) and by a
  bot that has it as knowledge. `set_note_agents { id, keep_from_agents }` →
  `Note` sets it (the user's choice; no new version; broadcast as
  `note_updated`). Agents can't change it. Notes from before batch 17 are
  readable by every agent. `all_bots` is kept for older apps and is always
  `!keep_from_agents`; their `set_note_bots { id, all_bots }` means
  `set_note_agents { id, keep_from_agents: !all_bots }`. Servers before
  batch 17 send no `keep_from_agents` (and there `all_bots` meant: bots with
  their own memory may read it too).
- `save_note { id?, title, content, base_version?, session_id? }` → `Note`:
  no `id` creates one (`by_user: true`); with `id`, a `base_version` that is
  no longer current fails with `conflict` (fetch, merge, save again), unless
  the note already has exactly this title and content: then it comes back
  unchanged, with no new version and no event (so a save repeated after its
  answer was lost is harmless). Saves are applied one at a time. An empty
  title becomes the first line. Content is at most 200 KB. `session_id` names
  the chat it came from (never a private chat).
- Every client receives `note_updated` for its own saves too, often before
  the save's answer: a client recognises its own versions (the app compares
  what it sent) and merges a change from elsewhere that touches other lines.
- `note_history { id }` → `[NoteVersion]`, newest first, without `content`:
  versions kept from before a change, one whenever the writer switches
  between the user and an agent and otherwise one per 10 minutes of editing
  (at most 50 or 2 MB per note). `NoteVersion { version, title, content,
  updated_at, agent_edit, edit_session? }`. `get_note_version { id, version }`
  → `NoteVersion` with its content (the current version too). Restoring one
  is an ordinary `save_note`.
- `preview_page { html }` → `{ path }`: `/preview/<token>/` serves the HTML
  (≤ 512 KB) like an HTML board's page (no network, `board` CSP, only directly
  on the server's machine) for an hour, the newest 16 only. The browser UI
  frames it `sandbox="allow-scripts"` to preview a note's HTML code; the apps
  serve previews from their own board origin instead (`preview_frame_url`).
- `delete_note { id }` → null; its tiles and kept versions go too.
- Pictures: a note refers to one as `![alt](assets/<name>)` (`alt|240` asks
  for a width). `put_note_asset { mime, data }` (base64; PNG, JPEG, WebP or
  GIF by its bytes, ≤ 8 MB, all of them ≤ 2 GB) → `NoteAsset { name, mime,
  size, width, height }`; `name` is `<16 hex of its SHA-256>.<ext>`, so the
  same picture is kept once. `read_note_asset { name, offset?, len? }` →
  `FileChunk` (≤ 1 MiB a piece). There is no HTTP address for them: they
  come only over the device's own (encrypted) connection. A picture that no
  note's text and no kept version refers to is removed once it is a day old.
- `note_backlinks { id }` → `[NoteInfo]`, newest first: notes whose text has
  `[[its title]]` (any case; `[[title|shown as]]`, `[[title#heading]]`) or
  `[[its id]]`.
- Events: `note_updated { note: NoteInfo }`, `note_removed { id }`.
- Tiles: `MosaicTile.note_id`. A note an agent writes shows in the chat as a
  card (the call's `details.note { id, title, version, action }`); only with
  `show: true` does its tile go right after the chat's tile, as for boards.
- Clients render notes as sanitised markdown, never raw HTML. Agents read
  them as data: hidden characters are removed and instruction-like text is
  labelled (and the user told), as for skills and memory.

## Web search keys

`set_search_key { provider: "brave" | "tavily", key }` stores a search API
key with the provider credentials (empty `key` removes it) and returns
`WebSearchInfo`, whose `keys` lists the providers that have one. Keys are
write-only and never reach a model.

## Voice

See [VOICE.md](VOICE.md) for the design. `get_voice` / `save_voice { config,
api_key? }` read and change `VoiceConfig`: `engine` is where speech is handled
(`auto`, `device`, `server`, `openai`, `openrouter`, `service`, `off`), `server_model`
(`auto`/`tiny`/`base`) and `server_voice` pick the server's model and voice,
`keep_audio` keeps recordings. Read-only parts: `server` (`VoiceServer`: state
`ready`/`missing`/`installing`/`failed`/`unsupported`, bytes still to download,
progress, `listen`/`speak`/`live`, the voices offered), `openai_key`,
`openrouter_key`, `kept` and `kept_bytes`. `openrouter_voice` is a stock ElevenLabs
voice ID, default `sarah`. Both key fields are read-only booleans and never keys.
`enabled`…`has_key` describe "another transcription service"
(`api_key` is write-only, as before). An older app's `save_voice` (no
`engine`) switches that service on (`service`) or off (back to `auto`).

- `transcribe { audio, mime, engine?, language?, partial?, operation? }` → `Transcript { text,
  engine }`. WAV is what the server's engine reads (Opus/AAC only if the
  server has ffmpeg). `engine` may only be what the choice allows: the server's
  own engine wherever the server handles speech; `openai`/`service` only when
  chosen (`bad_request` otherwise, nothing sent). `partial: true` asks for the
  words so far: never kept, never sent to a paid service, and `busy` while the
  server's engine works on another recording. Optional `language` is a short
  device-language hint such as `en-US`, used only if no language is saved. It is
  normalized to the primary code, validated and never persisted. This skips
  local Whisper language detection when a device language is available.
- `speak { text ≤ 4,000 chars, engine?, operation? }` → `SpeechAudio { audio (base64),
  mime: audio/wav | audio/mpeg, ms }`: the server's voice, OpenAI's or
  OpenRouter's when explicitly chosen. OpenRouter uses ElevenLabs v4 Turbo MP3
  and GPT-4o mini transcription. No paid service receives partial transcription.
- `cancel_voice { operation }` → null. Optional operation ids (1–128 bytes)
  belong to the sending connection. Stop cancels its partial/final STT or TTS,
  dropping the future and cleaning the child tree and scratch without closing
  the chat socket; another connection cannot cancel it. Duplicate active ids
  are rejected. Older clients may omit the id (then only disconnect cancels).
- `install_voice` starts downloading what the chosen model and voice need
  (pinned releases, size and SHA-256 checked) and then checks that both
  engines run; follow it with `get_voice`. `not_found` from `transcribe` or
  `speak` means the server's engine isn't downloaded yet.
- `delete_voice_recordings` → `VoiceConfig`.
- `ServerInfo.voice` stays "the server can transcribe now" (older apps show
  their microphone by it); `ServerInfo.voice_engine` carries the choice.
- `openrouter` reuses an API-key credential only from a provider configured at
  the exact canonical HTTPS OpenRouter `/api/v1` address, or an eligible
  `OPENROUTER_API_KEY` saved in Passwords & keys. The dispatch address is fixed
  server-side, uses TLS verification, no redirects, bounded responses and the
  same per-operation cancellation. `save_voice` rejects it without a key.
  `openrouter_voice` accepts only the stock voice IDs listed in settings.
- A voice-off client can offer an inline explicit engine choice, then call
  `save_voice` and resume `start(true)`. Opening that choice must not open the
  microphone or send audio. An available key does not change Automatic.

## Terminal chats (lean)

`create_session { lean: true }` starts a terminal chat (`SessionInfo.lean`,
sent only when true; ignored for private and bot chats). Its model is offered
only the coding tools (`read`, `write`, `edit`, `bash`, `job`, `grep`, `glob`,
`task`, and `web_search`/`web_fetch` when they are on) and a short prompt:
no skills index, no contacts, and of memory only the user profile. Edits still
report a language server's diagnostics. The terminal app and `tessera run`
(unless `--all-tools`) set it; the apps don't, and their chats keep every
tool wherever they are opened. Forks and rewound copies keep the flag. Old
servers ignore the field; old clients never send it.

## Subagents

The `agent` tool runs subagents in the background unless `wait: true`.
`update_session { subagents: false }` switches subagents off for a chat
(`SessionInfo.subagents`, default true and sent only when false): its model
is no longer offered `agent` or `agents`, and its instructions don't mention
delegating. A model can only run the tools it was offered.

Children are sessions of kind `subagent` with `parent_id`, except those with
`SessionInfo.internal`: harness helpers such as the background memory review,
which clients never list (what the review learned still arrives in the chat
as a notice with `learned` items), and which search skips. Their
`SessionInfo.steps` counts tool calls, and the tool result's `details`
lists `{ name, session_id, background }`. In a chat run by Claude Code, both
its own `Agent` helpers and Tessera helpers started through `mcp__tessera__agents`
(`action: "start"`, the native `agent` task shape, background unless `wait: true`)
set `SessionInfo.parent_call` to the UI's tool-call id. Native Tessera helpers
now also set that link. Optional `SessionInfo.helper_owner` is `"tessera"` or
`"claude_code"` (serde default: absent). This is ownership, not the provider:
Tessera-owned helpers, including bridged ones on any model, accept normal
messages and continuation. CLI-owned mirrors are watch-only (`send` is refused);
`abort` stops that helper alone through the CLI's task-stop control. Older
sessions with a call link and no owner remain conservatively watch-only in
clients. Stopping the chat stops its helpers, and deleting it removes them. The result's `details`
has the same `subagents` list (native Claude helpers also have `subagent`, the
card of older clients). Bridged helper usage contributes to the parent's
counter and is logged once under the helper's own model. Private Claude Code
chats have no bridge.

The native `agent` and bridge `agents` task objects accept optional `cwd`:
an existing server folder (relative to the chat cwd or absolute). Canonical
paths must remain inside the chat cwd or its configured allowed folder scope;
read-only folder ceilings and protected paths are retained. Worktree isolation
uses that folder's repository/subdirectory and checks the repository root too.
This never grants new machine permissions or selects a remote-machine cwd.

Shared display-only presentation maps known CLI tool names and argument keys
(e.g. `Bash`/`command`, `Read`/`file_path`) to native kinds/inputs. Unknown and
external MCP identities stay raw. Persisted `AssistantPart::ToolCall.name` and
arguments remain the execution/audit identity, never canonical dispatch input.
`ToolResult.details.duration_ms` is an observed execution interval or a reported
CLI interval, not inferred entry spacing; absent remains unknown. Existing
`diff`, `path`, `exit_code` and `interrupted` detail keys drive the same app and
terminal cards. Successful bookkeeping is folded/quiet, never deleted from
the transcript or expanded audit; pending, approval and failures stay visible.
Native background commands also return optional `ToolResult.details.job_id`
and `background: true`, both for an explicitly backgrounded command and one
that continues after its foreground timeout. The id belongs to Tessera's
`job` namespace; CLI `task_id` values are not mapped to it. This metadata does
not invent an exit code or completion for a still-running command.
`SessionInfo.role` says what a subagent is for: the agent definition it was
started with (`explore`, `review`…), or Claude Code's subagent type
(`Explore`, `Plan`, a custom agent; `general-purpose` when it gave none).
Clients show it next to the name. In a Claude Code chat its task tools
(`TaskCreate`, `TaskUpdate`; `TodoWrite` in older versions) keep
`SessionInfo.task.plan`, as Tessera's `task` tool does (the goal stays), and its
`AskUserQuestion` comes as asks on one `markdown` board of the chat: each
question a `choice` (plus "Something else…", then a `text` ask), the answers
going back to Claude Code. Each subagent has a budget
(`agent.subagent_steps`, default 40 tool calls; `agent.subagent_minutes`,
default 15). When it runs out, the subagent is asked for its report with no
tools offered, and it is stopped three minutes later. A report is delivered
to the parent as a user entry with author `subagent:<child id>`, or as
steering if the parent is busy. The parent resumes on its own once a batch
is complete. `abort` on the parent also stops its foreground, waiting and
background subagents, with no automatic follow-up. When helpers are still
working, the parent gets an info notice naming those being stopped before
cancellation; Claude Code's watch-only helpers stop with their parent too
(in a private chat the notice stays only in that chat). Output from a
subagent that was stopped mid-work is labelled as notes, never passed off
as a report. A cancelled helper waiting for a slot becomes `interrupted`,
not a waiting helper that could still start.

Helper limits: one call may start up to 32 helpers. Each chat may choose its
own helpers at once, tool-call budget and minute budget in Chat options →
Helpers. `set_chat_helpers { session_id, limits: { at_once?, steps?, minutes? } }`
→ `SessionInfo`; fields left out follow the server defaults, 0 means no limit.
Whole numbers up to the wire's u32 range are accepted, without preset buttons.
The optional `SessionInfo.helper_limits` survives restarts; an empty object
clears it. Helpers cannot set their own limits or start more helpers.

`get_helpers` → `HelperSettings { enabled?, at_once, default, max, steps?, minutes?,
capacity? }`; `set_helpers { at_once, enabled?, steps?, minutes? }` → `HelperSettings`
sets the defaults for chats without their own choice. All three allow 0 for
no limit. Saved in `[agent] max_subagents`, `subagent_steps`, `subagent_minutes`.
Legacy config `max_subagents = 0` reports `enabled:false` and remains disabled
when any defaults are round-tripped, including by older clients. Without the
new `enabled` field, a request's at_once=0 keeps its legacy disabled meaning.
Explicit `enabled:true, at_once:0` means unlimited and saves 4294967295;
`enabled:false` saves 0. The UI has a distinct Allow helpers checkbox.
Old clients' `set_helpers { at_once }` preserves the other defaults; old
servers omit the additive fields. Settings → Models uses number fields and
explicit No limit checkboxes, with a warning about plan usage/cost.

Concurrency changes wake waiting helpers at once; lowering never stops ones
already working. Server-disabled overrides even explicit chat limits and is
rechecked after lane/server waits at the start boundary. Step/time budgets
are captured when each run starts, not while waiting. Each call reserves one
remaining finite step atomically (parallel batches too). Consumption occurs
at release: native tool.run or Claude hook/permission allow. Over-budget
calls report “Budget exhausted; not executed” and keep the final-report turn.
No limit reserves no finite steps. Claude hooks/callbacks/bridge dispatch
share one claim per exact call id/name/input; proposed calls alone consume
nothing. Refund only on trusted Tessera denial of that exact call id or when
never released. An independent CLI self-refusal stays consumed, even with
"not performed" result text; execution can produce the same shape. Restart
returns only never-released claims; released-but-unconfirmed units remain consumed.
Monotonic per-session revisions always persist complete public and internal
snapshots together, with no unchanged-state/NULL coalesce shortcut; stale
writes nothing and emits no event. A losing UI settings write gets a plain
reload-and-retry conflict.
AskUserQuestion requires a nonempty call id; duplicate callbacks share one
question/outcome and cancel independently. Its shared owner validates/commits
the live claim while queueing the affirmative reply; release succeeds only
if at least one callback queues it. All failed/cancelled callbacks return a
never-released claim. Roles intersect current user ceilings at admission.
Automatic restarts and intermediate report/format retries retain accounting,
input hashes and the deadline; terminal run completion clears call records,
keeps the aggregate, and rejects late callbacks. Explicit continuation starts
a fresh allowance. Unfinished helper roster slots are reserved atomically
before session creation and released on failed creation or completion. The server's own
capacity (memory and processors, or `agent.helper_capacity`) applies across
all chats; low memory holds new helpers. Single-slot providers keep a lane
so only one helper requests their model at a time. Waiting helpers send
`SessionInfo.helper_queued: true` with status `running` (old apps can decode
and stop them); it clears when they start or stop. No-limit choices don't
bypass capacity, memory protection, provider lanes or the loop detector.
The `settings` tool offers `chat.helpers`, `chat.helper_steps`, and
`chat.helper_minutes` only for this chat; 0 / `"no limit"` removes a limit,
null restores the server default. Changes require user approval and the
agent must only request them when the user asks.

Other limits:
- 64 unfinished background subagents per conversation, 256 per server.
- Every task's model is checked before any subagent starts, so one bad task
  can't leave half a batch running.
- Reports share a budget of about 64k characters per batch: each gets 4k–24k
  and a longer one is cut at a paragraph break. The agent can read the whole
  report with `agents { action: "report", name }`, which only works for that
  conversation's own subagents.

Subagents v2 (all model-facing; clients only see the resulting sessions and
entries):
- **Agent types.** A task's `agent` names a definition: built-in `explore`,
  `review`, `research`, or a Markdown file in `~/.config/tessera/agents/` or
  a project's `.tessera/agents/` (frontmatter `name`, `description`,
  `tools`, `model`, `readonly`, `network`, `max_steps`, `max_minutes`; the body is added
  to the child's system prompt). Every definition's budget is clamped to
  the configured helper limits, again at launch/continuation. Project
  definitions can only narrow tools and permission, never grant network.
  Unknown names are refused before anything starts. The settings folder
  stays closed to tools: `agents { action: "types", name? }` lists the types
  available (tools, model, read-only, effective budget and network; one with
  its instructions) and `agents { action: "define", name, text }` saves one
  of the user's types (`write:agents`; refused in read-only chats and to
  helpers). Default/Ask approval cards show the full file, effective budget
  and network grant. Auto/YOLO saves without a card, like `skill_manage`.
- **Read-only helpers' commands.** A read-only helper gets `bash_readonly`
  instead of `bash`/`job`: no stored secrets, nothing in the background,
  build output and temporary files in a private scratch folder (removed
  when it finishes). Where the chat's sandbox is on and can confine writes
  (Landlock ABI 2+), any command runs with writes allowed only there and
  without the network unless the helper's type (the user's or a built-in
  one) says `network: true`; elsewhere only git reads, searches and listed
  cargo commands run (`policy::review_command`). On a server without that
  sandbox, these cargo commands run the project's own build scripts and
  tests unconfined: they can change files, read user-accessible files and
  use the network regardless of `network: false`/`CARGO_NET_OFFLINE`.
- **Structured output.** A task's `output` (JSON schema or format text)
  makes the report compact JSON: checked, with one retry, then passed through
  as text with a note. JSON reports are never shortened.
- **Isolation.** `isolation: "worktree"` runs the child in a detached git
  checkout under `.git/tessera/worktrees/<child id>` (its `SessionInfo.cwd`
  points there while it runs). Its changes become a patch artifact; the
  report's header line summarises them; `agents { action: "apply" | "discard",
  name }` applies (all or nothing, refused on conflicts) or drops them.
  Refused outside git repositories. Leftover checkouts are saved and removed
  at startup.
- **Continue.** `agents { action: "message", name, text }` appends `text`
  to a finished child's history and runs it again in the background with a
  fresh budget; the new report arrives like the first.

## Rewind and the session tree

`rewind { session_id, to_seq, files, conversation, dry_run }` →
`RewindResult`. `to_seq` is the last entry kept: to undo a user message and
everything after it, pass its `seq − 1`. If `to_seq` falls between an
assistant message and its tool results, it is moved back to before that
message (the result's `to_seq` says where it went). At least one of `files`
and `conversation` must be set; `dry_run` changes nothing and returns what
would happen (show it before asking the user to confirm). Refused with
`busy` while the session or any of its background subagents is running, and
for private chats and subagent sessions.

- **Files.** Every file changed after `to_seq` by `write`/`edit`/`lsp rename` (in the
  session or its non-isolated subagents) or by `agents apply` is put back as
  it was (`restored`). Files created after `to_seq` are deleted (`deleted`)
  only if they are unchanged since the agent last wrote them; otherwise they
  are left alone and listed in `skipped` with a reason. A restored file that
  also had later changes by other means is still restored, with a line in
  `notes`. **Changes made by shell commands are not undone**: those commands
  are listed in `not_undone`. When only files are rewound, the agent gets a
  user entry with author `harness` saying what was undone.
- **Conversation.** The session keeps its id; entries after `to_seq` are
  removed (`removed_entries`) and new ones continue from `to_seq + 1`. The
  full pre-rewind history, with its file snapshots, is first copied to a new
  chat titled "<title> (rewound)" (`branch`). Connected clients get
  `session_rewound { session_id, to_seq }` (drop local entries after
  `to_seq`, then refetch with `get_session`), followed by `session_updated`.

Session tree: `fork_session` copies (and `rewind` branches) are `chat`
sessions whose `parent_id` is the session they came from, so `list_sessions`
is enough to draw the tree (subagents, `kind: subagent`, are not listed; a
`parent_id` that isn't in the list means the parent was deleted). Clients
showing a chat's subagents must filter children by `kind == subagent`.

## Private chats

`create_session { private: true }` makes a chat that lives only in the
server's memory. The store keeps it outside SQLite, so it never reaches the
database, write-ahead log, FTS index, artifacts or tool-run records.
- Tools: `web_search` and `web_fetch` only.
- Prompt: minimal, with no memory, profile, skills or project context.
- No memory review.
- Titled locally from the first message.
- Summarised only by its own model, never the utility model.
- Can't be forked.
- `delete_session` wipes its text from memory and removes it. A server restart
  also discards it.
- Only the device that made it sees it (since batch 17; the server enforces
  it, by the connection's login): no other device, web browser or terminal
  app gets it in `list_sessions`, any event about it (`session_updated`,
  entries, stream, approvals, notices, `notification`s, web views, files) or
  its notifications in `list_notifications` / `pending_notifications`, and
  every request naming it (or its boards, files, approvals) fails with
  `not_found`, as for a chat that doesn't exist. The same device reconnecting
  keeps it; `revoke_device` (signing that device out) deletes its private
  chats at once. A session started under a private chat is private too, and
  that device's. The server itself (no device) sees all.

Clients must not persist anything from private chats.

## Secrets and logins

Settings → Passwords & keys in the apps.
`save_secret { name, kind, description, fields, remove_fields, ask, hosts,
bots?, on_server?, on_machines?, new_name?, machines_default? }` creates or edits a `token` (one
value, `$NAME`), a `login` or a `custom` set of fields, and returns its
`SecretInfo`. Fields become `$NAME_FIELD`, e.g. `$SHOP_USERNAME`,
`$SHOP_PASSWORD`. A `totp` field holds a base32 seed and provides
`$NAME_OTP`, the current 6-digit code; the seed itself is never passed to
commands. Values are write-only: an empty value keeps what is stored, a new
one replaces it, `remove_fields` drops fields. `bots`, `on_server` and
`on_machines` left out keep what is set (a new secret: any chat, this server
only); `bots: null` makes it any chat's again. `new_name` renames it, keeping
its values and settings (refused when taken, or when a field would then set a
reserved variable); agents' saved commands that use the old name stop getting
it. `list_secrets` returns `SecretInfo` { `name`, `updated_at`, `kind`,
`description`, `fields` (names), `ask`, `hosts`, `last_used`?, `bots`?,
`on_server`, `on_machines`, `machines_default` }. No request or event ever carries a value, its
length or a masked form. `delete_secret { name }` removes one; `set_secret
{ name, value }` is the old way to store a token.
The `description` is the title the apps show. `set_secret_title { name,
title }` → `SecretInfo` changes only that. `suggest_secret_titles` →
`[SecretTitle { name, title, suggested }]` lists the ones whose titles say
little (empty, the name itself, "saved from a chat", "…, typed in a chat")
with a clearer one made from the name, kind, sites and machine names (never a
value; no model involved), for the user to apply. New ones get a clear title
when saved: a known key format's label ("GitHub token"), the agent's `title`
in `save_secret` (refused when it holds one of the values), else one made the
same way.
- Who can use it (`bots`): absent = any chat, bot chats included; a list of bot
  ids = only sessions acting as one of those bots (an empty list: none). A
  session acts as its `bot_id`, in a channel as the bot speaking; subagents
  carry their root's bot (`bot_id` is copied when they start) and a bot's
  jobs run as the bot. Private chats and helper sessions never get a secret.
  For any other session a secret it may not use doesn't exist: it isn't in its
  instructions, a command naming it gets nothing, and a value typed into it
  becomes `[a stored secret]`. Removing a bot removes it from every list.
  `tessera_proto::bot_allowed` is the shared rule (machines use it too).
- Where (`on_server`, `on_machines`, `machines_default`): `on_server` covers
  `bash` and the browser on this server. Machines: with `machines_default`
  the secret follows the setting for all passwords (`SecurityInfo.
  secret_machines`: `off` none, `picked` the ids in `secret_machines_picked`,
  `all` every machine, also later ones); without it, exactly the machine ids
  in `on_machines`. Stored secrets without the flag follow the setting unless
  they named machines (so `off`, the default, changes nothing). `save_secret`
  with `machines_default` switches; without it a non-empty `on_machines`
  (an older app) makes them its own, and leaving `on_machines` out keeps
  everything. A command naming a secret this session may use, but not there,
  is refused with where it does work and that the user can allow it in
  Settings → Passwords & keys. The machine's own rules (its folders, commands,
  who may use it) still decide what runs there: the server checks them before
  anything is sent and the machine again. Values go only in that one
  command's `node_call` (`NodeCall.env`, over the end-to-end channel), are
  never logged on either side, and both sides remove them from output.
- Enforcement: on the server, when a tool asks for a command's environment
  (`ToolHost::secret_env(session_id, command, place)`), for every engine,
  including tools a Claude Code chat calls through Tessera.
- Least privilege: a command receives only the variables it names (`$VAR` or
  `${VAR}`).
- Approval: a secret for any chat with `ask` (the default) asks before the
  first command in each session that uses it (not in auto mode for `bash`).
  The bots a secret is for never ask for it. Asking a bot that may use
  secrets the caller can't (`message_bot`) asks first; a session can't change
  or delete another bot's job with `schedule`.
- Redaction: every secret value (a token, a password, a one-time code's
  seed, any field but `username`, `email` and a plain `url`) of 4 characters
  or more is removed from all output that could reach a model, whoever may
  use it: values under 8 characters only as whole tokens (never inside a
  longer word or number), longer ones wherever they appear. So are the
  base64, hex and URL encodings of values of 8 characters or more, and the
  HTTP basic-auth form of a login's username (or email) and password.
  Emails and distinctive usernames (6 characters or more with a digit or
  symbol, not this computer's own user or host name) are removed too;
  usernames of letters only (`root`, `admin`, a first name) and plain
  sign-in addresses are not secrets and stay readable.
- The agent's instructions list the name, kind, variables, description, hosts
  and (when not just this server) machines of each secret it may use, never
  values; descriptions are redacted first.
- Reserved names: a secret or login field can't produce a variable that
  changes how programs run (`PATH`, `LD_*`, `BASH_ENV`, `NODE_OPTIONS`…).
- Stored in `auth.json` (0600, made owner-only again at start), secrets saved
  before `bots`/`on_server`/`on_machines` existed read as any chat, this server.

`save_chat_secret { session_id, call_id, fields: SecretEnv, chat_only?: false }`
→ null is app-only secure entry for a completed Claude `save_secret` tool card.
The card's `details.secret_entry` holds name/title/kind/url and, once saved,
`saved: true`, never values. The bridge schema accepts only those metadata
fields and action (`request`/`list`). The request verifies that card, live
capabilities and saving policy, refuses used cards/taken names/Claude sign-in
credentials, and writes the vault directly, not model messages. `fields` Debug
is redacted like machine command environments. An `entries_changed` updates
the card. Card-only saves confirm "Saved securely in Tessera." and keep Claude's
safe resume link and warm process. If transcript redaction finds earlier values,
a warning explains that plaintext remains in Claude's history (never edited by
Tessera), drops the resume link and reconstructs sanitized history next turn.
Bridge schemas and credential names refresh between turns; card-only entry does
not replace the frozen appended context. Older apps can use Settings →
Passwords & keys instead; existing requests/events remain unchanged.
Secrets typed into a chat (with `save_secrets_from_chat`, the default):
- `send` and `edit_queued`: values in a known key format are saved as tokens
  (named after what they are, e.g. `ANTHROPIC_API_KEY`, `_2` when taken) and
  replaced by `$NAME` before the message is stored, queued or sent anywhere;
  stored values typed again are replaced too. A Tessera device token is only
  removed. Private chats save nothing (the value becomes `[removed: …]`).
- The agent's `save_secret` tool saves what the user typed (only values that
  appear in the user's own messages of that chat, never over an existing
  secret) and rewrites every stored entry of the chat holding a value
  (messages, tool calls and results, reasoning, summaries; reasoning signed by
  a provider when the turn ends). Clients get `entries_changed { session_id,
  entries }`: replace the shown entries with those `seq`s.
- Either way the chat gets an info notice starting
  `Saved to Passwords & keys: NAME (what)…` that names the variables
  (`$NAME`…), never values.
- Where they go follows `secret_saving` (`SecretSaving`): `auto` as above;
  `ask` and `chat_only` keep new ones for that chat alone
  (`SecretInfo.chat` = its id; only that chat and its subagents may use
  them, other chats get `[a stored secret]` when they type the value). Their
  notice starts `Kept for this chat only: NAME (what)…` (same shape as the
  saved one). With `ask` each one (not one-time codes) gets its own notice
  instead, waiting for the user: `Save to Passwords & keys? NAME (what) was
  taken out of your message and is kept for this chat only until you
  decide; the agent uses it as $VAR without seeing the value.` Clients show
  it as a card and answer with `decide_chat_secret { session_id, name,
  choice: "save" | "chat_only" | "forget" }` → null: `save` makes it an
  ordinary secret (any chat, asking first), `chat_only` keeps it as is,
  `forget` deletes it. The notice is rewritten (`entries_changed`) to the
  saved or kept notice, or `Forgot NAME (what): …`. `delete_secret` on a
  kept one is `forget`; `save_secret` can't rename a kept one. Deleting the
  chat deletes what it kept. With `ask` the agent's `save_secret` asks for
  approval (always, even in auto mode; no values in the request); denied, it
  keeps the login for the chat only. With `chat_only` it keeps it for the
  chat without asking.

## Security settings

`get_security` → `SecurityInfo`: the sandbox mode (`auto`, `strict`, `off`),
its status line, whether it is complete and a hint when not (or when the
server runs as root), Tessera's protected folders, the folder rules
(`{ path, write }`), `ask_outside`, `redact_patterns`, `require_e2ee` and
`save_secrets_from_chat` and `secret_saving` (left out by older servers:
read it as `auto`), `secret_machines` (`off` | `picked` | `all`, default
`off`) with `secret_machines_picked` (machine ids), and `bot_handoff` (`on` |
`ask` | `off`, default `on`; both left out by older servers), and
`default_permission`: how much freedom new chats get, one value for all the
user's devices (`default` | `auto` | `ask` | `read_only`; left out until a
device sets it, and by older servers: apps then keep their own choice; a
Windows server, which has no command sandbox, reports `ask` until then, or
`read_only` when `[agent] permission` is that). Once
set, a new chat that no bot, parent or client choice decides for starts with
it (unset: `[agent] permission`), and `setup_status.permission` reports it.
`set_security { sandbox?, folders?, ask_outside?, redact_patterns?,
save_secrets_from_chat?, secret_saving?, secret_machines?,
secret_machines_picked?, bot_handoff?, default_permission? }` changes what is
given (`folders` replaces the list; paths must be existing folders outside
Tessera's own; `secret_machines_picked` replaces the list and keeps only known
machines; a revoked machine leaves it; `default_permission` can be changed but
not unset) and returns `SecurityInfo`. Agents have no request or tool for
these; their `settings` tool can change only everyday settings, each after
the user approves it. See [SECURITY.md](SECURITY.md).

## Devices

Every paired device (`DeviceInfo`): `{ id, name, created_at, last_seen?,
current, notify, online, computer?, client?, host?, model?, named? }`.
- `list_devices` → `DeviceInfo[]`, first paired first. `computer`: devices on
  the same computer share it (the id of the first of them paired); clients
  show them as one entry. `client`: `"app"` (desktop app), `"phone"`,
  `"browser"`, `"terminal"` (the `tessera` command), `"machine"` (paired only
  as a machine) or `"unknown"`; as the device said, else from the names
  Tessera's own programs pair with. `host`: its computer's own name as it
  reported it; `model`: a phone's model in plain words; `named`: the user
  chose the name. Older servers send none of these (each device its own
  entry).
- `device_hello { client, computer?, host?, model? }` → this `DeviceInfo`.
  Sent by newer clients once per connection; fields left out stay as they
  were; it never changes the name. `computer` is the computer's key: a
  SHA-256 of the system's own id (`/etc/machine-id`, Windows' `MachineGuid`,
  the Mac's platform UUID) and its host name under a label of Tessera's own
  (`tessera_secure::computer_key`), the same for the desktop app, the
  terminal and the machine of one computer. It stays on the server.
- `rename_device { id, name }` → `DeviceInfo[]`. Renames the device and the
  devices shown on the same computer (not those paired only as a machine)
  and marks them `named`; 1 to 60 characters of one visible line. Logged.
  Refused for a computer that is only a machine: a machine's own name (what
  agents and approvals call it, `MachineInfo.name`) changes only on that
  computer (`tessera node name`, or its desktop app).
- `revoke_device { id }` → null: signs it out at once (its connections close)
  and ends its machine login.
- Event `machines_changed` also follows a rename and a `device_hello` that
  changed something: fetch `list_devices` again.

Which devices share a computer: those that reported the same key. A device
that reported none (an older program) joins a computer that did by an exact
match of that computer's name (its `host`, "Terminal on <name>", "<name>
(desktop app)", or the machine it is), only when exactly one computer has
that name and no device of the same kind is there or joins with it. Phones
and browsers never join by name, and devices that reported nothing never make
a computer among themselves.

## Machines

A machine is another computer that runs `tessera node` and connects with a
**machine login** (user guide: [MACHINES.md](MACHINES.md)). What
it allows, `MachineScope { folders: [{ path, write }], commands, ask:
"always" | "chat", timeout_secs, admin? }`, is set only on that computer and checked
there on every action; nothing in this protocol turns a machine on or widens
it. Defaults: no folders, commands off, `ask: "always"`, 300 s (max 14400).
`admin` ("Administrator", batch 17; sent only when not `off`): `"ask"` or
`"chat"` lets `run` actions with `admin: true` run outside the machine's
sandbox with `sudo` working; with `"ask"` each one needs `approved` (the
`machines` tool asks for it whatever the chat's mode), with `"chat"` it
follows `ask` and the chat's mode. It needs `commands`; machines refuse
`admin` runs while it is `off`, and turning it off stops running ones.
`stopped` (sent only when true) and `stops` (sent only when not 0): its user
pressed Stop on that computer (the Stop bar, its key, `tessera node stop`):
it refuses every action until resumed there, which only that computer can
do. The server refuses calls to it up front and drops the `desktop` tool for
it; "allow for this chat" for its screen is keyed by `stops`, so it ends with
each Stop. Each turn keeps the Stop counts it was accepted under (every
machine's, and the server computer's own `stop.json`), with its session, also
across restarts: `NodeCall.stops` is that turn's count for the machine (0 if
it didn't know it), and the machine refuses a call whose count isn't its own.
The server refuses such calls up front once it knows the higher count, and a
higher count on a machine of the server's own computer ends the server's
older turns.
A scope set on the machine (presets, `scope --set`) keeps both.

Any device:
- `list_machines` → `MachineInfo[]`: `{ id (device id), name, os, version,
  online, last_seen, paused, scope, sandbox, sandbox_ok, running }`.
  `sandbox_ok` false means the machine refuses commands (no Landlock).
- `set_machine_paused { id, paused }` → `MachineInfo`. Pausing fails the
  actions waiting on it and tells the machine, which stops what runs and
  refuses new ones; resuming never goes beyond its scope.
- `revoke_machine { id }` → null. Fails what waits on it, tells the machine
  to turn itself off (it records that locally) and refuses its hellos until
  it is turned on again on that computer (a higher `generation`). Revoking
  the device (`revoke_device`) revokes its machine too.
- Event `machines_changed` (no payload): fetch `list_machines` again. Sent
  when machines come and go, change scope, pause, and when actions start or
  end.

Machine logins. A token is either a device's own login (everything a device
may do) or a machine login, which may only send `hello` (answered without the
server's home folder or default model), `node_hello` and the other `node_*`
requests; anything else fails with `unauthorized` ("this login belongs to a
machine…"), and it never gets the chats' events. There are two kinds:
- A device paired as a machine (`PairHello.machine`): its only token.
- `machine_login` → `MachineLoginGrant { device_id, token }`, asked with a
  device's own login: a machine login for that device's machine side (the
  computer the desktop app or `tessera connect` runs on), under the same
  device id. Each call replaces the one before. `tessera node enable` (given
  the app's login on stdin, or using `tessera connect`'s) and `tessera node
  run` ask for it and keep only that in `node-server.json` (`"machine":
  true`); with a server that doesn't know `machine_login` they stay on the
  device login. Revoking the device ends its machine login too.

A connection that said `node_hello` with a device's own login (machines from
before machine logins) is held to the same requests from then on; the
device's other connections are not.

When a machine pairs again, `node_hello` replaces its previous machine
entry only if both device records have the same computer key and client
kind (`device_hello` may supply the kind later). The earliest machine's
name, server pause choice and history carry over; the computer still
supplies its own scope, folders and administrator choice. Its previous
login is revoked without sending `NodeControl.revoked`, which would
switch off the shared settings on that computer. A different or unknown
computer key is never merged, nor is an app with a separately paired
terminal on the same computer.

The machine's own connection (end-to-end encrypted, its machine login):
- `node_hello { name, os, version, scope, generation, sandbox, sandbox_ok,
  home?, computer?, host?, features?, guard_key? }` (`guard_key`: Tessera
  Guard's public key on that computer, hex (Windows); the server pins the
  first one it sees, and a machine that later reports another (or none) gets
  no actions until it is added again; `computer`, `host`: as in
  `device_hello`, kept for its device; `features`: the implemented backends,
  kept as `MachineInfo.features`) → `NodeWelcome {
  machine_id, paused, server }`, sent on every
  (re)connection. From then on the connection gets only its own `node_*`
  events, none of the chats' broadcast. A revoked generation gets
  `unauthorized`, and the machine turns itself off. Names are made unique.
  `features?: string[]` names built backends (`desktop` for native screen/input,
  `desktop_session` for Linux command desktop, `terminal`, `admin`, `sandbox`,
  `webviews`), not permission to use them. Local commands return the same list
  and the node's own `os` in `NodeStatus`. An absent list means an older node:
  clients fall back only to that node's `os` (old Windows has no desktop, terminals,
  administrator, sandbox or web views; macOS has no desktop or sandbox/full-home
  write access). An explicit empty list means no backends.
  Unknown names are ignored by clients. Settings only offer implemented choices;
  every operation still checks the node's locally chosen scope. Work in folders
  is always offered: its checked chosen-folder reads/writes need no backend
  capability; without a working `sandbox`, the preset sets `commands: false`.
- Event `node_call { call: NodeCall { call_id, action, session_id, chat,
  mode, approved, stops?, files_only? } }` (`stops`: the machine's `MachineScope.stops`
  when the action was asked for; a machine refuses a call from before its
  user's latest Stop, even after a Resume. Older servers send none: then only
  the Stop itself, while it lasts, holds such calls back; `files_only`: the bot
  acting may only read files and list folders there, so the machine refuses
  anything else for it, looking at the screen included), where `action` is `{ action: "read", path, offset?,
  limit? } | { action: "list", path } | { action: "write", path, content } |
  { action: "edit", path, old, new, all } | { action: "run", command, cwd,
  timeout_secs?, admin? } | { action: "desktop", desktop: DesktopOp, look? }`
  (`admin`: as administrator, see `MachineScope`; older machines ignore it
  and run the command in their sandbox. `desktop`: only to machines with the
  `desktop` feature and desktop control on; `DesktopOp` is `{ do: "screens" }
  | { do: "screenshot", screen? } | { do: "move", screen?, x, y } | { do:
  "click", screen?, x, y, button, clicks? } | { do: "drag", screen?, x, y,
  to_x, to_y } | { do: "scroll", screen?, x, y, dx, dy } | { do: "type", text
  } | { do: "key", keys }`, with `x`/`y` in that screen's screenshot pixels;
  looking is a read, the rest are changes; `look`: a screenshot after
  acting). `mode` is the chat's permission mode and `approved` says
  the user approved this action in the chat; the machine refuses changes
  for `read_only` chats and anything unapproved while it asks for
  everything. Calls go only to the connection that said hello. A `run` may
  carry `env` { NAME: value }: the stored secrets its command names that the
  chat may use on this machine (`SecretInfo.on_machines`, who can use it as
  for `bash`). The machine sets them for that command only (names that
  would change how programs run are dropped), scrubs their values from
  progress, the reply and its log, and never writes them anywhere.
- `node_progress { call_id, chunk, stream? }` → null. `stream` is `stdout`
  or `stderr`. Each pipe holds unfinished secret suffixes independently before
  publication or truncation. Older nodes omit it and use one legacy stream.
  When stored values exist, command output from a legacy stream is withheld
  until that node is updated. Current-node command results are rebuilt from
  server-redacted progress so node-side truncation cannot expose a fragment.
  Then `node_reply { call_id, result: NodeResult { output, ok,
  refused, exit_code?, image? } }` → null (`image`: `NodeImage { mime, data
  (base64), width, height }`, a desktop screenshot). Only that connection's answers to calls
  routed to it count; anything else is refused.
- Event `node_cancel { call_id }`: stop it now (the chat stopped it, or the
  server gave up `timeout_secs` + 30 s after sending it).
- Event `node_attest { nonce }` (machines with a pinned guard key), before
  every `node_call`: the machine asks its Tessera Guard to sign the Stop for
  `nonce` and answers `node_attestation { nonce, stopped, stops, generation,
  signature }` → null. The server checks the Ed25519 signature over
  `"tessera-guard-attest-v1\n{nonce}\n{stopped as 0/1}\n{stops}\n{generation}"`
  with the pinned key, refuses a stopped computer, a word older than one
  already seen, a bad signature or no answer within 5 s, and only then sends
  the call.
- Event `node_control { paused, revoked }`.
- Event `node_refused { call, reason }`: an action for this machine that was
  refused before it was sent (its reported scope, the chat's mode, a
  catastrophic command) or declined by the user. Only for the machine's own
  log: nothing runs and nothing is answered.
- `node_scope { scope, name? }` → null: the scope (or, with `name`, its
  name: `tessera node name`) changed on that computer. Names are made unique.
- The server fails waiting actions when the connection drops or is
  replaced ("may or may not have finished"); the machine stops running
  actions when it loses its connection.

The `machines` tool (offered once a machine exists) maps `list`, `run`,
`read`, `ls`, `write` and `edit` onto these. It checks the reported scope
first so plain refusals come before an approval; approvals use
`ApprovalRequest { tool: "machines", summary: "on <name>: <command or path>",
arguments: { machine, machine_id, action, … } }`. A machine with `ask:
"always"` asks even in `auto` mode. A bot's own access (`Bot.machines`, see
Bots) narrows this further, checked by the tool and again by the server
before a `node_call` is sent; machine-hosted web views need `allowed`.

**Web views on a machine** ("Web views here", `MachineScope.webviews`, set
only on that computer). A web view board may run in a machine's browser
instead of the server's: `Whiteboard.host` is the machine's id (absent: the
server). It is set when the board is made, from the device the user last used
the chat on (opened it with `get_session` without `since_seq`, sent in it,
answered an approval, made a board, attached to or navigated one of its web
views): when that device is a machine (same device id) that is online, not
paused, has web views on and may be used by the chat, the board runs there;
otherwise on the server, and `local` boards always on the server. The host is
kept for the board's lifetime. Devices use such boards with the same
`browser_*` requests; frames are relayed by the server. Attaching while the
machine can't show it fails with the reason ("Desktop is offline").

The server drives the machine's browser with the DevTools protocol, tunnelled
through the machine's own connection:
- Event `node_browser { msgs: NodeBrowserDown[] }`, in order:
  `{ type: "start", run, scale? }`, `{ type: "cdp", run, messages: [JSON
  text] }`, `{ type: "stop", run }`, `{ type: "screen", url? }`. `run`
  numbers each start.
- `node_browser { seq, msgs: NodeBrowserUp[] }` → null (`seq` from 1 per
  connection, handled in order): `{ type: "started", run, product, speed_key,
  max_tabs }`, `{ type: "failed", run, error }`, `{ type: "cdp", run,
  messages }` (replies and events), `{ type: "stopped", run, reason?, crashed?
  }`, `{ type: "screen", shown, error? }`.

`screen { url }` ("Show on <machine>'s screen", only ever for a user's
`browser_show_on_screen`) shows `url` (http(s) only) in an ordinary, visible
browser window on the machine's desktop, with the web views' own profile, so
the user can finish there what a web view can't (a site's verification
check). The machine checks it as it checks `start` (web views on, enabled,
not paused) and needs someone signed in to its desktop. Its browser for web
views is asked to quit first (`Browser.close`, so it saves its cookies; it
reports `stopped` with the reason), then the window starts with no debugging
pipe or port and no automation flags (nobody drives it), security keys still
off, the same Landlock confinement plus read access to the display's X11
credentials, and of the desktop session only the display (`WAYLAND_DISPLAY`,
`DISPLAY`, `XAUTHORITY`, `XDG_RUNTIME_DIR`; no session bus, so no keyring).
It answers `screen { shown: true }`, or `{ shown: false, error }` in plain
words. While the window is open `start` is refused. `screen {}` (no `url`)
asks it to quit (it saves what changed) and is answered `{ shown: false }`;
the window closed by the user there sends `{ shown: false }` by itself, and
the server opens the pages being watched again. Pausing, switching web views
off or losing the connection closes the window too. Machines from before
don't answer: the request times out with "may need a newer version".

The machine refuses `start` unless web views are on, it is enabled and not
paused, and passes on only an allowlist of DevTools commands with checked
parameters (pages only at http(s), `about:blank` or HTML `data:` addresses; no
local files, other targets or profiles, browser settings, raw file paths or
file drops, extensions, cookies); anything else gets an error answer "refused by
this machine: …". Commands implemented by the node include `Tessera.act { session, chat,
mode, bot, action, target, board }` (the agent is about to act: checked
against who may use the machine and the chat's mode, and logged) and
`Tessera.discardDownload { guid }`, and `Tessera.chooseFiles { backendNodeId,
files: Attachment[] }` on a known page session. The last requires that exact
file-input node's current chooser event, accepts only selected bytes (8 files,
4 MiB total), stages private copies in its own browser folder (64 MiB per run),
then sends `DOM.setFileInputFiles` itself. Arbitrary host paths remain refused;
navigation/detach expires the chooser and Stop/revocation refuses the command.
It sets downloads and page attaching
itself, names finished downloads (`tesseraPath` in `Browser.downloadProgress`)
and stops the browser when idle (10 min), over its memory limit, paused,
switched off or disconnected.

Local only, on the machine (`tessera node status|enable|disable|scope|service|log
--json`): `NodeStatus`, `NodeService` and `NodeLogEntry` are exported with
the protocol types for the desktop app, which runs these commands on its own
computer. They are never sent to the server.

## Terminals

The user's own terminals (a login shell on a pseudo-terminal), opened from
the "new" menu. Only a paired device's own connection may send these: never a
chat, tool, bot, job, the in-process backend or a machine login (those get
`unauthorized`). A terminal belongs to the device that opened it; for any
other device it doesn't exist (`not_found` "that terminal isn't open any
more"). Opening and showing need a private connection: end-to-end encrypted,
or a plain one made on the server itself (`TerminalStatus.private`). A web
browser's login ("Use Tessera in a web browser") is never private and may
only ask `terminal_status`; anything else is `unauthorized` ("Terminals open
only from the Tessera app, not from a web browser.").

- `terminal_status` → `TerminalStatus { settings: { access, idle_minutes },
  supported, sandbox_ok, sandbox, strict, private, max_open, grace_secs, open:
  TerminalInfo[] (this device's), recent: TerminalAudit[] }`. `access` is
  `off` (the default), `sandboxed` or `full`.
- `set_terminal_settings { settings }` → `TerminalStatus`. `sandboxed` needs
  the command sandbox on and complete; `full` is refused with `sandbox =
  "strict"`; `off` closes every terminal on the server. `idle_minutes`: 15 to
  240. Turning terminals on or giving them more needs a private connection
  (`unauthorized` otherwise); each time, the server sends a notification
  (also when a machine's `MachineScope.terminal` widens).
- `terminal_open { machine?, cols, rows }` → `TerminalInfo { id, machine?,
  place, sandboxed, shell, started_at }`. On the server when `access` isn't
  `off`; on a machine when it is online, not paused and its own
  `MachineScope.terminal` isn't `off` (it checks again). At most `max_open` (4)
  per device and place, 16 per place in all.
- `terminal_attach { id, cols, rows }` → `TerminalInfo`: show it on this
  connection again (after a reconnect or reload). The kept output (the last
  256 KiB) comes again first, from its offset, then the program is asked to
  redraw.
- `terminal_detach { id }`, `terminal_input { id, data }` (UTF-8, at most 64
  KB), `terminal_resize { id, cols, rows }`, `terminal_ack { id, upto }`,
  `terminal_close { id }` → null. Input, resize, ack, attach, detach and close
  are handled in the order sent. Input doesn't wait for the program: it waits
  in the terminal's own queue, up to 1 MB; past that more is refused and the
  terminal shows a line saying so. For a machine's terminal the server also
  holds at most 1 MB on its way there (a slow or stuck link: the typing, and
  a little for each other request); past that `terminal_input`, `_resize`,
  `_ack`, `_attach` and `_detach` for it fail (`bad_request`) until the
  machine catches up, `terminal_close` always goes, and nothing else on the
  connection waits for it.
- Events, only to the connection showing it: `terminal_output { id, seq,
  data }` (base64 bytes; `seq` is the offset of the first one) and
  `terminal_exit { id, code?, reason }`. A chunk that doesn't start where the
  last one ended is the kept output again: draw the screen afresh from it.

Flow control: the server (or machine) reads a terminal no further than 256 KiB
past what the device acknowledged with `terminal_ack` (after drawing it); the
program then waits. While no device shows it, only the kept output is held. A
terminal closes when its program ends, on `terminal_close`, `grace_secs` (5
min) after no device shows it, after `idle_minutes` without input while the
shell is at its prompt (8 h while a program runs in it), when its device is
revoked, and when terminals are turned off there. The server keeps who, when
and where for each (`terminal-audit.jsonl`, the last 100), never what was
typed or shown; its log has ids only.

Machines ("Terminal from my devices", `MachineScope.terminal`, set only on
that computer: `tessera node scope --terminal off|sandboxed|full`), relayed
through the machine's own connection:
- Event `node_terminal { msgs: NodeTermDown[] }`, in order: `{ type: "open",
  id, device, device_name, cols, rows }`, `input { id, data }`, `resize { id,
  cols, rows }`, `ack { id, upto }`, `attach { id }`, `detach { id }`, `close {
  id }`, `signed_out { device }`.
- `node_terminal { msgs: NodeTermUp[] }` → null, in order: `opened { id,
  shell, sandboxed }`, `failed { id, error }`, `output { id, seq, data }`,
  `exit { id, code?, reason }`.
A machine closes its terminals when it loses the server, is paused, revoked or
turned off, or its terminal setting changes; it logs `terminal_opened` and
`terminal_closed` (who and when) in its own log.

## Attachments

`send { attachments: [{ name, mime, data }] }` (base64 data). Limits: 10 files,
25 MB each, 50 MB per message.
- Images (png, jpeg, gif, webp) go to the model as images, and are also saved
  so text-only models and tools can use them.
- Other files are saved to `uploads/<session>/`: under the cache directory, or
  under `TESSERA_HOME`. The folder is 0700 and stays readable by the agent's
  tools and sandbox. The agent gets a line with the file's path. Text files
  up to 64 KB are also inlined.
- Names are reduced to their last path component.
- `delete_session` removes the session's folder.
- Private chats take text files and images only, and write nothing to disk.

The apps' composer turns pastes into attachments: pasted images (as files);
and text over 1,500 characters or 25 lines becomes `Pasted text.txt`
(`Pasted text 2.txt`, …), so the agent gets all of it inlined (up to 64 KB;
above that it is saved and read with `read`) without filling the text box.
On a phone or tablet the paperclip opens a small menu: "Take photo" opens the
camera app and attaches the shot, "Photos & files" is the normal picker. A
photo is an image like any other: shrunk to 2048 px on its longer side and
turned upright (the camera's orientation tag is applied, then dropped).

## Agents' notifications

The `notify` tool (and Settings → "Test from the server") stores a
`UserNotification` and sends `notification { notification }` to the devices
that take them (`DeviceInfo.notify`, set with `set_device_notify`; other
connections never get the event). A scheduled one is sent once with
`sent: false` (phones arm a wake-up for `due_at`) and again with
`sent: true` when it is due.

| Request | Returns |
|---|---|
| `set_device_notify { id, notify }` | `DeviceInfo[]` |
| `list_notifications` | scheduled ones (soonest first), then the latest 50 sent |
| `send_notification { title, body, session_id?, delay_secs? }` | `UserNotification` (at most a year ahead) |
| `pending_notifications` | legacy explicit agent messages only: `PendingNotifications { new, seen, next_due?, waiting }` for this device |
| `negotiate_notifications` | `true`: enable typed automatic `notification` events on this connection only; older servers reject it, and new apps keep the generic preference-aware `job_updated` fallback |
| `pending_notifications_v2` | the same shape, including typed automatic alerts; background callers understand `kind`/`quiet` and enforce previews/category preferences |
| `ack_notifications { ids }` | `null`: shown (or deliberately not) on this device |
| `seen_notifications { ids?, session_id? }` | `null`; every device gets `notifications_seen { ids }` and removes them |
| `cancel_notification { id }` | `null` |
| `list_notification_inbox` | the notification center: this device's scheduled reminders (soonest first), then up to 500 from the last 30 days (newest first, by `due_at`), read and quiet ones included; whatever the device's `notify` choice |
| `clear_notifications { ids }` | `null`: forget these sent ones (scheduled ones stay); every device gets `notifications_seen { ids }` |
| `get_notification_policy` | `NotificationPolicy` shared by devices |
| `set_notification_policy { policy }` | the saved policy, preserving separately edited source rules |
| `set_notification_source { key, level }` | saved policy, level is `page`, `digest`, `silent` or null to restore |
| `test_notification { delay_secs? }` | urgent `UserNotification`, up to an hour ahead, bypasses source and device opt-outs |
| `notification_deliveries { id }` | `{ device_id, name, delivered }[]`, only accepted OS posts count |
| `report_notification_delivery { ids }` | null, authenticated device's report, distinct from suppression acknowledgement |

### The notification center

`UserNotification` gained optional fields: `private` (redact every OS text
field regardless of previews or session hydration), `kind` (empty: an agent's
message; `finished`/`error`: long work in a chat ended; `job`: a scheduled
run's outcome or the end of its schedule; `approval`: an approval or a
whiteboard question waited over 30 s; `helper`: helpers reported back;
`machine`: terminals turned on somewhere; `system`: Tessera itself) and
`quiet` (kept for the center only, never a system notification). Clients
treat unknown kinds as a message. The server never sends a `notification`
event for a quiet one and leaves it out of `pending_notifications.new`;
in SQLite it is `sent = 2` (older servers only select 0 and 1, so a
downgrade can't turn one into an alert). An empty
`notifications_seen { ids: [] }` means "the notification center changed":
it marks nothing read and withdraws nothing (older apps ignore it), and it
reaches devices with `notify` off too, without content.

The server assigns `level` (`page`, `digest`, `silent`), `source` (bot,
chat, job, helper, system or external), stable `source_key`, a short plain
`reason` and `repeat_count` (at least one). Missing fields have serde defaults
and TypeScript fields are optional for old servers. An ordinary exact repeat
returns `grouped_into` with the original id and increments the existing count.
Changed text has a fresh id and stays as a separate item. Urgent revisions
always remain deliverable, even when repeated. No per-source time throttle
can hide a first occurrence or urgent flag.

`NotificationPolicy` is `{ hourly, daily_at, quiet, quiet_from, quiet_to,
sources }`. Times are minutes after midnight in the server's local zone.
Default daily time is 1080 (18:00), quiet hours are off. Daily allows up to
four valid times. Source rules are `{ key, name, level }` ceilings and are
set separately. Quiet hours move ordinary Page to Digest. Urgent bypasses
quiet hours and viewing suppression, not explicit owner source ceilings.
Tests bypass both ceilings and device opt-outs, but never OS permission.

Digest members are `quiet=true`. A single summary is `kind=digest`,
`level=digest`, `quiet=false` so the normal delivery path carries it without
sound. Shared digests exclude private, read and muted items. Assembly and
consumption are atomic. Empty digests send nothing. Daily/hourly scheduling
is persisted and a missed time produces one catch-up summary.

Presence is per device, not global. A desktop tile cannot hide a phone page.
Viewing acknowledges only on that device. Explicit reads and entering a
chat can still mark observed alerts read everywhere. New alerts arriving
while a tile stays open do not implicitly become globally read. Accepted
platform posts report delivery separately from `ack_notifications`; old
clients may only acknowledge and therefore never claim a delivery receipt.

Visibility is checked per request: `list_*`, `pending_notifications` and the
id actions (`ack`, `seen`, `cancel`, `clear`) leave out or refuse
(`not_found`) another device's private chat's and those about a chat that is
gone; a revoked, unknown or machine login gets `unauthorized`. A private
chat's notifications are kept in memory only (ids reserved in the database
without content) and go with the chat, its device's sign-out or a restart.
Deleting a chat deletes its notifications. The notify tool cancels only its
own chat's reminders. Older servers reject `list_notification_inbox` and
`clear_notifications`; apps then say the server needs an update and keep
everything else working.

### Scheduled jobs: runs and limits

Changed notification content gets a fresh `id` (the revision). Reads, clears
and acknowledgements use the specific observed id. A stale request cannot
consume newer content. Distinct source results stay in the inbox while
OS cards group under their stable source key. A reconnect/app downgrade must
negotiate typed notifications again. Without negotiation a new server sends
no automatic payload on the legacy alert route.

Local cron, at-times and until use timezone rules for the target instant:
Linux zoneinfo, Android system tzdata, Windows IANA bundle/CLDR mapping. Gaps
resolve to the first valid instant after the gap, folds to the first occurrence
only. Until is exclusive; a bare date means the following local midnight.

`Job` gained optional fields: `until` (unix ms: no run starts at or after it),
`max_runs`, `runs` (started so far, kept by the server), `last_end` and
`ended` (why the schedule ended: its runs used up, its end date, or a run's
`schedule end` saying its goal was met; the job is off from then on).
`last_status` is `running`, `ok`, `error`, `stopped` (by the user), `paused`
(the turn stopped short at its step limit or going in circles) or
`interrupted` (the server stopped and the run didn't continue); a run whose
turn ends while its helpers or bots still work stays `running` until they
report and its next turn ends. `list_job_runs { id }` → `JobRun[]` (the job's
latest 50 runs, newest first: `session_id`, `started_at`, `ended_at?`,
`outcome`); `tessera jobs <id>` prints them. The server logs each start and
end (identifiers, counts, fixed outcome/reason class and elapsed time only; never
names, prompts or free-text end reasons). Admission reloads current job state
under a per-job lock and commits its run reservation/count together. An old
run cannot end a duty reassigned to another bot.

`save_job`: a limit left out keeps the stored one (older apps don't know
them) and `0` clears it; `runs`, `last_end` and `ended` are the server's;
turning on a job whose schedule is over fails until its limit is raised or
cleared (`bad_request`), as does one with no run before its `until`.
Older servers ignore the new fields.

Apps show a `notification` as a system notification when they aren't in
front (a toast in front) and acknowledge it either way. Android with the app
closed: a job (every ~15 minutes, and an alarm at `next_due`) loads the app's
Rust side through JNI, connects end-to-end encrypted, calls
`pending_notifications`, acknowledges and posts (`notify/pull.rs`).

## Shared files

`SharedFile { id, name, mime, size, created_at, session_id?, board_id?, source, path }`:
`source` is `agent` (`share_file`: a 0600 copy in the data directory, up to
1 GB, refused for Tessera's own files, credential files and content holding a
saved secret, private key or access token; at most 5 GB kept, oldest first),
`web_view` (a finished download of the server's browser, offered as it is) or
`upload` (an attachment). `file_shared { file }` announces each one; a
`share_file` tool result carries it in `details.file`.

| Request | Returns |
|---|---|
| `list_shared_files { session_id? \| board_id? }` | `SharedFile[]`, newest first, still on disk |
| `read_shared_file { id, offset, len }` | `FileChunk { data, offset, size, eof }`, base64, at most 1 MiB |

Deleting a chat deletes its files (Tessera's copies) and unlinks its
notifications.

## Mosaic layouts

`get_mosaics` returns `MosaicLayout { version, mosaics }`, where each mosaic
is `{ id, name, layout, tiles: [{ id, session_id?, board_id?, note_id? }],
geometry? }`. A tile shows a chat, a whiteboard, a note, or nothing (an empty
"new chat" tile); tiles of boards or notes that no longer exist are dropped
on save.
`save_mosaics { layout }` must carry the version it was based on. A stale
version fails with `conflict`, and the client refetches and merges. A
successful save bumps the version and broadcasts `mosaics_changed` to every
device. Tiles showing private chats are never stored or synced. Limits: 32
mosaics, 48 tiles each, 64 KB of geometry.

## Setup and remote access

`setup_status` returns `SetupStatus`, which summarises what is configured
(model, web search, voice, remote access, devices, sandbox, LAN) with no
secrets. `complete_setup` marks it done. `detect_local_models` looks for
llama.cpp, Ollama, LM Studio and vLLM on their usual ports, on this machine
and on the other computers of its home network (the /24 around a private
address; nothing public is scanned). It only asks for model lists, which never
loads a model, adds what answers as providers, and makes one the default only
when there is none yet and it runs on this machine.

Cloudflare, from one API token (used for the calls, never stored):
- `cloudflare_zones { token }` verifies the token and lists its domains.
- `cloudflare_setup { token, zone_id, hostname }`:
  1. installs `cloudflared` (the official release, verified against its
     published SHA-256);
  2. creates a tunnel to this server, a proxied CNAME, a service token, and an
     Access application that only admits that token;
  3. starts the tunnel, supervised and restarted with backoff.
- Progress arrives as `setup_progress { task: "cloudflare", step, done, error }`.
  A failure undoes everything created so far, and only one setup runs at a
  time.
- New pairing codes carry the tunnel URL and the Access headers.
- `cloudflare_remove { token }` deletes all of it.
- Zero Trust: Access exists only on accounts with Zero Trust turned on (free).
  `cloudflare_zones` gives each zone `zero_trust`: `false` when Cloudflare
  refuses this token Access on the zone's account (almost always: Zero Trust
  is off; or the token lacks the Access permissions), absent when it couldn't
  be asked or from older servers. The setup asks Access before it creates
  anything; any Access refusal (then or later) comes back in plain words
  (what Zero Trust is, that it's free, `https://one.dash.cloudflare.com`, the
  two permissions), followed by `(Cloudflare said: …)`.
- The service token (the key devices carry) lasts a year:
  `TunnelStatus.key_expires_at` (unix seconds; absent for tunnels set up
  before the server kept it), and `renewable: true` from servers that renew
  it (older ones: absent, so the apps say nothing about the key). `cloudflare_renew { token? }` → `RemoteStatus`
  extends it: with a token (Account · Access: Service Tokens · Edit; used
  once, never stored) through Cloudflare's "refresh", the same key, so
  devices need nothing new; without one, the user refreshed it in
  Cloudflare's dashboard and the date moves a year on from now. From 30 days
  before it runs out the server sends the user's devices a notification ("Remote
  access needs renewing") at 30, 7 and 1 day(s) left, and "Remote access
  stopped" once it has; each once per date.

**Paired devices keep up with the server's addresses.** How the user's
devices reach the server (`ServerRoute { urls, headers }`: its home addresses,
then the remote ones, and the proxy headers, Cloudflare Access's included) is
what a pairing code carries. When it changes (`cloudflare_setup`,
`cloudflare_remove`, `set_remote_access`), the server broadcasts
`server_route_changed` (no payload), and apps ask `get_server_route`, also
after every (re)connect. The answer holds credentials: the server gives it
only on an end-to-end encrypted connection of an app paired as one of the
user's devices, or of a machine connecting from another computer (it needs
them to stay reachable away from home); a web browser's connection, a plain
(unencrypted) one and a machine on the server's own computer are refused
(`unauthorized`), and it is never logged. The apps (`tessera-client`
`merge_route`) keep saved addresses in their order and append newly learned
ones. Only retired server-added addresses and unchanged server-installed
header values are removed; user-pasted or edited headers stay, matched by
case-insensitive name. Legacy saved routes did not record ownership, so
ambiguous addresses and header values are preserved rather than guessed at.
A saved server reached only on this computer is left as it is. Connection attempts race
health, WebSocket upgrade and pinned-key login, with at most 400 ms of grace
for a preferred address after another completes. Reconnects prefer the last
successful address without reordering Settings; the apps persist that
preference for cold starts too. Proxy headers go only to remote HTTPS hosts,
not plain HTTP or LAN/loopback hosts (even if those use HTTPS).

The desktop and Android apps save route changes in place (`servers.json`),
without dropping a live socket. Only changed addresses or headers emit
`tessera://servers`; the page reads the list again, never reloads itself or
reconnects for that event. Route fetches are serialized and replies from a
replaced client are ignored. The terminal saves in `client.json`, a machine
in its login file.

**Pairing again.** An app that pairs again with a server it has saved (the
same server key) replaces that entry instead of adding a second one, and once
connected sends `retire_login { token }` with the login it held before: the
server signs that device out, handing its user-given name and its
notification choice to the new one. Only an ordinary device's login is
retired (not a machine's or a browser's, nor one whose computer serves as a
machine under it: that machine would stop), and only over the new, verified
connection, never inside the pairing itself.

`remote_status` reports LAN and remote URLs, the header names (not values),
the tunnel state, and `web: WebAccess { on, available, url?, needs_token,
browsers }` ("Use Tessera in a web browser": switched on, whether this server
has the web app, where to open it — the first https address + `/web/` —,
whether the tunnel Tessera made must let browsers through first, how many
browsers are paired).

`set_web_access { on, token? }` → `RemoteStatus` switches it. Turning it on
needs the web app (else bad_request), and when Tessera's own Cloudflare tunnel
lets only the apps' service token through (`needs_token`), a Cloudflare API
token (Account · Access: Apps and Policies · Edit; used once, never stored):
Tessera adds an Access application for `<hostname>/web` that lets everyone
reach that path (Access goes by the most specific path; the rest of the
address stays locked to the apps) and records its id; `cloudflare_remove`
deletes it with the rest. Turning it off needs nothing and closes paired
browsers' connections at once; they stay paired (signed out in Devices).
Agents can't change it (no tool does).

## Whiteboards

A whiteboard is a tile where an agent shows the user something (the
`whiteboard` tool), or the user shows themselves (`create_board`).
`Whiteboard { id, session_id, title, content, version, created_at,
updated_at, ask?, by_user, host? }` (`host`: the machine a web view runs on).
The content is one of:
- `html`: a page;
- `markdown`;
- `image { mime, data }`: base64;
- `url`: a page the device can reach, e.g. a remote desktop;
- `local { port, path }`: a web app on the server's loopback.

An agent's board shows in its chat as a card (the `whiteboard` call); the
server adds a board tile (`MosaicTile.board_id`) only when the agent asks for
one explicitly (`whiteboard` `open: true`, an `ask` on an existing board, the
`browser` tool's `show` or `needs_user`), right after the chat's tile. If the
chat isn't in a mosaic, the tile goes at the end of the first mosaic, or a new
"Main" mosaic is created. An agent's update of a board never brings back a tile
the user closed. Closing the tile
keeps the board; `delete_board` removes it and its tiles, and deleting the chat
removes its boards. Requests: `list_boards`, `get_board`, `create_board`,
`delete_board`, `answer_board`, `board_fetch`, `board_link`. Events:
`board_updated` and `board_removed`.

`create_board { session_id, title, content, placed? }` → `Whiteboard` with
`by_user: true` ("Shown by you"). Content is checked as for the agent's boards
and the tile goes beside the chat the same way, unless `placed: true` (the
device already shows it in a tile of its own, like a spawned web view, and
shares that tile with its layout). Devices leave such a board's tile where the
device that made it put it: the apps only move a tile that landed in a mosaic
without its chat when an agent showed it (`by_user: false`), or when the device
itself just asked for it beside the chat (no `placed`), and never switch the
mosaic on screen for it. The agent can then read it: the
`whiteboard` tool's `read` returns a board's text (HTML as readable text plus
a source excerpt; a `url` page fetched by the server like `web_fetch`, with
the same approval for local-network addresses; a `local` app's page through
`board_fetch`'s path) or, for a model with vision, the image. Agents see
their conversation's boards and any board the user puts in a mosaic next to
that chat; `list` says where each is on screen and who opened it. An agent
may update or delete only its own conversation's boards; updating a user's
board makes it the agent's (`by_user: false`).

Content is untrusted, and clients render it isolated:
- **HTML:** served on its own origin with no network access. Phones use the
  `tessera-board` custom protocol. Desktops use a server on the device's
  loopback (one port for the app, a random token per board in the URL), since
  WebKitGTK won't load a custom-scheme page in a sandboxed frame; the app
  fetches the HTML with `get_board`, so this works with remote servers too.
  The browser UI uses the server's `/board/<id>/<token>/` page, loopback only,
  with a token from `board_link`. That request and its `{ path }` reply are
  unchanged. Treat the path as opaque and request it again whenever viewing
  or reloading a tile, as the apps do. Server links have a signed one-day
  expiry and board revision. Desktop grants also expire after one day and
  are tied to the revision. An edit or deletion invalidates old links, which
  never reveal later content. Legacy permanent links are not accepted.
  Already loaded pages do not need a network request to keep displaying.
  The frame is `sandbox="allow-scripts"`. The
  policy header is `tessera_proto::board::csp(origin)`: `CSP` with the page's
  own origin named beside `'self'`, because WebKit matches `'self'` against
  the sandboxed frame's opaque origin, plus `sandbox allow-scripts`, so the
  page is sandboxed even opened on its own (the page's `<meta>` copy leaves
  that out because browsers ignore it there). Before loading a board or
  preview, every client pins its parent `frame-src` policy to the exact board
  origin and `/board/` and `/preview/` routes (`pinFrames`, a `<meta>` policy).
  A board's script-driven self-navigation cannot request another loopback
  port or the server's non-board routes. Native navigation guards also check
  subframes before sending requests on Linux, Windows and Android. Refused
  navigations never open the system browser. Pages may import bundled Three.js
  (import map
  `three`, `three/addons/…`) and d3 (`/board-lib/d3.js`), served with
  `Access-Control-Allow-Origin: *` because module imports come from the opaque
  sandboxed origin.
- **`local`:** shown as a web view; to open it in the browser, the apps run a
  proxy on the device's loopback, one port per board. It works only with its
  random token, which pages keep in their URL and
  subresources send in their Referer. The proxy forwards through
  `board_fetch`, which reaches only that board's port on the server's
  127.0.0.1 and never the Tessera server itself. HTTP only, no websockets.
- **`url`:** shown as a web view (the server's browser, see Web views), or
  opened in the browser.

Snapshots (what the user sees): the `whiteboard` tool's `look` broadcasts
`board_snapshot_request { request_id, board_id }`. A device answers only if
that board's tile is visible on its screen and it can capture, with
`board_snapshot { request_id, image?: SnapshotImage { mime, data, width,
height }, error? }` (`error`, e.g. "can't capture on this device", when it
can't). The image is cropped to the board's content, PNG, JPEG or WebP, at
most 1600 px on its long side and 4 MB. The first image within 10 seconds
goes to that tool call only (an answer after that, or to an unknown request,
gets `not_found`). The wait ends early once every connected client (each
WebSocket connection) has answered with an `error`, so a client that can't
capture should say so rather than stay silent. If no image arrives, or no
client is connected, or the model can't see images, or the board isn't in
any mosaic, the agent gets the board's content as with `read` and the reason.
Waiting never holds up the agent's other tool calls.

Asks: `ask { id, prompt, input }` with `input` one of `signature`,
`text { multiline, placeholder }`, `choice { options, multi? }` or `confirm`.
A `multi` choice lets several options be picked: answered with
`choices { indexes }` (a client from before it answers `choice { index }`,
which is taken as one pick). The agent
waits up to 30 minutes, and the chat's status is `awaiting_approval` meanwhile.
`answer_board { id, ask_id, answer }` must fit the question: a signature is a
base64 PNG of at most 2 MB, and the agent gets it as a file. `cancel` declines.
Asks are drawn by the apps' own UI, never by the board's content. The apps
notify like an approval.

## Web views (the server's browser)

Web view tiles (`url` boards, and `local` boards as
`http://127.0.0.1:<port><path>`) show a live tab in a Chromium-family browser
on the server (or on a machine: see Machines → Web views on a machine), so
logins work, and the session is the same on every device.
Clients render pictures of the tab and send input; nothing loads on the device.
The agent's `browser` tool works in the same tabs.

**Process.** One browser per server: the first installed of chromium,
google-chrome, brave or microsoft-edge (or `[browser] path`). It starts on
first use and is driven over CDP through `--remote-debugging-pipe` only. It
never opens a debugging port, so nothing else on the machine (the agent's
sandboxed commands included) can reach it. The profile lives in
`<data dir>/browser/` (0700, hidden from agent commands like the rest of the
data directory). Logins persist across restarts. It runs headless unless
`[browser] headless = false`. It stops after `idle_minutes` (default 10) with
no tile attached and no agent use. A page nobody watches or uses is frozen
after `freeze_seconds` (default 60; 0 = never). If the browser dies, it
restarts (at most 3 times in 2 minutes) and the watched tabs reopen at their
board's address. `get_browser` → `BrowserInfo { enabled?, low_memory?, found,
path?, name?, running, headless, tabs, quality?, speed? }`; `set_browser {
enabled?, headless?, quality? }` → `BrowserInfo` saves the settings (`enabled:
false` (absent means true) turns web views on this server off and stops an
unwatched browser; `headless`: an unwatched browser stops so its next start
uses it; `quality` applies to open web views at once). `low_memory` is the
server's memory in bytes when it is under about 2 GB: web views there are off
until the user turns them on (Settings → Web says so).

**Render scale.** Servers draw pages in software, one frame at a time on one
thread, so a frame costs in proportion to its pixels (about 2.5× more on CPUs
without AVX2, e.g. virtual machines' generic CPUs). Headless, each tab has a
window of its own on a virtual screen (`Emulation.addScreen`) whose pixel
ratio is that tab's render scale (0.5–3): what its moving frames are drawn
at. It follows the latest attacher's `dpr` × zoom, at most `[browser] scale`
(default 2), and `[browser] quality`: `sharp` (that), `smooth` (1, or down to
0.5 on a slow server) or `auto` (default: the sharpest scale that draws a
frame in about 20 ms, down to 0.75; a slow connection's smaller frames are
drawn at their own size). How fast the server draws is measured once per
browser version and CPU when the browser first starts (`speed`: sharp
frames a second of a 1280×800 page at 2x; under 30 is slow; `[browser] speed`
sets it instead) and kept in `<data dir>/browser/tessera-speed.json`.
Browsers without virtual screens (or `virtual_screens = false`) render every
tab at one scale (`--force-device-scale-factor`), picked the same way for a
1280×800 tile.

**Tabs.** Each saved web-view board has one independent tab, made on first attach, input or
agent use. As the page navigates, the server stores the new address in the
board (`url`, or a `local` board's `path` while on its port) without bumping
`version`, and broadcasts `browser_state { tab: BrowserTab }`:
`BrowserTab { board_id, url, title, loading, can_go_back, can_go_forward,
zoom, favicon?, popup?, opener?, check?, tab_ids?, file_chooser?, file_multiple? }` (`favicon` is a small `data:` URL;
`check`: the page looks like a site's "prove you're a person" check, looked
at as it loads and shortly after a click, Enter or a form sent; a hint for
the tile, which offers `browser_show_on_screen`). Web views have no security
keys or passkeys (WebAuthn off; a page's `navigator.credentials` requests
with `publicKey` are turned down at once with `NotAllowedError`).
Deleting the board closes the tab. Pages can't reach Tessera's own port, and
only `http(s)` addresses open (also `about:blank`).

**Popups.** A window a page opens (`window.open`, `target=_blank`, "Sign in
with …" and payment windows) is stacked on top of the tile's page, with its
opener link intact. While it is open, the tile's frames, input, navigation
and `browser_state` are the popup's (`popup: true`, `opener`: the address of
the page under it). When it closes itself, as sign-in windows do after
posting back to the opener, or on `back` with no history left in it, it is
closed and the tile shows the page under it again, which has received the
result as in any browser. Clients need nothing more than a "Back to
<opener host>" hint while `popup` is set. Chrome's normal `noopener` and
cross-origin rules remain intact. Initialization first positively confirms
independent script/lifecycle freeze and all-request Fetch holds. Page enable,
the exact key guard and debugger resume are then queued on that same CDP
session, and their replies (including a nonempty guard identifier) checked.
A renderer-less `noopener` popup may first receive a fixed empty, uncacheable
HTML document from the held navigation (no site content, cookies, redirect or
network). After every protection is confirmed, Chrome reloads that navigation;
its native reload keeps GET/POST, the form body, Origin and referrer. This
internal empty-document step is a deliberate difference from normal Chrome,
needed to create the renderer without allowing the site's first request to
escape mandatory setup. It is never used for a popup with a live opener.
This releases the about:blank ACK deadlock, not page execution or navigation
network. Only then may the protected-port Fetch configuration be installed,
positively acknowledged, and the lifecycle thawed. Mandatory guard/Fetch
rejection leaves the holds in place and closes the target. The successful
tab event loop continues only non-protected requests left by the temporary hold.
Up to 4 popups can stack. Text typed into the address bar that isn't an address
becomes a DuckDuckGo search.

**Requests** (all take `board_id`):
- `browser_attach { width, height, dpr, mobile, pipelined?, formats? }` →
  `BrowserTab`. Call it when the tile becomes visible, and again when its
  size changes. `width` × `height` is the tile in CSS pixels (clamped to
  200–3840 × 150–2400) and `dpr` the device pixel ratio. The latest
  attacher's viewport applies to the tab; `mobile: true` also switches to
  touch and narrow layouts, but never spoofs a phone's user agent/platform (and hides the page's
  scrollbars, which would fade out in a second of extra frames after every
  scroll). `pipelined: true`: the connection takes several frames at a time
  (see flow control). `formats`: picture types it draws besides JPEG;
  `["image/webp"]` gets the sharp still pictures.
- `browser_detach` → null, when the tile is hidden or closed. Closing the
  connection detaches everything.
- `browser_ack { frame }` → null, once frame `frame` is drawn, or skipped
  for a newer one. It covers every earlier frame too.
- `browser_input { event: BrowserInputEvent }` → null. The event is one of:
  - `mouse { kind: down|up|move|wheel, x, y, button, clicks, dx, dy, modifiers }`,
    where `button` is `none|left|middle|right|back|forward`;
  - `touch { kind: start|move|end|cancel, points: [{x, y, id}], modifiers }`,
    where `points` are the touches still down;
  - `key { kind: down|up, key, code, text?, modifiers }`, with DOM
    `KeyboardEvent.key`/`.code`;
  - `text { text }`, which inserts at the caret (typing, paste, IME commit).

  `modifiers`: Alt 1, Ctrl 2, Meta 4, Shift 8. Events are relayed in order
  without waiting for the page.
- `browser_nav { action }` → `BrowserTab`. The action is one of
  `go { url }`, `back`, `forward`, `reload`, `stop` or `zoom { factor }`
  (0.25–5, like browser zoom: the page lays out at `width/zoom` CSS pixels).
- `browser_new_tab { url }` → `Whiteboard`: creates a saved page in the same
  chat and on the same browser host, joins its bounded (16-page) tab group,
  and broadcasts each member's optional `tab_ids` (board IDs only). Groups
  are saved as board-ID metadata and come back after a server restart.
  Ordinary no-opener `_blank` targets become independent boards without
  reissuing the native navigation (POST/referrer/first-inline guards retained).
  Explicit popup geometry or opener-access sign-ins keep the popup stack.
- `browser_page { action }` → `{ text, link?, found? }`: device-only isolated
  page commands. Actions: `context { x, y, select_word = false }`, `selection`,
  `find { text, backwards = false }`, `clear_find`, `print_pdf`. Context returns
  only an HTTP(S) link and selected text; password selection is empty. Selection
  is limited to 64 Ki characters, find to 512 UTF-8 bytes. No host clipboard
  API or arbitrary script is exposed. PDF export is capped at 16 MiB and uses
  the existing private download offer; machine CDP accepts only bounded
  ReturnAsBase64 output, no templates or host paths. The ordinary private-chat
  and machine pause/Stop gates apply to all these commands.
- `browser_dialog_answer { accept, text? }` → null.
- `browser_choose_files { chooser, files: Attachment[] }` → null: the opaque
  `file_chooser` token in the current `BrowserTab`, and only the bytes of files
  explicitly selected by the user on their device. Empty files: cancel. At
  most 8 files and 4 MiB total; no paths, traversal/unsafe names, arbitrary node
  ids or stale tokens. Copies are private, capped at 256 MiB per browser run,
  cleared on the next launch. Navigation expires the token; clients must clear
  omitted chooser fields rather than merge a previous token back into state.
  Applies to server and machine tabs; older servers/nodes reject this method.
  It is not an agent-facing upload tool.
- `browser_upload { chooser, action }` → `{ upload?, received }`: user-device
  picker bytes in bounded chunks. `start { files: [{ name, size }] }` accepts
  1–8 declared files totaling at most 200 MiB and returns a random capability.
  `chunk { upload, index, data }` appends at most 256 KiB decoded base64 to the
  chosen file, refusing bytes beyond its declared size. `finish { upload }`
  accepts only exact completed sizes and sets the still-open input's files;
  `cancel { upload }` removes its pending copies. Capabilities bind the browser
  board, chooser and device connection; navigation/disconnect cannot finish a
  new picker with old bytes. Abandoned pending batches expire after an hour.
  Accepted copies remain until browser restart because a site may read later.
  No whole-file messages, arbitrary backend node IDs or filesystem paths.
  Machine forwarding uses narrowly filtered `Tessera.uploadFiles`; old nodes
  reject it without falling back to huge messages or bypassing their rules.
- `browser_to_server { board_id }` → `Whiteboard`: a web view whose machine
  is offline (or paused, or no longer allows web views) opens in the
  server's browser instead (`host` cleared, `version` + 1, its old tab
  closes).
- `browser_show_on_screen { board_id, show? = true }` → null: "Show on
  <machine>'s screen" for a web view on a machine (the machine's `screen`
  above, with the page's current address; a popup on top: its address);
  `show: false` closes that window and the pages being watched open again
  here. A web view on the server is refused: it has no screen ("…open the
  page in your own browser instead…"). Only from the user's devices (agents
  have no such action). The tile waits meanwhile and comes back when frames
  arrive again.
- `browser_watch { session_id, tab }` → `Whiteboard`: the user's "Watch" on
  a `browser` step. The agent's background tab `tab` (from the step's
  `details.tab`) becomes a web view with `by_user: true`, the page as it is;
  the server doesn't place it (the device opens it where the user is), and
  the agent keeps using the tab by its old name. A tab that is already a web
  view gives its board.

**Events to the attached connection only** (never broadcast):
- `browser_frame { board_id, frame, mime, data (base64), width, height,
  scroll_x?, scroll_y? }`.
  - `width` × `height` is the viewport in CSS pixels: map a pointer at
    (px, py) in a tile of size (W, H) to (px·width/W, py·height/H), whatever
    the picture's own size (draw it to the tile).
  - `mime` is `image/jpeg` for the live picture. Its size is at most
    width·dpr × height·dpr, with dpr capped at the tab's render scale (see
    Render scale), 2048 px on the long side and about 3.2 MP; while the page
    moves over a slow connection it is smaller (down to half that size, but
    not below width × height unless the tab renders below 1x).
  - A sharp still picture (only with `formats: ["image/webp"]`) at the
    device's own `dpr` (up to 3, at most 4.6 MP), sent once the page has
    stopped moving for 0.3 s (1.2 s after a lone change such as a click):
    `image/webp`, or `image/jpeg` (quality 80; quicker to make) from a slow
    server. It replaces the last picture; the next change comes as a live
    JPEG again. A connection without WebP gets the last picture again in
    full JPEG quality instead.
  - `scroll_x`/`scroll_y`: where the page was scrolled to in the picture
    (CSS px; absent while the page is pinch-zoomed). While a finger drags
    the page, a client can move the picture by the drag the frames don't
    show yet.
  - Flow control: frames go out within a window per connection. Without
    `pipelined` it is one frame: the next follows the ack. With it, as many
    bytes as the connection carries without queueing: the window grows
    while acks come back as fast as the quickest round trip (measured with
    a single frame on its way, at first and every 10 s) and shrinks when
    they slow down; at least 2 frames, at most 8. A frame waiting for about
    one other to go out doesn't count as slowing down: on a slow link big
    frames take a while each, and one arriving while the next is sent is
    what keeps the link busy. How fast the link carries frames is measured
    from acks that come while others are still on their way (the best of
    the last 3 s). The newest frame replaces any that
    wasn't sent, and the browser paints only as fast as the quickest
    attached connection takes frames. A static page sends nothing. An ack
    missing for 3 s counts as lost.
  - Frame size: while the page moves (several changes a second, or devices
    falling behind), frames step down a ladder (full size at JPEG quality
    80, 65, 50; ¾ size at 50; half size at 50, 35) when the slowest
    connection gets fewer than 20 a second, and back up when it has room.
    On sharp screens (`dpr` 1.5 or more, phones) a smaller picture would be
    blown up and look soft, so quality goes down first: full size at 80,
    62, 48, 38, then half size at 50, 36. A connection that measurably
    carries at least 10 frames a second of the current size isn't taken
    for a hopeless one while its window is still opening.
    Between movements (typing, a click, a blinking caret) frames are full
    size at quality 80–45 by the same step. A connection's step carries over
    to the next tab it shows.
- `browser_focus { board_id, editable, input_type }`: show the soft keyboard
  while `editable`; `input_type` is e.g. `text`, `email`, `password`,
  `number`, `search`, `textarea` or `contenteditable`.
- `browser_dialog { board_id, kind: alert|confirm|prompt|beforeunload,
  message, default? }`, also re-sent to a connection that attaches while the
  dialog is open; then `browser_dialog_closed { board_id }` once it has been
  answered anywhere. With no tile attached, alerts and "leave page?" are
  accepted after 2 s.

Downloads go to `<data dir>/downloads/` (safe file names; files 0600) and are
announced with a `notice` to the board's chat. File pickers are declined (with
a notice) for now. The `whiteboard` tool's `look` at a web view board uses the
tab's own screenshot, so no device has to answer. The device-side `url`
framing and `local` proxy described under Whiteboards remain for clients that
don't attach.

**Agent tool** (`browser`; details in `docs browser`):
- Actions: `open {url}` (a background tab; never on screen by itself),
  `tabs`, `navigate`, `back`, `forward`, `reload`, `snapshot {text?}`,
  `click {ref|text|x,y}`, `type {ref, text, submit}`, `press {key}`,
  `select`, `scroll`, `wait {text|ms}`, `screenshot`, `dialog`, `show
  {tab, reason?}` (put a tab in a tile beside the chat), `needs_user {tab |
  url, reason}` (put it on screen with a confirm ask carrying `reason`, and
  wait for Done / I can't) and `close`. A popup the page opens becomes what
  the agent acts on. Results say when one opened or closed, snapshots mark
  it, and `back` closes it.
- Result `details` for the transcript: `{ tab, board_id, board: { id, title,
  kind }, url, title, action, popup, shot? }`; `shot { mime, data, width,
  height }` is a small JPEG of the page after the step (not after typing a
  stored secret into a field that stays on the page).
- Snapshots: a compact outline of the page with element refs (`[e12]
  button "Sign in"`), built by a script in an isolated world that pages can't
  see or change. Refs are what `click`/`type` accept. Agent clicks and typing
  are real input events, so people watching see them happen.
- Approvals: navigation to local-network addresses asks, as `web_fetch`
  does. Typing into password fields, submitting non-search forms,
  buy/send/post/delete-like clicks and downloads ask unless the session is in
  auto mode. Read-only sessions can browse but not act; in ask mode every
  action asks.
- Secrets: stored secrets are typed only as `$NAME`, only on the secret's
  hosts, after approval (even in auto mode, unless the secret is set not to
  ask). Values never appear in results, which are also redacted.
- Page text passes the injection scan: hidden characters are removed and
  flagged text is labelled as data and reported to the user.
- No script evaluation, cookies or storage are exposed. Private chats don't
  get the tool.

## Agent documentation

Tessera's own docs for agents live in `crates/core/docs/agent/*.md` and are
compiled in. The `docs` tool lists, reads or searches them. The system prompt
carries only a one-line pointer, so the context stays small.

## Permission modes

`default` (ask for risky actions), `ask` (ask for every change), `read_only`,
and `auto` — shown to users as **YOLO**: nothing asks. Catastrophic commands
(`rm -rf /`, disk formatting, fork bombs) are still refused. Config files and
requests also accept `"yolo"` as an alias for `auto`.

The modes are ordered `read_only` < `ask` < `default` < `auto`, and nothing a
session starts may run above its own mode. Subagents get the parent's
effective mode (or `read_only`). A job the agent creates or changes through
`schedule` needs approval and is capped at the session's mode. Messaging a bot
whose mode is higher needs approval. Approval summaries and reasons show
control characters spelled out (`\r`, `\u{1b}`), so an escape sequence can't
hide part of a command. `web_fetch` of a loopback, private, link-local or
carrier-NAT address (or a single-label, `.local`, `.lan`, `.internal` name)
needs approval, even in `read_only`; a public name that resolves to such an
address, or redirects to one, is refused. Responses are capped at 10 MB.

## Folders (the folder picker)

`list_dir { path }` → `DirEntry[] { name, path, is_dir }`, folders first (an
empty `path` is the server's home folder; `~/…` is expanded).

`create_dir { parent, name }` → `DirEntry` of the new folder: "New folder" in
the picker, and "Create a folder for this bot" in a bot's home folder field
(which makes `~/<the bot's name as a plain folder name>`). It makes one folder
and nothing more, with a single `mkdir`:

- `name` is a plain name, trimmed: not empty, not `.` or `..`, no `/` or `\`,
  no control characters or invisible look-alikes (direction marks, zero-width
  characters), at most 255 bytes. Otherwise `bad_request` with a sentence the
  UI shows beside the name field;
- `parent` is a full path or `~/…` of a folder that exists (a link to one is
  fine: it is the folder the person browsed to), and not inside Tessera's own
  data or settings (the same places agents never get), else `bad_request`;
- anything of that name already in `parent` (a folder, a file, a link, even a
  dangling one) is never replaced or followed: `conflict`. The bot editor then
  offers a folder that was already there ("Use ~/name"), only on a click;
- only paired devices send it (a machine's own login can't), and the server
  logs the folder and the device.

The picker for This computer (the desktop app's own folders, a native command)
has no create yet and doesn't show "New folder". Old servers answer
"unknown method": the UI shows that sentence in the name field.

## Bots

A `Bot` is a named agent: `name`, unique `handle` (@mentions), `avatar`
(emoji or 1–2 characters), `color`, `role`, `personality`, `instructions`,
optional `model`/`thinking`, `permission`, a tool preset (`full`, `coding`,
`research`, `assistant`, `read_only`, `custom` + `allowed_tools`), home `cwd`,
`own_memory` (separate MEMORY.md), `share_user_profile`, `can_message`
(handles this bot may message privately; `"*"` = any), `askable`? (`false`:
no chat or bot may hand it work, only the user talks to it; absent = yes;
`save_bot` without it keeps the stored value), `knowledge` (note
ids, checked on save: only existing notes are kept, at most 50) and
`machines` and `secrets` (below).

- **Machines** (`machines?: BotMachines { default, each: { [machine id]:
  MachineAccess } }`, `MachineAccess` = `"no_access" | "read_only" |
  "allowed"`): what the bot may do on each of the user's machines, never more
  than the machine itself allows (its scope and its "who can use it"); the
  server checks both before anything is sent, also for the bot's subagents,
  its duties, its DMs and its turns in channels. `read_only` allows `read`,
  `ls` and `list` there; commands, writes, edits and web views on that
  machine are refused. `no_access` hides the machine from the bot's `list`
  and refuses everything, web views included. Machines not in `each` (and ones
  turned on later) follow `default`. Absent: a bot made before the setting,
  which may use each machine as it allows. `save_bot` without `machines`
  keeps the stored value (an older client's edit never loosens it); a new
  bot saved without it gets `{ default: "no_access", each: {} }`. The user's
  own chats (no bot) are not affected. Asking a bot that may do more on a
  machine than the asking bot (`message_bot`) asks the user first.

- **Passwords open to any chat** (`secrets?: BotSecrets { default: bool,
  each: { [password name]: bool } }`): which saved passwords that any chat
  may use (`SecretInfo.bots` null) the bot may use too. Ones kept for certain
  bots follow their own list (`save_secret`'s `bots`); ones kept for one chat
  follow that chat. Passwords not in `each` (and ones saved later) follow
  `default`. Absent: a bot made before the setting, which may use all of
  them. `save_bot` without `secrets` keeps the stored value; a new bot saved
  without it gets `{ default: false, each: {} }`; entries equal to `default`
  are dropped. Renaming a password moves bots' entries to the new name and
  deleting it drops them (each changed bot comes as `bot_updated`). A
  password typed into a bot's chat and saved for every chat (or a kept one
  saved from there) is that bot's to use. The check is in the one place
  every use of a password goes through (commands here and on machines, the
  agent's instructions, values typed again).

- **Knowledge**: the bot's instructions list its notes' titles and previews,
  and it reads them with the `note` tool (offered whatever its preset).
  Every agent and bot reads every note the user hasn't kept from agents
  (see Notes). A bot with `own_memory` gets from `recall` only its own chats
  and DMs and the channels it is in; other agents see every non-private
  conversation. Private chats and helper sessions are never recallable.
- **Memory review**: bot chats are reviewed like other chats; what is learned
  goes to the bot's memory (its own, or the shared one) whatever its tool
  preset. Channels are not reviewed.
- Bot chats have everything other chats have: whiteboards, notes, attachments,
  fast mode, the subagents switch (and the browser where the preset allows).

- `list_bots` / `save_bot {bot}` (empty `id` creates; the handle is normalised
  and must be unique) / `delete_bot {id}`; events `bot_updated`, `bot_removed`.
- **Bot chat**: `create_session { bot_id }`. The session's `bot_id` is set,
  and its model, thinking, permission and cwd default to the bot's. Assistant
  entries and `stream_started` carry `author: <bot id>`.
- **Duties**: a `Job` with `bot_id` runs as that bot on its schedule.
- **Private bot-to-bot messages**: bots use the `message_bot` tool. The
  conversation lives in a channel session (`kind: "channel"`,
  `bot_id` = recipient, `members` = [sender, recipient]) that the user can
  open read-only to inspect. The recipient answers with its *own* tools,
  permissions and memory (separation of duties); only allowed pairs can talk.
  Ordinary chats (no bot) follow `SecurityInfo.bot_handoff`: `on` hands
  work over (asking when the bot can do more, as above), `ask` asks the user
  before every hand-off whatever the chat's mode, `off` refuses it (the tool
  isn't offered and the bots aren't listed). A bot with `askable: false`
  takes work from nobody. The server enforces both in `message_bot` itself.
  A plain chat's agent (no bot) asking a bot uses one shared conversation per
  bot (`members` = [recipient], titled "Your chats → @handle"); each of its
  messages carries `author: "chat:<session id>"` of the chat that asked
  (servers before batch 11 left `author` out there: clients show such a
  message as from one of the user's chats, since nobody types into a DM).
- **Hand-offs in the background** (batch 17): `message_bot` returns at once
  unless the model passes `wait: true` (subagents, jobs, rooms and a bot
  answering a bot always wait). Its result's `details` is `{ bot: "@handle",
  session_id: <the bot's conversation>, background: true }`, and while the bot
  works its conversation's `SessionInfo.working_for` is `{ session_id, call_id,
  since, from_seq }` (the chat and call that handed it the work; sent only
  while set). When the bot's turn ends, `working_for` is cleared and its reply
  lands in that chat as a user entry with `author` = the bot's id, whose text
  starts with `[Reply from @handle · <took> · session <bot conversation>]`
  and a blank line (then the reply, or why there is none: stopped by the user,
  cut off by a restart, failed); as steering while the chat works, else as an
  entry after which the chat runs a turn (not when it was stopped). `abort`
  or `delete_session` on the chat also stops the bots working for it;
  `abort` on the bot's conversation stops just that bot. A bot already
  working for a chat refuses that chat's next hand-off until it replied. Old
  servers ignore `wait` (they always wait) and never send `working_for`.
- **Rooms**: `create_channel { title, members }` → a `kind: "channel"`
  session with `members` and no `bot_id`. When the user sends a message, the
  @mentioned members reply in turn (all members if nobody is mentioned).
  A bot reply that @mentions another member hands the floor to it, up to a
  bounded number of bot turns per user message. User entries written by a
  bot carry `author`.
- **Who wrote a user entry** (`author`): absent, the user; a bot id;
  `chat:<session id>`, that chat's agent (its message to a bot, the task it
  gave a subagent: a bot's subagent gets the bot id instead); `job:<job
  id>`, a scheduled job's instructions at the start of its run; and the
  harness notes `goal`, `harness`, `subagent:<id>`. Models are told who
  wrote a message where it matters (`[@nova]: …`, `[the user's agent in the
  chat "…"]: …`); a subagent's task and a job's instructions read as before.
- `list_sessions` returns `chat`, `job` and `channel` sessions (subagent
  sessions are internal).
