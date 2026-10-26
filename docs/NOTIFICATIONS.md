# Notifications

Tessera has one inbox and one server decision for every alert. Open the bell for the source, level, reason and repeat count. Settings → Notifications controls the shared policy, digest times, source choices and device delivery.

## Three levels

| Level | What happens | Defaults |
|---|---|---|
| Page | A system alert that can buzz | An approval or secret request waiting for you, an error that stops work, an urgent item, or a requested long reply while away |
| Digest | Stored now, included in one summary without sound | Bot messages, scheduled checks and progress |
| Silent | Inbox only | Helpers finishing and routine status |

The default digest is daily at 18:00 in the server's time zone. Choose Hourly or Daily with up to four times such as `09:00, 18:00`. An empty digest sends nothing. Read items, muted items and private-chat items are not copied into the shared summary. Items are grouped by source and include short titles and report excerpts. Assembly and consumption are one transaction. A restart keeps the next due summary. Missed summary times produce at most one catch-up summary, not a burst.

The server records `level`, `source`, `source_key`, `reason` and `repeat_count`. Sources are bot, chat, job, helper, system or external. Job keys survive fresh run chats. Bot messages use the bot key, not the current run. A user-requested chat completion stays a chat source, even when its assistant is a named bot.

Only exact matches of source, kind, title, body and urgent flag are repeats. Case, times, numbers and changed text are not normalized away. An ordinary repeat updates the existing item with ×2, ×3 and so on without a second page. Distinct results remain in the inbox. Urgent items always retain their own deliverable revision, even when the text repeats, with a cumulative repeat count. There is no per-source hourly or per-chat send throttle.

The notify tool says what happened. It reports Page, Digest, Silent, a repeat of a specific alert id, or a scheduled delivery time. `urgent` is reserved for security incidents, outages lasting a few minutes and results the user explicitly asked for. Non-urgent bot messages go to Digest. A reminder set for later with `at` always alerts: the user asked for it. The server does not attempt to read a model's prose to verify an incident. An urgent flag is observable and an owner can turn that source down.

## Source controls and quiet hours

Every non-private inbox item offers **Mute this source** and **Less like this**. Less like this steps Page → Digest → Silent. Source choices are a ceiling, not a promotion. They apply to future items and pending summaries from that source. Settings → Notifications lists these choices with **Restore**. Restoring affects future deliveries, not pages already sent.

Quiet hours are off by default. Turn them on and choose From and Until using the server's local time. During quiet hours ordinary pages become Digest. Urgent pages break through. An explicit owner mute or downgrade still applies to urgent items. The test alert bypasses these source choices and device opt-outs so it can diagnose delivery. OS notification permission, Do Not Disturb and disabled Android channels still win.

Device choices remain **On**, **Only when I'm needed** and **Off**. They narrow the server's decision for that device. Previews remain off by default. Automatic OS alerts and browser OS alerts stay generic unless the app's previews are enabled. Private chats always have generic OS text and never enter a shared digest. Browser notifications are opt-in and only work while the browser is running.

## Looking at a chat

Foreground presence is per device, refreshed every 15 seconds and stale after 45. A desktop tile cannot make the phone's page quiet or mark an arriving item read everywhere. The device looking at that chat records a local acknowledgement so it will not buzz for it later. Entering a chat or explicitly marking it read can still clear it everywhere. A new item arriving while a tile is already open does not trigger that global read action. One exception: a reply that finishes while you are watching its chat (focused) on any device is kept silent everywhere, since you already have it, so your phone doesn't buzz while you read it on the computer. Approvals and other actionable items stay each device's own, and urgent items bypass viewing suppression.

The server sends only deliverable alerts on `notification`. Digest members and Silent items produce inbox invalidations without a system alert. A summary is `kind=digest`, `level=digest`, `quiet=false`, so it goes through the same event and Android background paths but uses silent OS delivery. Ordinary stored Digest members have `quiet=true`.

## Android and test delivery

**Send a test alert** in Settings uses the server's urgent path, now or after one minute. Device reports distinguish **Delivered to this device** from **Waiting for a device report**. Delivered means the platform accepted an OS post, not proof of a sound, network push receipt or that the person saw it. Suppression acknowledgements are not delivery reports. Old apps cannot report delivery and stay Waiting.

Android Page uses a new high-importance **Page** channel. Digest uses a low-importance **Digest** channel with sound and vibration off. Source keys replace stable OS cards. Urgent revisions may alert again on the same card. Existing channel settings and system permissions are not overwritten.

