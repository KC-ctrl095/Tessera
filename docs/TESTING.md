# Testing

```bash
cargo test --workspace --exclude tessera-app     # 96 tests, ~10 s
cd apps/tessera/ui && npm run build              # UI typecheck + build
cd apps/tessera/ui && npm run mock               # UI against a scripted fake server (dev only)
```
## Real-server launch benchmark (opt-in)

`session-list.mjs` is in the normal mock e2e run. The heavier launch test is
separate. It starts a real `tessera serve` with a newly marked synthetic home,
empty HOME/XDG directories, no scheduler and a configured inert local model.
It never reads production data or credentials. Allow about 3 GB for the dataset
and more for the build. On a shared machine, run it at low priority so other work keeps going.

```sh
# Build the binary using your own CARGO_TARGET_DIR first.
timeout 3600 nice -n 19 ionice -c3 env \
  TESSERA_PERF_BIN=/absolute/path/to/tessera \
  PERF_WORK=/absolute/path/to/a-new-output-directory \
  PERF_PORT=18101 CDP_PORT=9441 \
  CHROMIUM="$HOME/.local/share/tessera-browser/chrome" \
  LAUNCH_PROFILE=1 bash scripts/launch-perf.sh
```

Run outside a `.git` directory, or rsync into a staging directory first as for
normal e2e. Dependencies must already be installed in the UI directory. The
runner refuses an existing output home. `LAUNCH_FULL=0` keeps all metadata but
only the five largest histories. Default full mode models a heavy long-term user: 6,774
sessions (chats, scheduled-job runs, helpers, channels), 489,230 entries and
approximately 2.9 GB of synthetic bodies. The five large chats have
17,844, 12,648, 11,480, 7,711 and 6,706 entries. `shape.json` records actual bytes.

The measurement build is a production Vite build with opt-in source probes.
It alone bypasses the bundle-byte check because instrumentation adds bytes.
A separate normal `npm run build` MUST pass unchanged startup and size gates.
No probes or bypass are shipped in normal builds.

Chromium uses encrypted Noise `/web/`, desktop and 390/344 px phone viewports,
4x/6x CPU throttling, browser-cache-cold and warm reloads, restored newest
messages and reconnect. JSON metrics record each RPC's response bytes and
client-observed duration, decrypt/parse, metadata installation, snapshot
installation/render work, long tasks, sidebar/newest paint, and out-of-renderer
CDP composer input-to-animation-frame latency. `LAUNCH_PROFILE=1` saves CPU
profiles. `ONLY=phone-4x` limits cases. `LAUNCH_CHECKS=1 ONLY=desktop` additionally
checks old pins, older sidebar search, helper counts, jobs, archive and bot/private
reconnect. Pairing is inserted only into the explicitly marked throwaway store.
The runner cleans up only its own server and start-identity-verified browser.

For exact list-store read and serialization timings, stop that throwaway server
and run the ignored Rust component benchmark below. It is not whole-RPC server
time. Chromium RPC duration includes transport, scheduling, decrypt and parse.

```sh
TESSERA_LAUNCH_STORE=/your/output/home cargo test -p tessera-core --lib \
  launch_store_component_timings -- --ignored --nocapture
```

For old-server fallback, run normal mock smoke/history tests with
`MOCK_LEGACY_SESSION_LIST=1`. New mock servers negotiate paging by default.

## Automated suites

