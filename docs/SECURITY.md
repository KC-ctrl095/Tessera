# Security

Tessera runs an AI agent with real access to your machine, so it is built to
be safe by default and still let the agent do the work you ask for. This page
explains what protects what, the settings you control (Settings → Privacy & safety in
the apps, `[security]` in `config.toml`), and what is deliberately left to you.

## What is protected, and how

**Confidentiality: your credentials and chats stay yours.**

- API keys, sign-ins and your saved secrets live in `auth.json`, the server's
  identity key in `identity.key`, chats in `tessera.db`. All are owner-only
  files (mode 0600) in owner-only folders (0700).
- Models never see secret values. Commands receive a secret as an environment
  variable only when they name it (`$GITHUB_TOKEN`), and every value (plus its
  base64, hex and URL-encoded forms) is removed from tool output before a model
  sees it; short values only as whole words, so a PIN never cuts up a number.
  A login's username is not a secret: emails and distinctive names are hidden,
  names of letters only (`root`, `admin`, your first name) are not, so logs,
  paths and commit authors stay readable. Anything that merely looks like a credential (known key formats,
  private keys, `PASSWORD=…` lines) is scrubbed too (`redact_patterns`).
- Keys and logins you type into a chat are moved to Passwords & keys (below).
- Sharing a file into a chat (`share_file`) refuses private keys, Tessera's
  own keys and, in text files, access tokens in known formats; a file holding
  one of your saved passwords or keys asks you first, naming it. Compressed
  files (zip, APK, tar.gz) are not looked inside.
- Whatever a tool returns, the model reads only what you would see:
  invisible characters (tag characters, direction overrides, zero-width
  runs) are taken out, and the model is told.
- Each saved password or key says who can use it (any chat, or only some
  bots) and where (this server, some machines); chats that may not use one
  don't even see it exists (below).
- Every command the agent runs is sandboxed (below): Tessera's data and
  settings folders appear empty to it, whatever it tries.
- Private chats live only in the server's memory and only on the device that
  started them: the server never lists them to, sends their events or
  notifications to, or opens them for any other device, web browser or
  terminal app (it answers as if they didn't exist). Signing that device out
  deletes them.
- Other Claude accounts sign in through Claude Code itself, each in its own
  folder (`CLAUDE_CONFIG_DIR`); Tessera only names the folder and never reads,
  writes or passes on what is in it. Reading a Claude Code sign-in file
  (`.credentials.json`) asks, as for the usual account.
- Apps connect end-to-end encrypted (Noise, keyed by a PAKE pairing code, the
  server key pinned). A tunnel or proxy only relays ciphertext. Plain browser
  access is allowed only on the server itself unless you set
  `require_e2ee = false`.

**Integrity: nothing changes behind your back.**

- In the default permission mode, commands that delete data, gain privileges,
  push or publish ask first; writing files outside the chat's folder asks;
  changing a skill (instructions later chats load) asks, with the agent's
  file tools or `skill_manage` (the background memory review saves skills on
  its own, but not from a stretch where pages from the web were read); writing to system
  folders, shell startup files, autostart entries, SSH keys, programs in
  `~/.local/bin`, git's config and hooks, `.envrc`, or Claude Code's settings,
  hooks and MCP servers (`.claude/`, `.mcp.json`) asks. Reading other
  programs' credential files (SSH keys, `.netrc`, `.git-credentials`,
  `.pypirc`, `~/.npmrc`, cargo and rclone tokens, Claude Code's sign-in…) asks.
  Catastrophic commands are always refused.
- Agents can't change Tessera's security settings, permissions, network
  settings, providers or credentials: those are refused to every tool, and
  Tessera's own files are hidden from commands. Everyday settings (model roles,
  memory, context, language servers…) can be changed by the agent's `settings`
  tool, and **every such change asks you, in every permission mode**.
- Folders you make read-only can't be changed by the agent at all.

**Availability: it keeps working.**

- Files are written atomically (write, sync, rename), so a crash never leaves
  half a settings or credential file. The database is SQLite in WAL mode.
- A command that runs long continues in the background instead of being
  killed. Stop targets retained process handles and marked descendants on Linux.
  Cleanup is best-effort, not namespace-wide in joined PID namespaces. Double-forked
  daemons that clear their environment before observation can escape. Incomplete
  checking is reported separately from the command's exit status. Shared supervisor
  adoption and crash cleanup are follow-ups, not guarantees of this release.
- The installed service restarts on failure. Interrupted chats are marked as
  such, never silently re-run (jobs and `auto` chats resume by default).

File reads and sharing keep the opened file from authorization through use,
including metadata and copying. On Linux each resolved path component is opened
relative to a held directory descriptor with `O_NOFOLLOW`; authorization checks
its actual `/proc/self/fd` location. A link inserted after the earlier approval
cannot redirect the read or copy. Writes create a private temporary file in the
verified directory, sync it, then use `renameat` in that same directory. Existing
permissions are retained and a changed old version is refused. Other platforms
recheck the resolved location after opening and before writing; without Linux's
directory-descriptor operations they do not have the same race guarantee. This
is a path/object boundary, not isolation from another process changing the
contents of the same file or making hard links to it.

