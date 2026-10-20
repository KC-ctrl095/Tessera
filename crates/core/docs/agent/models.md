# Models: roles, fallbacks and cost
**Roles** name models in `config.toml` so the rest of the setup can say what a model is *for*:
```toml
[models]
default = "openai-codex/gpt-5.5"   # new sessions (same as top-level default_model)
fast = "local/qwen3.8-27b"         # = utility: titles, compaction, web_fetch extraction
memory = "anthropic/claude-sonnet-4-5"  # the background memory/skills review (unset: @utility, then the chat's model)
smart = "anthropic/claude-opus-4-5"
vision = "@smart"                  # a role can point at another role
```
The apps can set roles too. Write `@role` anywhere a model is chosen: a chat's model (`/model @smart`), a subagent's `model`, a bot's or a job's model. A role resolves when the session starts, so a chat keeps one concrete model; change the role and new sessions follow it. Unknown roles and cycles are errors that name the configured roles.

**Fallback chains** keep work going when a provider fails after its retries (rate limit, 5xx/overload, outage, rejected key):
```toml
[models.fallback]
default = ["anthropic/claude-sonnet-4-5", "@fast"]   # for the default role's model
fast = ["local/other-model"]
"openai/gpt-5" = ["@smart"]                          # keyed by a model works too
```
The next signed-in model in the chain answers that request, the chat shows a notice ("… failed (HTTP 429 …); continuing with … for the rest of this turn"), and the rest of the turn stays on it. The next turn tries the chat's own model again. With a fallback waiting, the failing model gets 2 retries instead of 5, and none when a rate limit asks for a long pause.

**Retries.** A chat the user watches asks a failing provider again after 2, 4, 8, 16 and 32 s (5 tries; a server on the user's network gets 10, none paused over 60 s) and shows "retrying in Ns". Work nobody watches (scheduled jobs, helpers within their own budget, bots' work, chats in YOLO mode, chats or rooms whose user wrote over 15 minutes ago) keeps trying for 30 minutes with pauses of 1–2 minutes (a provider's own `Retry-After` first), tells the user once, and only then fails; a server of the user's that answers the same error five times in a row over two minutes ends it sooner. A room the user types in counts as watched. Stop, or a new message from the user, ends a wait at once.

**Context window of local models.** For a model on the user's own server (llama.cpp, Ollama, LM Studio) Tessera reads the window the server really runs it with, without loading anything; under 16,384 tokens Settings → Models says so, with how to raise it. If the user asks: Ollama starts with 4,096 tokens (Settings → Context length in its app, or `OLLAMA_CONTEXT_LENGTH=32768`); LM Studio: "Context Length" in the model's load settings; llama.cpp: `-c 32768` (or `ctx-size` in its preset). A `context_window` on a `[[provider.models]]` entry overrides what is read.

**Fast mode** (`/fast` in a chat in the apps, or its model settings there) asks OpenAI models (the OpenAI API and ChatGPT sign-in) for priority processing: quicker replies that use more credits or cost more. Other models ignore it. `fast = false` (or `true`) on a `[[provider.models]]` entry overrides whether a model offers it.

**Cost.** The usage stats (Settings → Usage in the apps) count every request with two figures: what was billed per token (`cost_usd`: Anthropic, OpenAI or OpenRouter API keys) and what the same requests cost or would have cost at API prices (`api_cost_usd`), so someone on a ChatGPT plan (`openai-codex`) or using their Claude Pro/Max plan through their own Claude Code (`claude-code`) can see what the plan saved. Local servers (llama.cpp, Ollama… on this machine or network) are free unless the user prices them. Prices are US dollars per million tokens, from, in order: `[prices]."provider/model"` (what the apps' "set price" writes), a model's `price` in `[[provider.models]]`, other `[prices]` keys (model id or `*` pattern), a built-in list of list prices (current Claude and OpenAI API models), then a public catalog (models.dev, fetched at most daily, only when a model has no price; `[pricing] catalog = false` turns it off). Models still without a price are listed as unpriced rather than guessed; offer to set one:
```toml
[prices]
"openai-codex/gpt-6.1-sol" = { input = 2.0, output = 10.0, cache_read = 0.1 }
"local/qwen3.8-27b" = { input = 0, output = 0 }
[pricing.subscriptions.openai-codex]   # the plan's fee, compared with API prices
plan = "ChatGPT Pro"
monthly_usd = 200
```
**Several accounts.** The user can add more ChatGPT sign-ins and more Claude accounts (Settings → Models → Connect → Another ChatGPT/Claude account): each is its own provider (`openai-codex-<name>`, `claude-code-<name>`, named like "ChatGPT (Work)") with its own models, usage and limits; a chat uses the account of its model. A Claude account other than the usual one is signed in by the user once through Claude Code in its own folder (`CLAUDE_CONFIG_DIR`; Settings shows the command); never sign in for them or touch those folders. `[accounts] continue_on_next` (off by default; a switch in Settings → Models) moves a chat whose account reached its usage limit to the same model on the next account of that kind.

A plan set on a provider used with an API key (a "coding plan") marks it as billed by plan. **Plan limits:** Settings → Usage also shows how much of a ChatGPT or Claude (Claude Code) plan's 5-hour and weekly limits is used and when each resets, as the provider last reported them with a reply (never fetched on their own; kept across restarts). Claude: Tessera uses Anthropic API keys (console.anthropic.com → API keys) and never signs in to Claude.ai; a Claude Pro/Max plan is used through the user's own Claude Code (`docs claude-code`).

**Model profiles and small model mode.** Tessera knows model families (Qwen, gpt-oss, Mistral/Devstral, GLM, Gemma, Llama, DeepSeek…) and sends each one's recommended sampling to the user's own servers, sets its default effort, and compacts small models earlier (they work best with about 20–32k tokens of context). Small model mode (Settings → Models, per model: Auto / On / Off; Auto = models on the user's own computer or network, and small models) gives a chat a short prompt and fewer, simpler tools (`docs tools`). Overrides per model, for the user or a benchmark:
```toml
[tuning."local/qwen3.8-27b"]
small = "on"            # "auto" | "on" | "off"
sampling = false        # don't send the model card's values (temperature, top_p, top_k, min_p…)
temperature = 0.6       # or set your own
context_budget = 0      # 0: compact only when the window fills
check = false           # skip the one-time "can it call a tool" check
```
A change applies to new chats (open ones at their next checkpoint).
