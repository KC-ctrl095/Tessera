# Tessera on Windows

Tessera runs on Windows 10 (1809 or newer) and 11 (64-bit): the server, the terminal app,
`tessera setup`, a machine (`tessera node`) and the desktop app. This page
says how to install it, where things go, and what works differently from
Linux. macOS support comes later.

New to Tessera? **Getting started on Windows** (Settings → Help & tour, or
[GETTING-STARTED-WINDOWS.md](GETTING-STARTED-WINDOWS.md)) walks you through
it step by step, in plain words.

## Install

**The desktop app's installer (easiest).** Download `tessera-desktop-windows-x86_64-setup.exe` from the project's release page. Public downloads are not published yet. It installs the app together with the server (`tessera.exe` and `tessera-service.exe`, in `%LOCALAPPDATA%\Tessera`) for your account only. No administrator rights, Rust or Node are needed. If Windows warns about an unknown publisher, stop and verify where the download came from. Only use **More info → Run anyway** if you trust it. Do not turn off Smart App Control. In the app, **Start Tessera here** runs the server on this PC and connects to it. The next page asks you to sign in to an AI account or add an API key.

A normal interactive installer offers **Tessera Guard** separately, with
**No** selected. Choosing it opens Windows' administrator prompt; cancelling
leaves agent access off. Silent installs never elevate. Guard can be installed
later in Settings → Devices → This computer. Updates offer to update the
shared Guard too; stop agent work here before approving it.

To update, run the newer installer: it stops the server and this
computer's machine (cleanly: chats that were working carry on), replaces
them and starts them again.

**With PowerShell** for the server and terminal app, with an optional desktop install. No administrator rights are needed. For server/terminal only, set `$env:TESSERA_NO_DESKTOP = '1'` before running this command.

```powershell
irm https://github.com/KC-ctrl095/Tessera/releases/latest/download/install.ps1 | iex
```

What it does:

1. Downloads Tessera for Windows from the latest release and checks it
   against the release's published checksums (SHA256SUMS) before installing
   anything.
2. Puts `tessera.exe` and `tessera-service.exe` in
   `%LOCALAPPDATA%\Programs\Tessera` and adds that folder to your PATH (open a
   new terminal window to use `tessera`).
3. Offers the desktop app when the release has it (its own per-user
   installer; it installs Microsoft's WebView2 runtime if your Windows lacks
   it, which is rare).
4. Offers the guided setup, `tessera setup`.

Run the same line again to update. A running background service restarts
with the new version.

**From another server** (a mirror, or a private Gitea or Forgejo):

```powershell
$env:TESSERA_REPO_URL = "http://gitea.example.com:3000/you/tessera"
$env:TESSERA_TOKEN = "<access token>"   # a private repository only: a read-only token
irm -Headers @{ Authorization = "token $env:TESSERA_TOKEN" } "$env:TESSERA_REPO_URL/raw/branch/master/scripts/install.ps1" | iex
```

**From a release zip you already have** (`tessera-windows-x86_64.zip`). Download `SHA256SUMS` from that same release and compare `Get-FileHash .\tessera-windows-x86_64.zip -Algorithm SHA256` with its entry before unpacking. Unpack it, then run the included installer with the local zip explicitly selected.

```powershell
powershell -ExecutionPolicy Bypass -File .\install.ps1 -Zip C:\full\path\tessera-windows-x86_64.zip -NoDesktop
```

Other options, as environment variables before the `irm` line (or as
parameters when you run the file): `TESSERA_VERSION` (`-Version v0.2.0`),
`TESSERA_DIR` (`-Dir`, where to install), `TESSERA_ZIP` (`-Zip`, a release
zip), `TESSERA_SOURCE=1` (`-Source`, build with cargo: needs Rust from
<https://rustup.rs> with the Visual Studio C++ build tools), `TESSERA_YES=1`
(`-Yes`), `TESSERA_NO_DESKTOP=1` (`-NoDesktop`).

## Where things are

| What | Where |
|---|---|
| The programs (PowerShell install) | `%LOCALAPPDATA%\Programs\Tessera` |
| The desktop app, with its own copy of the programs | `%LOCALAPPDATA%\Tessera` (Start menu: Tessera) |
| Your data and settings (chats, keys, memory) | `%APPDATA%\tessera` |
| The background service's log | `%APPDATA%\tessera\logs\tessera.log` (a machine's: `tessera-node.log`) |
| What a stuck server was doing (see below) | `%APPDATA%\tessera\stalls` |
| cloudflared, for remote access | `%APPDATA%\tessera\bin\cloudflared.exe` |
| The licence and the open-source notices | `LICENSE.txt`, `NOTICE.txt`, `THIRD_PARTY_NOTICES.md` and `THIRD_PARTY_LICENSES.txt` in the desktop app's folder (and in the release zip); in the app: Settings → Help & tour → Open-source licences |

