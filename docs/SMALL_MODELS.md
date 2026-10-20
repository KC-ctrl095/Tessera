# Model profiles and small model mode

Any model should be able to use Tessera well, from a 4B model on an old PC to the frontier models. Two
things make that work without asking anyone to tune anything:

- **Model profiles**: what Tessera does differently for a model family, kept as data
  (`crates/core/src/profiles.rs`). Users never see them.
- **Small model mode**: a simpler working setup for small and local models. One setting per model in
  Settings → Models: **Auto / On / Off** ("Simpler tools and shorter instructions, for models running on
  your own computer").

Both use published model cards, benchmarks and other harnesses' measured results.
The sources are listed at the end.

## Model profiles

A profile is matched on the model id (`qwen3.8-27b`, `Qwen3-30B-A3B-Instruct-2507`, `llama3.1:8b`…) and,
for a llama.cpp model whose id says nothing, on markers in its chat template (Mistral's `[TOOL_CALLS]`,
gpt-oss's channels, Gemma's and Llama 3's turn markers). It sets:

| What | Why |
|---|---|
| **Sampling** (temperature, top_p, top_k, min_p, presence/repetition penalty) as the model card recommends, with thinking on or off | Servers otherwise use their own defaults (llama.cpp: temperature 0.8, top_k 40, min_p 0.05), and GGUF files never carry a presence penalty. Qwen's cards: "DO NOT use greedy decoding … endless repetitions". Never temperature 0 for a thinking model. |
| **Size** (from the id, e.g. `30b-a3b`, else known) | Decides small model mode's Auto and the context budget. |
| **Context budget**: compact by about min(half the window, 32k tokens) for ~3B-active MoEs and dense ≤9B, 48k for mid-size MoEs (≤12B active); unchanged for dense models over ~20B and for big and frontier models (a 32k cap on qwen3.8-27b made a 30-module task in `bench/` thrash: files just read were trimmed and read again) | Small models' effective context is far below the advertised one (RULER: Llama 3.1 8B 128k → 32k; Fiction.LiveBench at 16k: Qwen3-30B-A3B 40.6%). |
| **Default reasoning effort** where the maker recommends one (gpt-oss: medium, never low for agent work) | gpt-oss-20b τ-bench Retail: 35.0 at low vs 54.8 at high. Qwen3.8 keeps its own default (`xhigh`): its card says lower effort fails more often. |
| **Reasoning replay** (which earlier reasoning is sent back) | gpt-oss and the original Qwen3 drop it after a final answer; most others keep it (the default). |
| **Edit form** (line edits + replacements; replacements only; `apply_patch`-trained) | Edit formats matter per model (Aider, Diff-XYZ, hashline results). |
| **Mistral call ids**: exactly 9 letters and digits | Mistral's chat templates refuse other tool call ids. |

Rules:
- The model card's sampling is sent **only to the user's own servers** (llama.cpp, vLLM, SGLang, LM Studio,
  Ollama on this computer or the network), and only fields that server takes: llama.cpp gets `top_k`,
  `min_p`, `repeat_penalty`; vLLM/SGLang `top_k`, `min_p`, `repetition_penalty`; LM Studio `top_k`,
  `repeat_penalty`; anything else (Ollama's `/v1`) just `temperature`, `top_p`, `presence_penalty`.
- What llama.cpp was started with (`--temp`, `--top-k`… in the router's arguments) stays the user's.
- Unknown models get nothing: the server's own defaults stay.
- Hosted models get nothing from the card; values the user sets in `[tuning]` go to any OpenAI-compatible
  endpoint.

| Family | Thinking | Thinking off / instruct |
|---|---|---|
| Qwen3.6 / 3.8 | T1.0 p0.95 k20 min_p0 presence 0 | T0.7 p0.8 k20 min_p0 presence 1.5 |
| Qwen3.5 | T0.6 p0.95 k20 min_p0 presence 0 | T0.7 p0.8 k20 min_p0 presence 1.5 |
| Qwen3 / QwQ | T0.6 p0.95 k20 min_p0 | T0.7 p0.8 k20 min_p0 |
| Qwen3-Coder, Qwen2.5 | — | T0.7 p0.8 k20 min_p0 repetition 1.05 |
| gpt-oss | T1.0 p1.0 | — |
| Devstral, Mistral Small | — | T0.15 |
| Magistral | T0.7 p0.95 | — |
| GLM-4.7 | T0.7 p1.0 | — |
| GLM-4.6 | T1.0 p0.95 k40 | — |
| Gemma 3 / 4 | T1.0 p0.95 k64 | T1.0 p0.95 k64 |
| Llama 3.x / 4 | — | T0.6 p0.9 |
| Phi-4-reasoning | T0.8 p0.95 k50 | — |
| DeepSeek-R1 and its distills | T0.6 p0.95 | — |
| MiniMax M2 | T1.0 p0.95 k40 | — |
| Kimi K2 | T1.0 | T0.6 |
| Nemotron 3 Nano | T0.6 p0.95 (tool calling) | — |
| SmolLM3 | T0.6 p0.95 | T0.6 p0.95 |

## Small model mode

**Auto** turns it on for a model on the user's own computer or network (unless its id shows it is large,
e.g. 70B dense), for known small models anywhere (dense up to ~35B, MoE up to ~12B active), and for a
model that failed the tool check (below). Never for Claude Code (it runs its own harness) or the hosted
frontier models. **On** and **Off** are the user's.

It is decided when a chat's prompt is built and frozen with it, so prompt caches hold; a change applies to
new chats, and to open ones at their next context checkpoint. In it:

- **A short prompt**: system prompt plus tools about 2,000 estimated tokens (5,200 otherwise). The rules
  kept: answer directly when no tool is needed; look, change, check; read before you edit (several files
  at once with `paths`); one change at a time, checked before the next; say done only after checking; ask only when the choice is the user's; reply in the user's
  language. Everything else is in `docs`. Budget test: `crates/core/tests/small_mode.rs`.
- **The core tools only**: `read`, `write`, `edit`, `bash`, `grep`, `glob`, `job`, `task`, `web_search`,
  `web_fetch`, `memory`, `docs`, and `whiteboard` for asking the user a question. Hiding core tools or the
  question tool costs small models more than it saves (Hermes Agent: +48% time on a 27B model), so they
  never go.
- **One switch to the rest**: `docs` with topic `tools` (or calling one of them by name) turns all the
  others on (browser, notes, whiteboards, schedules, subagents, machines, notifications, sharing files,
  skills, settings, recall, lsp, MCP tools) from the next step. The tool list changes once, so a model
  server at home re-reads the conversation once; they stay on for the chat.
- **Flat arguments**: `edit {path, old, new, all?}` or `{path, tag, at, text}`; `task {plan: [...]}`,
  `{done: [n]}`, `{goal}`. The calls are stored as the model sent them; the longer forms still work.
  Models that fumble line numbers (MiniMax, Kimi, DeepSeek) get `old`/`new` only.
- **Worked examples as messages**: the conversation opens with one short example task (a plan, one read of
  two files, an edit, a check, the plan marked done), always the same, so caches hold. Examples as messages help small
  models far more than the same text in a prompt (LangChain: Claude 3 Haiku 11% → 75%).
- **One call per reply, several files per read** (`parallel_tool_calls: false`): small models garble or
  duplicate parallel calls, and with them qwen3.8-27b got tasks that follow a written spec wrong far more
  often (bench/, batch 15: 6 of 14 runs passed with several calls per reply, 19 of 20 with one). One call
  per reply makes a request per call, though, and every request re-sends the conversation (batch 14 doubled
  qwen3.8-27b's input tokens on long tasks that way), so looking around goes in one call: `read` takes
  `paths`, several files at once, and the guidelines and the worked example show it. Where several calls
  per reply are turned on (`parallel_tool_calls = true`), a reply's calls are guarded: a call repeated in
  the same reply runs once, and after a change or command that failed, the reply's further changes and
  commands are not run ("Not run: `edit` before it in this reply failed…"); its reads still are.
- **Old tool output trimmed in batches**: past 48,000 tokens (well before a big window's compaction), old
  tool output is replaced with references in one go (the newest 16,000 tokens of tool output and the last
  three replies stay as they are, with their reasoning), and only when that frees at least 12,000 tokens, so a model server at home re-reads the conversation once per
  batch rather than every step. The originals stay retrievable (`recall`); reading a file again after it
  is not a loop.
- **Loops stopped sooner**: an identical failing call is refused after 3 (5 otherwise), as Roo, Cline and
  OpenHands do.
- **The plan, now and then**: every 6 steps, while the task ledger has open steps, the last tool result
  ends with `[Plan: step k/n — next step]` (added at the end, never to the prompt's start).

### The tool check

The first time a model on the user's own server is used for a new or short chat, Tessera asks it once,
with one tool: "What time is it? Use the tool." (well under 1k tokens; only when no other chat of
Tessera's is busy on that server, and not at the start of a long chat whose cached prompt it would push
out; only for that model, never another one on a llama.cpp router). A call (made, or written as
text and recovered) passes, quietly. Text instead, another tool or broken arguments fail: the chat says so
in plain words, and Auto turns small model mode on for that model. An inconclusive check (cut off, the
server didn't answer) is tried again after 6 hours. Results are kept in the database (`model_checks`).

## `[tuning]`: overrides per model

Keyed like `[prices]`: `"provider/model"`, a model id, or a pattern with `*`; where several match, the more
specific one wins, field by field. Everything is optional.

```toml
[tuning."local/qwen3.8-27b"]
small = "on"              # small model mode: "auto" (default), "on", "off"
sampling = false          # don't send the model card's sampling values
temperature = 0.6         # your own values, sent instead (also top_p, top_k, min_p,
                          # presence_penalty, repetition_penalty)
parallel_tool_calls = true # several calls per reply (default: yes; one per reply in small model mode)
reply_guard = false       # with several calls per reply in small model mode: run them all, even after a failed change
read_paths = false        # `read` takes one file per call
observation_budget = 0    # 0: no batched trimming of old tool output; or a number of tokens
context_budget = 0        # 0: compact only when the window fills; or a number of tokens
check = false             # skip the one-time tool check

[tuning."*"]              # every model
small = "off"
```

## Lean start for every chat (`[agent] lean_tools`)

Small model mode's switch, for chats on any model (off by default until benchmarked):

```toml
[agent]
lean_tools = true
```

A chat then starts with every tool except `note`, `whiteboard`, `browser`, `schedule`, `notify`,
`share_file`, `save_secret`, `settings`, `machines`, `message_bot`, `agents` and `skill_manage`; one line of
the prompt names them, and the first call to one (or `docs` topic `tools`) turns them all on from the next
step, so the prompt cache misses once, only in chats that use one. `edit`, `agent`, `task` and `memory` (and
`machines` and `whiteboard` once on) get shorter descriptions (same rules, same parameters), the skills index is capped at 2,000 characters (the most loaded skills first) and
the saved-passwords list at 10 (the most recently used first; `save_secret` with `action: "list"` lists them
all, names only). Not in private chats, channels, Tessera's background sessions, sessions with their own tool
list, Claude Code chats, or where small model mode applies. Off, every request is byte for byte what it was.
Measured with the engine's estimator before the user's first word: a bare server 5,252 → 3,290 tokens; a
set-up one (two computers, five bots, 25 passwords, ten skills, memory half full) 7,713 → 5,046
(`crates/core/tests/lean_start.rs`).

## Measuring it (A/B)

Every behaviour has its own switch, so a benchmark run can turn one off at a time with a `config.toml` in
its throwaway `TESSERA_HOME`:

| Variant | `[tuning."*"]` (or `[tuning."bench/qwen3.8-27b"]`) |
|---|---|
| Today's behaviour (before batch 14) | `small = "off"`, `sampling = false`, `context_budget = 0`, `check = false` |
| Profiles only | `small = "off"`, `check = false` |
| Small model mode without card sampling | `sampling = false` |
| Batch 14's small model mode | `read_paths = false`, `observation_budget = 0` |
| Several calls per reply (guarded) | `parallel_tool_calls = true` |
| Several calls per reply, unguarded | `parallel_tool_calls = true`, `reply_guard = false` |
| `read` one file per call | `read_paths = false` |
| No batched trimming of old tool output | `observation_budget = 0` |
| No context budget | `context_budget = 0` |
| Everything (the default) | nothing |

`small = "off"` restores the full prompt, all tools, the nested `edit`/`task` forms, no examples, the
loop limit of 5, no plan reminders, no reply guard and no batched trimming. Two profile values have no switch (they are fixed
data and don't apply to Qwen3.8): gpt-oss's default effort (medium, was the server's) and the reasoning
replay of gpt-oss and the original Qwen3 (this turn only, was all; their templates drop the rest anyway).

## Sources

- Model cards: Qwen3.8-27B, Qwen3.6/3.5, Qwen3 (and its technical report), Qwen3-Coder, gpt-oss,
  Devstral, Mistral Small 3.2, Magistral, GLM-4.7/4.6, Gemma 4, Phi-4-reasoning, DeepSeek-R1, Kimi K2,
  MiniMax M2, Nemotron 3 Nano, SmolLM3 (links in R3 §5).
- llama.cpp server README and PR #17120 (GGUF sampling keys); vLLM tool calling docs (Mistral's 9-character
  ids); Ollama OpenAI compatibility; LM Studio docs.
- Tool sets: GitHub Copilot (40 → 13 tools), "Less is More", RAG-MCP, LongFuncEval, Anthropic advanced
  tool use; Hermes Agent's tool search measurement.
- Few-shot as messages: LangChain, Meta-Tool, Anthropic tool-use examples.
- Context: RULER, NoLiMa, Fiction.LiveBench, "The Complexity Trap".
- Loops: Gemini CLI, OpenHands, Roo Code and Cline loop detectors; "looping" study arXiv 2512.12895.
- Edits: Aider leaderboards, Diff-XYZ, oh-my-pi hashline.
