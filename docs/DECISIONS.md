# Architecture decision records

Short records of choices that shape data models, protocols or security, with
the alternatives that were rejected.

## ADR-001 Rust core, single binary
**Decision.** Engine, server, CLI/TUI in Rust; one static-ish binary.
**Why.** "Lightweight" is a requirement: ~10 MB binary, millisecond startup,
tens of MB idle RSS, no runtime to install on a home server.
**Rejected.** TypeScript/Bun (omp's stack): faster to write, but a runtime,
more memory, larger installs.

## ADR-002 Server-centred topology
**Decision.** The harness runs on a machine you own; desktop, Android and
remote terminals are clients. Android never runs the harness.
**Why.** Long tasks continue when a phone sleeps; local GPUs are usable from
anywhere; one source of truth for sessions, memory and credentials.
**Consequences.** Clients reconnect and resync by sequence number; the web UI
inside Tauri never touches the network (so custom proxy headers work on
WebSockets and tokens never reach JavaScript).

## ADR-003 Endpoint lists instead of network detection
**Decision.** A saved server has ordered endpoints; the client probes them
concurrently and uses the first healthy one in preference order.
**Rejected.** Wi-Fi SSID rules (platform-specific permissions, fragile).

## ADR-004 Tauri 2 with one web UI for desktop and Android
**Decision.** SolidJS UI shared by Linux, Windows, Android and browsers.
**Why.** One interface to polish; responsive layouts for slab/foldable/desktop.
**Rejected.** Native Kotlin Android (two UIs to maintain).

## ADR-005 Durable task ledger outside the transcript
**Decision.** Goal, constraints, plan, decisions, questions, artifacts and
done-criteria live in session state, maintained by the `task` tool and
re-injected verbatim at every compaction.
**Why.** Critical task state must not depend on a summary.

## ADR-006 Append-only prompt between compactions
**Decision.** No history rewriting except at compaction boundaries; system
prompt frozen per session; reasoning replay defaults to `all`.
**Why.** Keeps llama.cpp's KV cache and provider prompt caches hot (large
latency/cost wins on long sessions) and satisfies providers that bind
reasoning signatures to the exact prefix.

## ADR-007 SQLite for everything
**Decision.** One database: sessions, entries (+FTS5), tool-run markers,
artifacts, file snapshots, jobs, devices, pairing codes.
**Why.** Crash-safe (WAL), searchable history for `recall`, zero services.
**Note.** Keep the data directory on local disk: SQLite locking over SMB/NFS
is unreliable.

## ADR-008 Device-code sign-in for ChatGPT
**Decision.** Use the device-code flow (URL + code on any device), not a
localhost redirect.
**Why.** Works on a headless home server and from a phone. The flow and
client id are the ones Codex CLI, Hermes and omp use; OpenAI may change them.

## ADR-009 Approvals as a policy layer, not per-tool prompts
**Decision.** Tools report risk (`None`, `Ask{key}`, `Block`); the engine
decides based on the permission mode and session allowances. Approval
requests are events any connected client can answer; subagent requests
surface on the parent session.

## ADR-010 Skills in the agentskills.io format
**Decision.** `SKILL.md` + frontmatter, index in the prompt, body on demand;
project dirs `.tessera/skills`, `.agents/skills`, `.claude/skills`, user dir,
extra dirs (e.g. `~/.hermes/skills`).
**Why.** Interoperates with existing skill libraries.