Commands (Bash, quick checks, Claude Code, Tessera's Git commands) keep their
working folder open in the server from its check through entry. Without the
command sandbox, Linux enters the held folder itself. With the sandbox the
folder is opened inside the sandbox's view and must match the held folder's
device and inode; otherwise nothing runs. In the shared mode, only these numbers
cross exec: the host folder descriptor closes before `nsenter`. A native launcher
(not a shell, and its executable is bound read-only in this view) enters and
checks the folder inside the mounts while still outside
the chat's PID namespace (`nsenter --no-fork`); only after it closes the checked
folder descriptor does it spawn the requested command inside that PID namespace.
The server retains the host folder until the process ends, preventing inode reuse
while entry is delayed. Peers cannot inspect/stop the trusted launcher via their
`/proc`, and no shell imports functions on its entry or refusal path. Hidden
folders, read-only rules and the chat's private `/proc` therefore hold for relative
paths too.

Both trusted loaders (`nsenter` and the native launcher) receive only a fixed
PATH/locale and private fd metadata. The requested program's environment is
serialized in a sealed memfd, not command-line arguments, read and closed by the
launcher, and applied only when spawning the program. Even an explicitly passed
`LD_PRELOAD`, `LD_AUDIT` or `GLIBC_TUNABLES` cannot affect trusted entry/refusal.
The requested child has a parent-death signal with a pidfd race check. Two
private anchors are born outside the joined namespace, drop inherited host fds,
and join the program's group behind a bounded before-exec barrier. Job-control,
TTY and polite termination signals are ignored. Their ownership channels detect
launcher loss; cleanup signals their own live group, not a stored/reusable pgid.
The launcher keeps the owned leader unreaped through bounded completion and
fallback cleanup before releasing the anchors. A private server-issued channel
registers retained pidfds for accounting; mutable process names are not authority.
A lightweight server-side owner retains those identities and the private cleanup
channels through the last anchor's exit. On launcher loss it continues either
stopped anchor, requests group cleanup, then uses pidfd SIGKILL after a bounded
grace. Construction failures kill every anchor already created and use the
failure-only child's SIGCHLD ignore disposition for automatic reaping, without
poll or wait loops in pre-exec. There is no third anchor. The trusted launcher
is non-dumpable before fork, and the child inherits that protection until exec.
The launcher itself polls a CLOEXEC exec-error pipe with a deadline and obtains
the child's pidfd immediately after fork, so stopping the child's pre-exec
cannot block launch indefinitely.
Ordinary same-group descendants are ended on Stop/timeout, including a stopped
watchdog or launcher. Joined-mode Claude uses this same working launch path and
whole-group cleanup rather than being refused. This is group ownership, not
subreaper containment of setsid/double-fork escapes. Such descendants remain
inside the filesystem and namespace sandbox but are not covered by this group
cleanup guarantee.

Known construction limit. There is a narrow window before the launcher reports
the anchors' identities to the server. A process in the same sandbox group that
can stop a fallback anchor and SIGKILL Tessera's own constructing launcher in
that window can leave the anchor orphaned and stopped. The independent owner
cannot clean an identity it has not received. This leaks a stopped process, not
an escape from the sandbox. Post-registration launcher loss is covered by the
retained-identity cleanup above.

On Windows the folder is
opened without delete sharing while the process starts, so it can't be renamed
or replaced in the meantime. NTFS is documented to refuse renaming the folders
above an open one as well; that part hasn't been confirmed on Windows yet. A
folder on a mapped network drive is entered by its drive letter (cmd.exe can't
start in a `\\server\share` folder); the drive mapping itself isn't held.

## Agent process environment

Agent shells, Claude Code, git worktree commands, language servers and syntax
checks start with a cleared environment. Tessera keeps ordinary shell, locale,
time zone and temporary-folder settings; XDG directory locations; Rust/cargo,
Python virtual environment, conda, nvm and pyenv locations; and the Windows
system/profile locations needed to start programs. PATH still finds your normal
git, cargo/rustup, node/npm and Python. DISPLAY and WAYLAND_DISPLAY remain for
local desktop programs; machine commands get a display only with desktop control.
Shell startup injection settings (`BASH_ENV`, `ENV`), Node options, provider keys
and arbitrary server values are not inherited. Requested vault secrets are added
only to the command that names them. The CLI's account directory and variables
Tessera sets itself still work, including its Windows Git Bash path.

The server deliberately keeps `SSH_AUTH_SOCK`: git/ssh can ask your SSH agent to
sign, which is authority to use its loaded keys, not just a harmless path.
Machines keep their existing stricter rule: no inherited SSH agent socket.
User terminal tiles keep their login-shell behavior and their own environment
policy; this change does not turn them into agent shells.

For a build that needs another variable, the user can set names (not values):

```toml
[agent]
pass_env = ["BUILD_CACHE_DIR"]  # default: []
```

This is a deliberate loosening: named values reach agent command shells and the
Claude CLI (and programs they start), and may reach their output. Supporting
language servers, syntax checks and worktree git keep the base allowlist.
Never list a credential here; save it in Passwords & keys instead. Tessera's private
`TESSERA_*` paths cannot be passed this way. Only the user can change this
setting in `config.toml`, not the agent settings tool. It applies on this server,
not remotely to a machine's own environment policy.

## The command sandbox

On Linux each command the agent runs (and each language server) gets:

- its own user and mount namespace, in which Tessera's data, settings and the
  desktop app's saved servers are empty and read-only (the skills folder stays
  readable), and the session's control sockets (`systemd --user`, D-Bus, tmux,
  X11) are hidden, since they could start programs outside the sandbox. The
  SSH agent stays usable for `git`;
- your read-only folders mounted read-only, and Tessera's program and its
  service files (`~/.config/systemd`, `~/.config/environment.d`,
  `~/.local/share/systemd`) too: readable, never changed (while the sandbox
  is on, the agent's file tools refuse to change them as well, even in
  `auto` mode);
