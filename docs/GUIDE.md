# Tessera guide

The [README](../README.md) is the short version; this is the long one: what
each part does, every way to set it up, and the settings behind it.

- [Setting up](#setting-up)
- [What it does, in detail](#what-it-does-in-detail)
- [Tiling, bots and YOLO](#tiling-bots-and-yolo)
- [Server, apps and remote access](#server-apps-and-remote-access)
- [Building the apps](#building-the-apps)
- [Configuration](#configuration)
- [Project layout](#project-layout)

## Setting up

One command installs Tessera (see the README's Quick start); then
`tessera setup` walks you through four short steps. Each shows what is
already set up, Enter takes the suggested answer and esc skips a step; run it
again any time, or one step at a time (`tessera setup models`):

1. **Your AI**: it first looks for what is already there: model servers
   (llama.cpp, Ollama, LM Studio, vLLM) on this computer and on the other
   computers of your home network (it only asks for their model lists, which
   never loads a model), and your own Claude Code. Then you pick: Claude
   (your Pro or Max plan, through your own Claude Code: Tessera can install
   it, you sign in to it yourself), ChatGPT (your Plus or Pro plan: a code to
   approve in any browser, also over SSH), a model server it found, or an API
   key (OpenRouter, OpenAI, Anthropic, or any OpenAI-compatible server's
   address). A server on this computer becomes the default by itself; one
   elsewhere on the network only when you pick it.
2. **Where you'll use it**: only this computer, also your phone and laptop
   at home (Wi-Fi), or also from anywhere: an automatic Cloudflare Tunnel
   (with your own domain), Tailscale (free, no domain) or your own proxy.
   Setup waits for a tunnel to connect and says what's in the way if it
   doesn't (often a firewall blocking port 7844). No domain? See
   [Reach Tessera from anywhere](REMOTE_ACCESS.md).
3. **Keep it running**: a background service (systemd) that starts with the
   computer and keeps going when you log out.
4. **Your phone**: a QR code for the Tessera app that carries the addresses
   and any access headers, plus a private link to paste on a computer
   without a camera.

Then a summary, with the safe defaults in plain words. More, any time:
`tessera setup web` (DuckDuckGo works without setup; or your own SearXNG),
`tessera setup where` (use a Tessera server you already have from this
terminal), `tessera setup security` (what keeps you safe).

It animates in a capable terminal, fits 80×24, and falls back to plain
numbered prompts with `NO_COLOR`, `TERM=dumb`, `--plain`, `--no-anim` or piped
input. Keys and tokens are read hidden and never printed. On a desktop
without a home server, choose **Start Tessera here** in the desktop app: it
starts the server, gets its own device token straight from the local store,
and connects; its setup then has the same steps. The defaults are the safe
ones:
- The server listens only on this machine until you choose otherwise.
- Pairing is end-to-end encrypted.
- Agents never see secrets.
- The sandbox is on.

Want to see a first run without touching your own setup?
`sh scripts/first-run.sh terminal` (or without `terminal`, the desktop app's)
runs it in a throwaway profile on port 7979; `sh scripts/first-run.sh clean`
removes it.

### Models by command

```bash
tessera providers                        # what is set up, and their models
tessera model local/qwen3.8-27b          # the model new chats use
tessera add-provider openrouter --url https://openrouter.ai/api/v1 --key "$OPENROUTER_API_KEY"
tessera add-provider anthropic --kind anthropic --url https://api.anthropic.com --key "$ANTHROPIC_API_KEY"
```

(A key in a variable stays out of your shell's history; `tessera setup
models` asks for it hidden instead.)

**Your Claude Pro or Max plan**: install
[Claude Code](https://code.claude.com/docs/en/setup) on the server and run
`claude` once to sign in. Tessera then offers **Claude (your subscription)**
models (`claude-code/sonnet`, `claude-code/opus`…): chats on them run your
own Claude Code headless, in the chat's folder and Tessera's sandbox, with
its permission prompts as Tessera approvals. Tessera never signs in to Claude
itself. See [Use your Claude plan](CLAUDE_CODE.md).

## What it does, in detail

- **Harness-first, lightweight.** One program (about 30 MB, 11 MB to
  download) that idles at about 20 MB of memory as a server; no Node, Python
  or Docker required. The
  terminal works with no GUI installed. Quick questions round-trip in under
  a second on a local model.
- **Context engine built for long work.** A durable *task ledger* (goal,
  constraints, plan, decisions, done-criteria) lives outside the conversation
  and is re-injected verbatim after every compaction. Compaction escalates
  only as far as needed: first stop replaying old reasoning, then stub old tool
  output, and only then write a validated structured checkpoint that keeps
  your own messages verbatim. The checkpoint is written in the background
  while the agent keeps working, so long tasks don't stall to compact, and
  it is condensed when it outgrows its budget, so a session can run for
  hours or overnight. History is append-only
  between compactions, so prompt/KV caches stay hot, and everything stays
  retrievable with the `recall` tool, which also finds earlier conversations
  ("what did we decide last week?").
- **Crash- and disconnect-safe.** Every step is persisted as it happens. Close
  the app mid-task and the server keeps working; reconnect and you resync
  exactly. After a server restart, tool calls whose outcome is unknown are
  reported to the agent as *uncertain* rather than silently retried.
- **omp-style coding tools.** Reads return `[path#TAG]` plus numbered lines;
  edits target line ranges with a content tag (stale edits are rebased when
  their lines are unchanged, refused otherwise), or use replacements with
  indentation-tolerant matching and helpful near-miss hints. Every edit
  reports the errors a language server says it introduced, and an `lsp` tool
  offers definitions, references and renames. Built-in gitignore-aware grep/glob, shell with live output and
  background jobs, web fetch/search, subagents.
- **Hermes-style memory, skills and learning loop.** Bounded `MEMORY.md` and
  user-profile notes, agentskills.io `SKILL.md` skills with progressive
  disclosure (compatible with Claude Code and Hermes skill folders), and a
  background reviewer that curates memory and skills. It runs every few
  turns, when a chat goes quiet, and before anything is summarised away, so
  it learns from everyday use. It saves what it saw rather than guesses, and
  makes skills only of verified, multi-step procedures likely to recur (never
  for simple tasks, one-offs or single facts, and never by asking you),
  updating existing ones first; Tessera refuses a new skill that is too thin,
  a near-duplicate or past 30. Skills can be deleted in Settings → Memory &
  skills. Everything is bounded (profile 2k characters, notes 4k, skills
  index 6k), so it never bloats.
- **Private chats.** Like the temporary chats on ChatGPT or Claude, but on your
  hardware: web search on, no memory or history loaded, titled without asking
  any model. Nothing is ever written to disk: not the database, its journal,
  its search index or logs. Only the device you made it on ever sees it: not
  your other phones and computers, not the terminal app, not another web
  browser. Deleting one, signing that device out, or restarting the server
  erases it for good.
- **Subagents that don't block you.** They run in the background by default,
  swarms of up to 32 at once, each with a step and time budget. Their reports
  land in the chat as they finish, and the agent picks up once they're all
  in. Reusable agent types (built-in `explore`, `review`, `research`, or your
  own), structured results, and code changes in a private git worktree that
  you apply or discard. Finished subagents can be messaged again. Switch
  them off per chat when you want the agent to work alone.
- **Steer, queue, rewind.** Messages you send while the agent works wait in a
  queue you can edit; `/steer` (or "Steer") reaches it at its next step,
  "Send now" stops it and sends the message at once, and `/goal` pins what the chat is for through every compaction. The agent
  checks the goal's done-when criteria before it says it's finished. Rewind a
  chat to any earlier message, putting back the files it changed; the full
  history is kept as a copy.
- **Whiteboards.** Agents show you things in the chat, and in a tile beside
  it when you ask: interactive diagrams and 3D explanations (Three.js and d3
  are bundled), markdown, images, a remote desktop or web page, or the web
  app they are building on your server, on any device. They can ask for
  input too, such as a signature. Open a page or image beside a chat yourself and the
  agent can read it, or look at it as you see it when you point something
  out. Board content is treated as untrusted: it renders on its own origin
  with no network access and can't reach the app.
- **A real browser in a tile.** Web view tiles show a Chromium running on
  your server: log in once, and the same session is there on your phone and
  your computer. Sign-in popups open on top of the page in the same tile
  and hand the login back to it, and back/forward, zoom, typing and touch all
  work. Your agent browses in background tabs of its own: each step shows
  in the chat with a small picture of the page and a Watch button, and it
  puts a page on your screen only when you ask, or hands it to you when only
  you can do something there (a sign-in, a CAPTCHA) and waits. Web views
  have no security keys, so sites offer their other ways to sign in; when a
  site wants you to prove you're a person, show the page on your computer's
  screen (tile options) and finish it there yourself. It reads
  pages as compact outlines and asks before it types a
  password, submits a form, buys something or downloads. Frames go only to
  the device that is looking, as many at a time as its connection carries,
  smaller while the page moves on a slow link, and sharp (at your screen's
  full resolution) once it stops; on a phone the page follows your finger
  at once. The browser is driven over a private pipe, never a port.
- **A terminal in a tile, for you alone.** New → More → Terminal opens a command
  line on your server, or on one of your machines that allows it (decided on
  that computer). Off until you turn it on (Settings → Privacy & safety → What commands can reach); sandboxed
  where the command sandbox works, so Tessera's own keys and saved passwords
  stay out of reach. Only the device that opened it sees it; agents never get
  one. It works like a native terminal: your own shell settings and colours,
  vim, htop and less, selecting copies and right-click (or Ctrl+Shift+V)
  pastes, Ctrl+click opens a web link, Ctrl+Shift+F finds, Ctrl+= and Ctrl+−
  change the text size; on a phone a row of extra keys (Esc, Tab, Ctrl, Alt,
  arrows, Paste). Its ⋯ menu switches it to another place. See
  [SECURITY.md](SECURITY.md).
- **Notes.** A library of markdown documents you and your agents keep, like
  artifacts: open one as a tile beside a chat and write in it like a
  document (it formats as you type, Obsidian-style: `[[links]]` between
  notes, a `/` menu, tables, tasks, callouts, pictures pasted, dropped,
  picked or taken with the phone's camera, sized with a handle or Small /
  Medium / Large and dragged into place), and ask the agent to update or
  extend it. It stays plain markdown underneath.
- **Usage and what it costs.** Every model request is counted (chats,
  subagents, memory review, titles, summaries, jobs) by model, kind and day,
  priced two ways: what API keys billed, and what it all would cost at API
  prices, so a ChatGPT or Claude plan shows what it saved against its
  monthly fee. Prices of current Claude and OpenAI models are built in, a
  public catalog fills in new ones, and you can set your own.
- **Scheduled and background work.** Cron or `@every 30m` jobs run in fresh
  sessions on the server and show up in every client.
- **Notifications and files, self-hosted.** "Let me know when it's done",
  "remind me at 5": agents notify your phone and computers (`notify`), even
  with the app closed, with no push service in between; tap to open the
  chat. They hand you files as downloads in the chat (`share_file`: to your
  phone's Downloads, or wherever you save on a computer; never credentials),
  and the web view's downloads land there too. See
  [NOTIFICATIONS.md](NOTIFICATIONS.md).
- **End-to-end encrypted.** Apps pair with a PAKE keyed by the pairing code
  and pin the server's key. Every connection is a Noise session to it, so a
  Cloudflare tunnel or reverse proxy relays only ciphertext it can't read or
  tamper with.
- **Credentials stay away from models.** Keep API keys, passwords and whole
  logins (username, password, one-time-code seed…) in the secrets vault
  (Settings → Privacy & safety → Passwords & keys). Commands get
  them as environment variables; models only ever see `$NAME`. Saved values can
  be replaced but never shown again, and each one can be kept for certain bots
  (say, a nightly homelab check) and certain machines. Everything credential-
  shaped is scrubbed from tool output, and a key or login typed into a chat is
  moved to the vault before any model sees it. On Linux the agent's commands
  also run in a kernel sandbox (user namespaces + Landlock, or a private
  process view on older kernels) in which Tessera's own credentials, sessions
  and settings don't exist. You choose the folders it may change without
  asking or not at all (Settings → Privacy & safety; see
  [SECURITY.md](SECURITY.md)).
- **Your other computers, on your terms.** Turn a desktop or laptop into a
  *machine* on that computer (`tessera node enable`) and agents in any chat,
  even one on your phone, can run builds and tests there and read or change
  its files, within the folders you allow it. Nothing remote can turn a
  machine on or widen it; by default it allows nothing, runs no commands and
  asks before every action. Commands there run in the kernel sandbox and can
  change only the folders you made read-write; every action is logged on the
  machine. Pause or revoke it from any device
  ([MACHINES.md](MACHINES.md)).
- **Safety by default.** Destructive, privileged or outward-facing commands
  need approval (allow once / for the session / deny); catastrophic ones are
  always refused; read-only and ask-everything modes.
- **Any model.** Sign in with a ChatGPT subscription (device code — works on a
  headless server), use API keys for OpenAI/Anthropic, or point at local
  servers (found on this computer and your home network). Several ChatGPT
  or Claude plans (personal and work) each get their own entry, models,
  usage and limits; a chat can go on with the next one when a plan's limit
  is reached, if you turn that on. Name model roles (`@fast`, `@smart`,
  and `memory` for the learning review) and fallback chains, so a rate limit
  or an outage moves to the next model. Fast mode (`/fast`) asks OpenAI models
  for priority processing. MCP servers (stdio or HTTP) add tools.
- **Bots you name and put to work.** Create bots with a name, avatar, colour,
  personality and responsibilities, their own model, tools, permissions,
  private memory that grows as you work with them, and notes as knowledge
  (a study partner with your syllabus, a household bot with the warranty
  list). Bots and agents can read your notes; a note's options can keep
  one from them ("Keep from agents"), and then only a chat you link it in
  (`[[Note title]]`) or a bot that has it as knowledge can read it. Give them scheduled duties ("every weekday at 8, brief me"), let them
  hand work to each other privately (only where you allow it; the asking chat
  keeps working, shows the bot at work with Stop, and carries on when its
  reply comes in), or put several
  in a channel and @mention them. A bot's home folder can be made in its
  setup ("Create ~/name"), and every folder picker has "New folder", so there
  is no need for the terminal. Every bot conversation can be opened in a
  tile to watch.
- **Tiling is the interface.** Hyprland-style workspaces 1–9 with auto-tiling
  chats side by side (dwindle, master, columns, rows, grid, monocle), drag to
  split, keyboard driven, in the desktop app. Swipe between
  tiles on phones; two tiles on unfolded foldables.
- **YOLO mode** when you want zero interruptions (`--yolo`, `/yolo`, or the
  permission toggle): nothing asks, only catastrophic commands are still
  refused. (The ⚡ bolt means fast mode, everywhere.)

## Tiling, bots and YOLO

The terminal app (`tessera`) is the lean one: a coding agent in one chat across
the window (the most recent chat in that folder, or a new one), with the
conversation, a message box and one status line (model, folder, how full the
context is). `/split` puts another chat beside it (Alt+←/→ moves between them,
`/close` closes one); everything else is a slash command: `/new`, `/chats`,
`/model`, `/thinking`, `/compact`, `/rewind`, `/goal`, `/copy`, `/settings`
(F2), `/help` (F1 or `?`), `/quit`. Its chats offer only the coding tools
(read, write, edit, commands, search, the plan and web search), and are the
same chats as in the apps: a chat started in the apps opens in the terminal
with all of its tools (it says so), and its whiteboards, notes and web views
show in the apps. Enter sends (while the agent works it waits its turn: ↑
then `e` edit, `s` steer, `x` remove), Alt+Enter steers, Esc interrupts,
Shift+Tab sets how hard the model thinks, Ctrl+C twice quits.

The tile keys in the apps:

| | Desktop / web app |
|---|---|
| New chat in a new tile | Alt+Enter or Ctrl+N |
| Move focus | Alt+H/J/K/L or Alt+arrows |
| Swap tiles | Alt+Shift+H/J/K/L |
| Resize | Alt+Ctrl+H/J/K/L, drag dividers |
| Close tile | Alt+W |
| Maximize / zoom | Alt+F |
| Workspaces (mosaics) 1–9 | Alt+1…9 (Alt+Shift+N moves the tile) |
| Cycle layout | Alt+Space (dwindle, master, columns, rows, grid, monocle) |
| Queue a follow-up while the agent works | Ctrl+Enter or Ctrl+Q |
| Steer the working agent (read at its next step) | Steer on a queued message |
| YOLO (never ask) | The tile's ⋯ → How much freedom |
| All mosaics at a glance | Ctrl+Tab or Alt+\` (phone: "All" in the tile overview) |
| Keep a mosaic on this device only | Right-click or long-press a mosaic → Only on this device |
| Show a page, image or notes beside the chat | Tile options → Show beside this chat, or `/show` |
| Goal for the chat | `/goal <text>` (`/goal clear`) |
| Fast mode (OpenAI models) | `/fast`, or the tile's ⋯ → More |
| Daybreak Blue, when OpenAI grants access | Chat options → Daybreak Blue |
| Model for memory & learning | Settings → Models → More |
| Bots | New → More → Bot (then the sidebar's Bots tab) |
| Channels | New → More → Channel (then the sidebar's Channels tab) |
| Scheduled tasks | New → More → Scheduled task (then the sidebar's Scheduled tab) |
| Everything that notified you | The bell beside New chat (folded phone: in the sidebar, beside Settings) |
| Archived chats | The line under the sidebar's chats |
| Colour theme | Settings → Appearance |
| Cheatsheet | `?` or Alt+/ |

Terminal themes (`/settings` → Theme): Auto (your terminal's own colours,
dark or light), Dark (Nocturne), Light (Travertine), High contrast,
Catppuccin, Gruvbox and Tokyo Night. The list repaints the screen as you move
through it; Enter keeps the theme (it's remembered), Esc goes back;
`TESSERA_THEME=gruvbox tessera` picks one for a single run. 256-colour
terminals get the nearest xterm colours; with 16 colours the terminal's own
palette shows and themes differ in their accents;
`NO_COLOR` or `--plain` turns colour off.

In the app you can also drag a chat from the sidebar onto any edge of a tile
to split there, drag tile headers to rearrange, and Ctrl-click or middle-click
to open a chat beside the current one.

Everything new starts from **New**, at the top right (on phones, of the tile on
screen): a chat, a private chat, a note, a web view, and under More a bot, a
channel, a scheduled task, a terminal or a mosaic (a screen of tiles). Each
tile's **⋯** holds its options. **Settings** (the gear) has eight sections,
each setting in one of them: *Appearance* (this device's theme, text size and
colour), *Notifications*, *Models* (the model for new chats, your services,
usage and cost), *Privacy & safety* (how much freedom new chats get, passwords
and keys, what commands can reach, your bots helping chats), *Memory & skills*,
*Web & voice*, *Devices* (and what agents did on your computers) and
*Connection*; *Help & tour* is at the foot of the list.

Mosaics are stored on the server, so every device shows the same ones: open,
close or rearrange tiles on one and the others follow live (the terminal app
has none: it leaves your tiles as they are). Concurrent edits are merged tile
by tile, the tile you're typing in is never taken away, and private chats are
never written into the layout. A mosaic set to "Only on this device" leaves
the shared layout until you turn that off again.

### Daybreak Blue

Daybreak Blue is OpenAI’s program for defensive security work. Access comes
from OpenAI, not from Tessera. Use it only for your own systems or work you are
allowed to do. OpenAI’s safeguards and usage rules still apply.

In a chat on a supported OpenAI model, open **Chat options** and switch
**Daybreak Blue** on. It starts off and Tessera remembers the choice for that
chat. The chosen model stays the same. Native helpers inherit the choice.
Switch it off to request standard safeguards.

The choice appears only when the signed-in ChatGPT account or OpenAI API
project explicitly lists both Blue and standard safeguards for that model. Red
access alone, a paid plan, or a model named `gpt-daybreak-blue-latest` is not
enough. If the choice is
missing, refresh the models in Settings and check your access with
[OpenAI](https://chatgpt.com/cyber). No approval or unknown model metadata means
Tessera keeps the choice hidden. A saved chat preference cannot grant access.

API keys work only with the official OpenAI Responses service. Tessera checks
OpenAI’s Codex model catalog using that service’s key, just as the official
Codex client does. Other OpenAI-compatible services are not included.

If OpenAI cannot confirm access, an opted-in request stops with an error rather
than silently changing its model or program. If you previously switched it on
and access can no longer be confirmed, chat options offers **Use standard
safeguards** to clear that saved choice. It does not retry a stopped request.
Changing to an unsupported model clears the choice. If OpenAI does not
advertise standard safeguards for a model, Tessera keeps its existing request
behavior. OpenAI may apply its own program defaults in that case, so a hidden
control is not proof of standard safeguards.

[OpenAI’s Daybreak guide](https://developers.openai.com/api/docs/guides/daybreak)
explains approvals, supported models, API defaults and errors.

## Server, apps and remote access

```bash
tessera serve --bind 0.0.0.0:7878       # on the home server
tessera remote https://tessera.example.com \
  -H CF-Access-Client-Id=… -H CF-Access-Client-Secret=…   # optional: tunnel + Access headers
tessera pair                            # QR code + one-time 8-digit code
```

On a phone, tap **Scan QR code** in the app and point it at the code from
`tessera pair` (or Settings → Devices → Get a code in an app that's already
paired). The QR carries everything: the LAN and tunnel addresses, the
proxy headers, the pairing code, and the server key fingerprint, which the
app checks automatically. On a computer without a camera, paste the private
link `tessera pair` prints under the code (desktop app: **On another
computer → Connect to a server**, or `tessera connect '<link>'`); `c` copies it to the clipboard.

`tessera serve` and `tessera pair` print the server's key fingerprint. After
pairing, the app shows the same fingerprint; if they match, nobody sat in the
middle. From then on all traffic between the app and the server is end-to-end
encrypted, whether it goes over LAN, Wi-Fi or a tunnel.

Without the QR code or link, choose **Enter details instead** (phone) or
**Enter the address and code instead** (desktop), add your LAN URL
(e.g. `http://192.168.1.10:7878`) and optionally a remote URL
(e.g. `https://tessera.example.com` through a Cloudflare tunnel). The app uses
whichever answers first — LAN at home, tunnel elsewhere — and reconnects on
its own. If the tunnel is protected by Cloudflare Access, add the service-token
headers (`CF-Access-Client-Id`, `CF-Access-Client-Secret`) under
**Advanced → Proxy headers**; they are sent on every request, including the
WebSocket. Pasting a pairing link into the address field fills all of this in.

Another terminal can use the same server:

```bash
tessera connect http://192.168.1.10:7878 https://tessera.example.com --code 1234-5678 \
  -H CF-Access-Client-Id=… -H CF-Access-Client-Secret=…
```

`tessera serve --web apps/tessera/ui/dist` also serves the web UI to a browser
on the server itself. A browser can't be protected from the proxy that serves
its code, so browsers elsewhere are refused unless you set
`require_e2ee = false` under `[security]`. Use the app for remote access.

### Remote access through Cloudflare, automatically

Setup (in the app under Settings → Connection, or in `tessera setup`)
does all of it from one Cloudflare API token:
1. creates a tunnel to your server and a DNS name such as
   `tessera.example.com`;
2. creates a Cloudflare Access application that only lets your devices'
   service token through;
3. downloads `cloudflared` (verified against its published SHA-256) and keeps
   it running.

New pairing QR codes then include the tunnel address and the Access headers,
so phones work anywhere with no extra steps. Cloudflare only ever sees
end-to-end encrypted traffic. The API token is used for setup and never
stored, so you can delete it afterwards. Create it at
<https://dash.cloudflare.com/profile/api-tokens> → **Create Custom Token**
with:

| Scope | Permission | Access |
|---|---|---|
| Account | Cloudflare Tunnel | Edit |
| Account | Access: Apps and Policies | Edit |
| Account | Access: Service Tokens | Edit |
| Zone | DNS | Edit |
| Zone | Zone | Read |

Limit **Zone Resources** to the domain you'll use and **Account Resources**
to your account. The account needs Cloudflare Zero Trust turned on once (free
plan). Setup then waits for the tunnel to connect; if a firewall
blocks Cloudflare's tunnel port (outgoing TCP/UDP 7844) it says so and checks
again once you've opened it. Devices paired before need pairing again to get
the new address. Every step for people new to Cloudflare, no domain,
Tailscale, your own proxy, and why quick tunnels aren't used:
[REMOTE_ACCESS.md](REMOTE_ACCESS.md).

### Keep it running

```bash
tessera service install                 # systemd service, started now and at boot (re-run to update + restart)
tessera service install --lan           # … listening on the LAN (0.0.0.0) instead of this machine only
tessera service install --dry-run       # show the unit file and commands, change nothing
tessera service status                  # installed? running? lingering?
tessera service uninstall               # stop and remove it (data and settings stay)
```

As a normal user it's a user service (`~/.config/systemd/user/tessera.service`);
it asks before running `loginctl enable-linger` so it keeps running after you
log out (without a terminal it just tells you how). As root — typical in a
Proxmox LXC or VM — it's a system service (`/etc/systemd/system/tessera.service`)
running as root, or as another account with `--user <name>`. The server runs
through your login shell so the agent's commands get your normal `PATH`. The
listen address is `server.bind` from the config (this machine only by default)
unless you pass `--lan` or `--bind`.

### Use your other computers (machines)

The server's own machine is always at the agents' disposal. To let them use
another computer too (say the server is a small VM and your desktop builds
much faster), turn it on **on that computer**: Settings → Devices → Add… →
Add a machine gives the one command to run there (or, in the desktop app on
that computer, one click: "Use this computer"). Then pick a preset: Look only,
Work in folders, Full access or Desktop control. For work that needs root
(a driver, a system service, installing packages) you can also turn on
**Administrator** there: agents may then run single commands with `sudo`,
outside the sandbox, each one asking you first with the exact command; the
password sudo needs stays on that computer (`tessera node admin on`,
`tessera node admin password`). Off by default; see [MACHINES.md](MACHINES.md).

```bash
tessera node enable '<pairing link>'             # pairs it, asks what agents may do
tessera node preset work --folder ~/code         # look | work | full | desktop | off
tessera node access --bot homelab-sweeper        # only that bot (default: any chat)
tessera node log                                 # every action done here
```

Agents then get a `machines` tool (approval cards read "on Desktop: cargo
build"). Every device shows your machines in Settings → Devices, where you can
pause or revoke one; only the machine itself can let agents do more. Details,
defaults and the security model: [MACHINES.md](MACHINES.md).

## Building the apps

`sh scripts/install.sh` in a copy of the source builds and installs the
`tessera` program and, where WebKitGTK and Node.js are there, the Linux
desktop app. By hand:

| Target | Command |
|---|---|
| `tessera` (server, terminal app, setup) | `cargo build --release --locked -p tessera` → `target/release/tessera` |
| Web UI | `cd apps/tessera/ui && npm ci --ignore-scripts && npm run build` |
| Linux desktop | `cargo install tauri-cli --version 2.12.1 --locked`, then `cd apps/tessera/src-tauri && cargo tauri build --no-bundle -- --locked` (needs `webkit2gtk-4.1` and the web UI built first) |
| Windows desktop | an installer is built (see `docs/WINDOWS.md`); by hand it is the same command on Windows (WebView2). Not yet run on a real Windows PC |
| Android | once after cloning, generate Tauri's Android files without losing the checked-in ones: `cd apps/tessera/src-tauri && mv gen/android /tmp/android-custom && cargo tauri android init --ci && cp -a /tmp/android-custom/. gen/android/`; then `sh scripts/android.sh [debug\|release] [aarch64\|x86_64\|all]` → `out/*.apk` (needs a JDK, the Android SDK and NDK) |

`scripts/android.sh` builds from a local-disk mirror because Tauri's Android
tooling needs symlinks, which some network shares lack.

## Configuration

`~/.config/tessera/config.toml` (or `$TESSERA_HOME/config.toml`). Secrets live
in `auth.json` (mode 0600) next to the database, never in the config.

```toml
default_model = "openai-codex/gpt-6-astra"
utility_model = "local/qwen3.8-27b"      # titles and compaction summaries

[models]
memory = "anthropic/claude-sonnet-4-5"   # the memory/skills review (default: utility)

[server]
bind = "0.0.0.0:7878"

[agent]
permission = "default"                   # default | auto | ask | read_only
review_interval = 10                     # background memory/skill review (0 = off)

[context]
compact_at = 0.8                         # share of the usable window
max_tokens = 0                           # cap the working context (0 = automatic: the model's window, at most 256k)

[browser]                                # web view tiles and the `browser` tool
headless = true                          # false: show its windows on the server's desktop
idle_minutes = 10                        # stop it when unused (logins persist)
quality = "auto"                         # moving pictures: auto (as sharp as this server keeps smooth) | sharp | smooth
scale = 2                                # the sharpest moving frames get; still ones match the screen

[terminal]                               # your own terminals (Settings → Privacy & safety → What commands can reach)
access = "off"                           # off | sandboxed | full
idle_minutes = 30                        # close one nobody types in (15–240)

[security]                               # Settings → Privacy & safety (docs/SECURITY.md)
sandbox = "auto"                         # auto | strict | off
ask_outside = true                       # ask before writing outside the chat's folder…
folders = [{ path = "/home/me/code", write = true }]   # …and these (write = false: read-only)
save_secrets_from_chat = true            # keys/logins typed in a chat go to the vault
secret_saving = "auto"                   # then: auto (Passwords & keys) | ask (a card in the chat) | chat_only
secret_machines = "off"                  # saved passwords on your machines: off | picked (secret_machines_picked = [ids]) | all
bot_handoff = "on"                       # chats handing work to bots: on | ask (approve each) | off

[[mcp]]
name = "github"
command = "github-mcp-server"
args = ["stdio"]
defer = true                             # its tools wait until first used (unset: when all MCP tools would take over a tenth of the model's context)
```

### Usage and pricing

Settings → Usage shows tokens and cost by model,
kind and day. Each request has two prices: what it was **billed** per token
(API keys) and its **API price**, what it would cost on a pay-per-token API.
For a plan (ChatGPT sign-in, Claude Pro/Max through your own Claude Code),
enter the plan and its monthly fee to compare it with the API price of what
you used: the apps' **API prices** view shows what everything costs (or
would cost) per token, **Subscription** what you pay (each plan's fee spread
over the period, plus anything billed per token) and what each plan saved.
Local models are free. Prices (US dollars per million tokens) come
from your own settings, then a built-in list of current Claude and OpenAI
API prices, then the public [models.dev](https://models.dev) catalog
(downloaded at most once a day, only when a model has no price; nothing
about you is sent). A model without any known price is shown as unpriced,
never guessed. The apps write these settings for you:

```toml
[prices]                                 # your prices win over all others
"openai-codex/gpt-6.1-sol" = { input = 2.0, output = 10.0, cache_read = 0.1 }

[pricing]
catalog = true                           # false: never download the catalog

[pricing.subscriptions.openai-codex]     # your plan, compared with API prices
plan = "ChatGPT Pro"
monthly_usd = 200
```

### Small machines

Tessera runs on small computers too (a Raspberry Pi, an old laptop, a 1 GB
server): the server alone takes about 20 MB of memory when idle and
30–70 MB with a few long chats. When it starts it reads how much memory the
machine (or its container) has, and below about 2 GB it picks lighter
defaults. Anything you set yourself in `config.toml` or the apps wins.

| Under about 2 GB of memory | Default there | Elsewhere |
|---|---|---|
| Web views in a browser on the server (`[browser] enabled`) | off: Settings → Web & voice says why and has a switch; web views can still open on one of your computers | on |
| Browser tabs at once (`[browser] max_tabs`; pages nobody looks at also close when memory runs low) | 2 | one per GB, up to 16 |
| Subagents at once (`[agent] max_subagents`) | 2 (1 under 1 GB) | 4 |
| Language servers (`[lsp] max_servers`, `max_memory_mb`) | 2 (1 under 1 GB), each up to a quarter of the memory | 4, up to 8 GB each |
| Kept file versions; parsed chat histories | 8–13 MB; none under 1 GB, 2–3 MB above | up to 64 MB; up to 24 MB |
| Finished background commands kept for `job` | 8 | 24 |
| Database log kept between writes | 4 MB | 16 MB |

The installer doesn't download a browser for web views on such a machine, or
where less than 1.5 GB of disk is free (it says why, and asks; `--yes`
answers no there). Run it again and answer yes to get one anyway.
`TESSERA_MEMORY_MB=1024` makes Tessera (and the installer) act as if the
machine had that much memory, for a container whose limit it can't see.
`tessera doctor` shows what the running server's memory holds.

Use models that run elsewhere (a hosted API, or a model server on a bigger
computer of yours). Ready-made builds are for 64-bit Linux (x86_64 and
aarch64) and macOS; there is none for 32-bit ARM (the default Raspberry Pi
OS on a Pi Zero 2 W), and building Tessera there isn't possible (a build
needs about 2 GB of memory): install the 64-bit Raspberry Pi OS instead.

## Project layout

| Path | What |
|---|---|
| `crates/proto` | Wire protocol (Rust types; TypeScript bindings generated from them) |
| `crates/core` | Engine: sessions, agent loop, providers, tools, context engine, memory, skills, jobs, MCP |
| `crates/server` | HTTP/WebSocket server, device pairing |
| `crates/client` | Connection manager used by the CLI and the apps |
| `crates/cli` | The `tessera` binary (TUI, one-shot runs, admin commands) |
| `apps/tessera/ui` | Web UI (SolidJS) shared by desktop, Android and browsers |
| `apps/tessera/src-tauri` | Tauri 2 shell for Linux, Windows and Android |
| `docs/` | Protocol, architecture, decisions, capability matrix, testing |
