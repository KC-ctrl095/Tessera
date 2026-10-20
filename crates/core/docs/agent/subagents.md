# Subagents
`agent` starts subagents: fresh-context workers with the same tools (or read-only with `readonly: true`), optionally on another `model` (`provider/model`, part of a name, or `@role`).

- Background by default: the call returns at once, each report arrives in the conversation as it finishes, and you are prompted to continue when the whole batch is done. Don't poll. Use `wait: true` only when you can't do anything useful without the results.
- Up to 32 tasks per call; helpers follow this chat's limits (otherwise server defaults: 4 at once, 40 tool calls, 15 minutes each). The user sets numbers or No limit in Chat options → Helpers; server defaults are in Settings → Models. More helpers queue visibly until this chat, the server's capacity/memory and the model's lane have room. Budgets begin then, not while waiting. Only when the user asks, use `settings` paths `chat.helpers`, `chat.helper_steps`, `chat.helper_minutes`: positive number, 0/"no limit", or null for defaults. Unlimited work can use a lot of their plan/credits; permissions and loop detection still apply.
- Per-task `daybreak: true` runs the helper with OpenAI's Daybreak Blue (defensive security work); its `model` must be an OpenAI one the account offers it on, e.g. `openai-codex/gpt-6.1-sol`. Without it, a helper inherits its chat's Daybreak choice.
- Optional per-task `cwd` selects an existing server folder, relative to this chat's working folder or a full path. It must be inside that folder or a configured allowed folder; symlinks cannot escape that scope. Read-only folder rules stay read-only. Machine access is inherited, not widened: this option does not select a remote machine. With `isolation: "worktree"`, `cwd` selects the repository and subfolder; the whole repository must also be in scope.
- Prompts must be self-contained: the subagent sees nothing of this chat. Say what to return and how deep to go.
- Long reports are shortened when delivered to keep your context small; `agents {"action":"report","name":...}` returns the full text. `agents` also shows status or stops them.
- Subagents can't start subagents, change schedules, or write memory/skills.
- The user can switch subagents off for a chat; then `agent` and `agents` aren't offered and you work alone.

## In Claude Code chats
Use `mcp__tessera__agent` for native helpers on GPT/local models; `agents` manages them:

```json
{"tasks":[{"name":"Review","prompt":"Review the diff; return ranked file:line findings.","model":"openai-codex/gpt-6.1-sol","readonly":true,"cwd":"tessera"}],"wait":false}
```

Background is default; `wait: true` joins. Same slots, provider lanes, budgets, reports and worktrees; reports reach this chat without polling, even after your turn ends. The bridge allows24h; helper budgets still apply. Saved `agents {action:"start",tasks,wait}` still dispatches, but new starts use `agent`.

Model order here is task `model`, then agent type's `model`, then the server default, **not this chat's model**. If the server default is Claude Code, set a model explicitly: it won't start another Claude Code process by surprise. An explicit task or type model may choose Claude Code deliberately. Unknown task or type models refuse the whole batch and list available models; unlike native chats, an unavailable type model does not fall back to Claude.

Helpers inherit this chat's permissions, machines, secrets and tool preset, with watch-only chats above it. CLI children cannot nest or delegate to bots/jobs. Stop/delete retains native controls. Private Claude chats have no helper bridge. Main hand-offs need exact validated hook id/tool/input and affirmative admission, not a transcript card.

## Agent types
`agent` on a task picks a definition; the `agent` tool's description lists the ones available. Built in:
- `explore`: read-only codebase search; answers with `path:line` references (30 steps).
- `review`: code review of a diff, commits or files; findings ranked P0–P3 with `path:line`. It may run `git` and tests but has no `write`/`edit`.
- `research`: read-only web research from primary sources, every claim cited (30 steps).