- the folders holding any of those can't be moved or removed, so nothing
  hidden or read-only can be moved aside and replaced by another (such as a
  made-up `~/.config/tessera` for the next start to read);
- no way around this through other processes: with Landlock ABI 2 or newer
  (Linux 5.19+) a Landlock domain keeps commands away from other processes'
  `/proc` entries and from mounting; on older kernels (e.g. Ubuntu 22.04's
  5.15) each chat's commands instead share a private process namespace of
  their own, with its own `/proc`, so they see only processes that chat (and
  the subagents it started) ran: a server one command starts can still be
  listed and stopped by the next, while other chats' commands, with their
  environment and open pipes, stay out of sight;
- the files you attached to that chat (and to no other chat): the rest of
  the uploads folder is hidden, and file tools refuse other chats' files;
- on Linux older than 6.12, where a command could still reach an X display's
  socket, the display's login cookie (`~/.Xauthority`) reads as empty.

`sudo` and setuid programs can't gain privileges inside. The agent is told to
give you the exact command instead.

Tessera's own `git` commands for a subagent working in its own checkout (a
worktree) run in the same sandbox and without Tessera's credentials in their
environment: a repository can name programs for git to run (filters, diff
drivers), and the agent can change the repository. They never use an
external diff program or text conversion the repository names.

| Mode | Behaviour |
|---|---|
| `auto` (default) | Sandbox wherever the full safe box works. If it is incomplete or unavailable, every command asks for one-time approval, even in existing YOLO chats. Only explicitly choosing Off removes this fallback. Other command launchers fail closed if they cannot ask. |
| `strict` | Commands never run without the complete sandbox; they fail where it is missing. |
| `off` | No sandbox: commands can read Tessera's keys and chats. Only for machines where nothing untrusted ever reaches the agent. |

The status line in Settings → Privacy & safety → What commands can reach (and in `tessera` banners) says:

- **on (namespaces + Landlock)** or **on (namespaces + a private process view)**: complete.
- **partial (…)**: namespaces work but other processes' `/proc` is reachable.
  Settings shows how to fix it, e.g. on Ubuntu 22.04
  `sudo apt install linux-generic-hwe-22.04` and a reboot (a kernel with
  Landlock), or installing `util-linux` (for `nsenter`).
- **unavailable (…)**: user namespaces are blocked. On Ubuntu 23.10+ AppArmor
  blocks them for programs without a profile; Settings shows the one-line
  profile that allows them for Tessera. Containers often block them too.

The sandbox keeps Tessera's secrets from commands. It is not a container.
Commands still run as you, with your readable files and network. Ordinary
sandboxed commands cannot change shell startup files, fish configuration,
autostart entries, user systemd locations, SSH startup hooks or reachable
cron spools. Missing names are protected too. Their existing parents stay
read-only, with safe existing siblings opened back up, so some new sibling
names may need the file tools instead. Pre-existing hard links to protected
startup files make all command writes read-only rather than leaving an alias
writable. Use `write` or `edit` for an exact, one-time approved startup-file
change. Tessera's own program and service configuration remain owner-only.
With Off, text checks for startup paths, links and scripts are best effort,
not a boundary. A Docker socket or project-specific hook can still run code
elsewhere or later. Never give untrusted commands access to those.

**Run Tessera as a normal user, never root.** As root, commands could change
system files; Settings → Privacy & safety → What commands can reach warns when that is the case.

### On Windows

Windows has no command sandbox in Tessera yet, and Tessera says so (the
status line reads **unavailable (Tessera has no command sandbox on Windows
yet)**, with what to do). The agent's commands run with your account's
rights: a command could read Tessera's own settings and keys, and anything
else your account can. What still holds: secrets reach a command only when
it names them, secret values are scrubbed from what the model reads, risky
commands ask first (on Windows also deleting folders recursively, changing
the registry, services, scheduled tasks, the firewall, Defender or the
PowerShell script policy, elevating, and downloading and running a script),
catastrophic ones are refused (deleting a whole drive, Windows or your home
folder, formatting a disk, managing Tessera itself), and `strict` refuses
every command. Automatic requires one-time approval for every command until
you explicitly choose Off in What commands can reach. With Off, your chosen
permission mode applies as before. Run the server on Linux for the sandbox.
Tessera's data folder
(`%APPDATA%\tessera`) inherits your profile's permissions: your account,
plus Windows' administrators and the system, as root on Linux. See
[WINDOWS.md](WINDOWS.md).

## Your settings

| Setting | Default | What it does |
|---|---|---|
| Sandbox | `auto` | See above. Turning it off asks for confirmation in the apps. |
| Folders | none | Folders beyond a chat's own. **Read & write**: the agent changes files there without asking. **Read only**: it can read but never change them (file tools refuse; with the sandbox on, commands see them read-only), even in a chat working inside one. The most specific folder wins, so a writable `~/code/site` inside a read-only `~/code` works. Tessera's own folders can't be added. |
| Ask before writing elsewhere | on | Writing a file outside the chat's folder and your folders asks first. Off: it doesn't (system and startup files still ask). |
| Scrub credentials from tool output | on | Pattern-based scrubbing on top of the exact secret values, which are always removed. |
| Keep passwords and keys out of chats | on | See below. |
| Typed in a chat (Passwords & keys page) | Save automatically | Save automatically, Ask me first, or Don't save. See below. |
| Require end-to-end encryption | on | Shown for reference; change it only in `config.toml`. |

