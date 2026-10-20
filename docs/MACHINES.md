# Machines: let Tessera use your other computers

Your Tessera server runs the agents. A **machine** is a computer of yours
that you allow them to use: a desktop with a fast CPU for builds and tests, a
laptop with the files you need, a homelab box. Once a computer is on as a
machine, an agent in a chat (started from your phone, your desktop or the
terminal) can run commands and read or change files there, within what you
allowed on that computer.

```
  phone ──┐                                   ┌── desktop (machine "Desktop")
          ├── end-to-end encrypted ── server ─┤   runs `tessera node`:
 laptop ──┘                                   │   "Work in folders", ~/code
                                              └── (the server's own computer is
                                                  reachable as usual)
```

The machine connects *out* to your server as one of your paired devices, end
to end encrypted, so it needs no open port and works wherever your server is
reachable. Phones are never machines; they can use machines.

Everything lives in **Settings → Devices**: one entry per computer, phone
or browser. A computer's entry gathers everything on it: its Tessera app, its
terminal (`tessera`) and its machine ("Tessera app, terminal and machine for
agents"), each sign-in with its own Sign out, and the machine with what it
allows ("Machine · Work in folders"), who can use it, what it did lately, and
Pause / Stop agents using it. Every device shows the same entries. In the desktop
app, this computer's entry is pinned first with its own controls.

**Names.** Rename an entry from any of your devices (Rename, inside it). A
machine's own name, the one agents and approval cards use ("on Desktop: …"),
changes only on that computer: in its desktop app, Rename in its own entry
gives it one name everywhere (its sign-ins and what agents call it), or
`tessera node name "New name"` there. When the two differ, the entry says
what agents call it.

## Add a machine

**In the desktop app on that computer:** Settings → Devices → Add… → Add a
machine → **Use this computer**. One click; then choose what agents may do.

**Any other computer:** Settings → Devices → Add… → Add a machine shows one
command to run in that computer's terminal, with a copy button:

```bash
tessera node enable 'tessera://pair?…'
```

It pairs that computer with your server (for the machine only), asks for a
name and what agents may do there, and offers to keep it on in the
background. The new machine then shows up in the list by itself. The command
holds a one-time code (it works once, for ten minutes): run it only on that
computer. Tessera not installed there yet? The same panel shows the one
command that installs it (`curl -fsSL …/install.sh | sh`; see the README's
Quick start).

A computer already paired in its terminal (`tessera connect …`) can simply
run `tessera node enable`.

### A machine's login can only serve as that machine

What `tessera node` keeps on a computer to reach your server is a **machine
login**: it can say "I am this machine" and answer the actions it is sent,
nothing else. Whoever copies it off that computer can't read your chats, add
devices, change settings or run the server's agent with it.

- `tessera node enable '<link>'` pairs the computer as a machine and gets a
  machine login straight away.
- Turning on the computer the desktop app runs on ("Use this computer"), or
  one paired in its terminal (`tessera connect`), gives the machine a login of
  its own for that device: the app or the terminal keeps its own login, the
  machine gets one that can only serve as the machine. It is still one entry
  under Devices, and removing that device removes its machine's login too.
- **Machines turned on before this version** keep working without anything to
  do. The first time their updated `tessera node` starts, it trades the
  login it had for a machine login and keeps only that (in
  `node-server.json`); the machine keeps its name, what each bot may do on
  it and the passwords allowed on it. Until that computer's Tessera is
  updated, the server holds the old login's connection to the machine's
  requests once it says it is a machine. If the trade can't happen (the
  server is out of reach), it uses the old login as before and tries again
  at its next start.
- A machine that paired by itself before this version (with
  `tessera node enable '<link>'`) still has its old device login on the
  server, unused once the trade happened. To remove it: on that computer run
  `tessera node disable`, remove the device under Settings → Devices, then
  Add a machine again and run the new command there.

## What agents may do: presets

Each machine has one plain choice. The same five words appear in the apps and
in `tessera node preset`:

