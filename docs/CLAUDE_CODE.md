# Use your Claude Pro or Max plan

Tessera can run chats through **your own Claude Code**, so they use your
Claude Pro or Max plan instead of a pay-per-token API key, with Tessera's
own tools built in: notes, whiteboards, memory, notifications, the browser,
your machines and your stored passwords & keys.

## What Tessera does and doesn't do with your Claude account

Anthropic lets you sign in to the official, unmodified Claude Code with your
own subscription and run it headless (`claude -p`), including from another
program. That is all Tessera does.

**Tessera does:**

- run the `claude` you installed, unmodified, as you, with documented flags
  (`-p`, `--model`, `--resume`, `--mcp-config`, `--tools`,
  `--append-system-prompt-file` and fast-mode `--settings`);
- register policy/snapshot, post-write diagnostics and bounded Stop hooks
  with that one process through the documented SDK pipe, never settings files;
- ask it `claude --version` and `claude auth status` to tell you whether
  it's installed and signed in;
- give it Tessera's tools and a few lines of context for each turn (below).

**Tessera doesn't:**

- sign in to Claude, ask for your Claude password, or show a Claude login of
  its own: you sign in to Claude Code yourself, once, in a terminal;
- read, copy, store or pass on Claude Code's credentials or tokens, or read
  or change `~/.claude`, `~/.claude.json` or your Claude Code settings;
- change the `claude` program, or call Anthropic's API with your account
  itself;
- share your plan with anyone else: each person who uses Claude in Tessera
  signs in to their own Claude Code with their own subscription.

Everything a chat on Claude Code does counts against your plan's usage
limits, exactly as if you used Claude Code yourself.

## Set it up

1. **Install Claude Code** on the computer that runs the Tessera server,
   following [code.claude.com/docs/en/setup](https://code.claude.com/docs/en/setup)
   (for example `curl -fsSL https://claude.ai/install.sh | bash`).
2. **Sign in once**: run `claude` in a terminal on that computer, as the
   same user that runs Tessera, and log in with your Claude account.
3. **Press Check again** in Settings → Models → *Your Claude plan* (or run
   `tessera setup models`). Chats can now pick a model under **Claude (your
   subscription)**, e.g. `claude-code/sonnet`, `claude-code/opus` or
   `claude-code/haiku`, and you can make one the default.

That's all: Tessera's tools and context come with every Claude chat, for
everyone who adds it. There is nothing to configure in Claude Code. From a
terminal, `tessera add-provider claude-code --kind claude-code` adds it too
(it finds `claude` by itself; `--url /path/to/claude` names one), and
`tessera login claude` says what to do next.

Tessera looks for `claude` on the `PATH`, then in `~/.local/bin` and
`~/.claude/local`. It asks `claude` about itself only when something needs
it (setup, Settings, refreshing the model list, the first chat on it),
never at start-up, and remembers the answer. A service often has a shorter
`PATH`; if `claude` lives elsewhere, name it in `config.toml`:

```toml
[[provider]]
id = "claude-code"
kind = "claude_code"
base_url = "/usr/local/bin/claude"   # the claude command to run
```

The model list starts with `opus`, `sonnet` and `haiku` (the latest of each)
and fills in with the exact models your Claude Code offers after its first
turn. The thinking menu sets Claude Code's effort (*Default* leaves it to
your Claude Code settings).

## Tessera's tools in Claude chats

In a chat on Claude Code, Claude Code does the work with its own tools
(Read, Edit, Bash, WebFetch, its subagents…) and also has Tessera's:

| Tool | What it's for |
|---|---|
| `note` | your notes in Tessera (what "my notes" means) |
| `whiteboard` | something visual, as a card in the chat (a tile when you ask to see it), or a question asked through one |
| `browser` | a real browser working in the background; each step shows in the chat and you can Watch it |
| `memory` | what it keeps across chats about you and the work |
| `notify` | a notification on your phone or computer, now or later |
| `share_file` | a file from the server as a download in the chat |
| `machines` | your other computers, through Tessera |
| `recall`, `skill`, `schedule`, `message_bot`, `settings`, `docs` | earlier chats, Tessera's skills, scheduled jobs, your bots, Tessera's settings and guides |
| `agent`, `agents` | start native per-task helpers with `agent`; manage them with `agents` status/report/stop/message/apply/discard; CLI Agent remains separate |
| `read`, `write`, `edit`, `grep`, `glob` | Tessera's tagged/paged files, seen-line edits and folder searches; the same native checks and approvals |
| `bash`, `job` | ordinary commands and stored-credential commands, plus Tessera background-job controls; offered even without any saved secrets |

They show in the chat as the same cards as anywhere else in Tessera (a note,
a whiteboard, a notification), and run exactly as they do for Tessera's own
agent: the same approvals, the same refusals, the same rules for which bot
may use which password or machine. A stored credential is injected only into a
command that names it; known secret values are redacted in bridged results. A
program can still send the value anywhere; listed sites are not a network boundary.