Start acknowledgements include the **effective** role/tools/permission ceiling, what its commands may do, and authorized machine inventory. Warnings flag common write, command and remote-machine mismatches without granting access. `readonly:true` swaps `bash`/`job` for `bash_readonly` (when the type's tools include `bash`, or there's no type), with no stored secrets and nothing in the background. With the sandbox on and Linux 5.19+ any command runs, writing only to the helper's private scratch folder (`TMPDIR`, `CARGO_TARGET_DIR`, removed when it finishes) and without the network unless its type says `network: true` (the user's or a built-in type; a project's can't). **Without a write-confining sandbox (sandbox off, older kernels, Windows), allowed cargo commands run the project's own build scripts and tests unconfined: they can change files, read user-accessible files and use the network.** Scratch paths and `CARGO_NET_OFFLINE` are not confinement for that code. Only listed commands run there: git reads (status, diff, log, show, blame, grep, branch/tag listing…), searches and reads (rg, grep, find, cat, ls, wc…) and cargo test/clippy/check/build/bench/doc/`fmt --check`. Do not promise these helpers cannot change anything on such a server; run builds only for trusted projects. Review roles have no file-write tools; have the parent save findings. Machine `read`/`ls` can remain available read-only when the role allows `machines`; command runs cannot.

A registered tool may be unavailable because parent authority is temporarily busy. That is a fail-closed permission result, not a renamed/unknown tool: retry later or return the blocker, never bypass the refusal. Parent/bot restrictions continue to apply if they tighten after launch.