```toml
[security]
sandbox = "auto"                  # auto | strict | off
ask_outside = true
save_secrets_from_chat = true
secret_saving = "auto"            # auto | ask | chat_only
redact_patterns = true
require_e2ee = true
folders = [
  { path = "/home/me/code", write = true },
  { path = "/home/me/Documents", write = false },
]
```

Only you change these, from a paired device or `config.toml`. Agents can't:
there is no tool or request for them, and `config.toml` is hidden from commands
and refused to file tools.

## Keys and logins typed into a chat

Pasting a key into a chat would send it to the model and keep it in the
history. Instead, with "Keep passwords and keys out of chats" on (Settings → Privacy & safety →
Passwords & keys):

- **Known key formats** (Anthropic, OpenAI, OpenRouter, GitHub, GitLab, Slack,
  Google, Hugging Face, Groq, Replicate, xAI, Perplexity, npm, Stripe,
  Telegram bot tokens, private keys…) are caught by the server before the
  message is stored or sent anywhere. Each is saved as a secret named after
  what it is (`ANTHROPIC_API_KEY`, `GITHUB_TOKEN`; `_2` when taken), the
  message keeps `$ANTHROPIC_API_KEY` in its place, and the chat shows
  "Saved to Passwords & keys". Pasting a key you already saved reuses it.