Your data folder inherits your user profile's permissions: only your account
(and, as always on Windows, administrators and the system) can open it, the
same as `~/.local/share/tessera` on Linux, where only root can read it
besides you. `TESSERA_HOME` puts everything in another folder; keep that
inside your user folder too (`C:\Users\you\…`): on a second drive like
`D:\`, every account on the PC can usually read new folders unless you
change their permissions.

Tessera doesn't encrypt its keys file with your Windows password (DPAPI), on
purpose: without a sandbox on Windows, anything running as you (the
agent's commands included) could decrypt it just as well, and an
administrator resetting your password would lose every key and pairing.
To protect the disk of a PC that could be stolen, turn on BitLocker or
Device encryption (Settings → Privacy & security).

## Running in the background

`tessera setup` offers it, or:

```powershell
tessera service install     # starts now and whenever you sign in to Windows
tessera service status      # running? where's the log?
tessera service uninstall   # stops it and removes it; your data stays
```

It is a startup program of your own account (Task Manager → Startup apps
shows "tessera-service" and can turn it off), not a Windows service: no
administrator rights, and it runs while you are signed in. The desktop app's
**Start Tessera here** sets it up the same way when "Keep Tessera running" is
ticked; untick it and the app starts the server only until the PC restarts
(still restarted after a crash, but it doesn't keep the PC awake). To change
that later, in the desktop app: **Settings → Devices →** this computer **→
Keep Tessera running** (on or off; Tessera restarts once, and chats that were
working carry on).

- **Restarts by itself.** It has no window. If Tessera stops with an error
  it starts it again after 5 seconds; after 5 failures within 2 minutes it
  waits 10 minutes before the next try, and so on (it never gives up, and
  never starts it over and over). The log says why each time.
- **A stuck server is restarted too.** A watchdog inside the server notices
  when it stops working or stops answering. It writes down what it was
  doing (in the `stalls` folder next to your data), and after 3 minutes
  still stuck it ends itself, so the startup program starts it again. The
  helpers it runs (the browser for web views, cloudflared) end with it and
  are never left behind.
- **Stays awake while plugged in.** A server started this way keeps Windows
  from going to sleep by itself while the PC is plugged in, so your devices
  can reach it (the screen still turns off; `powercfg /requests` lists it).
  On battery Windows sleeps as usual, and closing a laptop's lid or choosing
  Sleep still puts it to sleep. Some laptops with "Modern Standby" may still
  sleep when the screen turns off: set "When plugged in, put my device to
  sleep after" to Never (Settings → System → Power) if yours does. To let
  Windows sleep as usual, add `keep_awake = false` under `[server]` in
  `config.toml` (in your data folder) and restart Tessera.
- **Only while you're signed in.** Windows runs startup programs when you
  sign in, so after a restart Tessera is back once you sign in (you can lock
  the PC right away). For restarts after Windows updates, turn on Settings →
  Accounts → Sign-in options → "Use my sign-in info to automatically finish
  setting up after an update": Windows then signs you in (locked) and
  Tessera starts.

Stopping asks the server to stop cleanly, so chats that were working carry
on when it starts again.

## Network, firewall and remote access

By default the server listens on this PC only (`127.0.0.1`): nothing on
your network can reach it, and Windows' firewall never asks about it. That,
plus remote access through Cloudflare (below), is the safest setup: your
phone reaches Tessera through the encrypted tunnel at home and away, and
nothing listens on your network.

**At home without a tunnel:** Settings → Connection → "Let other devices on
my network connect", then restart Tessera. Windows Defender Firewall asks
once whether `tessera.exe` may use the network: tick **Private networks**
only and click **Allow** (it may ask for an administrator's password).
Windows treats new Wi-Fi networks as Public, where the firewall keeps
devices out: set yours to Private (Settings → Network & internet → your
network → Network profile type). Clicked Cancel? "Allow an app through
Windows Firewall" → Change settings → tick Private for tessera.exe. Devices
still have to pair, and everything stays end-to-end encrypted.

**From anywhere (Cloudflare):** Settings → Connection → "Set it up with
Cloudflare" works on Windows as on Linux (the guide "Reach Tessera from
anywhere" explains the account, the domain and the token). On Windows:

- Tessera downloads `cloudflared.exe` from Cloudflare's official GitHub
  release, checks it against the SHA-256 published with that release (it
  refuses a file that doesn't match), and keeps it in the `bin` folder of
  your data folder. A `cloudflared.exe` you installed yourself (e.g. with
  winget) is used when Tessera has no copy of its own.
- Tessera runs it without a window, restarts it if it stops, and stops it
  with the server; it can't outlive the server, even after a crash. If the
  file goes missing (an antivirus, a cleanup), the next start downloads and
  checks it again.
- It only connects out (TCP and UDP port 7844 to Cloudflare): no firewall
  prompt and nothing to open on your router. Windows' firewall lets that
  out; some other security programs block it, and Settings → Connection
  then says the tunnel can't reach Cloudflare.
- Tessera uses a tunnel in your own Cloudflare account, with a fixed
  address and Cloudflare Access in front of it: only your paired apps get
  through. Phones you paired before pick up the address by themselves (open
  the app once at home), and Tessera reminds you to renew the Access key
  once a year. It doesn't use Cloudflare's free "quick tunnels": their address
  changes at every restart (your phone would lose it) and anyone who finds
  it reaches the door.

## Commands the agent runs

Models know bash best, so the agent's commands run in **Git Bash** when Git
for Windows is installed (<https://git-scm.com>, recommended). Without it they
run in PowerShell 7 if installed, else Windows PowerShell 5.1 (part of
Windows), else cmd. The agent is told which one it has. Set `TESSERA_SHELL`
to the full path of bash.exe, pwsh.exe, powershell.exe or cmd.exe to choose
yourself (then restart Tessera).

**There is no command sandbox on Windows yet.** On Linux the agent's commands
run in a sandbox where Tessera's own keys, chats and settings don't exist. On
Windows they run with your account's rights: a command could read Tessera's
settings and keys, and anything else your account can. Tessera says so in
Settings → Privacy & safety and in `tessera setup`. That's why on a Windows
server new chats start in **Ask me** until you choose otherwise: each
command and each change to a file waits for your OK (Allow, Allow for this
session, or Deny). Settings → Privacy & safety changes it for all your
devices (a stricter `agent.permission` in `config.toml` stays). For the
sandbox, run the Tessera server on a Linux computer (a small VM is enough)
and use Windows as a machine.
Everything else stays: secrets reach a command only when it names them,
secret values are removed from the command output the model reads (not from screenshots: keep secrets off the screen while agents look), risky commands ask
first (Windows ones too: deleting folders, changing the registry, services,
scheduled tasks, the firewall or Defender), and catastrophic ones are
refused.

Long commands, background jobs and stopping work as on Linux: a command and
everything it starts is one Windows job, stopped as a whole. Nothing it starts
can leave that job, a command that can't be put in one isn't run, and when
it ends whatever it left running there ends with it (as on Linux).

## This computer as a machine

`tessera node` works on Windows: pair it (`tessera node enable --link …`),
choose what agents may do, keep it on (`tessera node service install`, a
startup program like the server's).

| Preset | On Windows |
|---|---|
| Look only | as on Linux: agents read the folders you choose |
| Work in folders | agents read and change the folders you choose; **no commands** (they need Full access on Windows) |
| Full access | your home folder and commands (web views hosted on a Windows machine aren't available yet). Commands run without a sandbox: they can reach everything your account can, including passwords and keys. This is close to giving the agent your account. |
| Desktop control | Full access, plus your screens, mouse and keyboard while you're signed in (see below). Startup locations stay out of reach of file actions (not of commands, which run as you). |

**Desktop control on Windows**: agents can take screenshots of your screens
and use your mouse and keyboard (click, drag, scroll, type, keyboard
shortcuts) through Tessera's `desktop` tool, in the session you're signed in
to, as you would. Windows keeps two things apart, and Tessera doesn't try to
get around them: the secure screen (the sign-in and lock screen, User Account
Control prompts, Ctrl+Alt+Delete), which agents can't see or use, and
programs running as administrator, which they can't click or type into (the
agent is told, and asks you to do that step). Desktop control stays off while
Tessera itself runs as administrator ("Run as administrator"), or when
Windows won't say at which level it runs: such a Tessera could reach
administrators' windows too. It works while you're signed
in and the machine runs in your session (started at sign-in, or by the
desktop app's This computer); not as a Windows service, nor while the
session is locked or disconnected. Every action goes through "Ask first":
"Every action" asks before each one, "Only risky ones" before screenshots
(also one taken after acting) and any mouse or keyboard input (moving the
pointer and scrolling too), each time
(the machine's own rule: no "Allow for this session" there), and "Never"
leaves it to the chat: YOLO chats don't ask, every other chat asks first
(once per chat with "Allow for this session"; read-only chats may only
look, and a bot with read-only access to this computer may only read files,
not look at the screen). The approval shows exactly what would be typed, Enter and Tab
included (in the terminal a long one scrolls with ↓, and can be allowed only
once all of it was on screen). One chat uses the mouse and keyboard at a time
(another waits until a minute after the last action, and a chat's helpers
count as other chats; looking takes no turn),
Claude Code's own helpers can't use the screen (the main chat can), and Stop
ends an action between two steps, letting go of any key or button it held.
If Windows itself doesn't answer within 10 seconds, the chat says the action
may still be going on: anything it still sends is refused where it arrives,
and no desktop action runs on this computer until it has ended and let go of
what it held (one that never ends keeps the screen out
of agents' reach until Tessera restarts there).

**What is kept where.** What an agent types, and the screenshots it takes,
are part of the chat: the approval shows the text, the chat keeps both like
anything a tool returns (until you delete the chat; a private chat keeps
nothing), and the chat's model gets them. Only this computer's activity log
(`tessera node log`, Activity in the app) and the Stop bar leave them out:
they say what kind of action it was ("type 12 characters", "screenshot"). So
don't have agents type passwords: Passwords & keys hands them to commands
without the model seeing them.

**The Stop bar.** A slim bar at the top of your main screen says which chat
is doing what through this computer's machine ("“Fix the tests”: click
640,400"; a command shows as "a command", never its text), with a **Stop**
button: all the time while desktop control is on, otherwise while agents work
here (and for a minute after); nothing an agent asks for runs here before the
bar shows it. Its Stop key (Ctrl+Alt+Esc, or Ctrl+Alt+Pause
or Ctrl+Alt+Shift+S when another program holds it) works from other programs
whenever this computer is on as a machine. Clicking anywhere on the bar, or
the key, stops every agent on this computer:

- clicks, drags and typing end before their next step, and a drag's mouse
  button is let go at once;
- commands and web views here are told to end, and so are the chats of a
  Tessera server on this computer (its tools don't run while stopped; the bar
  doesn't list that server's own work, the app does). They look once a
  second, so this normally takes a second or two; something already in the
  middle of a slow step (a large file being written, a program that ignores
  being stopped) can take longer, and is reported as such;
- nothing runs until **you** resume it here: Resume on the bar with your own
  mouse, This computer in the app, or `tessera node resume`; each asks for
  your approval in Windows' administrator prompt (Tessera Guard). It stays
  stopped across restarts and updates (as long as Windows lets the guard save
  it: the registry, or failing that a file in the guard's folder). With this
  version of Tessera's server,
  actions asked for before the Stop, and turns of chats that began before it
  (with their helpers and the bots they asked), don't do anything on this
  computer after the Resume, whichever server they run on. Messages admitted
  before Stop stay in the queue for you, also across restart: Resume or a
  later new message does not send them automatically. Send them again, or
  explicitly choose Send now on the queued message. Each queued message keeps
  its own admission mark; a newer send cannot authorize older queued work.
  Every chat, YOLO ones too, asks again before it uses the screen.

Agents can't resume it: with Tessera Guard (below) only a Resume you approve
in Windows' administrator prompt undoes a Stop (see "What it can't do" for
what still runs as you); a stopped machine does nothing, the bar ignores
clicks programs send, and commands that would resume or widen the
machine (`tessera node resume`, `preset`, `scope`…) are refused. Tessera's own
windows (the bar, the app's approvals and settings) refuse the clicks and key
presses its agents send: checked right before each click and every few keys,
and again where the input arrives (Windows can't promise where a click lands
if a window moves in between, so both; and since Windows can quietly take that
guard away, each action first checks it still works and sets it up again if
not). Without a working bar and its key
(Windows refused them, the bar stopped answering, or that guard couldn't be
set up), nothing runs on this computer: desktop actions, commands, file
actions and web views are refused, and what was running is ended. You must
always be able to see and stop agents. If it says the bar isn't working,
turn this computer off and on again as a machine (This computer in the app)
or restart Tessera.

What it can't do: take back what already happened (a click Windows already
took, a program an agent started that runs on by itself); be seen over a
full-screen game, the lock screen or Windows security prompts (the key still
works over games); stop agents that run commands here some other way than
Tessera. With Full access, commands run as you, and so do programs an agent
starts through the screen. Tessera Guard (below) keeps the Stop out of their
reach and ends everything Tessera's machine started, but a program an agent
started some other way (Task Scheduler, through the screen) runs on as one of
yours, and could act as this computer's machine with its key (which your
account can read).

Where a Stop is kept: with Tessera Guard (which commands and desktop control
need), the guard's own record counts: a registry value only it and
administrators can change, or, if Windows refuses that, a file in its
protected folder. If neither takes it, it holds in the guard's memory until
the guard or the computer restarts (the guard tries again each second and its
log says so; meanwhile your server refuses this computer's machine, since the
guard vouches for nothing it hasn't saved). If the guard finds its record missing or unreadable when it
starts, it starts stopped with a new signing key: your server then refuses
this computer's machine until you remove it in Settings → Devices and add it
again. The machine also keeps copies in your account (`stop.json`, a
file next to it, and the registry): they stop this computer until the guard
has the Stop, and none of them can undo it. Without the guard (file actions
and web views only), those copies are the record: a Stop holds if any of them
took it, and one that can't be read counts as stopped; if none takes it, it
holds in memory alone, Tessera tries again each second, and until then only
Resume on the bar itself undoes it (the app and `tessera node resume` can't);
then, or if the bar can't be used (over Remote Desktop, say), turn this
computer off and on again as a machine (This computer in the app): like a
restart, that drops a Stop that was never saved (its log says so). A Resume
that can't be saved resumes nothing (press it again; with the guard it counts
as a new Stop, since Windows may have kept part of it). Over Remote Desktop, Resume on the bar may need
This computer in the app or `tessera node resume` for a saved Stop (Windows
may count remote clicks as sent by a program). The bar covers a little of the
top of the screen; agents can't click there.
**Tessera Guard.** Commands and desktop control on Windows need it. It is a
small Windows service, installed once with an administrator's approval
(Settings → Devices → This computer → Install Tessera Guard, or
`tessera guard install`; Windows asks), that keeps your Stop where your own
programs, an agent's among them, can't undo it without your approval (limits
below):

- it keeps the Stop in a registry key only it and administrators can change,
  from a copy of Tessera in `Program Files\Tessera Guard` that only
  administrators can change;
- anyone may stop; **Resume asks Windows for your approval** (its
  administrator prompt, on the secure desktop) for the guard's own program,
  so an agent's program can't resume by itself. Approve it only when you
  pressed Resume;
- this computer's machine is taken in by the guard when it starts: on a Stop
  the guard ends it, with everything started in its job (they can't leave it), after
  two seconds for it to let go of keys and buttons (a Resume within those
  seconds doesn't save what was running); and when the machine
  ends for any reason, whatever it started ends too. The machine starts again
  by itself, stopped, and lets go of keys left down;
- nothing runs on the machine while the guard doesn't answer, or before it
  has taken the machine in (what ran before that is ended); a Stop reaches
  the guard even if its pipe is crowded (a counter anyone may raise, under a
  name chosen afresh each time the guard starts), and is saved whole (see
  "Where a Stop is kept" above);
- every program Tessera starts for agents on this computer, the server's
  own too, is taken in by the guard before it runs (without the guard, none
  is started): the user's Stop ends them whatever they do (not what they
  start some other way, see below);
- keys and buttons are let go in your session right after the guard ends a
  machine (a small program of the guard's own, started there as you, in the
  guard's own job; if it fails or hangs, it is ended and the guard's log
  (`guard.log` in its folder under Program Files) says
  so, and the machine also lets go of what it wrote down when it starts
  again). New input isn't held back until it has finished;
- your server pins the guard's key when this machine first reports it (so
  pair a machine yourself, before giving agents anything on it), and before
  every action sent through it asks the guard's signed word that this
  computer isn't stopped (fresh each time). A program that takes this machine's
  place with its key (which your account can read) gets nothing new to do
  once the guard has your Stop (what it was already doing keeps going if
  that program was started outside Tessera). A machine that later reports another guard key (a
  reinstall) gets nothing to do until you remove it in Settings → Devices
  and add it again. A server older than this version doesn't ask the guard:
  this computer's machine still refuses everything while stopped, but such a
  server can't tell a program that took the machine's place; update it.

It needs User Account Control on, with its prompt on the secure desktop.
If your account is an administrator, set User Account Control to "Always
notify" (Control Panel → User Accounts → Change User Account Control
settings): at Windows' default level some of Windows' own programs raise
themselves without asking, which other programs can use to approve
themselves. This computer says so if it isn't, and refuses commands and
desktop control until it is. Microsoft doesn't treat User Account Control as
a security boundary: this makes undoing your Stop take your approval, not
impossible for every program on earth. Install the guard before you give
agents Full access or desktop control (they can't have them before), so no
agent's program could have changed the Tessera you approve. Remove it with
`tessera guard uninstall` (Windows asks again). The app's uninstaller offers
separate Guard removal, with **No** selected: accepting calls the protected
Guard copy and asks Windows for an administrator, declining keeps it for any
other Tessera installation. Silent uninstall keeps it. Stopping or updating
Guard ends the machine (it starts again
by itself). One guard serves the whole computer: anyone signed in may stop
it, and resuming needs an administrator's approval. Between Stops the
machine runs as you, so a program of yours could change what it does (the
guard still ends it when you stop, and your server still asks the guard
before each action); a program already running as administrator (a window
left open) could resume, so close those. Programs an agent starts some other
way than through Tessera (through the screen with Explorer or Run, Task
Scheduler, WMI) run on as your own after a Stop: the guard can't tell them
from yours. Closing that needs agents' commands to run as a separate, more
limited Windows identity (a command sandbox, not built yet); with desktop
control, what an agent starts through the screen is yours by nature. User
Account Control has to be at "Always notify" for an administrator account:
Tessera never changes it for you, it says so and refuses commands and
desktop control until you do.

Turn it on in the desktop app (Settings → Devices → This computer → Desktop
control) or with `tessera node preset desktop`; turn it off the same way.
Chats on another server need that server updated too (it brings the `desktop`
tool).

File actions (read, list, write, edit) check every path on Windows too:
Tessera's own data is never read or changed, nor its program folder changed;
other programs' credentials (the Windows credential store and DPAPI keys,
browser profiles, `.ssh`, cloud sign-ins…) stay out of reach unless you allow
that very folder; startup locations (the Startup folder, PowerShell profiles)
can be read but never changed. Commands, which run as you, aren't held to any
of this. Paths can be written `C:\Users\you\projects`,
`C:/Users/you/projects` or `~\projects`.

Windows machines can host web views when **Web views here** is turned on
on that computer. New nodes report `webviews` in their `features` list; older Windows
nodes do not offer it. Edge or Chrome is found just like the server's browser.
Its profile, temporary files and downloads belong to Tessera, never to your
usual browser. Download destinations and local-file access are checked by
the same browser-command rules as Linux. On Windows these browser folders
use an owner-only, protected permission list (DACL), including folders that
already exist. If Tessera cannot set and verify it, the browser does not start.
The browser's pipe queues have both count and byte limits; a blocked or
flooded connection stops its browser work rather than growing without bound.

**Windows differs from Linux:** Chromium's renderer sandbox stays on. The
browser and its private pipe relay start suspended in a mandatory Windows
job, with no permission to leave it; everything ends on disconnect, turning
web views off, or the node ending. A job also caps memory. No debugging port
opens. Unlike Linux's Landlock, this does **not** restrict the browser's own
main process to its folder: a compromised browser main process still has
your account's file access. An AppContainer or restricted token for that
process has not been validated and is not silently enabled. The native
Windows launch needs runtime acceptance; “Show on this computer's screen”
still requires Unix.

## Web views

The server's shared browser uses Google Chrome, Microsoft Edge (part of
Windows 10 and 11), Brave or Chromium, whichever it finds first, driven only
through private pipes (never a network port), as on Linux. Nothing to
install.


## Interactive terminal tiles

Terminal tiles are implemented on Windows 10 **1809 or newer** and Windows 11,
on the server and on a Windows machine (native runtime acceptance is pending).
They use Windows' built-in ConPTY, so no
terminal package or extra download is needed. Tessera chooses PowerShell 7
(`pwsh.exe`) from PATH, then Windows PowerShell, then cmd. Input and output
use UTF-8 and run as the account that started Tessera, without requesting
elevation or switching accounts.

Terminals start **off**. On the server, use its terminal settings to choose
Full access. On a machine, enable **Terminal from my devices → Full access**
on that computer in Settings → Devices → This computer, or run
`tessera node scope --terminal full`. Another device or an agent cannot
turn machine terminals on. **Windows terminals are not sandboxed**: they
have that account's full file access. Sandboxed is not offered or accepted,
even if the command sandbox supports other kinds of commands.

Only a paired app over an end-to-end encrypted connection (or the server's
local connection) can open them. Each belongs to its opening device;
models, chats, tools, schedules and machine logins have no terminal API.
Typing and output are not recorded or sent to models. The record keeps
only who opened it, where, when and why it ended. Service secrets are not
copied into the shell's environment, and service handles are not inherited.

As on Linux: four open at once per device, bounded paste/output queues,
idle and reconnect limits, replay and a whole-process-tree close. The shell
starts suspended and must join a kill-on-close Windows Job before it can
run. Closing, signing out, disabling terminals, pausing a machine or ending
its connection closes its programs too. An older Windows node does not
advertise terminal support and stays unavailable until that node is updated.

ConPTY cannot reliably tell a prompt from an in-process PowerShell command.
Windows therefore uses the longer running-idle bound (8 hours without typing),
not the shorter prompt-idle bound. A long cmdlet such as `Start-Sleep` is not
mistaken for an idle prompt.

Native Windows terminal runtime acceptance is still required for this
implementation; cross-compilation and Linux tests alone do not prove
interactive Windows behavior.

## The desktop app

The release's `tessera-desktop-windows-x86_64-setup.exe` installs it for you
only (no administrator rights; the installer from install.ps1 runs silently).
It uses Microsoft Edge WebView2, which Windows 10 and 11 already have; the
installer downloads it if not. "Run on this computer" in the app starts the
server and a machine with no console windows.

Batch 24 adds Windows-machine-hosted web views, native click-to-chat and
withdrawable notifications (while the app is running), and visible whiteboard
tile snapshots for “Ask about this”. These native paths are cross-compiled,
not yet accepted on a real Windows PC. Notifications use the installed
app's Windows identity; older toast history cannot launch a closed app.
The terminal **chat app** (`tessera`) works; Windows server terminal tiles
and desktop control are implemented by the other batch 24 tracks.

## App updates from your server

In the Windows desktop app, open **Settings → Help & tour → App updates**
and **Check for updates**. The server owner first offers a build:

```text
tessera updates add tessera-desktop-windows-x86_64-setup.exe --platform windows-x86_64-desktop --version 0.2.0
```

Use the x64 **NSIS setup**, not the portable app or an MSI. NSIS may use a
32-bit setup stub for its x64 payload; the owner must select the right
payload. The server checks the PE/NSIS structure, size and platform. The
app checks again, downloads over its existing pinned encrypted connection,
and verifies every piece, the exact size and the saved file's SHA-256.
A changed offer/server/login or a truncated/corrupt download is refused;
re-pairing the same saved server during consent/download cannot reuse the
old connection's consent.

**Install update…** requires an extra native Windows confirmation, then
opens the normal interactive setup. Nothing checks, downloads or runs
silently in the background. Setup's existing hooks stop/restart the server
and machine from that installation; chats and settings stay. “Installer
opened” means it started, not that installation finished. A cancelled
setup leaves the old app. The update may close the app; its temporary setup
folder may then remain under your account's temp folder.

This verifies the bytes your server offered, not a publisher signature.
Only accept builds you trust; unsigned-build Windows warnings still apply.
Native installation and file-lock behavior need real Windows acceptance.
The app locally refuses invalid versions and downgrades, before confirmation
and again before running setup; a server's timestamps cannot override that.
Same-version builds need an explicit `+rebuild.<id>` version suffix and a
different known setup hash. A local receipt is saved only if setup finishes
successfully while the app survives. Without that receipt (including when
setup closes the app), same-version updates are refused; install such builds
manually if you need them. Newer valid versions do not need that receipt.

## Uninstall

**The desktop app:** Settings → Apps → Installed apps → Tessera → Uninstall.
It stops the server and this computer's machine and removes them from the
programs Windows starts at sign-in (only those that run from the app's own
folder), then removes the app. Your chats, keys and settings stay (below).

**The PowerShell install:**

```powershell
tessera service uninstall; tessera node service uninstall
Remove-Item -Recurse "$env:LOCALAPPDATA\Programs\Tessera"
```

Then remove that folder from your PATH (Settings → System → About → Advanced
system settings → Environment Variables), the desktop app in Settings → Apps,
and, only if you want your chats and keys gone for good, `%APPDATA%\tessera`.
Both installed? Updates and uninstall leave startup entries from a different
installation alone. The desktop uninstaller preserves another install's
"Tessera" entry even though Tauri's template deletes that value by name.

## Troubleshooting

- **`tessera` isn't found**: open a new terminal window (PATH changes reach
  new windows only), or run `%LOCALAPPDATA%\Programs\Tessera\tessera.exe`.
- **The background service doesn't start**: `tessera service status` names
  the log; the end of it says why. A server you started by hand holds the
  port: `tessera service install --replace`.
- **After an update the app can't reach its server**: opening the app starts
  this PC's own server (the one whose key the app pinned) when it isn't
  running; if that fails, the app says why and where the log is, with
  **Start it again**. The installer notes how the server and this computer's
  machine start at sign-in (each startup entry's whole command line: its data
  folder, its address) before it stops them, and puts exactly those back
  afterwards. If it fails there, or the update was cut short, it says so and
  keeps the notes: the app finishes it when it opens, or run
  `tessera service restore-update`. Each step of the installer has a time
  limit, so it never hangs on a program that doesn't end.
- **The desktop app's "Start Tessera here" seems stuck**: it shows what it is
  doing and for how long; a first start can take a minute (a new database,
  an antivirus looking at new programs). "Stop waiting" stops waiting
  without stopping the server; press "Try again" a moment later to connect
  to it (while the first start is still under way, it waits for that one
  rather than starting Tessera a second time; two servers never run on the
  same data). If it doesn't start, the message names its log
  (`%APPDATA%\tessera\logs\tessera.log`). Before Oct 8 the app could wait
  for ever at "Starting Tessera…" with "start when I sign in" ticked, even
  though the server had started: update the app.
- **Devices can't reach a PC that was left on**: is someone signed in (after
  a restart, Tessera starts at sign-in)? Did it go to sleep (on battery, or
  a laptop's lid)? `powercfg /requests` shows whether tessera.exe keeps it
  awake. At home without a tunnel: the network must be Private, and
  tessera.exe allowed through the firewall for Private networks (above).
- **The server stopped answering and came back a few minutes later**: its
  watchdog restarted it; the `stalls` folder next to your data has what it
  was doing. Please send the newest file there with a bug report.
- **Windows Firewall asks about cloudflared.exe or Chrome**: it shouldn't
  (they only connect out); choose Cancel and tell us.
- **"running scripts is disabled on this system"** when running install.ps1
  as a file: use `powershell -ExecutionPolicy Bypass -File .\install.ps1`
  (that applies to this one run only).
- **Windows blocks the programs** ("Windows protected your PC", or Smart App
  Control on a fresh Windows 11): Tessera's programs aren't code-signed yet.
  SmartScreen offers "More info → Run anyway"; Smart App Control can only be
  turned off as a whole (Windows Security → App & browser control), which is
  your call.
- **Commands fail with bash syntax errors**: the agent runs in PowerShell or
  cmd because Git for Windows isn't installed. Install it and restart
  Tessera, or tell the agent which shell it has.

## Readiness of the Windows preview

Desktop control, Stop and Guard have been cross-compiled and checked on Linux,
not accepted on a real Windows PC. Cross-compilation does **not** establish
Windows runtime behavior. Remaining isolated Windows acceptance includes denied
UAC, locked/elevated windows, startup replay, Stop persistence and same-user
programs launched outside Tessera.

For Claude Code, install Git for Windows and the official, unmodified `claude`
CLI, then sign in using that CLI. Tessera finds custom Git installations from
`git.exe` on PATH as well as the usual folders and supplies Claude's documented
`CLAUDE_CODE_GIT_BASH_PATH` when you have not set it yourself. Choosing PowerShell
as Tessera's command shell does not remove Claude Code's need for Git Bash.

A Windows startup program is **not** a service that runs before sign-in. For
an always-on PC, someone must sign in after each reboot; locking the PC is fine.
Tessera does not turn on automatic login or change Windows account security.

### Why not a task that runs before sign-in?

Windows has a password-free scheduled-task logon, **S4U**, but Microsoft
[documents that it has no access to the network or encrypted files](https://learn.microsoft.com/en-us/windows/win32/api/taskschd/ne-taskschd-task_logon_type).
That is not a validated replacement for a server using providers, tunnels,
network folders and your account's profile. A task using an interactive
token still needs sign-in. Password-based tasks require the password when
registered; this flow does not collect or store your Windows password.

Running as **SYSTEM** avoids a password but gives the agents system-wide
privileges and uses a different account/profile. A low-privilege service
account also needs its own explicitly set-up data, keys and folder access;
it is not the current user's existing server. Tessera does not silently
switch accounts, copy keys, elevate tasks, open firewall rules or disable
Windows security to make boot start appear to work.

Tessera therefore keeps the existing per-user sign-in start. A future
opt-in service needs a real Windows test of least-privilege identity, profile
and network access, ACLs, startup-file protection, locking, and installer
upgrade/uninstall ownership. That has not been established here.

Web views need an installed Chrome, Edge, Brave or Chromium browser. The Windows
installer supplies WebView2 for the app, not a full Chrome/Edge browser. Unlike
the Linux installer, it does not download Chrome for Testing when no browser
is installed. Install a browser from its official vendor if one is missing.
