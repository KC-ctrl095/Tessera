# Notifications

## The default

Tessera alerts only for something the user needs to do, something they asked to hear about, or a user-facing reply that finished while nobody was looking at its chat. No tool-step, helper-report, progress, restart, reconnect, or routine successful-job popups.

* Approvals and questions have a two-second grace period. Answering immediately creates no alert. A secure password-entry request gets a generic action-needed alert without credential values.
* One final reply per root turn. Helpers and bot-to-bot messages never create their own completion alerts. A failed or blocked task that needs the user alerts even if it was short. Recoverable automatic retries and usage-limit waits stay quiet.
* `notify` and `tessera notify` remain explicit requests. The tool reports when the server saved an item quietly rather than claiming it pushed.
* Routine successful or user-stopped scheduled runs stay in task run history, not the bell. Failed, paused and interrupted runs can alert. Schedule completion is inbox-only.
* Jobs share a stable source identity across their per-run chats. Repeated content, including case, whitespace and timestamp-only changes, goes quietly to one updated inbox item. Changed nonurgent results are limited to one alert an hour per job. A new urgent failure bypasses that limit. Identical urgent failures wait fifteen minutes. A changed measurement or count is not normalized away. Message fingerprints and last-alert times survive restart, reads and clearing the inbox. Fingerprints store hashes, not duplicate message bodies. No new settings control these limits.

## One live card, not a stack

Live browser, desktop and Android alerts use the root chat key, or the scheduled job key across runs, rather than an event id. New alerts replace older cards. Native and browser burst throttles keep short bursts from repeatedly making sound. Android live and background delivery use the same replacement id and group. Its required ongoing connection indicator remains silent. A user-requested completed-download receipt also remains silent.

Opening a chat marks it and descendants read and withdraws posted cards on connected devices. Browser OS notifications are closed too, not only in-app toasts. A background phone withdraws them on its next poll. Revision ids remain distinct so an old read, clear or acknowledgement cannot consume a replacement. Modern shells stop inferring local completion, approval and transient-error alerts once they negotiate the server-owned policy. They retain a legacy fallback against older servers, except helper alerts are always disabled.

## Across devices

Authenticated apps send a foreground heartbeat every fifteen seconds, with the focused chat if any. It expires after forty-five seconds and is not persisted. With a live heartbeat, only the most recently active device receives live messages for other chats. Other devices and Android polls stay quiet. If the event's chat is focused anywhere, its alert is quietly kept rather than replayed later. The focused device still shows the conversation or its normal approval controls. Without a foreground device, opted-in devices may receive the alert, which is intentional so an unattended desktop cannot steal a phone alert.

Old clients need not send heartbeats and still receive the existing `notification` event. New fields have serde defaults. Automatic payloads still require `negotiate_notifications`, keeping old clients from mistaking a typed event for an explicit agent message. Privacy and visibility filtering happen before events, inbox lists, pending polls, acknowledgements and reads are exposed.

## Settings

The device choice is **On**, **Only when I'm needed**, or **Off**. On includes awaited replies. Only when needed excludes ordinary completion but retains approvals, failures and explicit requested messages. Existing off and finished settings map to those choices without a new config schema or extra category knobs. Legacy category preferences remain readable. Helper alerts are off regardless of old preference values. Existing preview, sound, quiet-hours, receiving-device and Android background-connection controls remain supported. Browser OS alerts remain opt-in and generic.

## Inbox and retention

The bell is silent history, not a second push mechanism. Explicit messages, meaningful job outcomes, approvals and merged helper reports can appear here. Routine machine availability, progress and successful frequent-job runs need no inbox entry. Quiet updates refresh the bell without a popup. Items are bounded to five hundred sent rows and thirty days. Future reminders are retained until sent or cancelled. Private-chat history is memory-only and scoped to the owning device. OS payloads are generic unless native previews are enabled, and always generic for private chats.

`source_key` identifies a scheduled job without exposing its prompt. `pending_notifications_v2.withdraw` carries bounded stable tags for background withdrawal, including replacement or cleared revisions that no longer exist. Public acknowledgement tag metadata is bounded to five hundred per device. Private chat tags are not persisted in that metadata.

## Verification

The policy tests exercise waiting approvals, short and long replies, helpers, transient retries, foreground ownership, heartbeat expiry, replacement revisions, scheduled repeat and burst limits, urgent failure exceptions and restart behavior. Native no-Tauri tests exercise server-owned versus legacy inference, tag replacement, cross-device withdrawals, poll de-duplication and compatibility. UI e2e covers the bell/inbox and plain settings at desktop and 344 px, light and dark. Real Android scheduling, notification permission behavior, desktop daemons and Windows toast delivery still require real-device checks.

The L5 inventory and scripted scenario report live in the launch evidence folder, not in the product prompt. No production data is used in tests.