| Preset | What it allows |
|---|---|
| **Off** | Agents can't use this computer. |
| **Look only** | Agents can read the folders you choose, and change nothing. |
| **Work in folders** | File actions read and change only the folders you choose. Linux commands change only those folders but can also read other files your account can read, except the protected passwords and keys listed below. The folder list is not a privacy boundary for commands. Without a working command sandbox, commands stay off. Supported web views can optionally run here too. |
| **Full access** | Agents can read and change everything in your home folder, run commands and open web views here. Startup files, passwords and keys stay protected. (On Windows, which has no command sandbox yet, commands reach everything your account can, passwords and keys included: see WINDOWS.md.) |
| **Desktop control** | Full access, plus the desktop session supported by that computer: on Windows, screenshots and mouse/keyboard input; on Linux, command-based desktop session and startup/window manager settings. This is close to giving the agent your account. |

**Work in folders is available on every platform**, including macOS, Windows
and Linux without a sandbox. Reading and changing files uses checked file
actions limited to your chosen folders; it does not need the command sandbox.
Without that sandbox, the app and terminal explain that commands aren't
available with this choice. Linux with a working sandbox keeps its existing
confined-command behavior.

**Advanced** keeps per-folder control (read-only or read-write folders,
commands on or off, desktop control, web views, a time limit) for setups the
presets don't cover; the apps show it as "Advanced" when the folders are
mixed.

Choosing a preset that allows more asks first, in plain words; choosing one
that allows less applies at once. Switching presets never makes the machine
ask less (only the "Ask first" choice does); Desktop control always starts at
"every action".

Desktop control is offered only when **that computer** reports support.
A Linux server may use a Windows node's native screen/input, or the reverse;
the server's OS does not decide. Older Linux nodes retain command-based
desktop control; unsupported nodes refuse it. Windows requires the separately
approved **Tessera Guard** service before commands or desktop input can run.
The Stop bar and its Ctrl+Alt+Esc key (fallback Ctrl+Alt+Pause or
Ctrl+Alt+Shift+S) are part of that permission, not optional decoration.

**Windows Stop is not a command sandbox.** It ends Tessera's adopted jobs and
blocks new actions until you approve Resume. Programs started through
Explorer/Run, Task Scheduler or WMI may instead run as your own programs and
survive Stop. UAC is not a security boundary; Guard doesn't change that or
protect against an already elevated program. See [WINDOWS.md](WINDOWS.md)
for the exact requirements, approval rules and limits.

### Ask first

| Choice | Means |
|---|---|
| **Every action** (the default) | Every action, even a read, waits for your approval in the chat, from any device. |
| **Only risky ones** | Commands the safety rules flag (deleting outside the project, pushing, installing, `sudo`…) and changes to startup files ask; everyday actions just run. |
| **Never** | The machine asks nothing on its own; each chat's mode decides (YOLO chats don't ask, read-only chats never change anything). |

The stricter of the machine and the chat wins: a chat set to ask still asks.
Catastrophic commands are refused whatever you choose.

### Who can use it

