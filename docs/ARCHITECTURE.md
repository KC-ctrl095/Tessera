# Architecture

## Layers (brief §"Product hierarchy")

| Layer | Crate / app | Depends on |
|---|---|---|
| 1. Core harness | `crates/core` | `proto` only |
| 2. Compatibility interfaces | `crates/cli` (TUI + one-shot), `crates/server` (HTTP/WS), `crates/proto` | core |
| 3. Rich clients | `apps/tessera` (Tauri: Linux, Windows, Android; web UI) | `client`, `proto` — never `core` |
| 4. Higher-level agent systems | subagents, scheduled jobs, learning loop (in core, all optional at runtime) | core |
| 5. Optional extensions | MCP servers, skills | configured, not compiled in |

The core never depends on a GUI. The apps never link the engine: they talk to
a server, so the Android app stays small and a phone never runs agent work.

## Processes

```
tessera serve            one engine per data directory, owns the SQLite DB
  ├─ HTTP  /api/health   probing (public)
  ├─ HTTP  /api/pair     pairing code → device token (rate limited)
  └─ WS    /api/ws       all requests + event stream (token auth)

tessera (TUI / run)      if a server runs on this data dir → connects to it as a
                         local device; else runs the engine in-process
Tauri app                Rust side owns the connection (headers, endpoint
                         selection, reconnect); web UI talks to it over IPC
```

## Engine (crates/core/src/engine.rs)

- **Sessions** are actors-by-convention: one `Rt` per loaded session holding
  info, internal state, the live streaming snapshot, queued steering and
  follow-up messages, and the cancel token of the running turn.
- **Turn loop**: drain steering → maybe compact → build request → stream
  (retries for transient/rate-limit errors, compact-and-retry on context
  overflow) → persist the assistant entry → run tools (concurrency-safe calls
  in parallel, others in order; approvals; guardrails) → persist results →
  repeat; follow-ups start after the model stops.
- **Persistence**: every entry is appended to SQLite before it is broadcast.
  Tool calls are marked started/finished in `tool_runs`, which is how crash
  recovery distinguishes *never ran* from *outcome unknown*.
- **Events** are broadcast to every subscriber; deltas are not persisted, and
  `get_session { since_seq }` returns missed entries plus the in-progress
  stream so clients resynchronise exactly.

## Context engine (crates/core/src/context.rs)

What the model sees for a session:

```
[system prompt — frozen per session, rebuilt only at compaction]
[checkpoint: prefix + task ledger (verbatim, as of the last history edit) + summary + user messages (verbatim) + files changed]   ← only after a compaction
[entries from first_kept_seq … now]
    tool results older than pruned_until → one-line stubs (recall seq=N)
    oversized results older than demote_before → head+tail (recall seq=N)
    thinking before the latest history edit (or thinking_before) → dropped
```

Every history edit (a trim, a checkpoint) and every system prompt that a
setting or new credential changed mid-chat sets `thinking_before` to the end:
reasoning is replayed only under the exact prefix it was written after.
Recent Claude models check this ("preserved thinking") and refuse a replayed
block whose earlier history changed; dropping a leading run of reasoning is
what they accept. Should Claude still refuse one, the request is sent once
more without any reasoning (`llm/anthropic.rs`).

Budget: `window` (model window, capped by `context.max_tokens`; when that is 0,
at 256k tokens, so a 1M-token model compacts near 180k instead of re-reading
~770k on every step), `trigger = (window − output reserve) × compact_at`,
`keep_recent ≈ 8% window`. The chat's context meter shows this working window.
Token estimates are calibrated per session against provider-reported usage.
One tool result is shown up to `context.max_tool_output` (32 KB) and at most
~4% of the working window (`context::tool_output_limit`: ~19 KB at 128k,
~5 KB at 32k); past twice that, the engine keeps head and tail and stores the
whole as an artifact, and `read` stops just short of it with "Continue with
offset". Pictures over ~3.6 MB or 8000 px on a side (what every provider
takes) never enter the conversation: tools get a note, attachments stay files.

Compaction order: (0) stop replaying earlier reasoning if that alone gets
under ¾ of the trigger (the bulk of a long tool loop, and no facts are lost);
(1) stub old tool output (never a loaded skill) if that gets under ¾ of the trigger; (2) otherwise
summarise the folded region with a fixed-section
prompt, iteratively updating the previous checkpoint, chunking when the
region exceeds the summariser's window, validating the result (retry once,
then a deterministic fallback — history is never dropped silently) and
condensing a checkpoint that exceeds 1.5× its budget, so it stays bounded
over arbitrarily long work; (3) if the verbatim tail is still too large (one giant parallel batch),
shorten oversized outputs inside it.