- **Two-factor codes**: a setup key for an authenticator app (an
  `otpauth://` link from a QR code, or a key you call one, like "my 2FA key:
  abcd efgh …") joins the login named after the site it is for, or becomes a
  login of its own (`TWO_FACTOR` when nothing names it); the agent types the
  current code as `$NAME_OTP`. A one-time code you were sent ("my
  verification code is 482 913", "2FA: 551204") is kept as `$ONE_TIME_CODE`,
  replacing the last one. Only numbers the words around them call a code
  count: an error code, a port or a year stays as typed.
- **Anything else** (a username and password, a key without a telltale
  prefix) the agent recognises and moves with its `save_secret` tool: the
  values go to the secret store and are replaced by their variables
  everywhere in the stored chat (your message, the agent's own call and
  reasoning), so later turns never contain them. It can only save what you
  typed in that chat, never text from pages or files, and never overwrite an
  existing secret.
- A stored secret you type again is replaced by its name (or, in a chat
  that may not use it, by `[a stored secret]`, so the chat doesn't learn
  its name either).
- A Tessera device token is always removed and never kept: it controls your
  server. Private chats keep nothing, so there keys are only removed.

Manage everything in Settings → Privacy & safety → Passwords & keys. New secrets
are for any chat and ask before their first use in a chat. Which ones a bot
can use is in its editor (Passwords & keys): new bots start with none of the
ones any chat may use, and get each one you switch on there (a key typed in
the bot's own chat is its to use). Turn this off if
you need to show a model a key on purpose (for example, a test fixture).

### Save automatically, ask first, or don't save

What happens to a password or key once it is out of the message is your
choice, at the top of Settings → Privacy & safety → Passwords & keys (`secret_saving` in `config.toml`). The model gets only `$NAME` in
every case, and the value is scrubbed from all output.

- **Save automatically** (`auto`, the default): it goes to Passwords & keys
  at once, as described above, and the agent's `save_secret` saves a login
  you typed without asking.
- **Ask me first** (`ask`): it is kept for that chat alone, and a card in
  the chat asks: **Save** moves it to Passwords & keys (any chat, asking
  before its first use); **Don't save** leaves it with that chat only. Until
  you answer, that chat (and its helpers) can use it; no other chat can, nor
  does it learn the name. When the agent wants to save a login you typed,
  you approve it first, even in a chat set to run without asking; if you
  deny, it is kept for that chat only (it still leaves the conversation). A
  one-time code is good for minutes, so it is kept for the chat without a
  question.
- **Don't save** (`chat_only`): everything stays with the chat it was typed
  in, never in Passwords & keys; the agent's `save_secret` keeps it for the
  chat too.

Keeping it for one chat is the safe middle ground: the value never reaches a
model, and it can't spread to other chats, bots or scheduled duties. It is
deleted with the chat; until then it is listed under "Kept for one chat" in
Passwords & keys, where you can still save or delete it (the chat's card
changes to say so). A forked chat doesn't take it along.

## Passwords & keys

Settings → Privacy & safety → Passwords & keys holds the logins, API keys and
sets of values your agents use in commands as `$NAME` (a login's fields as
`$NAME_USERNAME`, `$NAME_PASSWORD`…, its current one-time code as `$NAME_OTP`).

**Saved values are never shown again.** No request returns them, and the
apps show only that a value is saved: not the value, not its length, not a
masked hint. You can still change everything about one later (Edit): its
description, sites, whether it asks, who can use it, where, its name, and its
values. To change a value, choose Replace… and type the new one; a value
field you leave empty keeps the saved one. You can't read a value back; if you
lost it, get a new one from the service.

**Renaming** keeps the values, but what agents type changes with the name
(`$ROUTER_PASSWORD` becomes `$LAN_ROUTER_PASSWORD`). Commands, skills,
scheduled duties and notes that use the old name stop getting it until you
change them too; the editor reminds you before you save.

**Who can use it** (least privilege):

- *Any chat (asks first)*: every chat, bot chats included. The first command
  in each chat that uses it asks you, unless you turn asking off for it or
  the chat itself runs without asking (Auto). Every secret saved before this
  setting existed is like this.
- *Only these bots*: only the bots you pick, wherever they act: their chats,
  a channel when it's their turn, their scheduled duties (which run as the
  bot) and the helpers (subagents) they start. They use it without asking,
  so a bot can, say, check your homelab every night while you sleep. Every
  other chat behaves as if it didn't exist: it isn't in its instructions, a
  command that names it gets nothing, and typing its value into that chat
  replaces it with `[a stored secret]`. Picking no bots means nothing can
  use it.
- Private chats and Tessera's own background helpers never get any.

Two ways around this are closed: a chat can't rewrite or delete a bot's
scheduled duty (only that bot, or you in the sidebar's Bots tab, can), and asking
such a bot to do something with `message_bot` shows you an approval first
("can use saved passwords or keys this chat can't"), unless the chat runs
without asking.

**Chats asking bots for help** (Settings → Privacy & safety): *Yes* (the default) as
above; *Ask me first*: every time a chat hands work to a bot you approve it,
even in chats that run without asking; *No*: chats never hand work to bots
(they aren't told about them, and the server refuses it). Each bot can also
refuse work from everyone but you (Bots → the bot → *Other chats may ask it
for help*). Bots asking other bots follow each bot's own *Can message* list.

**Sites are not a network boundary.** A command given a password or key can send
its value anywhere, not just to the sites listed for it. The approval card says
so. Explicit bot-only grants skip first-use approval, and the bot editor says
that its commands can send the value anywhere. Direct curl/wget/HTTPie commands
naming an obvious different HTTP(S) destination are refused before any value is
injected (on this server and machines). The text check compares listed sites and
their subdomains, including URL user-info correctly; it does not enforce
redirects, proxies, DNS, programs/scripts or computed URLs. Unlisted secrets have
no destination check. Use asking grants for sensitive values and approve only
programs you trust. There is no credential broker or general network restriction.

**Where it can be used**: *This server* covers commands Tessera runs on the
server and its browser. On your machines, each password is *like my other
passwords* (the setting **Let saved passwords be used on my machines** at the
top of the page: *Off*, the default; *Machines I pick*; or *All my machines*,
also ones added later) or *chooses for itself* (exactly the machines you tick,
possibly none). A new password chooses for itself with no machines ticked, so
it works on this server only until you allow a machine for it: a test or lab
machine never gets a fresh password by default. The list marks every password
another computer can use and names those computers, with one button to keep
them all on this server again. Turning the setting up asks first. A secret not allowed where
a command runs is refused (the agent is told where it works, and that you can
allow it here). Each machine still decides what runs on it (its folders,
whether commands are on, who may use it): the server checks that before
sending anything, and the machine again. A value goes only to the one command
that names it, over the end-to-end encrypted connection; neither side writes
it to a log, and both remove it from what comes back. A program on that
computer could still keep it, so allow only computers you trust.

**Titles**: each entry's title says what it is for ("GitHub token", "Bank
login (mybank.com)"); agents give one when they save a login, and one that
holds a value is replaced by a title made from the name. Entries saved by
older versions ("saved from a chat") get suggestions you can edit and apply
at the top of the page.

These checks happen on the server, at the moment a command asks for the
value, for Tessera's own agent and for Claude Code chats alike (both go
through the same tools). Every saved value, whoever may use it, is still
scrubbed from everything any model sees.

**On disk**, values are in `auth.json` as they are typed, protected by its
owner-only permissions (Tessera tightens them again at start if a restore
loosened them), the owner-only data folder and the command sandbox, which
hides the folder from commands. Encrypting the file with a key stored next to
it wouldn't stop anything that can read the folder. If you turn the sandbox
off, commands run as you and could read the file directly (who-can-use-it
then governs what Tessera hands out, not what a determined command could
dig up); Tessera refuses commands that name its folders, a safety net rather
than a wall.

## Pairing, devices and remote access

- A pairing code is 8 digits, single use, valid for 10 minutes, and limited
  to three answers (counted before they are checked, so answers sent all at
  once can't get more). The pairing QR code and link carry the code, your
  addresses and any Cloudflare Access headers: treat them like a password
  while the code is valid. The Access headers only get a device past
  Cloudflare; it still has to pair.
- When the addresses or Access headers change (remote access set up,
  changed, removed or its key renewed), paired apps get them from the server
  over their end-to-end encrypted connection, so nobody has to pair again.
  Only the user's apps get them (and a machine connecting from another
  computer, which needs them to stay reachable); web browsers, plain
  connections and machines on the server's own computer don't, and the
  values are never logged.
- Nobody learns from the server whether a code is active (it answers the
  first step the same either way). Ten failures in ten minutes pause pairing
  from the network; pairing on the server's own computer has an allowance of
  its own, so strangers can't lock you out there.
- Every paired device can do everything (there are no lesser roles but a
  machine's login, below). Remove lost devices in Settings → Devices.
- A machine's login (what `tessera node` keeps on that computer) can only
  serve as that machine: say it is one, and answer the actions it is sent. It
  can't read chats, add devices, change settings or start anything. See
  [MACHINES.md](MACHINES.md#a-machines-login-can-only-serve-as-that-machine).
- Devices send Cloudflare Access headers only to `https` addresses, never to
  a plain-`http` home address (which on another network can be anyone's). A
  device tries its addresses in order and moves on from one whose answer
  can't prove the server's key; it only gives up when none can. A login
  without a pinned server key is only ever used on the server's own computer.
- Strangers can't tie the server up for long: request headers must arrive
  within 10 seconds, connections are capped (512 at once, 64 per network
  address), and encrypted handshakes too (64 at once, 8 per address, the
  first message within 5 seconds). Behind a tunnel or proxy on the server's
  computer the address it names (Cf-Connecting-Ip, X-Forwarded-For) counts.
- A device removed while connected is cut off at once, also when it is
  removed in the moment it connects.
- **Web browsers** ("Use Tessera in a web browser", off by default): while
  off, nothing at all is served at `/web` and paired browsers can't connect
  (switching it off cuts them off at once). While on, the web app is served
  only over https (through a tunnel or proxy on the server's computer) or to
  a browser on the server itself, with a strict policy: nothing from other
  sites, no inline script but the page's own, never framed, no referrer, no
  location/USB/payment/security-key access. A browser pairs with a code from
  one of your devices (end-to-end encrypted like the apps) and brings a
  Web Crypto key no script can read out; each connection signs that
  session's handshake with it, so its saved login is useless without the
  key, and works only at `/web` (the apps' logins don't work there). Its
  requests must come from the app's own page: an `Origin` that is one of
  the server's own https addresses (or, on the server itself, its own
  address), so a page that had its name point at the server (DNS
  rebinding) is turned away. Its pairing is rate-limited apart from the
  apps', and by address; since any program on the server's computer can
  claim to be the tunnel and name any address, past the overall limit each
  address only gets fewer tries, so nobody can shut the owner out (they
  can still use up a code's three tries; make a new one). Never a terminal
  from a browser. The one
  weakness the apps don't have: the page itself comes from the server
  through whoever runs the address (Cloudflare, a proxy), who could change
  it; the switch says so. With Tessera's Cloudflare tunnel, turning it on
  adds an Access application for `/web` only (made once, with a token you
  give then); the apps' part of the address stays locked to their service
  token.
- See [REMOTE_ACCESS.md](REMOTE_ACCESS.md) for reaching the server from
  elsewhere.

### Whiteboards and previews

Whiteboard pages and note previews are written by agents, so they never run
as Tessera: they are served with a policy that allows no network and
`sandbox allow-scripts` (an origin of their own, even opened by themselves in
a browser tab), and the apps frame them sandboxed. The app and web UI frame
nothing but those pages: a board that sends itself to another address (your
router, a service on your computer) gets nothing, so an agent can't look at
those through your screen. On Linux a page in a frame can't open your browser
by itself (only a link you click does, a few at a time); on old Android web
views the app's bridge is never written into a board's page. In a web view on
a phone, a page focusing one of its fields by itself doesn't take the
keyboard: it follows only right after you tap the page, and never leaves a
field you are typing in. A web app from the server opened in your browser
gets a name of its own (`<random>.localhost`), so it shares no cookies with
other services on your computer.

## Machines

Another computer becomes a *machine* agents can use only when you turn it on
there, and only within the folders, commands switch and time limit you set
there; nothing remote can widen it, and it checks every action itself.
Commands there run in the sandbox with Tessera's data and other programs'
credential stores hidden, and change files only in its read-write folders.
Any device can pause or revoke it. See [MACHINES.md](MACHINES.md).

**Administrator** (off by default; turned on only on that computer, and only
with commands on) lets agents run a command there as root: outside the
sandbox, with `sudo` working. By default each such command asks you first in
the chat, showing the exact command, even in YOLO chats, and the machine
itself refuses one that wasn't approved; the looser "as the chat says" is a
separate, explicit choice on that computer. `sudo`'s password, if needed, is
saved on that computer only (checked with `sudo` first, owner-only, hidden
from sandboxed commands, never sent to the server, a model or another
device) and handed to `sudo` on a private pipe that is closed before the
command runs; it is wiped from the output, and `sudo -k` forgets the cached
login when the command ends. Switching it off there, pausing or revoking the
machine stops running administrator commands at once, and each one is in the
machine's log. Honest limits: an administrator command is root, so it could
read that saved password or change Tessera's settings on that computer, and
the computer takes the server's word that you approved: Administrator trusts
your Tessera server with root there. Keep it off unless you need it.

## Terminals

A terminal (New → More → Terminal) is a command line as the server's account, for
you alone. Off until you turn it on (Settings → Privacy & safety → What commands can reach): it is the most a
device can get, without the agent's approvals or a chat record, so a lost
phone or a copied device token shouldn't be one tap from a shell.
- **Sandboxed** (recommended where the command sandbox works): the same
  sandbox as the agent's commands, so Tessera's keys, logins and saved
  passwords stay out of reach, and Tessera's program and its service files
  (`~/.config/systemd`, `~/.config/environment.d`, `~/.local/share/systemd`)
  can be read but not changed. Its environment is only what a shell needs
  (no session bus, display or desktop variables). Your SSH agent stays
  usable, as for the
  agent's commands: it signs for you but can't hand out your keys.
  **Full access** is an explicit choice, asked about first; with
  `sandbox = "strict"` only sandboxed terminals open (and choosing strict
  ends full-access ones). Sandboxed is still your account: your own files,
  your shell's startup files included, can be changed from it, and the
  server's service runs your login files when it starts (see the command
  sandbox above).
- Only from a paired device's own connection, end-to-end encrypted or on the
  server itself: never from a chat, tool, bot or job, never over a plain
  connection from the network (a proxy or tunnel would see every keystroke),
  and never from a web browser (its page comes from whoever serves the
  address, who could change it).
- Turning terminals on, or giving them more, works only from the Tessera
  app or the server itself (any connection may turn them off), and every
  time your devices get a notification, also when a machine does it.
- A terminal belongs to the device that opened it; other devices are told it
  doesn't exist. It closes after 30 minutes without typing at its prompt (8 h
  while a program runs), 5 minutes after its device went away, when that
  device is signed out, and when terminals are turned off. At most 4 at once
  for each device, 16 in all. A program that doesn't read what you type
  holds up only its own terminal; past 1 MB waiting, the terminal says so
  and takes no more.
- Its output goes only to that device's connection, under flow control (a
  noisy program waits instead of filling memory), never to the chats, memory
  or logs. Kept: who opened one, where and when, and how it ended
  (`terminal-audit.jsonl`), never what was typed or shown.
- Like a native terminal, the shell starts as a login shell and reads your
  own shell settings (bash: its profile, then `~/.bashrc`), with
  `TERM=xterm-256color` and `COLORTERM=truecolor`; sandboxed, its
  environment adds only `LS_COLORS` to the list above. Links in its output
  open in your browser only with Ctrl/⌘+click (on a phone after asking), and
  only `http`/`https` ones.
- Machines decide for themselves, on that computer ("Terminal from my
  devices", see [MACHINES.md](MACHINES.md)). Turning terminals on for a
  computer trusts your Tessera server with that account (with full access,
  everything it can do): the computer takes the server's word for which of
  your devices asks. Sandboxed there, Tessera's program and the startup
  files can't be changed, as for the agent's commands.

## Approvals and `auto` ("YOLO") mode

`auto` mode runs everything without asking, with these limits that always
hold: catastrophic commands are refused; Tessera's own files stay hidden and
refused; read-only folders stay read-only; Tessera settings changes still ask;
the browser still asks before typing a secret that is set to ask first. A
command's first use of a secret does **not** ask in `auto` mode, so use it
only where nothing untrusted reaches the agent.

An approval shows the whole command, every line of it, with characters that
would hide or reorder text (control characters, right-to-left overrides,
zero-width spaces) spelled out. A command with several risky parts names each
of them, and **Allow for this session** allows exactly those: a later command
with another risky part (say `git push` after you allowed `sudo`) still asks.
In a channel each bot's "Allow for this session" is its own; one bot's answer
never covers another bot.

Shell reads receive **no prompt exemption**. In **Ask me**, every command asks,
including `cat`/`grep`/`head` pipelines. Explicit read operands are checked through
native-file authorization: resolved symlinks and `..` cannot enter Tessera's store,
and credential targets need a question outside YOLO. Missing paths, globs,
recursive/indirect inputs (`grep -r`, `grep -f`, `sha256sum -c`) and other ambiguous
reads prompt rather than guessing. The server cannot authorize another machine's
file operands, so its read commands ask under **Ask me** or **Only risky actions**.
Use the file tools for ordinary authorized reads without a shell question.
Secret-use, Read-only, machine folder and every-action rules remain unchanged.

In **Ask me** mode, a grant for an otherwise ordinary command covers the exact
command and working folder; an ordinary write/edit grant covers its resolved
path, never all uses of that tool. Pending approvals are rechecked before native
execution and cannot release an action after a permission change.

**Deny is not Stop.** Deny says you declined that tool action. Cards left unanswered
expire after 15 minutes; a cancelled card says nobody answered, not that you
refused it. Only Stop ends the turn. These causes are retained across native
and Claude Code calls; denial responses include a message and do not interrupt
the CLI. Each card outcome and policy refusal has one INFO audit line with a
time, acting session ID, tool, outcome and answering device (or `rule`). Commands,
arguments, summaries, permission keys and rule text are never in this audit line.
The card has a monotonic deadline: a reply at or after it is rejected, and a queued
reply consumed after the deadline cannot release the action either.

**Turn trace.** Each finished turn adds one INFO line (`turn session=… kind=… model=…
outcome=… secs=… steps=… tools=… approvals=… errors=… tokens=… compactions=…`):
per tool the runs that succeeded, failed or never ran and their time, approval
outcomes by kind, errors by class (`overloaded`, `rate_limit`, `timeout`, `auth`…),
token counts. No message text, arguments, output, error text, file names or titles;
private chats are logged as `session=private`. It lets someone diagnose a failure
without reading the chat.
### Claude Code's built-in tools

Claude chats and Tessera helpers register a mandatory catch-all `PreToolUse`
policy hook using documented CLI control messages, before user work is sent.
This checks actions Claude Code would otherwise auto-approve against the same
native risk assessments, live effective mode, protected paths and actor ceiling.
The ceiling comes from full capabilities, not lean schema loading. Explicit
built-in allowlists preserve the documented local tools, with native policy
adapters for commands/Monitor, instructions, helpers, schedules, worktrees,
notifications and MCP resources. PowerShell also uses Windows catastrophic
command rules even on Unix. Write and Edit remain distinct; local Workflow
scripts additionally need command capability. Unknown tools and cloud execution
(RemoteTrigger and remote Agent isolation) stay refused. Report-only helpers
have no action tools.

The policy hook and `can_use_tool` share one approval per CLI call ID, exact
input and mode; bridged tools ask once at native dispatch. Snapshot file reads
and storage follow authorization inside that same callback, never a concurrent
pre-approval edit hook. Session grants use native keys and existing per-chat,
per-bot namespaces. Denial refuses that action, not the whole turn: the agent
continues with other allowed work, without retrying the declined action. A refused
call ID stays refused even if a later callback omits its helper metadata. Mode changes update the running CLI through `set_permission_mode`,
cancel pending cards and are rechecked before release. Helpers also obey the
live parent ceiling (unreadable/contended ceiling: refuse, never widen).
Documented CLI child/plan metadata can only narrow host authority. Successful
hooks do not override CLI permissions: they return `{}`, not an explicit allow,
and CLI refusals stay failed tool results. Stop lifecycle callbacks are
independent of verification preferences.

Ordinary unrestricted Claude chats retain the user's MCP servers and claude.ai
connectors; `--strict-mcp-config` is reserved for private/internal runs and
limited actors without an explicitly permitted external integration. Non-Tessera
MCP calls pass the same host hook. Default and Ask require one approval per
call, with session grants scoped to that exact external tool and the existing
chat/bot namespace. YOLO skips host cards, not actor ceilings or Claude's own
refusals. Read-only permits only a known native read-only MCP classification
whose documented CLI server provenance matches the configured server's
command/args or URL. A name such as `read_file`, model-supplied `readOnlyHint`,
or an invented hook annotation is not proof. Integrations known only to Claude
stay refused in Read-only rather than guessing their effects. Documented MCP
resource read/list tools keep their read semantics but cannot bypass a limited
actor's integration ceiling. CLI hook provenance is retained for subsequent
permission callbacks, so missing metadata cannot widen the action. See the
[built-in mapping table](CLAUDE_CODE.md#built-in-adapters) for the full inventory.

### Claude bridge capability entry

Claude keeps its own filesystem, shell and fetch tools. Its native results
reach Claude's model before Tessera's transcript redaction. The mandatory policy
hook refuses obvious credential-file targets for native Read/Grep/Glob/Bash,
PowerShell and NotebookEdit, including `.env.*`, private keys, registry login
files, kubeconfig and cloud CLI credentials. Native recursive Grep is redirected.
Tessera also bridges `read` and `grep`: their results are redacted before the CLI
receives them, with normal approvals; account files stay refused. This cannot
cover passwords hidden in ordinary files, indirect scripts or integrations.
Tessera cannot rewrite native results inside the unmodified CLI or erase its
history. See [CLAUDE_CODE.md](CLAUDE_CODE.md#credential-file-results).

Tessera relays LSP,
configured external MCP and the chosen search provider through normal dispatch,
with the same approvals, actor ceilings and transcript logging. When Tessera
search is chosen, CLI WebSearch is disallowed for that chat. Private chats
bridge only that search tool: still no local context, credentials, filesystem
tools, external MCP servers or persisted CLI session.

Claude's `save_secret` schema contains **metadata only**, never values. Its
completed card lets the user enter a token/login with Tessera's password inputs.
`save_chat_secret` is an app-only vault request whose `SecretEnv` Debug output is
redacted; no value is appended to messages, tool calls, context or events. It
checks the originating completed card, live saving setting, actor permission,
name, allowed fields and scope; repeat saves are refused. Claude sign-in tokens
are refused, never kept by this entry flow. Normal saving choices still apply.
Saving compares credential values to this chat's transcript transiently in
memory during redaction, without logging them. If earlier plaintext is found,
the chat warns that it stays in Claude's own history, which Tessera never edits,
and invalidates the opaque resume link (including one a live turn might save).
The next turn uses sanitized Tessera history. A secure-card-only save instead
confirms "Saved securely in Tessera." and preserves the safe resume link and
warm process; updated variable names, never values, arrive as turn context.
No extra deferred value comparisons are kept for a card-only save. Unknown
plaintext sent to Claude before saving cannot be erased there by Tessera;
enter passwords in the secure card instead.

Tool/context changes take effect only between turns, including queued turns;
a live CLI turn is never restarted to refresh its inventory.

These are host policy checks, not an OS sandbox around the CLI. Claude executes
its own tools under its own process/sandbox settings; command text assessment
cannot prove the behavior of arbitrary scripts/programs. Obvious account-file
paths are refused, without reading Claude's credential files. A maliciously
indirected unsandboxed command remains a risk, as with native commands without
an OS sandbox. A mid-turn downgrade cannot undo work already released. See
[CLAUDE_CODE.md](CLAUDE_CODE.md#approvals) for interfaces and remaining limits.

## The browser

The web view and the `browser` tool run a real Chromium on the server, outside
the command sandbox. It opens only `http(s)` pages (no `file:`, `chrome:`,
`javascript:`…), can't reach Tessera's own server, and asks before typing into
password fields (outside `auto` mode) or typing a stored secret. Its downloads
land in a folder commands can read.

Web views have no security keys or passkeys (nobody can plug a key into
them): sites see a browser without them and offer their other ways to sign
in (a password, a code), and a site that asks for a key anyway is turned down
at once instead of waiting for one. When a site wants you to prove you're a
person (a "verification challenge"), Tessera doesn't try to get past it:
a web view on one of your computers can be shown on that computer's screen
in a normal browser window (tile options → **Show on <computer>'s screen**,
the same sign-ins), where you finish it yourself; a web view on the server has
no screen, so open the page in your own browser instead. That window is an
ordinary browser nobody drives, confined like the computer's web views, and it
gets only the display (no keyring or session bus).

## Backups

`auth.json` holds your keys in plain text, protected by file permissions.
Exclude Tessera's data folder from backups, or keep the backups encrypted.

## Reporting a problem

Please report security issues privately rather than in a public issue: the
security policy (`SECURITY.md` at the top of Tessera's source) says how, and
what happens next.