**Any chat** (the default), or **only these bots**: for example a "homelab
sweeper" bot that may use the machine on its own, in its chats, the helpers
it starts (subagents) and its scheduled duties. Other chats don't even see
that machine. With only a trusted bot allowed, "Ask first: never" (and that
bot's own permission mode) lets it work unattended. The server checks this
before anything is sent, for Tessera's own agent and for Claude Code chats
alike, and the machine checks again.

### What each bot may do

Each bot also has its own choice per machine, in its settings (**Machines**,
in the bot editor), so you can give one bot your desktop and keep another
off it:

| Choice | Means |
|---|---|
| **No access** | The bot can't use that machine at all; it doesn't even see it. |
| **Read only** | It can read files and list folders the machine allows. No commands, no changes, no web views there. |
| **As allowed** | Whatever the machine allows (its preset and "Who can use it"). |

"Other machines" covers the rest, including machines you turn on later. This
never gives a bot more than the machine allows: both have to say yes, and the
server checks both before anything is sent (for the bot's chats, its
helpers, its scheduled duties, its private conversations and its turns in
channels). Settings → Devices shows, under each machine, which bots may use
it.

New bots start with **No access** everywhere: give them a machine when they
need one. Bots made before this setting keep working as before (as each
machine allows) until you choose less. Your own chats (not a bot's) are not
affected. When a chat asks a bot that may do more on a machine than the chat
itself, you are asked first (also in YOLO mode). What counts is what each may
really do there: the machine's own "Who can use it" too, so a plain chat or a
bot the machine keeps out asking one it lets in asks first. In a channel each bot acts with its own access
when its turn comes: putting bots in a channel together lets them hand each
other work, so mix a limited bot with a trusted one only if that is fine with
you. Removing a bot switches its duties off.

### Web views here

Chats and bots that may use the machine can open web views whose browser
runs on it. **Those pages can reach the machine's local network and services
on localhost.** Included in Full access and Desktop control, a switch under
Work in folders, never with Look only.

Where a web view runs is decided when it is made: on the machine you are
using the chat on (the desktop app shares its pairing with the machine it
turns on), when it has web views on and the chat may use it; otherwise on the
server. It keeps that place: a phone shows a desktop's web view through the
server, and a web view made on the phone runs on the server. The tile says
"on Desktop"; if that machine goes offline it says so and offers to open the
page on the server instead. Sites you sign in to stay on the computer you
signed in on.

On Linux this virtual view runs ordinary Chrome on its own in-memory
screen, not a disguised headless browser or your desktop. Cookies and site
storage follow Chrome's defaults and persist in this computer's profile.
Narrow phone layouts do not pretend to be Android Chrome. See
[Web views: sign-ins and homework](WEB_VIEWS.md) for current limits.
What keeps it safe:
- The browser is Tessera's own (its profile and downloads in Tessera's data
  folder on that computer), never your own browser or its logins, and it gets
  no part of your desktop session (display, clipboard, keyring, session bus).
- On Linux it runs in a Landlock sandbox: it can read the system and write
  only its own folder, never your home folder. Chromium's own sandbox stays
  on, and it listens on no port.
- On Windows, Chromium's renderer sandbox stays on, and a mandatory Windows
  job ends the browser and its private pipe relay with their owner (no
  breakaway); its profile, temp files and downloads are private to Tessera.
  There is no Landlock-equivalent restriction on the browser's main process:
  if it is compromised, it still has your account's file access. No untested
  AppContainer or restricted-token setup is claimed. Windows launches are
  compiled, not yet accepted in a real Windows run. Showing the ordinary
  verification window on that computer's screen remains Unix-only.
  A pending show-on-screen request is registered before desktop discovery.
  Stop, pause, revocation or disconnect cancels it while it waits for a desktop
  or the previous browser to close. The live Stop generation is checked again
  at launch and registration; a child launched just as Stop wins is killed,
  never left as an unregistered window.
- The server drives it only through this machine's encrypted connection, and
  the machine passes on only an allowlist of browser commands: pages only at
  web addresses, no local files, no arbitrary host-file uploads, no extensions,
  no other browser settings. When a page opens an upload field, you choose
  files on your device; only those selected bytes are copied into this
  browser's private folder (up to 8 files totaling 4 MiB). The page cannot
  request a path from this computer, and agents cannot choose files for you.
  The chooser expires on navigation and the machine still enforces Stop,
  pause/revocation and its admission rules.
- Who may use the machine applies to its web views too, and every action an
  agent takes in one is in the machine's log.
- At most 8 tabs; the browser stops when unused for 10 minutes, when it uses
  too much memory (a quarter of the computer's memory, between 1 and 8 GB,
  counting memory its processes share once), and whenever the machine is
  paused, switched off or loses its connection. Before that limit (or when
  the computer runs short of memory) the page nobody looked at or used for
  longest closes first; it opens again at its last address when used. After
  a stop for memory the pages on screen open again by themselves, and an
  agent is told on its next step that unsent form contents are gone.

**Show on its screen.** Some sites ask you to prove you're a person before
you sign in, and a web view can't always pass that. From the tile's options,
**Show on <computer>'s screen** opens the page in a normal browser window on
that computer, with the web views' own browser profile (the same sign-ins);
finish the check (and the sign-in) there, then close the window or press
**Back to the tile**, and the web view carries on with what you did. Only you
can do this from your devices (agents can't), only while that computer has
web views on, and only when someone is signed in to its desktop. The window
is an ordinary browser that nobody drives (no automation of any kind), in
the same file sandbox, with the display but no keyring or session bus; the
web views there wait while it is open. Sign-ins a site keeps only until the
browser closes ("remember me" off) may need doing again in the tile.

### Terminal from my devices

Separate from what agents may do: your own paired devices (never agents, chats
or bots) may open a terminal on this computer, from **New → Terminal** in the
Tessera app (never from a web browser). Off unless turned on here, on this
computer only (Settings → Devices → This computer, or
`tessera node scope --terminal off|sandboxed|full`).

**Turning terminals on here trusts your Tessera server with this account.**
This computer can't check for itself which of your devices asks for a
terminal: it takes the server's word. With full access that is everything
this account can do; sandboxed, less (below). Your devices get a
notification whenever terminals are turned on or given more, here or on the
server.

It is a command line as your account here, starting in your home folder:
- **Sandboxed** (Linux 5.19 or newer, as for commands here): Tessera's own
  files on this computer (its login to your server) and your desktop
  session's control sockets are hidden. Tessera's program and your startup
  files (shell startup files, login services, autostart, window manager
  settings) can be read but not changed, so nothing can be left there to
  run outside the sandbox at your next login. Directly in the folders that
  hold them (your home folder itself, `~/.config`, `~/.local`,
  `~/.local/share` and the folder with Tessera's program, such as
  `~/.local/bin`), what is there can be changed but nothing added, removed
  or renamed; inside your other folders, everything as usual. Your SSH agent stays usable, as for
  the agent's commands: it signs for you but can't hand out your keys.
- **Full access**: everything your account can do here.

The terminal belongs to the device that opened it; nobody else sees or types
in it. A program that doesn't read what you type holds up only its own
terminal; past 1 MB waiting, the terminal says so and takes no more. It
closes when you close its tile, after 30 minutes without typing at its
prompt, 5 minutes after your device went away, when that device is signed
out, and when this computer is paused, turned off, loses its server or the
setting changes. Its log records who opened one and when, never what was
typed or shown.

On Windows 10 1809+ / Windows 11, ConPTY terminals are implemented (native
runtime acceptance pending): **Off / Full access** only, never Sandboxed.
They choose PowerShell 7, then Windows PowerShell, then cmd from PATH, use
UTF-8, and put the shell and its children in a mandatory kill-on-close Windows
Job. They do not inherit Tessera's service handles or secret environment
variables. Older Windows nodes stay unavailable until updated; new nodes
advertise their terminal backend independently of the local permission.
See [WINDOWS.md](WINDOWS.md#interactive-terminal-tiles).

### Administrator

Off by default, and switchable only on this computer (Settings → Devices →
This computer → **Administrator**, or `tessera node admin on`). It needs
commands on (Work in folders, Full access or Desktop control).

**Administrator lets agents run commands here as root.** Normally an agent's
command runs in Tessera's sandbox, where `sudo` can't work (the sandbox is
built so that nothing in it can gain privileges). With Administrator on, an
agent may ask to run one command *as administrator* (`run` with `admin`):
that command runs **outside the sandbox**, as you, with `sudo` working. It can
change anything on this computer: system files, services, other accounts,
installed programs, and Tessera's own settings here. Turn it on for work you
want done that way (fixing a driver, installing packages, restarting a system
service) and off again when you're done.

- **Ask each time** (the default once on): every administrator command waits
  for your approval in the chat, showing the exact command, whatever the
  chat's mode, YOLO included. The machine itself refuses one that wasn't
  approved. "Allow for this session" on such a card covers only that exact
  command, in that folder, in that chat.
- **As the chat says**: administrator commands follow this computer's "Ask
  first" choice and each chat's mode, so in a YOLO chat they run without
  asking. Choose this only if you would run those commands yourself unread.
- **Off**: commands stay in the sandbox.

`sudo`'s password, if your account needs one for `sudo` (most do): save it on
this computer (**Save password** under Administrator, or
`tessera node admin password`). Tessera checks it with `sudo` before saving,
keeps it in its own settings folder here (readable by your account only,
hidden from sandboxed commands and refused to file actions) and **never sends
it anywhere**: not to the server, not to the model, not to any device. When an
administrator command runs, its shell first hands the password to
`sudo -S -v` on a private pipe (closed again before your command starts), so
the command's own `sudo` calls find it already accepted; `sudo -k` forgets
that when the command ends. The command never gets a copy in its environment
or arguments, and the password is wiped from anything it prints. If your
`sudo` needs no password (your own `sudoers` rule), nothing is saved.
**Forget** (or `tessera node admin forget`) removes it.

It is revocable at once: switching Administrator off here, pausing or
revoking the machine from any device stops running administrator commands
immediately. Every administrator command is in this computer's log as "run as
administrator", with the chat, the bot and the exact command. Not on Windows
or macOS yet.

What it can't protect: an administrator command is root, so it could read
the saved password file or change Tessera's settings on this computer. That
is why each one asks you first by default. Since this computer takes the
server's word that you approved (below), Administrator also trusts your
Tessera server with root on this computer.

## Security first

Turning a computer on, and anything that lets agents do more there (a
preset, a folder, asking less, letting more chats or bots use it, desktop
control, web views, administrator commands), happens **only on that computer**: in its own terminal
(`tessera node …`) or its own desktop app. Nothing on the server or another
device can turn a machine on or widen what it allows. Other devices can only
take things away: pause it, resume it (never beyond what it allows) or
revoke its access. Their entries say "Change what it allows on that
computer". A bot's own choice (above) is set on the server, from any of your
devices, because it can only keep a bot below what the machine allows.

The machine keeps what it allows in its own Tessera settings and checks
every action itself; it never relies on the server's view of it.

Always out of reach, whatever you allow:

- Tessera's own data and settings on that computer (its device login, its
  settings, its log and backups, the desktop app's saved servers, and a
  server's data if one runs there too). File actions refuse them and
  commands don't see them.
- Other programs' credential stores: `~/.ssh`, `~/.gnupg`, `~/.aws`,
  `~/.kube`, `~/.docker`, `~/.password-store`, `~/.config/gh`, browser
  profiles, keyrings and similar, unless you allow that folder itself (for
  example `~/.ssh` read-only so `git` over SSH works). Single credential files
  in your home folder (`~/.netrc`, `~/.git-credentials`, `~/.pgpass`,
  `~/.npmrc`, `~/.pypirc`, `~/.Xauthority`, cargo's token) are never handed
  out: file actions refuse them and commands read them as empty.
- The Tessera program itself.
- Without desktop control, files that run programs later, outside any
  sandbox: shell startup files (`~/.bashrc`, `~/.profile`, `~/.zshrc`…),
  `~/.config/systemd`, `~/.config/autostart`, `~/.config/environment.d`,
  window manager and session settings (`~/.config/hypr`, `i3`, `sway`,
  `niri`, `river`, `labwc`, `uwsm`, `waybar`, `fish`). Not even new ones can
  appear: with your home folder allowed, commands can change everything
  inside its folders but can't add entries to the home folder itself (create
  a new folder there with a file action, then work inside it).

Full access is honest about its limits: it protects startup files and your
keys, but an agent that can change your files can also change things other
programs run later (a script in `~/.local/bin`, an editor's plugin folder).
Choose it for agents you trust with your files.

**On a Mac** only chosen folders can be read-write (Work in folders, or
Advanced): Full access and desktop control aren't offered, and nothing is
changed through a rule for your whole home folder or in `~/Library`. A Mac's
login items and app data live there, and its disks ignore letter case, which
these checks don't fully cover yet. Commands are Linux only anyway.

## Desktop control

Off by default, and switchable only on the machine itself, with a plain
warning: **this is close to giving the agent your account.** When it is on:

- startup files and folders become changeable (shell startup files,
  autostart, systemd user units, `~/.config/hypr`, `i3`, `sway` and the rest
  above). **Each changed startup file is backed up first**, by file action or
  command: the machine keeps a copy of every startup file's last content
  (owner-only, in its own data folder) and records every one an agent
  changed, added or deleted. Put one back in Settings → Devices → This
  computer → "Startup files changed by agents", or in the terminal:

  ```bash
  tessera node restore        # what agents changed, numbered
  tessera node restore 12     # put that file back as it was before
  ```

  Putting back is recorded too, so it can be undone the same way; a change
  you make yourself is never mistaken for an agent's.
- commands get your **graphical session**: `WAYLAND_DISPLAY`, `DISPLAY`,
  `HYPRLAND_INSTANCE_SIGNATURE` (and sway's, i3's, niri's sockets), the
  session bus, and the sockets they need in your runtime folder, so
  `hyprctl reload`, `notify-send`, `gsettings`, `systemctl --user` and the
  like work. Nothing else in the runtime folder is visible.

How the session is found, each time a command starts (a background service
usually starts before your desktop, or outlives it): the machine's own
environment when it has a display; otherwise what your desktop handed to
your user services at login (`systemctl --user show-environment`; Hyprland,
Plasma and GNOME do this, other setups with
`dbus-update-activation-environment --systemd --all` in their startup);
otherwise a running compositor or desktop shell of yours. Wayland and X11
both work. `tessera node status` says which session commands would get.

The session bus is reachable only through a filter (`xdg-dbus-proxy`, part of
Flatpak's dependencies on most distributions): commands can talk to
notifications, settings, the window manager and shell, user services, media
players and input methods, but **not to your keyring or wallet**, where your
passwords live. Without `xdg-dbus-proxy` installed, commands get no session
bus (the rest still works).

What it can't prevent: programs started through your desktop (for example
`hyprctl dispatch exec`, `systemd-run --user` or a user service it starts
through the session bus, or a new autostart entry at your next login) run as
you, outside Tessera's sandbox. Such a program sees everything you can: your
credential folders, your passwords and keys, Tessera's own files, and this
machine's own settings, which it could widen (more folders, no asking). That
is why it is off by default, starts at "Ask first: every action", and is
logged and backed up. Commands themselves still don't see your credential
folders or Tessera's files; what they start through your desktop does.

## Turn a computer on (in its terminal)

```bash
tessera node enable                              # asks for a name and what agents may do
tessera node preset work --folder ~/code         # or: look, full, desktop, off
tessera node preset --ask risky                  # Ask first: every | risky | never
tessera node access --bot homelab-sweeper        # who can use it (or --any)
tessera node service install                     # keep it on in the background
tessera node status                              # everything at a glance (--json for scripts)
```

Anything that allows more asks before it applies (`--yes` skips the
question, for scripts and the desktop app); desktop control shows its warning
first. Narrowing never asks and takes effect within a second, also on actions
already running: an action the narrower settings no longer allow is stopped.

Advanced, folder by folder:

```bash
tessera node scope                               # show it
tessera node scope --add-folder ~/Documents      # read-only
tessera node scope --add-folder ~/code --write   # read-write
tessera node scope --remove-folder ~/Documents
tessera node scope --commands on|off
tessera node scope --desktop on|off              # desktop control
tessera node scope --webviews on|off             # web views here
tessera node scope --terminal off|sandboxed|full # terminals from your devices
tessera node scope --timeout 3600                # one hour per action
```

In the desktop app, the same lives under **Settings → Devices → This
computer**; the app runs these commands on the computer itself, never
through the server.

### Stay on

`tessera node run` keeps the machine online while it runs. To keep it on
without a terminal, `tessera node service install` installs a user service
(`tessera-node.service`) that starts when you log in, and offers to keep it
running when you're logged out too (`loginctl enable-linger`, useful on a
computer you reach over SSH). `tessera node service uninstall` removes it.
On Windows it is a startup program instead ("Tessera machine", run by
`tessera-service.exe` without a window), which runs while you're signed in.

## How agents use it

Agents get a `machines` tool once a machine exists: they can list the
machines their chat may use and what each allows, run a command (in an
allowed folder), and read, list, write or edit files. Paths are the
machine's own (`/home/you/code/app` or `~/code/app`).

Approval cards name the machine: **on Desktop: cargo build --release**. Results
go into the chat like any tool's, with credential-looking values scrubbed;
long output is kept in full as an artifact the agent can page through.
Changes made on a machine are not undone by rewinding a chat.

## Commands on a machine

Commands need Linux with the kernel sandbox Tessera uses for the server's own
commands, plus Landlock ABI 3 (Linux 6.2 or newer).
Without it, commands are refused rather than run unconfined
(`tessera node status` says whether this computer qualifies). Each command:

- runs as you, through your login shell's `PATH`, in its own process group,
  with stdin closed and no terminal;
- can **change files only in your read-write folders** and its own private
  temporary folder (`$TMPDIR`, removed afterwards). Everything else it can
  read like you can, but not change: writing elsewhere fails with
  "Permission denied". If a build writes outside the project (for example
  `cargo` downloading into `~/.cargo`), allow that folder read-write too;
- doesn't see Tessera's data or the credential stores above, nor (without
  desktop control) your desktop session (D-Bus, `systemctl --user`, tmux,
  X11/Wayland) or SSH agent. On Linux older than 6.12 a command could still
  reach an X display's socket; it doesn't get the display's login cookie
  (`~/.Xauthority` reads as empty), so the display turns it away
  (`tessera node status` notes this);
- gets your environment without anything credential-shaped (`*TOKEN*`,
  `*SECRET*`, `*PASSWORD*`, `*API_KEY*`, `SSH_AUTH_SOCK`…);
- is stopped at once, with everything it started, when the chat stops it,
  its time limit runs out, or the machine is turned off, paused, revoked,
  narrowed or loses its connection.

The network is not restricted.

### On Windows

Windows has no command sandbox in Tessera yet, so a Windows machine runs
commands only with **Full access**, and then they are not confined: they run
with your account's rights and can reach everything it can, passwords and
keys included (the preset's description on that computer says so). With
Look only, Work in folders or Advanced, commands are refused and agents get
the files of the folders you chose. Each command:

- runs in Git Bash when Git for Windows is installed, else PowerShell 7, else
  Windows PowerShell 5.1, else cmd (agents see which: the machine's system
  reads like `windows x86_64, shell Git Bash (bash)`);
- runs below normal priority, with no window, with stdin closed and the same
  environment rules as above;
- is stopped with everything it started (a Windows job) when the chat stops
  it, its time limit runs out, or the machine is turned off, paused, revoked
  or narrowed.

File actions check paths on Windows as on Linux, with Windows' own credential
stores (the Credential Manager's files, DPAPI keys, browser profiles…) and
startup locations (the Startup folder, PowerShell profiles) out of reach
unless allowed. Desktop control on Windows gives agents the screens, mouse
and keyboard of your signed-in session (the `desktop` tool; see
[WINDOWS.md](WINDOWS.md)); startup locations stay protected there. Machine-hosted web views are implemented (native acceptance pending). Keeping the machine on: `tessera node service install`
adds it to your startup programs (no administrator rights). More in
[WINDOWS.md](WINDOWS.md).

## Pause, resume, revoke

From any of your devices, a machine's entry under **Settings → Devices**
offers:

- **Pause**: running actions stop and every new one is refused until you
  resume it.
- **Resume**: back to exactly what it allows; never more.

On the machine itself you can **Stop** every agent at once: the Stop bar's
button or its key on Windows (see [WINDOWS.md](WINDOWS.md)), or
`tessera node stop` anywhere. Running actions are told to end (normally
within a second or two; a Tessera server on that computer ends its chats'
turns too and runs no tools), and it refuses everything until you resume it
**on that computer**
(`tessera node resume`, This computer in the app, or Resume on the Stop
bar); no other device can, and Tessera refuses it from agents. With an
up-to-date server nothing asked for before the Stop runs after it, and chats
ask again before using its screen.
- **Stop agents using it**: running actions stop, and the machine turns itself off.
  Only that computer can turn it on again (`tessera node enable` there).
- **Sign out** (one of its sign-ins): it is disconnected and must be paired
  again. When the machine connects through that sign-in, the question says
  so: agents then can't use the machine either until it is turned on again
  there.

On the machine itself, `tessera node disable` (or the Off preset) turns it
off at once (running actions are killed) and removes the background service.

## What is logged

Every action is recorded on the machine in its own log, owner-only and out of
reach of the actions it records: when, which chat and which bot asked, what
(the command or path), whether it was allowed or refused and why, and how it
ended (exit code, time limit, stopped). Actions that never reached it,
because the server already knew they were outside what it allows or you
declined them, are logged there too while it is online. Turning on and off,
changes to what it allows, startup files changed and put back, connections,
pauses and revokes are logged as well.

```bash
tessera node log            # the last 50 entries
tessera node log -n 500 --json
```

The server also keeps each machine's latest actions (shown on its entry in
Settings → Devices), and each action appears in the chat that asked for it.

## What the server can and can't do

The machine is the authority on its file actions, command switch, write folders,
who may use it, time limit and the protections above. A server cannot widen
those settings or turn the machine on. File actions read only the chosen folders.
With commands on, the folder list limits **writes**, not reads. Commands can
also read other files the account can read, except the hidden locations above.

The machine does trust the server to say that you approved an action in the
chat, and which bot an action comes from, because approvals and bots live
where you are. The server is your own and your devices talk to it end-to-end
encrypted, but if you want a machine to be safe even from a compromised
server, keep its preset to what you would accept the server doing unasked.

## Limits

- A command that deliberately detaches itself (a daemon that starts a new
  session) can outlive a stop; it stays inside the sandbox.
- Commands can read other files the account can read, not just the chosen folders,
  except the hidden locations above. To keep other files private, leave commands
  off and use checked file actions only. Moving a file outside the chosen folders
  does not hide it from commands.
- File reads show up to 8 MB of a file (named pipes and other special files
  aren't read); writes and edits take files up to 16 MB. Web views here keep
  at most 8 GB of downloads (4 GB per file); past that a download stops. At most 8 actions run on one machine at a time. Startup files larger
  than 2 MB are noted when changed but not backed up.
- Background services: systemd on Linux, a startup program on Windows
  (`tessera node service install` on both); elsewhere run `tessera node run`
  yourself. Commands run on Linux (sandboxed) and on Windows with Full access
  (unsandboxed, below); not on macOS yet.

## Command reference

| Command | Does |
|---|---|
| `tessera node status [--json]` | On or off, connected, what it allows, session, service, log file |
| `tessera node enable ['<pairing link>'] [--preset P] [--folder F]… [--name N] [--yes]` | Pair (with a link) and turn this computer on |
| `tessera node preset [off\|look\|work\|full\|desktop] [--folder F]… [--ask every\|risky\|never] [--webviews on\|off]` | Show or choose what agents may do |
| `tessera node access [--any \| --bot B…]` | Who can use it |
| `tessera node scope [...]` | Advanced: folders, commands, desktop control, web views, asking, time limit |
| `tessera node admin [on [--ask every\|chat]\|off\|password [--stdin]\|forget]` | Administrator commands (root with sudo, outside the sandbox) and sudo's password, kept here only |
| `tessera node restore [N] [--json]` | Startup files agents changed; put one back |
| `tessera node disable [--json]` | Turn it off now; running actions stop; service removed |
| `tessera node service install\|uninstall\|status` | Keep it on in the background |
| `tessera node log [-n N] [--json]` | What was done here, newest first |
| `tessera node name ["New name"] [--json]` | Show or change what agents and your devices call this computer (only here) |
| `tessera node run` | Run it in the foreground (what the service runs) |
| `tessera machines [--pause\|--resume\|--revoke M]` | From any device: list machines, pause, resume, revoke |

`tessera node enable --server-stdin` takes the server login on stdin (used by
the desktop app to hand over its own pairing); like a pairing link's login it
is stored owner-only for the machine alone, and must pin the server's key,
since a machine only connects end-to-end encrypted.