## ADR-011 Bots are configuration over one engine, not separate processes
**Decision.** A bot is a record (persona, model, permission, tool preset,
memory policy, contacts). Sessions reference a bot; in channels the engine
switches the acting bot per turn and rebuilds the view from its perspective
(its own replies are assistant turns; others' are labelled input).
**Why.** Separation of duties without duplicating the agent loop; every bot
conversation is an ordinary session (persistence, resync, compaction,
approvals and panes all work unchanged).
**Security.** A bot's tools are filtered by its preset; `message_bot` is only
offered when a permitted contact exists; recipients act with their *own*
permissions and memory; `recall` is limited to the bot's own sessions;
bot-to-bot nesting is capped at 3.

## ADR-012 End-to-end encryption above TLS
**Decision.** Apps pair with SPAKE2 keyed by the pairing code and pin the
server's X25519 key. Every session is a Noise `NK` handshake to that key,
followed by chunked transport frames. The device token is sent only inside
the channel. Plain token access is kept for the browser UI but refused from
the network (`security.require_e2ee`, on by default).
**Why.** Remote access through a Cloudflare tunnel (or any TLS-terminating
proxy) otherwise shows the proxy every prompt, file, tool output and token.
TLS alone can't prevent that, because the proxy *is* the TLS endpoint.
**Choices.** SPAKE2 rather than trust on first use: the 8-digit code already
exists, and with SPAKE2 a man in the middle gets one online guess, not the
key. Noise `NK` rather than `IK` plus device keys: the token-in-channel
keeps revocation and the device table unchanged and still gives forward
secrecy. Browsers are local-only because a proxy that serves a page's
JavaScript can rewrite it, so E2EE in a browser via that proxy would be
theatre.
**Limits.** Metadata (timing, sizes, endpoints, Access headers) stays visible
to the proxy. Rotating the server key (delete `identity.key`) requires
re-pairing every device.

## ADR-013 The command sandbox also cuts off the user's session
**Decision.** Besides Tessera's own directories, sandboxed commands get
empty, read-only views of the session's runtime directory
(`$XDG_RUNTIME_DIR`, `/run/user/<uid>`), tmux/zellij/screen/emacs socket
directories and `/tmp/.X11-unix`. The SSH agent socket is put back. On Linux
6.12+ the Landlock domain also blocks connecting to abstract Unix sockets made
outside it. `security.sandbox = "strict"` refuses to run without Landlock.
Bash children don't inherit `TESSERA_*` or well-known model-provider key
variables.
**Why.** Hiding files isn't enough when the same command can ask a process
*outside* the sandbox to read them: `systemd-run --user --pipe cat
~/.local/share/tessera/auth.json` over the session bus did exactly that, and
tmux, editors and container engines offer the same thing. Without Landlock,
`/proc/<pid>/root` of any of the user's other processes shows the real
filesystem.
**Costs.** Commands can't use `systemctl --user`, D-Bus, the GPG agent
(so no commit signing inside the agent), rootless Podman/Docker or GUI apps.
**Limits.** This is not a container. A rootful Docker socket (docker group),
the user's shell startup files, cron and anything else that runs code later
or elsewhere as the user are still reachable. For strong isolation run the
server as its own user or in a VM/container.

## ADR-014 Untrusted repository content never names Tessera's files
**Decision.** Everything the harness reads because of the working directory
(AGENTS.md and friends, project skills, grep/glob roots) is resolved to its
real path and dropped or refused if it lands in Tessera's protected
directories or a known credential file. Skill tools also refuse paths that
leave their skill folder. The server's private key is redacted from tool
output like any other secret.
**Why.** A cloned repository controls its symlinks. A link called
`AGENTS.md` or a project skill's supporting file could otherwise put
`identity.key` into the system prompt, or let `skill_manage write_file`
rewrite `config.toml` (and with it the MCP commands Tessera runs).


## ADR-015 An agent's work never puts a tile on the user's screen
**Decision.** What an agent does shows in the chat: browsing in background
tabs (each step's card has a small picture of the page and a Watch that
opens the live page where the user is), whiteboards and notes as cards with
Open, images in the chat itself. A tile goes up only through an explicit
call: the browser's `show` (the user asked to see it) or `needs_user` (a
sign-in, CAPTCHA or check only the user can do, shown with the reason and
waited for), `whiteboard` `open: true` or an `ask` on a board, a note's
`show: true`. `open` never shows a page, even when an older instruction
says `show: true` (the result tells the agent how instead). Updating a board
never brings back a tile the user closed. Placement is unchanged: beside the
chat in its mosaic, never moving the user to another mosaic.
**Why.** Prompt wording alone cannot guarantee that background work stays out
of the user's workspace. Defaults in code enforce this for every model and
engine (Claude Code's bridged tools run the same code).