**How they get there.** Each turn starts `claude -p` with
`--mcp-config '{"mcpServers":{"tessera":{"type":"sdk",…}}}'`: the Agent
SDK's in-process server kind, so Claude Code sends its tool calls for
Tessera over its own pipe to Tessera (the same stdin/stdout the turn already
uses), and Tessera answers them for that chat only. There is no port,
password or token involved; the connection belongs to that one `claude`
process and ends with its turn. Tessera's context (your memory and profile,
a bot's persona and duties, the names of the passwords & keys it may use,
your own instructions, a few lines on the tools) goes in a file passed with
`--append-system-prompt-file`, readable only by you and deleted after the
turn. A conversation gets the same context on every turn (it is part of
Claude's system prompt, and a changed one makes Claude re-read the whole
conversation at full price): something saved to memory mid-chat reaches it
once Claude Code compacts the conversation or in the next one, and changing
a setting or a password & key brings it up to date on the next turn. Tessera
turns off Claude Code's auto-memory **for its own child processes only**, using
Anthropic's documented `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` environment variable
([memory guide](https://code.claude.com/docs/en/memory)). Its appended context
points the model to Tessera's `memory` tool as the single persistent memory
system. Your account files, saved settings and ordinary CLI sessions are not
changed. Your own
Claude Code setup still supplies project instructions, CLI hooks, your MCP
servers and claude.ai connectors. Tessera authorizes every action before it
runs, including external integrations Claude would otherwise pre-approve.
Limited actors retain isolated MCP configuration unless their ceiling explicitly
includes the integration. Trust the project's instructions/settings as you would
when running the CLI there.

**Bots on Claude.** A bot whose model is a Claude one works the same way:
its persona and duties are its context, its tool preset limits both
Tessera's tools and Claude Code's built-ins, including definition-specific
helpers. A research bot gets no Bash or Edit. Its scheduled jobs run as it,
and in a channel each bot keeps its own Claude Code conversation.

Session delegation (`agent`, `agents`, `message_bot` and schedule changes)
requires a main-chat policy hook, affirmative admission and unchanged call id,
tool and arguments. An open card alone is not authority. Missing, child,
malformed, mismatched or denied delegation is refused, not read-only.
Schedule actions are exactly `list`, `create`, `update`, `delete`; unknown
spellings and case variants are refused for every caller.

### Helpers on other models

Claude Code's own **Agent** runs Claude models. Tessera's **agent** bridge
(`mcp__tessera__agent`) can instead hand a task to any model you connected,
such as GPT through Codex or a local model. Ask Claude to use Tessera helpers
on that model to save Claude usage. Other providers bill their own work;
helper usage is included in the chat's counter and recorded under its model.

The call is `agent { tasks: [{ name, prompt, model:
"provider/model" }], wait: false }`. It returns right away by default, and
reports arrive in the chat and wake Claude when the batch finishes. Use
`wait: true` when Claude needs the answers before proceeding. The bridge
allows 24 hours per call, not a short MCP timeout; each helper still has
Tessera's normal step and time budget. Long background work does not keep
Claude's turn waiting.
New starts use `agent`; `agents` advertises management only. The older
`agents { action: "start", tasks, wait }` still dispatches for saved conversations.

The model is chosen from the task, then its agent type, then the **server
default**, not the Claude chat's model. If the server default is Claude Code,
Claude must choose explicitly rather than silently launch another Claude
process. Unknown task or type models are refused with the available choices.
You can deliberately choose a Claude Code model on a task or type too.

Helpers show in the shared strip and cards, with chats of their own.
Tessera-owned helpers can be continued normally, on any provider; Claude-native
mirrors remain watch-only. They obey **Helpers at once**, queue behind single-slot
local providers, and support private git worktrees. They inherit the chat's
permissions, machine access and secret rules, never more access. Read-only
stays read-only, helpers cannot start helpers, and private Claude Code chats
have no helper bridge. Stop a helper to stop only it, Stop the chat to stop all its
helpers, and deleting the chat removes them. Claude can use `agents` to
read a full report, send a follow-up, stop helpers, or apply/discard worktree
changes, as in other Tessera chats.

## Shared chat presentation

Apps and the terminal use the same display kinds for equivalent tools: `Bash`
and `PowerShell` show as shell calls, `Read`/`Write`/`Edit`/`MultiEdit` as file
calls, search/fetch as their native cards, and Task/Agent as helper cards.
The input shown in the card uses canonical keys; **Execution details** (or
`ctrl+o` in the terminal) retains the raw tool name and arguments. This is only
display metadata, never a change to execution, approvals or the Claude binary.
Unknown tools keep their name and raw input rather than an invented adapter.
Diffs, real shell output, errors, stopped/unknown states and observed durations
use the shared components. Missing timing/output stays missing. Older chats no
longer estimate tool duration from the gap between transcript entries.

Successful tool discovery and task bookkeeping sit quietly inside the collapsed
steps strip, without inflating meaningful work counts. Expand it to audit every
call. Approval, errors and unknown outcomes are not silently hidden.
Thinking, goals, the plan ledger and bot authors use the same native components.

All helpers share the same strips/cards. A Claude-native helper with no mirrored
session can show only its reported facts: it has no invented Open or Stop.
CLI-owned mirrors can Open and Stop; they cannot be sent messages independently.
Tessera-owned helpers can continue normally regardless of which provider started
them or which model they use. A failed helper Stop never falls back to killing
the parent turn.

For a bridged or native helper task, optional `cwd` chooses its existing working
folder, for example `{"name":"Review","prompt":"Review the project","cwd":"tessera","isolation":"worktree"}`
when the chat folder is the repository's parent. It must be within the chat's
folder or a configured allowed folder. Symlinks cannot escape scope, read-only
ceilings remain, and worktree isolation checks the whole repository's scope.
This does not select a remote machine or grant new access.

## Approvals

Claude Code's actions use the same Tessera policy assessments and approval
cards on every device, including actions its CLI would normally pre-approve.
A mandatory catch-all `PreToolUse` hook runs in every chat and helper. Tessera
waits for successful hook initialization before sending user work.

| Chat's permission mode | Claude Code runs in | What asks you |
|---|---|---|
| Default | `acceptEdits` | native risky commands and file paths, plus anything Claude Code independently flags |
| Ask me | `default` | each command/write, even if Claude Code would auto-approve it |
| Read-only | `plan` | mutations are refused, not approved |
| YOLO | `acceptEdits` | no host approval; hard refusals still apply |

Every action is assessed with the **current** effective mode and complete actor
capabilities, not just the schemas currently loaded. Mode changes are sent to
the running CLI through `set_permission_mode`, and pending cards are cancelled.
Helpers are capped by their parent's live mode; if that ceiling cannot be read
safely now, the action is refused. Old card answers cannot release an action.
Work already released cannot be undone by a later downgrade. The hook and
`can_use_tool` deduplicate by CLI tool-use ID, exact input and mode: one card
per action. Tessera MCP tools have only their native dispatch card. Rewind
snapshots run inside the same ordered callback, only after policy/approval
succeeds and before the CLI is released; separate CLI hooks can run concurrently,
so they cannot do pre-approval file copies. Denying a card refuses that action,
not the query: it carries on with other allowed work, without retrying the denied
action. Unanswered cards expire after 15 minutes; cancelled cards say no answer.
Denial responses always include a message and `interrupt: false`. Only a real
Stop cancels the turn and sends the CLI an interrupt.

**Helpers waiting on you.** When a helper (on any model) waits for an approval,
its card is in the chat that started it, and that chat is told at once (a short
note it reads, which wakes it if its turn had ended); `agents status` shows the
wait. That chat may answer it only where it could do the same itself without
asking at the moment it answers (its permission mode then, with Claude Code's
own policy for a helper running on Claude Code, or you allowed that action there
for the session); otherwise only you can approve it, though the chat may decline
it. Each wait has a short id the chat names when it answers. Unanswered, it
expires after 15 minutes and the helper is told plainly that nothing ran. A
helper's kept Claude Code never starts work by itself (it works only through its
helper run), and Stop on an idle chat closes the Claude Code kept for it.

**Oct 11 refusal finding:** before this fix, a built-in Deny (including an
unanswered/cancelled approval) cancelled the turn and sent an `interrupt` even
though its permission response carried a nonempty message. A bridged native
"declined" result also cancelled the turn. The exact misleading sentence
("The user doesn't want to take this action right now. STOP what you are doing
and wait for the user to tell you how to proceed.") is present in the installed
Claude Code 2.1.286 binary, not Tessera's refusal strings. Tessera had no approval
outcome log, so the historical call cannot be attributed to Deny, cancellation
or a CLI-side folder check. The new bridge never treats a refusal as an interrupt,
and sends explicit wording even for otherwise empty denial messages. Shell
read chains no longer skip approval: Ask mode asks even for ordinary `cat`/`grep`
pipelines, and unresolved/indirect read targets prompt in Default too. Both
bridged and native command guards check credentials and protected paths. Use
bridged file reads for guarded reads without a shell question. Claude's own
stricter folder checks remain authoritative; no path scope has been widened.
Fake-CLI regressions cover the contract; actual fallback behavior still needs
a small live check by the release owner.

**Allow for this session** uses native risk keys in the existing chat/bot
namespace and survives CLI resume. An otherwise ordinary Ask-mode file grant
covers that resolved path; an ordinary command grant covers its exact command
and working folder, never the whole tool. Native chats use the same keys.
Protected paths, read-only folders and catastrophic-command refusals hold in
all modes. File paths include resolved symlinks; searches spanning Tessera's
protected store are refused. Obvious Claude account paths are refused without
opening account files.

Every launch has an explicit `--tools` ceiling derived from effective Tessera
capabilities (Write and Edit are separate), independent of lean schema loading.
The documented local tools remain available through the mappings below; unknown
future tools and cloud execution (`RemoteTrigger`, Agent `isolation: remote`)
stay refused. Bot, helper-definition, depth and report-only ceilings are
rechecked at use time. Search-only actors retain Glob/Grep and Skill without
commands. Workflow additionally needs command capability because its local
script can execute arbitrary programs. Documented hook `agent_id`/`agent_type`/
`permission_mode` metadata only narrows authority: CLI-native helpers cannot
spawn more helpers, and Plan/Explore actions retain a read-only floor. A refused
call ID stays refused if another permission callback arrives without its original
helper metadata. Hook cwd and provenance are preserved for later callbacks.

### Built-in adapters

The local tool inventory was checked against the public
[tools reference](https://code.claude.com/docs/en/tools-reference.md) and
[SDK input reference](https://code.claude.com/docs/en/agent-sdk/typescript.md).
Each tool still runs in the unmodified CLI, not a replacement implementation.

| Claude Code tools | Native capability / policy |
|---|---|
| Bash, PowerShell, Monitor | `bash`; protected-path, secret and command checks; PowerShell also uses Windows catastrophic-command rules. Monitor WebSockets ask outside YOLO. |
| Read, LSP, Skill | `read`; known file paths are assessed, Skill loads instructions |
| Write | `write` |
| Edit, MultiEdit, NotebookEdit | `edit` |
| Grep, Glob | `grep`, `glob` |
| WebFetch, WebSearch | `web_fetch`, `web_search` |
| Agent, Task | `agent`; local helpers only |
| Workflow, SendMessage, ListAgents | `agents`; Workflow also needs `bash`; starting work/messages ask outside YOLO and are refused in Read-only |
| TaskOutput, TaskStop, BashOutput, KillShell | `job` |
| ScheduleWakeup, CronCreate, CronList, CronDelete | `schedule`; changes ask outside YOLO and are refused in Read-only; listing is read-only |
| EnterWorktree, ExitWorktree | `write` in the CLI's current working folder; destructive removal asks outside YOLO |
| PushNotification | `notify` |
| TodoWrite, TaskCreate, TaskUpdate, TaskGet, TaskList | `task`; low-risk CLI list operations. Successful creates/updates reconcile into Tessera's authoritative ledger plan, using the result's task id. |
| ReportFindings, SendFeedback, SubagentHandback, EndConversation | `task`; sending feedback asks outside YOLO and is refused in Read-only |
| ListMcpResourcesTool, ReadMcpResourceTool, ReadMcpResourceDirTool, RefreshMcpTools, WaitForMcpServers | `read` plus the external-integration ceiling; resource reads/listing are documented read operations |
| Artifact, SendUserFile, ShareOnboardingGuide | `share_file`; file policy/scanning and delivery approval; Artifact listing is read-only |
| Projects | `note`; documented project read/search/info operations are read-only, changes ask outside YOLO |
| ShowOnboardingRolePicker | `skill_manage`; profile/instruction changes ask outside YOLO |
| ToolSearch, AskUserQuestion, EnterPlanMode, ExitPlanMode | Discovery/conversation controls, available only while the actor has tools; ExitPlanMode cannot lift Tessera Read-only |

### Credential-file results

Claude Code's native tool results reach its model inside the CLI, before Tessera
can redact the transcript it receives. The mandatory PreToolUse policy therefore
refuses native Read/Grep/Glob/Bash/PowerShell/NotebookEdit targeting obvious
credential files: `.env` and `.env.*`, private-key names and extensions, registry
login files, kubeconfig, cloud CLI credentials and the credential paths known to
Tessera. A project `.npmrc` or `.pypirc` is treated conservatively, without opening
it to see whether it contains a token. Recursive native Grep is redirected too,
because it cannot reliably exclude credential filenames. Use
`mcp__tessera__read` for redacted configuration reads or `mcp__tessera__grep` for
folder searches. They retain native Tessera approvals and redact known secrets
before returning anything to the CLI. Claude account files remain refused even
through the bridge.

This is a mitigation, not a guarantee that secrets never reach Claude: an
ordinary file can contain a password, a script can choose a path indirectly, and
an external integration can return credentials. Tessera cannot rewrite native
CLI results before its model sees them or remove them from its history. The CLI
is unmodified and the guard uses its documented PreToolUse interface. Use the
bridged tools for sensitive configuration and avoid placing unknown secrets in
ordinary project files.
### Native capabilities through the bridge

Tessera keeps Claude Code's tools and own loop/compaction. Native `read`, `write`, `edit`, `grep`, `glob` and `web_fetch` are also bridged, so Claude can use Tessera's tagged files, seen-line edits, searches and reader rather than a similar CLI built-in. Bridged reads redact before the result reaches Claude; built-in CLI results cannot all be redacted by Tessera first.

- `lsp` when enabled and a server matches the project: navigation, references, rename and diagnostics, with native permission checks.
- Tessera-configured external MCP tools (`mcp__SERVER__TOOL`) relay with Tessera's approvals and actor ceilings.
- `web_search` when a provider is chosen in Tessera (explicit provider, or Auto with SearXNG/a keyed service configured). Claude `WebSearch` is removed from `--tools` and disallowed. With default Auto and no configured provider, Claude `WebSearch` stays instead.
- `save_secret`: list credential names or request secure app entry (token/login metadata only). The user types values in Tessera's password inputs, never model arguments. Card-only saving confirms "Saved securely in Tessera." and keeps the safe resume link and warm process. If transcript redaction finds earlier plaintext, saving instead warns that it stays in Claude Code's own history (which Tessera never edits), invalidates the resume link and starts the next turn from sanitized Tessera history.

Project guidance and user/project skills use Tessera's scanned context and the same short skill index as native chats. List/read use bridged `skill`; nothing is copied into Claude folders.

At the next turn boundary after settings change (password, machine, LSP, search), a clean CLI run is normally relaunched with current tool names **and schemas**, context and the appropriate resume link. Never mid-turn. Queued turns obey the same rule. Unchanged turns keep the warm process and frozen context. A secure-card-only password save is the exception: the bridge refreshes between turns without discarding the warm process; fresh credential **names only** arrive as turn context, while appended context remains frozen. Flags, permissions, account, sandbox and context epoch must still match. Existing compaction resume rules stay. Earlier exposed plaintext still requires sanitized-history reconstruction.
### User MCP servers and claude.ai connectors
Ordinary unrestricted chats no longer pass `--strict-mcp-config`: your existing
MCP servers and Claude connectors remain available alongside Tessera's bridge.
Private/internal helpers still use strict configuration, as do limited actors
without an explicitly admitted external tool. The policy hook rechecks the
ceiling even if Claude somehow offers such a tool anyway.

Non-Tessera `mcp__…` calls are **external integrations**, not unknown built-ins:

| Mode | External tool call |
|---|---|
| Default / Ask me | One Tessera approval, even if the CLI pre-approves it. **Allow for this session** covers that exact tool, in the existing chat/bot namespace, not another tool or actor. |
| YOLO | No host card, provided the actor's ceiling admits the integration; Claude's own checks remain. |
| Read-only | Refused unless Tessera has a registered read-only classification for that exact tool and documented CLI `mcp_server` provenance matches its configured server (stdio command/args or HTTP/SSE URL). |

The public hook protocol does not provide MCP tool annotations. Tool names,
model arguments and invented `readOnlyHint` fields do not prove read-only.
A connector known only to Claude therefore stays refused in Read-only; use an
approval-capable mode, or register the same read-only integration in Tessera.
Tessera does not inspect Claude's settings or credentials to classify it.
MCP resource discovery/reads have their documented read semantics, but cannot
escape a bot/helper integration ceiling. Default/Ask ask for external resource
operations too. Unknown/bounded actors need the exact external native-tool
entry in every ceiling; full-capability actors inherit the parent's ceiling.

Successful policy hooks return `{}`, not an overriding `permissionDecision:
allow`. Claude Code retains its own refusals and failed tool results remain
failures. Stop lifecycle observation is always registered; only the verify
nudge depends on verification settings.

This is a policy gate, not an OS sandbox around Claude Code. Its tools execute
under its own process/sandbox settings. Command text analysis has the same
limits as native Tessera without an OS sandbox: arbitrary program indirection
is not a filesystem boundary. Stand-in tests verify the contract; real-CLI
compatibility is left to the release owner.

**Private chats** keep nothing on disk: Claude Code runs without saving its
conversation, with only web tools, no external MCP servers and no local tools,
memory or appended context, in an empty scratch folder. A chosen Tessera search
provider is bridged alone (with CLI WebSearch off); otherwise CLI WebSearch stays.
Each turn brings the conversation so far along, without a persisted CLI session.

Its `claude` also starts without your own and the project's customizations:
`--safe-mode` (no CLAUDE.md, hooks, plugins, skills, MCP servers or auto memory;
unlike `--bare` it keeps the CLI's own sign-in), `--setting-sources ""` (no
settings files) and `--disable-slash-commands`, with `CLAUDE_CODE_SAFE_MODE`,
`CLAUDE_CODE_DISABLE_CLAUDE_MDS` and `CLAUDE_CODE_DISABLE_AUTO_MEMORY` set. Of
the variables you pass to agent programs (`agent.pass_env`) it gets none that
load code or settings (Node/library preloads, Python paths, Claude Code's own
switches); your account folder (`CLAUDE_CONFIG_DIR`) stays. Tessera's own small
requests for a private chat (a page summary, say) start the same way. A CLI too
old for these controls ends at start, before anything of the chat is sent to it.
Ordinary chats keep your integrations and settings unchanged.

Limits, as Anthropic documents them: admin-managed policy (including its hooks)
still applies in safe mode, so this is no privacy promise against an
organization's mandatory settings; the CLI may still look at its settings and
plugin registries on disk without applying them. Checked against the real CLI
2.1.286 in an isolated run (B21): the private search bridge and Tessera's
permission hooks keep working; user/project hooks and memory stay out.

## Usage and limits

Settings → Usage lists Claude chats under **Claude (your subscription)**
with the tokens used and what they would have cost at API prices (as Claude
Code reports it). Enter your plan and its monthly fee there to see what it
saved. The same page shows how much of the plan's 5-hour and weekly limits
(and a model's own weekly limit, when Claude reports one) is used and when
each resets, as Claude Code last reported them with a reply; Tessera never
asks Claude just to find out.

When your plan's 5-hour or weekly limit is nearly used up, the chat says so.
When reached, a watched chat stops cleanly; eligible unattended work can wait
within one bounded budget. See [Plan limits](#when-your-claude-plan-reaches-its-limit)
for reset times, attendance and Stop behavior. If another `claude` is renewing
your sign-in at the same moment, the chat tries again 30 seconds later.

## More than one Claude account

Many people have more than one Claude plan (personal and work). In Settings →
Models, **Connect → Another Claude account** asks for a name ("Work") and
shows one command to run once in a terminal on the server, such as:

```
CLAUDE_CONFIG_DIR='/home/you/.local/share/tessera-claude/claude-code-work' claude
```
That is Claude Code's own, documented way of keeping accounts side by side:
each account has its own configuration folder, and you sign in to it there
with Claude Code's login, as with the first one. Tessera only names the
folder (next to its own data, made private to you) and runs `claude` with it
for that account's chats; it never reads, writes or passes on anything in it.
Press **Check again** once you've signed in.

Each account then has its own models (in the model menu under its name),
usage and limits (Settings → Usage), and its own Claude Code conversations. A
chat uses the account whose model you pick for it; with several accounts the
model's name says which one ("Opus · Work"). Removing an account in Tessera
leaves Claude Code signed in to it in its folder; sign out there (run the
command, then `/logout`) if you want to.

With two or more accounts, you can turn on **When an account reaches its
limit, go on with the next one** (off by default): a chat whose plan's limit
is reached moves, with its model, to your next Claude account that is signed
in to a plan and hasn't reached its own, starting a fresh Claude Code
conversation there that is told what happened so far. Without it, watched
chats stop and eligible unattended work may wait within the bounds described
under [Plan limits](#when-your-claude-plan-reaches-its-limit).

## Titles, memory and channels without another model

Chat titles, the background memory review and channels need a model of
Tessera's own. With Claude Code as your only model they use it too, with
its smallest model (Haiku): a title is one short `claude -p` answer with no
tools at all; the memory review is a Claude Code run with only Tessera's
memory and skill tools and none of its own; nothing of either is kept in
Claude Code's history. Set a utility model in Settings → Models if you'd
rather use another one.

## Compaction, rewind, subagents

- Claude Code compacts its own conversation as it fills up. **Compact
  context** (in the chat's options or the command palette, or `/compact` as
  a message) asks it to now. The chat uses the same compacting status and
  expandable boundary card as native chats. Counts appear only when reported;
  no summary is invented and Claude's private files are never read. A stored
  native checkpoint remains available when switching engines.
- Rewinding a chat puts back the files Claude Code changed with its Edit,
  Write and NotebookEdit tools since that point (its subagents' changes
  too), as for Tessera's own agents: Tessera keeps a copy of each file just
  before Claude Code changes it, in every permission mode. Files bigger than
  16 MB aren't copied. What its commands changed (Bash) isn't undone; the
  rewind lists those commands. Remote-machine and external integration effects
  are not restored. Missing snapshots appear as skipped in preview and the
  result, never called restored. If a file changed since the agent's edit,
  preview warns that rewind will overwrite those changes; an explicit rewind
  keeps native behavior and reports that it did so.
- Claude Code's own subagents (its Agent tool) show like Tessera's own: a
  card and a row in the chat's Subagents bar with how many are at work, what
  each is for (its type: Explore, Plan, General purpose, or an agent of your
  own), what it is doing, its steps and time, and a chat of its own to open
  (what it was asked, its replies, its tool calls and their results). They
  work on what Claude Code gave them, so they take no messages. **Stop** on
  one of them stops that one alone; the chat's turn goes on without it. One
  running in the background goes on after its card's call returns until its
  stream reports it ended.
- **Waiting for Claude Code's background work.** Background Agent/Task calls,
  commands, monitors and scheduled wake-ups reported by the CLI keep its input
  open even if the task event is missing its background flag. The chat shows
  an idle wait with the confirmed-running count. A background request whose
  launch result is missing also keeps input open, but is not counted as
  confirmed running. A foreground Agent's `agentId` is only correlation
  metadata, not background evidence. Stop ends the wait; a new message goes
  into that same child. Once the work ends, the existing follow-up grace
  applies, then normal idle-process retention or closure. Each continuous wait
  has a 12-hour hard cap with a plain notice, not a silent 10-minute cutoff.
  **If Claude's stream drops or garbles terminal events, the chat may keep
  waiting until Stop or the 12-hour cap.** Tessera cannot infer an unseen end.
  Terminal task/call IDs and aliases are kept for the Turn's whole life so
  late launch results or replayed starts cannot bring ended work back.
  Process teardown uses the existing cleanup path; whole-tree retention is
  a separate reviewed integration, not a new supervisor in this change.
- **When it carries on by itself, that is a turn like any other.** The chat
  shows it working; what it does asks for approval exactly as a turn you
  started would (nothing is allowed that would otherwise ask; a plain read of
  its own output doesn't ask); Tessera's tools work; Stop ends it; the
  completion check and the work state apply. This holds also when it takes a
  turn up in the process kept between turns (below): that becomes a turn of
  the chat, with nothing new sent to it. After the work it waited for ended,
  a slow first answer (a long conversation, a cache gone cold) is waited for
  up to 10 minutes instead of 20 seconds. If the chat's settings changed
  meanwhile, that process is closed and the chat goes on in a new one with the
  current settings (the chat says so); after a Stop on this computer it
  doesn't go on at all. Anything Tessera turns down is answered in Tessera's
  own words and never as a Stop, so Claude Code doesn't fall back to its
  "STOP what you are doing" text.
- **Native work state.** Tessera's `task` owns the complete ledger: goal,
  constraints, plan, criteria and their evidence, decisions and artifacts.
  Claude's TaskCreate/TaskUpdate feed that same plan: successful creates add
  steps (or adopt a matching unbound bridged step), updates change their status
  or subject, and deletion removes them. The CLI result's task ids are kept
  across turns, resume and compaction. Failed calls do not change the plan.
  TaskGet/TaskList remain CLI reads; task tools are enabled on every model
  (`CLAUDE_CODE_ENABLE_TODO_TOOLS=1`). Older TodoWrite replaces only the ledger's
  plan, not its richer fields. Bridged `task` remains available for all fields
  and plan updates. Full-list replacements reconcile retained steps' ids by
  text, even when reordered; removed steps lose their bindings. There is one
  visible plan, not a CLI mirror competing with it. `/goal`, even a cleared
  goal, is a versioned user-context update. Fresh/resumed turns get changed
  ledger state outside the frozen system prompt. After compaction the complete
  current ledger is appended as documented synthetic `shouldQuery: false`
  context: held for the next query, not a new paid turn.
- **Checks before it finishes.** Native scoped verification tracks files:
  checking `a.py` cannot clear `b.py`. Failed checks remain failed evidence;
  local and each remote machine stay separate. CLI edits run native
  post-write quick checks/LSP under the same settings. `verify_check = "on"`
  accepts clean file-scoped automatic diagnostics; `"strict"` needs an
  explicit command, and `"off"` disables verification. Text/data edits don't
  count. Native repeated-failure/unchanging-tool observations and incomplete
  ledger criteria/blocked steps are combined with verification into **at most
  one Stop continuation per user turn**. A child/background result cannot
  renew it. No continuation is required by an empty ledger; completion
  checking can be disabled. Exhausted helpers are report-only: CLI, native
  and external tools are refused.
- **Context length.** The documented `get_context_usage` control reports
  current tokens and the window, with request-id correlation and a bounded
  wait. The UI labels this **reported**; unsupported CLIs fall back to
  **estimated**. Cumulative billing usage is never called current context.
- **Fresh context transfer.** Engine/account switches and unavailable resume
  links send structured state, not a 24,000-character tail: the full ledger,
  last stored native checkpoint, recent user turns, labelled tool/evidence
  stubs and actual image parts from the history. Omitted text is explicitly
  retrievable through `recall`. Claude's hidden checkpoint is never claimed
  to be available. Many or very large images can still exceed Claude's own
  input limits; there is no promise of automatic image compression.

- When it asks you something (its questions with options), the questions come
  as Tessera's own question cards in the chat, on your phone too: tap an
  option (or several, where it lets you pick more than one), or "Something
  else…" to type your own answer. It gets your answers and carries on; Cancel
  tells it you'd rather not say.
- If you switch a chat between Claude and another model, each one is told
  what the other said, so the conversation carries on.

## Fast mode

The lightning bolt (fast mode) works in Claude chats on models that offer
it (Claude Code lists them: Opus 5.5, Opus 5 and Opus 4.8 today). Claude
Code's fast mode answers up to 2.5 times faster with the same model, at a
higher price per token, and **it is paid from your Claude account's usage
credits, not your plan's limits**; usage credits must be turned on for your
account (claude.ai → Settings → Usage). When Claude Code can't turn it on,
the chat says why and runs at the usual speed. Turning it on in the middle
of a long chat costs more than at the start (the whole conversation is read
again at the fast price once).

## Speed: one process across turns

Starting Claude Code takes a few seconds before it answers (measured on a
small server: 3.8 s for a new chat, 3.0 s to continue one, against 0.6 s
for a message to a Claude Code that is already running; through Tessera on
Sonnet, 5.1 s to the first word for a new chat against 1.4 s for the next
message). So after a turn,
Tessera keeps that chat's Claude Code running for up to 10 minutes; your
next message goes straight to it. It is started afresh when the chat's
model, thinking level, permission mode, fast mode, folder or Tessera's
context for it changed, after a rewind, and when it was stopped. At most
three are kept (each holds a few hundred MB); one that takes a turn up by
itself meanwhile goes on as a turn of the chat (above). Set
`claude_keep_open_minutes` under `[agent]` in
`config.toml` (0 starts a new one for every turn).

### Long-lived Turn bookkeeping follow-up

Resolved policy/approval records are deliberately **not compacted in this
change**. Original hook input/provenance, gate outcomes and denials prevent a
late or changed duplicate callback from acquiring fresh approval or losing its
actor floor. Safe compaction needs separate approval/provenance regressions,
not deletion of those security records during background accounting.

Memory grows linearly for the lifetime of one retained Turn: at most one
`policy_hooks` entry per distinct hooked call ID (full input/provenance), one
`gates` entry per distinct prompted call ID (tool/input/mode/result), one
`denied` outcome per distinct denied call ID, and one cancellation token per
approval/question request ID. `counted` keeps one small ID per distinct
assistant message to avoid charging replayed usage twice. Terminal identities
keep one small string per distinct ended task/call/alias, with no eviction.
The policy/gate byte cost also grows with the stored input sizes, not just the
number of calls. All are released when that Turn is dropped, not at every
result; recurring wake-ups can keep the Turn alive across multiple waits, so
the 12-hour continuous-wait cap is not a bound on this total bookkeeping.
Compacting resolved policy records while preserving duplicate-approval and
provenance protections remains a follow-up.

## Troubleshooting

- **Not listed / "Install Claude Code on …"**: the server doesn't find
  `claude`. Install it there, or set `base_url` as above, then press Check
  again.
- **"Sign in by running `claude` once on …"**: Claude Code isn't signed in
  for the user the server runs as. Sign in in a terminal as that user.
- **"Signed in with an API key"**: Claude Code uses an API key there (for
  example `apiKeyHelper` in its settings), so its use is billed per token.
  Run `claude`, then `/login`, to use your plan. Tessera doesn't pass its own
  provider keys (`ANTHROPIC_API_KEY`…) to Claude Code.
- **A wrapper script named `claude`** (for example one that points Claude
  Code at another backend) is what Tessera runs. Name the real one in
  `base_url` if that's not what you want.
- It runs inside Tessera's **command sandbox**: Tessera's data, settings and
  secrets are hidden from it, as from every command an agent runs. Your
  `~/.claude`, the chat's folder and the network work as usual.
- **When a turn goes wrong, the chat says why** in a line of its own, and you
  can send your next message as usual:
  - "too old for Tessera": update Claude Code (`claude update` on the
    server); the chat keeps its conversation.
  - "stopped before finishing its turn (it was killed, signal 9 …)": the
    computer likely ran low on memory; what it had said stays in the chat.
  - "Claude's servers didn't answer …; Claude Code is trying again": said
    once per turn while it retries; if it gives up, the reason follows.
  - "Tessera's own tools … didn't connect": this turn goes on without notes,
    memory and the like; the next message tries again.
  - "This chat's folder … doesn't exist": choose another folder for the chat.
  - A turn that ends with nothing to show says so too, never in silence.

## When your Claude plan reaches its limit

Claude Code's public stream can report a 5-hour or weekly plan rejection and
its reset instant. The chat gives a plain notice, with that instant shown in
each app device's local time zone (including the date). Stored notices and
notification bodies keep the reported UTC instant; no account files are read.
A missing or invalid reset stays “Claude Code didn't report a reset time.”
Tessera does not guess one from free-form CLI text. A request for usage credits
says so and does not wait for a reset that cannot purchase credits.

A chat you are watching stops cleanly at the rejection: send a message later to
continue, or choose another model. A job, helper, internal/bot hand-off, YOLO
chat, or chat whose last human message is at least 15 minutes old can wait and
continue itself. The turn gets **one total six-hour wait budget, at most three
retries**, and at most one notification in total for that episode if work
notifications are enabled (not another terminal error or Finished notification).
Deduplication is persisted for chat, account, limit type and reset instant,
only after recording succeeds. Reconnects, restarts and a later send do not
repeat that window. A different account/window/reset can notify again. System
notifications show reset times in the receiving device's local time.
**Stop cancels the wait.** Resets beyond the budget or reset-less rejections
end with the same plain line. The configured same-model account rotation still
happens first; there is no silent switch to another model.

## Bridge overhead and native-tool boundaries

The extra guidance is short: tool descriptions give actions rather than a
second list of full descriptions in the system prompt. Task ledger snapshots
are injected only when their content version changes; cleared goals count as
changes. Completion nudges remain bounded by the existing acknowledged state.
No extra model request is needed for an MCP handshake or tool inventory.

The public MCP tool-search metadata requests that note, whiteboard, memory and
notify load upfront. Other tools, including native file writes/edits/glob and
the reader, are eligible for on-demand loading when CLI tool search is enabled
(its documented default). A user's disabled search or an incompatible backend
can still load all tools upfront; Tessera does not override their CLI settings.
Per-tool `alwaysLoad:false` requires CLI 2.1.285 or later (checked here with
2.1.286). These are supported requests, not a claim that the CLI's private
prompt or exact token bill is visible to Tessera. Persona, memory, project guidance and environment context
are still included; byte measurements exclude those variable user blocks.

The native bridge uses the same tool schemas and actor-scoped dispatch as other
Tessera engines. Tools disabled by a bot preset or Read-only are not offered;
a forged call is refused. Private Claude chats retain their existing narrow
bridge: only an explicitly selected public search provider, not the native
reader or local file tools. Claude's own similarly named tools remain available
within the same permission ceiling, but are alternatives: CLI Edit is not a
Tessera tagged-line edit, and CLI WebFetch is not Tessera's reader. Exact private
compaction summaries, CLI child billing and independent messaging/apply/discard
of CLI mirror helpers remain unavailable through the documented stream.

Only the tagged `lines` edit mode requires the targeted lines to have been
shown by read/grep; `replace` instead requires a unique old-text match unless
`all` is set. Both use the native dispatch and checks.