Stage 2 runs ahead of time (`engine/compaction.rs`): past the trigger (or at 85% of it when the
cheap stages provably can't free enough) the checkpoint is written in the background while the
agent keeps working, and swapped in at the next step once ready; the agent waits only if the
context reaches the hard limit (60% of the way from the trigger to the window limit) first. Small
windows, where the longer verbatim tail this leaves would crowd the next round, summarise on the spot.
Stopping a turn mid-compaction saves nothing. The ledger in the checkpoint is refreshed only at
history edits (trims, checkpoints), so `task` updates in between don't change the prompt's start
(each would be a full prompt-cache miss); the agent sees its updates in its own `task` calls,
which return one line.

Invariants: the task ledger and the user's own words are never subject to
lossy summarisation; the full history stays in the store and is reachable
through `recall`; the prompt prefix is append-only between compactions.

## Providers (crates/core/src/llm, providers.rs)

| Kind | Wire | Notes |
|---|---|---|
| `openai_chat` | Chat Completions SSE | local servers, OpenRouter, Groq, DeepSeek, Gemini-compat; `<think>` and `reasoning_content` handled; reasoning replay configurable (`all` keeps caches hot); on OpenRouter (`llm/openrouter.rs`) the chat id as `session_id` (sticky provider) and, for Claude, cache breakpoints on the system prompt plus the automatic one (others cache by themselves) |
| `openai_responses` | Responses API | API key; encrypted reasoning items replayed |
| `openai_codex` | Responses API at chatgpt.com/backend-api/codex | ChatGPT subscription via device-code OAuth; automatic refresh |
| `anthropic` | Messages API | adaptive thinking + effort on current models, budget tokens on older ones; signed thinking replayed; top-level cache control |

Model profiles (`profiles.rs`): per model family, matched on the id and a llama.cpp chat template's
markers: the model card's sampling (sent to the user's own servers only, in each server's field
names), size, context budget, default effort, reasoning replay, edit form, Mistral's 9-character
call ids; `[tuning]` overrides them per model. Small model mode (`engine/small.rs`) is decided with
a session's prompt and frozen with it: short prompt, core tools with flat arguments, worked examples
as the first messages, one call per reply, loop limit 3, plan reminders; `docs tools` turns the
other tools on once. See `docs/SMALL_MODELS.md`.

## Tools (crates/core/src/tools)

`read write edit grep glob bash job web_fetch web_search task memory skill
skill_manage recall agent schedule` plus `mcp__<server>__<tool>`, and `lsp`
when a language server matched the session's directory (`crates/core/src/lsp/`:
lazily started servers, sandboxed like `bash`, whose new diagnostics are
appended to every edit result).
Policy (`tools/policy.rs`) classifies shell commands into *block* (always
refused), *ask* (privilege, destructive, outward-facing) and allowed.
Permission modes: `default`, `ask`, `auto`, `read_only`.

## Security boundaries

| Concern | Owner | Mechanism |
|---|---|---|
| Who may connect | server | pairing codes (one active, single use, 10 min, cancelled after 3 answers counted before they're checked, brakes apart for local and network) → per-device tokens (SHA-256 stored), revocable; machines get machine logins that only serve as the machine |
| Traffic confidentiality | `crates/secure` | SPAKE2 pairing pins the server key; Noise `NK` sessions (forward secret) carry everything, so tunnels/proxies see only ciphertext; plain access is local-only (ADR-012) |
| Provider & vault secrets | engine (`auth.json`, 0600, dirs 0700) | never in prompts, logs, history or client responses; vault values reach commands only as env vars; known values and credential patterns scrubbed from tool output |
| Tessera's own files vs the agent | `sandbox.rs` + `Protected` | in-process tools refuse Tessera's data/config/app dirs (skills excepted); shell commands run in a user+mount namespace where those dirs are empty read-only tmpfs, plus a Landlock domain so `/proc/<pid>/{root,environ,mem,fd}` of outside processes and mount changes are denied; server/CLI are non-dumpable |
| Windows (no sandbox yet) | `platform/` + `Protected` + policy | in-process tools still refuse Tessera's folders; commands run unsandboxed with the account's rights (said in Settings, setup and docs), in Git Bash/PowerShell/cmd (`platform/shell.rs`), each tree in a Job Object it joins before it runs; Windows-specific risky commands ask or are refused; a Windows machine runs commands only with Full access (docs/WINDOWS.md) |
| The agent administering Tessera | policy | `tessera pair/connect/auth/secrets/config…` refused; nested `tessera` agents need approval |
| Destructive actions | engine policy | approval flow; hard blocks; session-scoped allowances keyed by rule |
| Remote exposure | operator | bind `127.0.0.1` by default; tunnels/Access headers supported end-to-end |
| Client token storage | app | Rust side only (`servers.json`, 0600); never exposed to the web view |