| Suite | Where | What it proves |
|---|---|---|
| Unit | `crates/*/src/**` | edit engine (line anchors, stale tags, fuzzy replace, CRLF), grep/glob, command policy, cron parser, config, store/FTS/pairing, SSE and provider body shapes (Chat, Responses, Anthropic), context view/pruning/budget, memory limits, skills parsing, prompt assembly, TUI rendering/editor/markdown (46 in the CLI) |
| Engine end-to-end | `crates/core/tests/e2e.rs` | the real engine against a scripted OpenAI-compatible mock model |
| Long-horizon harness | `crates/core/tests/harness_long.rs` | checkpoints written while the agent keeps working (and waited for only at the hard limit), Stop mid-compaction saves nothing, ledger updates keep the prompt prefix cache-stable, `task` confirms in one line with bounded lists, cut-off replies retried and kept visible, step-limit progress checks and the loop-aware pause, messages reaching running subagents |
| Verification and loops | `crates/core/tests/verify_loops.rs` | one `[Check]` note when code changed and nothing checked it (not after a check, not when told not to run anything, not for docs; strict/off), quick syntax checks, the check note in place of the completion check, a repeating reply cut and nudged once, an announced action with no call nudged once, near-duplicate failing commands warned and blocked, per-file edit counts, the closest block on "no match", "already applied", `apply_patch` from GPT models only; live: `live_verify.rs` |
| Bots end-to-end | `crates/core/tests/bots.rs` | validation/uniqueness, persona + tool presets + authorship in bot chats, private memory with a shared user profile, private bot-to-bot messages within permissions, channel @mention routing and hand-offs, duties running as the bot, recall privacy, YOLO alias |
| Server/client | `crates/server/tests/ws.rs` | real sockets: pairing, endpoint fall-through, proxy headers, typed errors, revocation, 401s, brute-force throttling |
| Phone route-follow | `crates/client/src/lib.rs`, `crates/server/tests/route.rs` | saved order and last-success preference; health, upgrade and encrypted login raced against a stalled LAN address; idempotent route refresh without reconnect; user-owned headers/addresses and legacy config preserved; Access headers excluded from HTTP and TLS LAN hosts |
| Phone UI | `apps/tessera/ui/tests/e2e/{mobile,phone-fixes,remote-cf,devices,setup,smoke,chat-ui,offline-models,stop-helpers,claude-tools,help-legal}.mjs` | cover/Fold layouts in both themes; shell startup keeps the connection; repeated route notifications keep the workspace, composer and draft; Stop notices and helper cards; keyed Devices refresh keeps focus and scroll; licences fit and Back dismisses them |
| First-load budget | `apps/tessera/ui/build/first-load-budget.mjs`, `tests/first-load-budget.test.mjs`, `tests/first-load-budget-build.mjs` | Final emitted HTML seeds scripts, module/script preloads, inline JS and their static **and startup dynamic** dependencies anywhere in the closure (including JS assets/workers), counted once from disk. Unknown/external/missing code fails closed. `npm run build` gates static launch at **610 KiB** and paired encrypted-browser cold start at **670 KiB** (static closure + `lib/e2e` and its pre-chat imports). Onboarding (browser and native Setup) and **FileWindow shell only** totals are printed but ungated; negative fixtures and the real-build HTML/manifest proof cover both gates. |
| App polish (b27) | `apps/tessera/ui/tests/e2e/{polish,desktop-ui,polish-targets}.mjs`, `tests/{photo-bytes,snippet}.test.mjs` | desktop, cover screen and unfolded Fold in both themes: text contrast and accent button ink; horizontal tables; edge tooltips; offline send/header; separate server-backed voice/search saves with delayed-response edits; pairing; literal highlighted search/note previews; schedule round-trip; freedom and queue controls; actual 40px+ hit-tested fine/coarse targets and coordinate click/tap effects; three named mosaic tabs and server-empty Bots → Add new bot → create; hashed camera chunks |

