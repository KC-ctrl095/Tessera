# Completion checks: internal protocol

The completion check prevents an agent from ending a turn with actionable work left.
It is not a message to the user. A ledger already explains what is done, blocked or
waiting, so there is no separate “Checked” footer or new setting.

## Entry and stream contract

- Native agents receive `[Completion check]` as a `harness` user entry.
- Claude Code receives the same instruction through the main agent's Stop hook
  `additionalContext`, without a preceding user entry.
- An acknowledgement is persisted with assistant `author: "harness:completion"`.
  Its model history and usage remain intact. A mixed acknowledgement plus `task`
  calls retains the tool calls but not the protocol prose.
- Snapshots and `entry_added` omit protocol entries at the server, protecting clients
  that predate the marker. Sequence numbers can have gaps. A check reply is held
  from live display until committed; substantive replies are still delivered.
  Short acknowledgement-shaped text prefixes outside a check are buffered until
  they become content. This avoids a transient bare mark while streaming.
- Rust clients share `tessera-proto` detection. The Solid UI has the corresponding
  `completion.ts` helper. A conservative old-server/history fallback recognizes
  bare ✓, ✔, ✅ and one-line Done/Checked/OK/Okay/Acknowledged with a mark. It does
  **not** hide meaningful text such as “✓ Tests passed”.

## Skip rules

The ordinary bound is one check per turn. A persisted digest includes just the
open plan/criterion text and statuses (sorted, retaining duplicates). Numbering,
closed steps, notes and evidence wording do not cost another model turn.

Blocked plan items are explicitly waiting and omitted from actionable plan work.
Unchecked/unmet criteria are evaluated independently: a blocked device step cannot
hide a fixable test criterion. The ledger has no per-item helper dependency links,
so an in-progress item is not considered waiting merely because any own helper
runs. Mark genuinely waiting plan work `blocked`; criterion `unverifiable` plus
evidence records a criterion that cannot currently be checked.

Native ledger checks also bypass the no-ledger tool/delegation heuristic: a helper
result or a plain post-restart answer cannot hide independently open ledger work.
The agent may acknowledge leaving the same open-state digest on purpose. That
cache (including any pending in-flight acknowledgement) is invalidated when this
chat receives an own-helper result, including before queued report admission, or
when a restart interrupts the chat or its own child. A persisted value-free
`claude_dependencies` marker also invalidates an idle Claude parent on restart
when a background command has no child session; unaffected idle chats retain
acknowledgements. Task start/removal persists the marker and final completion
clears it. It is not a roster or a public protocol field.

Claude Code's next main Stop can check after an actual tracked task transition.
An admitted native `agents` helper report renews that allowance only when its
exact main-chat UUID + content + FIFO echo confirms acceptance (`send_input` /
`accept_input`), not merely when saved/enqueued. Child/synthetic/foreign echoes,
duplicate source reports and duplicate/unknown task notifications cannot renew
it. Unaccepted reports retain the old resume boundary and force full ledger
redelivery, composing with confirmed-ledger receipts. Conversation rewind also
clears the cache. Pending/actionable work and independent criteria still check.

## Complete path map