Your own: the user's types (everywhere) are kept in Tessera's settings folder, which tools and commands never open (don't try to read or list it): `agents {"action":"types"}` lists every type available here with its tools, model, read-only flag, effective budget and network setting (`name`: one type with its instructions), and `agents {"action":"define","name":"test-writer","text":"<the whole file>"}` saves or replaces one of the user's types. Default/Ask modes show the whole file plus its effective budget (clamped to the configured helper limits) and network grant in the approval card. **Auto/YOLO saves without a card, like `skill_manage`, including types with `network: true`; choosing full autonomy includes saving instructions for future chats.** The saved acknowledgement still shows the effective settings. A project's types are Markdown files in `.tessera/agents/` (from the working directory up to the repository root), written with the ordinary file tools. Project definitions win over the user's, which win over the built-ins.

```markdown
---
name: test-writer
description: Writes focused unit tests for one module.
tools: read, grep, glob, edit, write, bash   # allowlist; omit for all; `mcp__github__*` matches a family
model: "@fast"                                # optional; `inherit` or none = the chat's model
readonly: false
network: false                                # read-only helpers' commands may use the network (not in a project's file)
max_steps: 25
max_minutes: 10
---
You write unit tests. Read the module and its callers first...
```

The body becomes the subagent's `# Agent role` in its system prompt. **All types, including the user's and built-ins, can only lower `agent.subagent_steps` and `agent.subagent_minutes`, never exceed them.** Limits are checked again when a queued or continued helper starts; saved types do not bypass later configured reductions. A project's definition comes from the repository, so it gets the same trust as its AGENTS.md: it can narrow tools, force read-only and lower budgets, never grant network, and a subagent never has more permission than the chat that started it. `network: false` confines sandboxed read-only commands only, not unconfined project code. If a definition's model isn't available, the task runs on the chat's model (the start message says so).

## Structured reports
`output` on a task asks for JSON: a JSON schema (`type`, `properties`, `required`, `items`, `enum`, `additionalProperties: false` are checked) or a short description such as `"a list of {path, line, issue}"`. The subagent is told the format; its answer is parsed (a code fence or surrounding prose is tolerated) and checked. If it doesn't fit, it gets one retry (no tools); after that you get its text with a note saying what didn't match. A valid report reaches you as compact JSON and is never shortened.

## Worktree isolation
`isolation: "worktree"` gives a subagent that edits code a private, detached git checkout (in the repository's `.git/tessera/worktrees/`), starting from the current state including uncommitted changes and untracked files (up to 5 MB). Your files don't change while it works, so several can edit in parallel. When it finishes its changes are kept as a patch, the checkout is removed, and its report says what changed. Then:
- `agents {"action":"apply","name":...}` applies the patch to your files as unstaged changes, all or nothing: if anything conflicts with what changed meanwhile, nothing is applied and you get the patch's artifact id (read it with `recall`) to apply parts by hand.
- `agents {"action":"discard","name":...}` drops it.

Only in git repositories (others are refused). The checkout has no ignored files (build outputs, `node_modules`, `.env`), so builds there start cold. Checkouts left by a crash are saved as patches and removed at the next start.

Per task, `cwd` selects an existing unprotected folder **inside the chat's folder or a configured allowed folder**, relative to the chat or absolute. For a chat in `~/projects` and a repo in `~/projects/tessera`, use `{"cwd":"tessera","isolation":"worktree"}`. Each task can pick its own repo; Tessera does not guess among child repos. Canonical paths outside that scope, including symlink escapes, are refused before any child starts. Configured read-only folders stay read-only. Change the chat folder explicitly if another repository is needed. Project roles are discovered in the selected folder. Start/status show the selected folder and planned checkout (not yet created while queued); running/completed status gives the actual worktree path. For linked Git checkouts, the common-dir path is resolved after a slot and then shown in status.

The selected folder is bound to its directory identity, not just its name. If it is moved/replaced (including a new symlink), launch, continuation and tool writes fail closed; restore the original directory or start a new helper. Helpers saved before folder-identity protection must be started again rather than silently trusting their old path. For worktree isolation, Git's resolved per-checkout metadata, common directory and destination must also be writable within the parent's existing scope and outside protected stores; a `.git` file/link is not authorization. These checks run again before recreation. An explicitly allowed writable metadata folder is supported, while read-only metadata cannot be widened by isolation.

Launchers keep that directory open until the process has started. This applies to Bash, quick checks, full/one-shot Claude launchers and isolation's Git commands (Git must not override entry with absolute `-C`). Without the command sandbox, Unix enters it by descriptor, not by its replaceable name. With the sandbox, the folder is entered by its path inside the sandbox's view and must be the held directory there, so hidden folders, read-only rules and the private process view also hold for relative paths; a hidden, moved or replaced folder runs nothing ("Nothing was run", exit 126 in the shared namespace of older kernels). Windows holds the folder without delete sharing, so it can't be renamed or replaced until the process is in it (that folders above it can't be renamed either is documented NTFS behaviour, not yet confirmed on Windows). A command-specific helper cwd must stay beneath the bound helper folder and is opened from it, one name at a time, without following links.

## Continuing a subagent
`agents {"action":"message","name":...,"text":...}` continues a finished subagent: it keeps its conversation (and, if isolated, its checkout, re-made from its patch), gets a fresh budget, and its new report arrives like the first. After `apply`, only newer changes come back; a discarded one can't be continued.

`status` distinguishes **queued** (waiting for a provider lane, chat limit or server capacity) from running, shows run-budget remaining, and retains children interrupted by a server restart. Start/continue acknowledgements mean accepted, not necessarily executing. Budgets begin after admission. A planned restart resumes accepted helpers automatically under the persisted chat limits; inspect reports/files for uncertain side effects instead of replaying tasks. For a stopped/interrupted helper, continue explicitly with `agents message` and its **session id**. `report` keeps all final reports across follow-ups, newest first with stable assistant-entry ids; a short continuation does not replace the original evidence.

## When a helper waits for an approval
A helper whose action needs the user's approval waits for it (the card is in your chat). You are told at once: a `[Helper "name" waits for approval <id> …]` message with the action, and `agents status` shows each wait with its id and the minutes left. If you could do that action yourself without asking right now (your permission mode as it is when you answer, or the user allowed it in your chat for the session), `agents {"action":"approve","name":...,"text":"<id>"}` answers it; otherwise only the user can approve it. `agents {"action":"decline","name":...,"text":"<id>"}` is always yours: the helper is told you declined and carries on. Without `text`, approve takes the oldest you may approve and decline the oldest; the answer names the action it answered. Unanswered, it expires after 15 minutes and the helper is told plainly that nothing ran. Don't approve what the user should decide; when in doubt, decline or tell the user.

## Rewind
Users can rewind a chat (not something you call): files you changed with `write`/`edit` (and your subagents' edits, and applied patches) go back to how they were at an earlier point, and/or the conversation is cut back to it, with the full history kept as a "(rewound)" chat. Shell commands' changes are not undone. When only files are rewound you get a `[Rewind]` note listing what was undone.