The paired-browser limit is fixed, not recalculated at each build: `2b74376` measured **681767 bytes** including the HTML's 372 inline bytes; 670 KiB leaves **4313 bytes (0.63%)** of documented headroom. This ordinary, already-paired `/web/` launch awaits encryption before chat can show. The gate excludes later user-triggered overlays and idle/post-chat preloads, not the encryption startup path. `LAUNCH_MODULES` selects the conditional roots and `build/startup-edges.mjs` explicitly documents excluded conditional/post-chat edges by source and mode. **Unclassified dynamic edges anywhere in a static or dynamic startup dependency are counted by default**, not skipped. FileWindow is labelled **shell only** because the separate TextView is requested after the opener hands it a file. If startup changes, review this contract and both-gate negative fixtures; never silently call a new pre-chat branch optional.
Deferred imports are audited by **importing module and specifier/count** in `build/startup-calls.mjs`, not by destination alone or literal source prefixes. The proportional AST guard requires each exempt import to be inside a function, and rejects a loader or lazy component's `.preload()` called from a module's top-level statement (including `void` and IIFEs, with direct named/namespace imports checked across modules, named function expressions bound to both their variable and internal name, and optional member/call forms recognized). Emitted per-owner edge metadata (`startup-edges.json`) must match the owning chunk; late unaudited or duplicate imports count, including imports from shared static dependencies. Negative fixtures cover top-level imports/loader calls/preloads and the same-target escape. This is not whole-program effect analysis. The **real-network paired cold-start test** in `tests/e2e/built-ui.mjs` is the independent backstop and is required on the release checklist.

`tests/built-output-run.mjs` serves actual emitted UI files and a throwaway real encrypted server, records the paired cold-cache `/web/` network set until chat insertion, and compares it to the paired gate file set. Built CSS/JS and URL-less helper failures prove local Retry repairs the helper itself using an **inlined bundler-emitted URL**, validated for app origin/asset path (never parsed from error text), then awaits CSS and code together. Voice uses the SAME `onDemand`/`parts.json` recovery mechanism: an emitted voice failure followed by a second Record is tested, while `voice-lazy` and `voice-intent` prove cancellation/unmount/chat or server replacement abort the one `RecordingIntent` token. Capture checks that token before permission and immediately after a grant; a stale grant stops all tracks before recorder construction, and constructor/start failures also release every track. Tests include chat replacement without Composer unmount, delayed import/grant, and discarding a live recording without sending transcription. A second real build changes entry/card hashes. Stale evidence is typed (`updated`, `unchanged`, `unknown`), latched, and probed with a 2.5-second timeout (`tests/on-demand.test.mjs`). A stale build hides futile Retry. Reload writes and verifies both workspace/pane identity and composer text before navigating. Private text, unsent attachments/pastes, pending preparation, or storage failure block Reload and omit the “drafts kept” claim. All retained non-empty memory-only private drafts block Reload, mounted or not. The banner names each chat and offers Open/Discard (a draft from another server must be opened after switching back to that server). Default Copy/Save stays scoped to non-private blocking composers; private text is included **only after the user selects that named draft**, with an explicit private-data warning. A private-hero-to-public-chat fixture proves retained text stays protected and Open restores it. Save uses existing `saveText`/`saveBlob` native/browser handling, with success/error feedback and cancel leaving state untouched. Pending attachment preparation is registered synchronously; a delayed image-decode fixture blocks Reload and backup until it finishes. Backup never automatically reloads or authorizes dropping memory-only state. `reload-safety` exercises native save success/failure/cancel and privacy scope. Run after `node tests/first-load-budget-build.mjs <proof.json>` with `BUDGET_PROOF=<proof.json>`, isolated ports and normal e2e environment.
Finish releases every microphone track synchronously immediately after calling `MediaRecorder.stop()`, with idempotent release independent of `done`. The Composer retains the recorder handle until `onstop` or a 2.5-second timeout; identity changes/unmount cancel that pending finish without sending transcription. Real-source units and `voice-intent` cover delayed and missing `onstop` after Finish. Attachments, staged handoffs and pending preparation are memory-only and keyed by **server/pane/chat identity**, including unmounted composers. One Discard removes that identity's text, attachments, pending state and private marker; late preparation cannot resurrect it. Open never copies, moves or merges text: it focuses the own identity or reopens its original pane key, only replacing a chat destination with no text, attachments or pending preparation. An occupied destination yields a warning and no changes. Same-session drafts in other panes are not reused. `reload-identity.test.mjs` exercises the actual memory/Open/Discard functions with Solid browser-store semantics; `reload-safety` covers mounted/retained attachment Discard, an attachment-only destination and same-session collisions, preserving both drafts.

