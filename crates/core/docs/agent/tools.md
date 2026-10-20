# Tools

**Small model mode.** For models running on the user's own computer or network (and other small models), Tessera starts a chat with the core tools only, with simple arguments, so the model has less to read and choose from: `read`, `write`, `edit`, `bash`, `grep`, `glob`, `job`, `task`, `web_search`, `web_fetch`, `memory`, `docs`, and `whiteboard` for asking the user a question. Reading this topic (`docs` with topic `tools`) turns all the others on from your next step; that costs one full re-read of the conversation on a model server at home, so do it when a task needs one of them, and only once. Calling one of them by name turns them on too.

The other tools:
- `browser`: a real web browser (pages that need clicks, logins or JavaScript), shared with the user in a web view tile. `docs browser`.
- `note`: the user's notes library (documents they keep). `docs notes`.
- `whiteboard` (all of it): show the user a page, picture, chart or web app in a card or tile, and look at what they see. `docs whiteboards`.
- `schedule`: jobs that run a prompt on a schedule. `docs scheduling`.
- `notify`: a notification on the user's phone and computers, now or later. `docs notifications`.
- `share_file`: a file the user can download.
- `agent` / `agents`: subagents that work in parallel, in the background by default. `docs subagents`.
- `machines`: the user's other computers. `docs machines`.
- `message_bot`: hand work to another bot; its reply arrives later in this chat. `docs bots`.
- `skill` / `skill_manage`: load a saved procedure; save one (rare). `docs memory`.
- `recall`: search this and earlier chats, and fetch exact earlier output after a checkpoint. `docs context`.
- `lsp`: a language server's definitions, references and errors (where one runs). `docs code`.
- `settings`: read or change Tessera's settings (the user approves changes).
- `save_secret`: keep a password or key the user typed in the vault.
- Tools from the user's MCP servers (`mcp__…`). A server whose tools would take more than a tenth of the model's context up front waits until first used: the prompt names its tools in one line (`# MCP tools`); calling one turns that server's tools on from your next step, so call it again then (this topic turns them all on).

**Simple forms.** In small model mode `edit` takes one change per call: `{path, old, new}` (exact text, `all: true` for every match) or `{path, tag, at, text}` (the TAG and line numbers from your latest read). `task` takes `{plan: [steps]}`, `{done: [step numbers]}` and `{goal}`. The longer forms (`replace`, `lines`, `update`, `verify`…) still work.

**Lean start** (`[agent] lean_tools`, off unless the user turned it on): the same switch for every chat. A chat starts with every tool except `note`, `whiteboard`, `browser`, `schedule`, `notify`, `share_file`, `save_secret`, `settings`, `machines`, `message_bot`, `agents` and `skill_manage`, which the prompt lists in one line; calling one of them (or reading this topic) turns them all on from your next step, so call it again then. `edit`, `agent`, `task` and `memory` (and `machines` and `whiteboard` once on) have shorter descriptions (the rules are the same; details here and in `docs code`, `docs subagents`, `docs memory`, `docs machines`, `docs whiteboards`). The prompt lists at most 2,000 characters of skills, the most used first (`skill` lists them all), and at most 10 saved passwords, the most recently used first: `save_secret` with `action: "list"` lists every one this chat may use (names only, never values).

**Otherwise** every tool the chat may use is on from the start.

## Shell cleanup

Stop and `job kill` target retained Linux process handles and descendants carrying
this launch's private job marker. The start identity and marker are rechecked on
numeric compatibility attempts, which are not race-free. Discovery and descriptor
failures produce a separate cleanup state saying some processes could not be checked,
in results, `job list`, and `job output`. A command's exit code does not prove cleanup.

Cleanup is best-effort. Double-forked daemons that clear their environment and detach
before observation can escape. A joined PID namespace does not grant namespace-wide
cleanup. There is no per-job subreaper or crash/startup recovery guarantee in this
release; adoption of a shared T0/T9 supervisor is a follow-up.

Stop publishes pending cleanup immediately. An expired wait returns an addressable
job id. Results distinguish cleanup running from cleanup finished/output draining;
records stay until cleanup and final drain/exit are published, and pending jobs are never pruned.
Own `job list/output/wait` remain readable in read-only sessions; `kill` requires command permission.
At 8 pending cleanups in a session or 32 globally, new commands are refused until earlier cleanup finishes.

Windows cleanup retains the command's spawn-time Job handle, terminates off the
async worker, and checks the Job's active-process count with a bounded wait.
Missing ownership or failed checks mean "not checked", never a clean-tree result.
Other Unix groups can be signalled only while their actual leader is unreaped;
no delayed numeric signal or post-reap group cleanup is allowed.
