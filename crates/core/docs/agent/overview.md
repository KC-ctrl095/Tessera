# Tessera overview
Tessera is an agent harness the user runs on their own machine or home server. One **server** (`tessera serve`, often a systemd service) runs the agents and keeps everything; **clients** connect to it end-to-end encrypted: the terminal UI (`tessera`), the desktop app and the Android app.

- **Sessions** ("chats") hold the history. Kinds: chat, job (scheduled run), subagent, channel (a room where bots talk). Private chats live only in memory and have only web tools.
- **Mosaics** are the user's workspaces: tiles (chats, whiteboards, notes) arranged like a tiling window manager, synced across devices. See `docs mosaics`.
- **Whiteboards** show the user visuals, pages, images or web apps in a tile, and collect input such as signatures; you can read them or look at them as the user sees them. See `docs whiteboards`.
- **Notes** are markdown documents the user keeps (a library, like artifacts). See `docs notes`.
- **Bots** are personas with their own instructions, tools and memory. See `docs bots`.
- **Memory and skills** make the agent better over time. See `docs memory`.
- **Secrets** are credentials commands can use without the model seeing them. See `docs secrets`.
- **Scheduled jobs** run prompts on a schedule. See `docs scheduling`.
- **Small model mode** (models on the user's own computer, small models): fewer tools with simpler arguments and a shorter prompt; `docs tools` turns the other tools on. See `docs tools`.
- **Machines** are the user's other computers turned on for Tessera on that computer (`tessera node`); agents use them with the `machines` tool, within what each allows. See `docs machines`.
- **Devices** pair with a one-time code. A phone scans the QR code from `tessera pair` (or an app's Settings → Devices → Get a code); a computer pastes the pairing link instead (desktop app: On another computer → Connect to a server). Both carry the server's addresses, any Access headers and its key, which the app checks. Reaching it from anywhere: setup's "Anywhere" step or Settings → Connection — automatic Cloudflare (needs a domain on Cloudflare; a cheap one can be bought there), Tailscale (free, no domain) or the user's own proxy; to walk the user through it, see `docs remote-access`.

Where things live (Linux defaults): settings in `~/.config/tessera/` (`config.toml`, `skills/`), data in `~/.local/share/tessera/` (database, `auth.json`, memory). `TESSERA_HOME` puts both in one directory. Agents cannot read Tessera's own data or settings directories (the sandbox hides them and file tools refuse them); the skills directory and attached files are the exceptions. Change Tessera's settings with the `settings` tool (the user approves each change); security settings are the user's (`docs security`).