`recall-lazy` intercepts the real recall import and proves wrapper-owned expansion survives arrival, plus same-page failed-import Retry with intact draft/neighbors and Reload for stale chunks. `rewind-lazy` proves the loading/error host is cancellable, Retry recovers without a page reload, a cancelled dialog never resurrects when the chunk arrives, and no non-dry-run request occurs before explicit confirmation.
Recovery buttons are at least **44 × 44 px**; `recall-lazy` asserts bounds and no horizontal overflow on 344px phone, Fold and desktop in both themes. Microphone capture is deferred until Record is pressed to offset the already-loaded recovery bootstrap without loosening either cap.

## Session lock-order regressions

```sh
cargo test -p tessera-core --lib engine::lock_order_tests -- --nocapture
cargo test -p tessera-core --test restart --test security --test subagents_v2 --test e2e
```

The session-map lock must never be held while waiting for a session's state.
Clone the live `Arc<Rt>` references under the map lock, release it, then read
states. A helper may hold its state while looking up its parent's permissions
or hold its parent's state while creating a session. Trying the parent's
state lock does not make that map lookup nonblocking. Keep permission lookup
fail-closed. `save` snapshots state, releases both state and map guards, and
persists with an UPDATE-only transaction. A finishing worker cannot recreate
a deleted stored or private chat, and a slow disk write cannot own the live map.

The two list regressions gate those actual paths with per-engine test-only
barriers. Each runs in a separate subprocess with a 15-second deadline enforced
by its parent process, not by Tokio. On blocked completion an independent
thread checks parking_lot's lock cycles. The fixtures only create sessions;
they never call a model. Companion tests cover parent ceilings, save after
delete, live child snapshots, planned-restart IDs and private-device filtering.
Run under the host's heavy-job/resource limits with an isolated test home.

## Worker starvation and stall breadcrumbs

```sh
cargo test -p tessera-core --lib store::stall_tests -- --nocapture
cargo test -p tessera-core --lib activity::tests
cargo test -p tessera-core --lib blocking::tests
cargo test -p tessera --bin tessera watchdog::tests -- --nocapture
```

The raw store probe deliberately occupies all four runtime workers with
synchronous waits on the database mutex. Its timer must miss a 500 ms deadline.
The fixed probe sends four real store operations (history, entries, KV write,
notification scheduling) through the same contention; its timer must run.
A third probe contends on the parsed-history cache while four memory-report
readers call `history_cache_stats`; its timer must also run. All probes are
externally bounded subprocesses and unblock their holder before asserting,
so a failed regression cannot strand the test runner.
A session snapshot test covers a current-thread runtime; a second SQLite
writer proves that a waiting save does not own the live-session map.

Store entrypoints use `block_in_place` on multi-thread runtimes: the scheduler
core is handed off before SQLite/mutex/disk work, without changing synchronous
tool APIs. Parsed-history cache guards are released before SQLite version
queries and history reads, so memory reports do not inherit disk waits.
On current-thread runtimes this handoff is impossible; callers must
use `spawn_blocking`, as GetSession and ListSessions now do. This is not a claim
that every synchronous file/tool path is offloaded, nor a fix for a genuine
logical lock cycle (the lock-order regressions cover that separately).

Watchdog reports include bounded, reusable per-thread atomic breadcrumbs.
An `active` name means the thread is inside that operation's poll or synchronous
store call; `last` means only the most recent monitored operation, not a current
wait. Poll guards are cleared on Pending, cancellation and panic, and follow
worker migration correctly. Labels contain only fixed request/task/store names
and allowlisted tool names, never arguments, session IDs, paths or output.
Unknown tools use a generic label. No stuck-code mutex is needed to read them.
Parking_lot reports each lock cycle only once: "none newly detected" must not
be interpreted as proof that no deadlock exists.