| Surface | Production path / symbol | Treatment |
|---|---|---|
| Check construction and open-state comparison | `crates/core/src/tools/meta.rs`: `completion_note`, `completion_state` | Only explicitly blocked plan items wait; open criteria are independent. |
| Native check delivery | `crates/core/src/engine.rs`: `agent_loop`, `completion_check` | Check remains in model history, never in device transcript. |
| Claude Code Stop hook | `crates/core/src/engine/claude_code.rs`: `claude_hook`, `stop_output`, `claude_flush` | Main reply committed before deciding to check, so an ack updates the skip state; child Stop hooks do not flush the main reply. |
| Persisted marker (native, Claude, helpers and bot turns) | `crates/core/src/engine.rs`: `append`, `record_entry`; `crates/proto/src/completion.rs` | Ack stamped centrally; protocol omitted from `entry_added`. |
| Persisted skip state | `crates/core/src/store.rs`: `SessionInternal.completion_acknowledged`, `.completion_pending` | Internal-only optional state; no public wire-schema change. |
| Cached reasons after dependencies/restart | Core `engine.rs`: `invalidate_completion`, `record_entry`, `mark_interrupted`; `engine/subagents.rs`: `deliver_report`; `engine/claude_code.rs`: `accept_input`, `claude_dependencies`, tracked task completion | Own native report renews Claude Stop only on correlated acceptance. Runtime dependency marker expires idle background-command acknowledgements on restart, with no child required; duplicate/foreign transitions cannot renew. |
| Reconnect/history | `crates/core/src/engine.rs`: `snapshot` | Protocol entries and pending/ack streaming omitted. |
| Live text, native and Claude | `crates/core/src/engine/deltas.rs`: `send`, `finish`; both engines call `finish` before stream end/flush | Check replies and ambiguous prefixes held, substantive text released; prefix examination bounded to 32 characters. |
| Web/desktop/phone/Fold transcript | `apps/tessera/ui/src/lib/completion.ts`, `transcript.ts`: `deriveItems`; `components/chat/Transcript.tsx`; `Messages.tsx` | Source marker and legacy fallback; no mark, harness check row or Checked footer. Real answers/tools after a check remain visible. |
| Copy answer and message count | UI `transcript.ts`: `turnText`; `components/chat/Transcript.tsx`: `messageCount` | Protocol is not copied or counted as an answer. |
| TUI committed/live transcript | `crates/cli/src/tui/transcript.rs`: `quiet_checks`, `last_assistant_text`; `tui/ui.rs`: `draw_live` | Marker and fallback independent of a preceding native check entry; last-answer copy skips it. |
| Terminal `tessera run` output/report | `crates/cli/src/run.rs`: event processing | No protocol entry becomes final output (including JSON); source and legacy stream-prefix buffers prevent marks from printing live. |
| Chat previews | Core `record_entry`; rewind preview reconstruction in `engine/rewind.rs`; UI `store.ts`: `normInfo`; TUI `app.rs`: `session_search_text` | Last substantive answer retained; old-server bare-mark previews sanitized. |
| Titles | Core `generate_title` / `local_title` | Generated from the first user message, not the last assistant answer; no ack input. |
| Finished notifications | `crates/core/src/engine.rs`: turn-end notification; `engine/notify.rs`: `last_words` | Last substantive answer selected; ack-only turns cannot produce a generic Finished alert. |
| Agent/scheduled/missed notifications | Core `engine/notify.rs`: `send_notification`, list, due delivery, pending | Protocol rejected on send, filtered from old stored notifications/replay. |
| Browser/app UI notifications and scheduled list | UI `lib/alerts.ts`: `receiveNotification`; `components/settings/NotifySections.tsx` | Old-server protocol is neither toasted nor shown in scheduled settings. |
| Native OS notifications, including Android background pull | `apps/tessera/src-tauri/src/notify.rs`: `agent_message`, `session`; `notify/pull.rs`: `decide` | Old-server marks acknowledged without OS posts; meaningful preview preserved. |
| Helper reports / agent tool reports | `crates/core/src/engine/subagents.rs`: `last_answer`, `child_answer`, `subagent_report` | Marker and legacy fallback skip the ack; earlier substantive report returned. |
| Bots, rooms and bot handoff replies | Core `last_reply_by` and its callers (`channel_loop`, `bot_reply_text`, handoff engine) | Same marker/fallback filtering; an ack cannot become a bot's report or trigger mentions. |
| Recall and search | Core `tools/meta.rs`: recall entry/window/search; `store.rs`: `searchable_text` | Protocol absent from new full-text index and filtered from old hits/history. Raw DB/model history remains internal. |
| Checkpoint, recovery and memory-review summaries | `crates/core/src/context.rs`: `transcript` | Protocol omitted before summaries or report text are built. |

## Regression coverage

- Core unit tests: independent criteria under blocked plans; actual parent helper
  activity, queued finished reports with another slow helper, stale in-flight marks,
  idle-parent child restart invalidation, semantic digest and narrow ack detection.
- Native mock-model integration: raw marker persistence, clean events/snapshots,
  unchanged-note skip, changed-status recheck, real checked answer retention,
  notification rejection/history filtering and ack-only automatic-alert suppression;
  blocked-plan criteria, foreground delegation and interrupted plain-answer recovery.
- Claude fake CLI: a Stop acknowledgement without a user check entry is internal;
  unchanged/new-note turns do not re-ask. Independent criteria under a blocked plan,
  two own helpers with one report ready, replayed notifications and interrupted
  recovery are covered through real fake-process Stop hooks. Native bridged
  `agents start` reports test one live CLI, unconfirmed/confirmed/duplicate echo,
  lost input and resumed full redelivery. Idle outstanding background Bash tests
  persist/expire reasons on restart without a child, alongside an unaffected
  idle chat. Unknown stop-storm notifications and disabled checks remain bounded.
- CLI: legacy streamed marks are buffered for terminal/JSON output; a substantive
  checked answer releases its held prefix exactly once.
- TUI: marker/legacy ack without a preceding check is neither a row nor last answer;
  meaningful checked text remains visible.
- Native notification unit test: live/scheduled legacy marks produce only Ack,
  not Post/WakeAt; Android pull also omits them.
- UI `completion` and updated `extras` suites: desktop, ~344-pixel phone and open
  Fold in light/dark, both engines, helpers, source markers, copied answers,
  old previews, alerts, scheduled notifications and meaningful checked text.
  Successive old-server stream deltas buffer only an ack prefix, then show real
  short answers (including “✓ Tests passed”) before any assistant entry commits.

A real-Claude live run is a release verification step. Fake-CLI coverage does not
claim a tested result on a user's subscription, phone install or Windows runtime.