With the app closed, `PullJob` connects using the native encrypted client and polls `pending_notifications_v2`. It falls back for older servers. It runs about every 15 minutes and also arms a wake-up for a scheduled item. Deep sleep can delay jobs. Background posting saves accepted ids separately from deliberately suppressed ids, then reports only accepted posts on the next connection. A successful native foreground post reports immediately. No Firebase or Google push service is involved. An always-on connection can make pages faster at a battery cost.

Keep-alive defaults to **While busy**. **Always** keeps the Android foreground connection on after the app is closed and offers the battery exemption. **Off** leaves periodic background polling. A foreground-service start denied by Android never takes down the activity or discards the WorkManager fallback. A scheduled item retains its due time even when policy will put it in Digest or Silent.

## Compatibility and verification

New wire fields have serde defaults. Old clients ignore levels and still honor `quiet`. Typed automatic alerts, including summaries, require `negotiate_notifications` on every connection. Legacy explicit urgent messages still use the old path. Old servers reject the new settings calls and the UI leaves the existing device controls usable.

The synthetic replay models a busy week of 500 alerts (helpers, finished replies, scheduled checks, approvals, password requests and 26 urgent items); none of it is real content. It produces 49 Page-classified, 158 Digest and 293 Silent items (replies watched as they finished stay silent). With the phone itself viewing 160 routine items it delivers 32 phone pages, including all 26 urgent items. Keeping every urgent item deliverable makes fewer than 26 pages impossible for those flags, so fewer pages also needs fewer urgent flags; a real week is the real test.

Targeted tests cover defaults, first occurrences, exact repeats, repeated urgent news, quiet hours, source choices, per-device viewing, atomic and empty digests, restart persistence, delivery receipts and the synthetic week. Browser e2e covers inbox actions, source mute/downgrade, restoring sources, digest settings and test receipts at desktop, phone cover and Fold widths in light and dark themes. A physical closed-app Android test and a real week remain release acceptance checks.

## Homelab inbound design, not enabled or implemented

Do not point ntfy, Gatus or Wazuh at the agent RPC or reuse a device token. This batch deliberately adds no inbound HTTP endpoint. Adding a public bearer-authenticated write surface deserves its own security review. No external content is ever sent to an agent as instructions by this feature.

A follow-up can implement the following narrow contract.

- Off by default, returning 404 while off. Owner Settings creates, labels and revokes each source. No agent tool can enable it or grant paging.
- `POST /api/notification-sources/<opaque-source-id>/events`, TLS only except direct loopback, no cookies and no CORS. Authorization is a per-source random 256-bit bearer token shown once, never in a URL. Store only its SHA-256 hash and compare in constant time. Revocation takes effect before each write. Never log the authorization header or message body.
- Plain UTF-8 `text/plain` body, up to 4096 bytes before allocation. No HTML, markdown rendering, links, attachments, commands, chat ids or agent destinations. One bounded title can be derived from the first line, the rest is body. Validate controls with the existing plain-text limiter. Source name, identity and permission come from the token record, not the submitted text.
- Default level Digest. An optional `X-Tessera-Level` asks for page, digest or silent, but the server caps it at the owner-granted source level. A source cannot assert urgent to bypass its cap or quiet hours. Owner-granted security paging is a separate explicit permission.
- Per-source token bucket of 10 events per minute with a burst of 10, global 120 per minute, bounded concurrent body reads and 30-second header/body deadlines. Return 413, 415 or 429 with `Retry-After` as appropriate. Authenticate before reading the body. Failed tokens share a bounded IP/global limiter. Limit source count and stored events. Never print or return token hashes.
- Store `source=external` and the trusted source key with the same levels, repeat grouping, reason, retention, digest and inbox controls. Never start a chat, agent, schedule, tool or URL fetch on receipt. Keep audit metadata with ids, fixed result classes and counts only.
- Tests must cover off-by-default, missing/wrong/revoked tokens, constant-size failures, no accidental device-token acceptance, body/time/concurrency limits, plain text rendering, no agent dispatch, paging caps, quiet hours and exact repeats. A separate security review must sign off before enabling it.

For Gatus, use a custom webhook alert whose URL is the endpoint above, with `method: POST`, `Content-Type: text/plain`, the per-source Authorization header and `body: "[ALERT_TRIGGERED_OR_RESOLVED] [ENDPOINT_NAME] [RESULT_ERRORS]"`. Have Gatus wait several failed checks before requesting Page. Keep its token in the service's local secret store. Verify placeholder names against the installed Gatus version before rollout.

For ntfy, use a dedicated subscriber/forwarder service to read only the chosen topic and POST each message as plain text with the per-source header. Native ntfy HTTP publish/subscription JSON is not an inbox API or an instruction format. Give each producer a distinct token so muting Gatus never mutes Wazuh. Wazuh can use a narrow custom integration with the same POST contract. Do not configure these until the endpoint and its review exist.
