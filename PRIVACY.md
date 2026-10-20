# Privacy

*Last updated: 20 October 2026*

**In short.** Tessera is software you run on your own computer. The people
who make Tessera don't run a service, don't have an account for you, and
receive none of your data: Tessera has no telemetry, no analytics and no
crash reporting. Your chats, memory, notes, files and passwords stay on your
server. They leave it only to go to the services *you* connect: mainly the AI
models you choose, which see what you and your agents send them.

This page explains what is kept where, what goes to whom, and how to delete
it.

## What Tessera keeps, and where

Everything is kept by your Tessera server, on its own disk:

| What | Where |
|---|---|
| Chats, their files and pictures, notes, whiteboards, scheduled jobs, bots, usage and costs | the data folder (`tessera.db` and folders beside it) |
| Memory (what agents learned about you and your projects) | the data folder (`memory/`) |
| Saved passwords and keys, sign-ins to AI providers, your devices' keys, the server's own key | the data folder (`auth.json`, `identity.key`) |
| Web views' browsing data (cookies, sites you signed in to) | the data folder (`browser/`) |
| Settings and skills | the settings folder (`config.toml`, `skills/`) |
| Voice recordings, only if you turn on **Keep recordings** | the data folder (`voice/recordings`) |
| Logs (what the server did, and errors) | the system log (Linux: `journalctl --user -u tessera`), or `logs\` in the data folder on Windows |

The folders: on Linux `~/.local/share/tessera` (data) and
`~/.config/tessera` (settings); on Windows `%APPDATA%\tessera` (both); or
the folder `TESSERA_HOME` names. `tessera doctor` shows the paths.

- **Only your account can read them** (and, as always, the computer's
  administrators). Tessera creates these files readable by the user it runs
  as and nobody else. It doesn't encrypt them itself: on a computer others
  can get at, turn on disk encryption (BitLocker, FileVault, LUKS). Backups
  of the data folder contain your saved passwords in readable form, so
  exclude the folder or encrypt the backups.
- **Agents are kept out of them.** File tools refuse Tessera's own folders,
  and with the command sandbox on (Linux) agents' commands don't see them at
  all; without it (on Windows, or if you turn it off) a command could read
  them ([docs/SECURITY.md](docs/SECURITY.md)). Either way, models never see
  the values of your saved passwords and keys.
- **Private chats** are kept in memory only and never written to disk.
- **Logs** are for finding problems. Tessera keeps passwords and keys out of
  them, but they can contain things like chat titles, file names, commands
  and web addresses.
- **Your devices** (the apps) keep the server's address, their own key, your
  display settings and unsent drafts. The chats themselves stay on the
  server.
- **Your other computers** (machines) keep a log of every action agents took
  there, on that computer (`tessera node log`).

## What leaves your server, and where it goes

Nothing goes to the makers of Tessera. Data leaves your server only for the
services below, and only when you set them up or use the feature.

**The AI models you choose.** To answer, a model has to read the
conversation. Tessera sends the model provider your messages, the files and
pictures you attach, what the agent reads with its tools (file contents,
command output, web pages, notes and memory it looks up; when it asks to
look, a picture of a whiteboard tile on your screen, which each device can
turn off in Settings → Privacy & safety), and the instructions Tessera
adds. It removes the values of your saved passwords and keys first, and
anything that looks like a key. What the provider then does
with it is set by its own privacy policy and your account settings there
(for example whether it may train on your data, and how long it keeps it).
Models you run yourself (llama.cpp, Ollama, LM Studio) keep everything on
your own computers.

**Voice, if you use it.** With voice set to **On my server** or **On this
device** (where the device recognizes speech locally), your voice stays on
your own hardware. With **OpenAI (your key)** or another transcription
service you choose, the recording is sent to that service to be turned into
text, and spoken answers are made by sending the reply's text to it. Your
voice is not kept unless you turn on **Keep recordings**.

**Claude Code.** If you use your Claude plan, Tessera runs your own Claude
Code on the server, and Claude Code sends the conversation to Anthropic
under your account, under Anthropic's privacy policy and Claude Code's own
settings.

**Web search and websites.** A search sends your agent's search words to the
search provider you set up (your SearXNG, Brave or Tavily), or, when none is
set up or they fail, to DuckDuckGo. When an agent fetches a page or uses a
web view, the website sees your server's internet address and whatever the
agent sends it, as with any browser.

**Cloudflare, if you turn on remote access through Cloudflare.** Your
devices then reach the server through Cloudflare's network. The connection
between your devices and your server is end-to-end encrypted, so Cloudflare
carries it without being able to read it. Cloudflare does see that a
connection happens: internet addresses, times, your Tessera address and how
much data passes. While "Use Tessera in a web browser" is on, the page a
browser loads (Tessera's app, not your data) also comes through Cloudflare.
The Cloudflare API token you paste during setup is used once and not kept.
See [docs/REMOTE_ACCESS.md](docs/REMOTE_ACCESS.md).

**Tailscale or your own proxy, if you use them,** carry the same encrypted
connection, under their own policies.

**Downloads.** The installer downloads Tessera from its release page (and,
on Linux if you agree, Google Chrome for Testing from Google), and setting up
Cloudflare downloads `cloudflared` from Cloudflare's releases on GitHub.
These sites see your address and what was downloaded. Tessera doesn't check
for updates by itself.

**The web browser Tessera uses for web views** (Chrome for Testing, Chrome,
Chromium or Microsoft Edge) is its maker's program. Tessera starts it with
sync and several background features off, but the browser can still contact
its maker, for example for security updates, under the maker's privacy
policy.

**On Android, scanning a pairing QR code** uses Google's ML Kit through
Google Play services. Google says ML Kit sends it information about the
device and app and how the feature performed (for diagnostics and usage
statistics), not the picture or the code. To avoid it, pair with a pairing
link instead of scanning. On Windows, the desktop app draws its window with
Microsoft's WebView2, under Windows' own diagnostic settings.

**Your home network.** When you run setup or ask the apps to look for AI
models, Tessera checks this computer and the other computers on your home
network for model servers, on four common ports. That traffic stays on your
network.

**Notifications** reach your phone over your own connection to your server.
Tessera doesn't use Google's or Apple's push services.

**People you share with.** Bots, channels, shared files and pairing other
people's devices are yours to set up; whoever you let in sees what you share
with them.

## Deleting your data

- **A chat**: delete it in the app (its menu → Delete…). It is removed from
  the database right away. As with most databases, pieces of it can remain in
  unused space inside the file until that space is reused.
- **Memory and skills**: Settings → Memory & skills. **Saved passwords and
  keys**: Settings → Privacy & safety → Passwords & keys. **Voice
  recordings** (if you kept them): Settings → Voice.
- **A device**: Settings → Devices, sign it out. It can't connect any more.
- **Remote access**: Settings → Connection → Remove remote access (deletes
  the tunnel, its address and its Access settings from your Cloudflare
  account).
- **Everything**: stop and remove the background service
  (`tessera service uninstall`; on a machine also
  `tessera node service uninstall`), then delete the data and settings
  folders above. On a phone, uninstall the app. [docs/WINDOWS.md](docs/WINDOWS.md)
  has the Windows steps.
- **Data you already sent to an AI provider or another service** is kept by
  that service under its own rules. Ask that service to delete it.

## Running Tessera for other people

If other people use your Tessera (family, friends, a team), their chats and
data are kept on your server, and you decide what happens to them. In many
places that makes you responsible for that data under privacy law. Tell them
what is kept and which AI services see it, and delete their data when they
ask.

## Children

Tessera isn't made for children. Most AI services require their users to be
adults; follow the age limits of the services you connect.

## Contributing

If you contribute to Tessera, your name and email address in your commits
and sign-offs become part of the public project history (see
[CONTRIBUTING.md](CONTRIBUTING.md)).

## Changes and contact

We update this page when Tessera changes what it sends or keeps; the date at
the top says when it last changed. Questions: open an issue at
https://github.com/KC-ctrl095/Tessera/issues.