Phone regression checks (batch 18):

```sh
cargo test -p tessera-client
cargo test -p tessera-server --test route
sh apps/tessera/ui/tests/e2e/run.sh mobile phone-fixes remote-cf devices setup
sh apps/tessera/ui/tests/e2e/run.sh smoke chat-ui offline-models stop-helpers claude-tools help-legal
```

On a shared test host, limit concurrent builds and browser runs to its available
resources. The phone e2e shell is an Android-user-agent stand-in in
Chromium, not a native APK: verify a cold launch on mobile data, Wi-Fi/mobile
handover, foreground resume and folding on the real phone after building the
APK elsewhere. Never use the production server or its saved config for these
automated checks. In the mobile test, a double-tap sends its touch events
without the CDP `tap()` helper's menu-settling waits: two of those waits can
exceed the app's 320 ms gesture window on a software-rendering host.

## Brief acceptance checks

| Check | Status | Evidence |
|---|---|---|
| TEST-001 fold/unfold keeps conversation and draft | ✅ manual | Pixel Fold emulator: typed a draft unfolded, folded → outer display, draft and session intact |
| TEST-002 two chat panes stay independent | ✅ manual (browser) | UI tiling verified at 1440×900 / 884×1104; per-pane session + draft state |
| TEST-003 close client mid-task, reconnect, recover | ✅ | `get_session{since_seq}` resync; TUI verified against a server that was killed and restarted; web UI verified with forced socket drops |
| TEST-004 interrupt and resume without blind repeats | ✅ | `interrupted_tool_calls_are_reported_after_restart`, `abort_stops_a_running_command` |
| TEST-005 secrets out of prompts/logs | ✅ | `secrets_are_redacted_from_tool_output`; tokens never reach the web view (Tauri side) |
| TEST-006 bot access separation | ✅ | `crates/core/tests/bots.rs`: bots can only message allowed bots (the tool isn't even offered otherwise), cannot recall other bots' conversations, keep private notes, and act with their own tool presets |
| TEST-007 provisioned environments | ⏸ | Kasm/VMs not started |
| TEST-008 completion only with evidence | ✅ live + automated | one `[Completion check]` per turn when the ledger has criteria or open plan items, `task.verify` records evidence (`completion_check_asks_once_then_the_turn_ends_after_verify`, `no_ledger_means_no_completion_check_and_no_extra_request`); live qwen3.8-27b verified its criterion with `xxd` output as evidence (`live_harness.rs`) |
| TEST-009…012 whiteboard/channels | ⏸ | pane types reserved |
| TEST-013 terminal with no GUI | ✅ | `tessera run`/TUI; all engine tests are headless |
| TEST-014 minimal core install | ✅ | one binary; MCP/skills/jobs are configuration, not dependencies |
| TEST-015 compaction keeps goals/constraints | ✅ live + automated | `ledger_survives_compaction_and_checkpoint_is_injected`; live qwen3.8-27b run with a 24k cap: after compaction the agent recalled the secret word and constraint from the first message verbatim |
| TEST-016 resume across restarts | ✅ live + automated | unattended jobs and `auto` chats resume with a recovery trace (completed / uncertain / not run), interactive chats wait (`unattended_work_resumes_after_a_restart_and_chats_wait`); live: a job interrupted mid-command resumed in 2 s and checked the command's effect before anything else |
| TEST-017 short tasks stay cheap | ✅ measured | "capital of France" end-to-end 0.5–0.9 s on local qwen (engine start + model call), no ledger/planning |
| TEST-018 competitive benchmark | ⏸ | not started (DEC-019) |
| TEST-019 terminal branding degrades cleanly | ✅ | `NO_COLOR`/`TERM=dumb`/`--plain`/narrow widths → plain header; render test from 0×0 to 250×70 |
| TEST-020 skills only for real procedures | ✅ automated + live | `crates/core/tests/skills.rs` (scripted model): no skill without real work, one per turn, thin and near-duplicate ones refused, the user's own request relaxes the work and size checks, a quick chat's review is not offered the skill tools; unit tests in `skills.rs`; live below |
| TEST-021 skills can be deleted in Settings | ✅ | `skills.rs` (`DeleteSkill`/`ReadSkill`: only listed skills, never a root, links not followed) and e2e `skills.mjs` (grouping, confirm, cancel, error, empty state; desktop and phone, dark and light) |
| TEST-022 chat first, tiles when asked | ✅ live | `live_choices.rs`: 11 questions an agent could answer in chat open no tile; 4 requests for a web view, whiteboard, login or local app open one |

## The installer

```bash
sh scripts/test-install.sh          # every case (builds Tessera twice: slow the first time)
sh scripts/test-install.sh quick    # only the ready-made-build cases
```

Runs `scripts/install.sh` the ways people do, in a throwaway home (`systemctl`
and `sudo` are stand-ins that only write down what they were asked): from a
copy of the source with `--yes </dev/null` (as production is updated: the
`KillMode=mixed` unit migration and the service restart), piped from curl with
a ready-made build from a local release (checksum verified), a download that
doesn't match its checksum (nothing installed), questions answered from the
terminal, the fallback to building the release's source, and Chrome for
Testing updates. CI runs it with `shellcheck`. The README's pictures come from
`sh apps/tessera/ui/tests/e2e/run.sh readme-shots` (see `docs/images/shrink.py`).

## Measured footprint (release build, x86_64 Linux)

| Metric | Value |
|---|---|
| `tessera` binary | 30.2 MB (stripped, LTO thin; 11 MB as the release's `.tar.gz`), October 2026 |
| Startup (`--version`) | 1.2 ms median |
| Idle `tessera serve` | 19 MB resident, 2.8 MB private, 0.008 wake-ups a second (a fresh server after its first quiet minute; 21 MB, 6 MB and 0.15 before batch 14) |
| After three long chats (3 × 15 turns, 6 commands of ~29 KB output each, fake provider) | 29 MB private right after, 18 MB a minute later (40 MB and staying before batch 14) |
| A streamed answer (2,000 pieces at 100 a second) | about 0.74 s of server CPU for one watcher, 1.1 s for six; ~510 messages per watcher instead of ~2,015 |
| Android release APK (arm64) | 10.2 MB |
| Web UI | entry + preloaded chunks + CSS: 892 KB raw, 267 KB gzipped (`tessera serve --web` now sends text files gzipped; a first visit took 1,320 KB over the wire before) |
| Base prompt (system + tools) | ≈ 5.2k estimated tokens (with `lsp` more); ≤ 5,260 enforced by `base_prompt_and_tool_specs_fit_the_budget` (`tests/code.rs`) |
| Small model mode prompt (system + tools) | ≈ 2k estimated tokens, 13 tools; ≤ 2,500 enforced by `small_mode_prompt_tools_and_requests` (`tests/small_mode.rs`) |

Measurement method: throwaway servers (temporary `HOME`,
`TESSERA_HOME` and XDG folders, a free port), a fake OpenAI-compatible provider,
the kernel's counts from `/proc/<pid>/status` and `/proc/<pid>/stat`, repeated
and interleaved with the earlier build while the machine was otherwise quiet.
`tessera doctor` shows what a running server's memory holds.

## Live checks with a local model

With an OpenAI-compatible server on `127.0.0.1:1234`:

```bash
tessera run "The tests in test_calc.py fail. Find the bug, fix it, and verify."
```

Observed with qwen3.8-27b: parallel glob/read → edit → test run → correct
fix in ~8 s. Android (phone and foldable emulators) paired over the emulator
host link, ran a tool-using prompt end to end, and rendered the result.

Long-horizon run (a six-step package build under a small context cap, which
forces repeated compaction and checks that the result passes its tests and
that a standing rule survives every compaction):

```bash
TESSERA_LIVE_MODEL=local/qwen3.8-27b TESSERA_LIVE_CAP=32000 \
  cargo test -p tessera-core --test live_long -- --ignored --nocapture
```

Observed with qwen3.8-27b at a 32k cap: 10 min, 33 tool calls, 2 checkpoints
(17.8k→5.2k and 18.9k→4.3k tokens, the second smaller than the first), all 10
Python files kept the standing first-line rule, tests passing, `NOTES.md`
complete, with no nudges. At a 14k cap (about 5k tokens of working room) an
earlier build compacted almost every step but still kept the rule through 8
checkpoints. That run led to the reasoning-first stage and checkpoint
condensing.

Skill creation and tile use (the same local model; `live_skills.rs` runs the
memory review on scripted chats through the engine, `live_choices.rs` sends the
real system prompt and tools to the model with canned tool answers, and
`PROBE_LEGACY=1` swaps the tile and skill wording back to what it was):

```bash
TESSERA_LIVE_MODEL=local/qwen3.8-27b PROBE_RUNS=2 \
  cargo test -p tessera-core --test live_choices -- --ignored --nocapture
TESSERA_LIVE_MODEL=local/qwen3.8-27b \
  cargo test -p tessera-core --test live_skills -- --ignored --nocapture
```

Observed with qwen3.8-27b, 68 runs each (2 per scenario): the old wording
opened a whiteboard unasked in 1 of 22 chat-only runs and let the review save
a skill for converting HEIC photos in 1 of 28 runs on trivial chats (20 reviews and 8 inline); the tuned
wording did neither in any run, still opened a tile in all 8 runs that asked
for one, and the review saved a skill in all 8 runs on chats that worked a
procedure out (a Canvas API walk, a blog deploy, a release checklist, a
requested standup routine). Engine-driven, the 7 trivial chats made no skill
and the 4 worked-out ones each made one (and nothing was asked of the user).
That model was already restrained, so these runs show no regression more than
a large gain; the checks in `skills.rs` are what holds when a weaker model is
not.

Swarm (8 background subagents in one call, more than can run at once, so
some queue; the parent must combine all 8 reports):

```bash
TESSERA_LIVE_MODEL=local/qwen3.8-27b \
  cargo test -p tessera-core --test live_subagents swarm -- --ignored --nocapture
```

Observed with qwen3.8-27b on a single-slot llama.cpp: all 8 reports and the
correct total in 24–31 s over three runs. One earlier run timed out while the
slot was busy elsewhere, so the test now prints each subagent's state every
minute. The mock suite covers the rest: 24-subagent swarms where the parent
resumes once per batch, report excerpts and `agents report`, isolation
between conversations, and budgets that start only when a subagent gets a
slot.

## Security checks

- `crates/secure`: pairing agrees only on the same code and rejects reflected
  messages. Sessions detect tampering, replay, reordering, oversized messages
  and a wrong server key.
- `crates/server/tests/ws.rs`:
  - encrypted pairing and sessions end to end;
  - wrong codes cancel the code;
  - plain access is refused from proxies and tunnels;
  - `a_proxy_in_the_middle_sees_only_ciphertext`: a recording relay sees no
    token, key, code, name, title or method, with a plaintext control that
    shows the check works.
- `crates/core/src/sandbox.rs`: secrets are hidden however they're addressed.
  Skills stay readable but read-only. `/proc/<pid>/root` and `environ` of
  outside processes and `umount` are denied. Nothing leaks back to the real
  filesystem.
- Working folders in the sandbox (`tools/file_object.rs`, `tools/bash.rs`,
  `tests/sandbox_shared.rs`): paths relative to the working folder see the
  sandbox's view; a hidden, replaced or symlinked folder runs nothing. The
  shared native launcher receives only device/inode values, stays outside the
  group's PID namespace until entry is checked, and never imports shell
  functions. Tests stop that launcher and try concurrent same-group descriptor
  capture, and inject exported printf/other functions plus shell startup files
  into refused launches, checking for visible side effects.
  A constructor-library regression passes LD_PRELOAD/LD_AUDIT/tunables but
  records execution only in the requested program, never the trusted loaders or
  refused launch. Lifecycle tests cover launcher death, stopped launcher timeout,
  Stop/group kill and descendants; exit/signal statuses; PTY foreground input and
  process accounting; full/one-shot mock Claude turns. No paid model call.
  Exact-head acceptance builds the actual CLI and sets TESSERA_TEST_LAUNCHER_EXE
  to it, so the real main dispatch is tested, not just the example.
  Anchor accounting and tty/job-signal checks require live anchors to remain
  separate from the program/status pid and ignore ordinary terminal signals.
  Bounded readiness tests SIGSTOP or kill either watchdog before readiness or
  while work runs, including launcher loss with a confirmed stopped watchdog.
  The completion regression uses the production contract: the launcher retains
  its exited leader unreaped while the fallback is killed, then resumes cleanup.
  Background work and both anchors must disappear including zombies, while an
  unrelated sentinel survives. It does not promise cleanup after an early-reaped
  leader removes production's identity-pinned group fallback. A requested program
  named tessera-anchor must still be selected, counted and traversed. Only private
  registration plus retained pidfds excludes actual anchors. Joined Claude
  integration expects successful checked-folder entry on the same group-owned
  launch path, not refusal or setsid/double-fork subreaper containment.
  Both anchor indices are exercised, with /proc State T confirmed before launcher
  loss. A separate pause after the primary's normal exit covers the last fallback.
  Construction errors after zero, one and two forks expose each created identity
  through a private test-only stream and require /proc disappearance within eight
  seconds, including zombies. Failure-only SIGCHLD ignore lets the kernel reap
  without rollback poll/wait loops blocking synchronous spawn on a Tokio worker.
  The joined-peer regression starts two commands in the same shared namespace and
  skips only when unforced user namespaces are unavailable. Forced lanes still
  fail rather than skip. The peer
  stops the child's trusted pre-exec and confirms State T; the launcher must
  refuse within its own exec-error-pipe deadline and clean owned work. The trusted
  child is non-dumpable until exec; ordinary exit/signal/PATH/background-server
  semantics remain unchanged.
  Memfd tests cover absent/bad/low/closed fds, non-memfd descriptors, every missing
  seal, empty/maximum/oversize/unterminated/duplicate payloads, independent offsets
  on duplicate fds, closure on success/refusal and non-inheritance by the program.

Sandbox tests need user namespaces. Where they are unavailable (for example,
restricted containers or remote commands), tests skip with a `SKIP …` line on stderr.
A check run that must exercise the sandbox says which mode, and then a skip fails the test:

```sh
# Test-only native entry companion (the same code as CLI dispatch).
# Build once per checkout/target before running shared-namespace unit tests.
cargo build -p tessera-core --example sandbox_launcher
# Landlock ABI 2+ (Linux 5.19+): each command gets its own namespaces
TESSERA_TEST_SANDBOX=direct cargo test -p tessera-core --lib -- sandbox file_object bash
# older kernels (or pretend): a chat's commands share one namespace
TESSERA_TEST_SANDBOX=shared TESSERA_TEST_LANDLOCK_ABI=1 \
  cargo test -p tessera-core --lib -- sandbox file_object bash
TESSERA_TEST_SANDBOX=shared cargo test -p tessera-core --test sandbox_shared --test sandbox_compat
```
- `crates/core/tests/e2e.rs::credentials_stay_out_of_the_models_reach`:
  - vault values reach commands but not models;
  - obfuscated and base64 reads of `auth.json` find nothing;
  - in-process tools refuse outright;
  - the agent can't run `tessera pair`;
  - no request to the model contains a secret in any form.

