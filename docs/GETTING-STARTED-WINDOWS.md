# Getting started on Windows

This guide walks you through running Tessera on your Windows PC and using it
from your phone. You don't need to know anything technical: each step says
what you'll see and what to click. It takes about 15 minutes.

**What you need**

- A Windows 10 (1809 or newer) or 11 PC that you can leave switched on.
- An ARM64 phone running Android 7.0 or newer. Download `tessera-android-arm64.apk` and `SHA256SUMS` from the same trusted release. Open the APK in your browser or Files app and allow that app to install it if asked. Turn that permission off afterwards. Public downloads are not published yet.
- An AI account: a ChatGPT Plus or Pro plan is the easiest. A Claude Pro or
  Max plan works too (step 3 says how).

## 1. Install Tessera

1. Download `tessera-desktop-windows-x86_64-setup.exe` from the project's release page. If someone gave you a differently named copy, confirm where it came from before opening it. Public downloads are not published yet.
2. Open the installer. If Windows shows **Windows protected your PC**, stop and check the source and publisher. Only choose **More info → Run anyway** if you trust that download. A warning is not proof that a program is safe.
3. Click **Next** or **Install** until it is done. The main app installs for your Windows account without an administrator password.
   The installer includes the server and service helper. You do not need Rust, Node or a terminal.
4. The installer may offer **Tessera Guard**. This optional Windows service needs administrator approval and keeps agent commands and desktop control tied to your Stop button. Choose **No** if you only want to chat for now. Installing Guard does not turn agent access on. You can add it later in **Settings → Devices → This computer**.
5. Choose **Finish**. If Tessera does not open, find **Tessera** in the Start menu.

If Windows says **Smart App Control blocked an app**, do not turn off that protection to get Tessera running. Ask the publisher for a supported, trusted installer. The [release checklist](RELEASING.md) tracks launch downloads and signing.

## 2. Start Tessera on this PC

Tessera asks **"Where should Tessera run?"**

1. Under **On this computer**, leave **Keep Tessera running: start it when I
   sign in to Windows** ticked. This keeps Tessera running in the background,
   even with its window closed, and stops the PC from going to sleep while
   it's plugged in, so your phone can always reach it. The screen still
   turns off as usual.
2. Click **Start Tessera here**. After a few seconds the setup continues by
   itself.

You can change this later: Settings → Devices → this PC → **Keep Tessera
running**.

## 3. Choose your AI

The setup asks **"How should Tessera get its AI?"**

**ChatGPT (easiest):** next to **ChatGPT · your Plus or Pro plan**, click
**Sign in**. A box **Sign in with ChatGPT** shows a short code. Click **Open
sign-in page**: your web browser opens. Sign in to ChatGPT there and type
the code when it asks for it. Back in Tessera the box says **Signed in**.
Under **Model for new chats**, pick a model (the one already chosen is
fine), then click **Continue**.

Until you connect an account or model server, Tessera says **No AI is connected yet** and waits. A model name in a signed-out account is only a preview, not a ready AI.

**Claude (if you pay for Claude Pro or Max):** Tessera uses your plan through
Anthropic's own program, Claude Code, which you install once:

1. Install **Git for Windows** from <https://git-scm.com/download/win>
   (click through its installer, the suggested choices are fine).
2. Open the Start menu, type **PowerShell** and open it. Paste this line and
   press Enter: `irm https://claude.ai/install.ps1 | iex`
3. When it's done, type `claude` and press Enter, then follow what it says
   to sign in. Close PowerShell afterwards.
4. Restart the PC and sign in again, so Tessera finds the new programs. Open
   Tessera: if the setup isn't showing, open it with Settings → Help & tour →
   **Run setup again**.
5. Click **Check again** next to Claude. It says **Claude is ready**. Pick a
   "Claude (your subscription)" model, then **Continue**.

## 4. Where you'll use it

The setup asks **"Where you'll use it"**.

**At home (same Wi-Fi):** turn on **Let other devices on my network
connect**, then click **Restart now**. Windows may show a box: **"Windows
Security: Windows Defender Firewall has blocked some features of this
app"**. Tick **Private networks**, then click **Allow** (Windows may ask for
your PC password). If you clicked Cancel by mistake, see "Something isn't
working" below.

**Away from home too (optional):** Tessera can set up a private, encrypted
link through Cloudflare, a free service. You need a Cloudflare account, its
free **Zero Trust** turned on (Tessera tells you if it isn't, with a link),
and your own web address (a domain, about $10 a year, which you can buy right
in Cloudflare). Click **Set it up with Cloudflare** and follow the steps; the
guide **Reach Tessera from anywhere** (Settings → Help & tour) explains each
one. You can also skip this now and do it later in Settings → Connection:
your phone picks up the new address by itself (open Tessera on it once while
it's at home), with no need to pair it again.

Click **Continue**.

## 5. Pair your phone

The setup shows a square code (a QR code).

1. Open the Tessera app on your phone.
2. Tap **Scan QR code** and point the camera at the code on your PC.
3. The phone connects. Done: your chats show up on both.

To add another phone or tablet later: Settings → Devices.

## 6. You're set

The last step lists what keeps you safe. Click **Start using Tessera**.

## Staying safe

On Windows, Tessera can't fence off the commands the AI runs: they run with
your own rights, like a program you started yourself. That's why new chats
start in **Ask me**: before the AI changes a file or runs a command, a card
appears in the chat showing exactly what it wants to do, with three
buttons:

- **Allow**: do it this once.
- **Allow for this session**: do this kind of thing in this chat without
  asking again.
- **Deny**: don't do it. The AI will try another way or ask you.

If you don't understand what a command does, click **Deny** and ask the AI
to explain it first. You can change how much the AI may do on its own in
Settings → Privacy & safety, but "Ask me" is the safe choice on Windows.

Your passwords and keys stay on your PC, everything between your phone and
your PC is encrypted, and only devices you paired can get in.

## Keeping it running

- Leave the PC on and plugged in. You can close the Tessera window: it keeps
  running in the background.
- Tessera starts by itself when you sign in to Windows. After a restart,
  sign in once (you can lock the PC again right away). To turn that on or
  off: Settings → Devices → this PC → **Keep Tessera running**.
- With remote access through Cloudflare, Tessera reminds you once a year to
  renew its key (a notification, and a line in Settings → Connection with a
  **Renew** button). It takes a minute; your phone needs nothing.
- **Windows updates** restart the PC at night. So that Tessera comes back by
  itself: Settings → Accounts → Sign-in options → turn on **Use my sign-in
  info to automatically finish setting up after an update**.
- **Laptops**: on battery, Windows lets the PC sleep as usual to save power,
  and closing the lid puts it to sleep. Keep it plugged in and open (or set
  "When I close the lid" to "Do nothing" in Control Panel → Power Options).
- If something goes wrong inside Tessera, it restarts itself within a few
  minutes.

## Something isn't working

**My phone can't connect at home.**
Is the PC on and signed in? Is the phone on the same Wi-Fi? Is **Let other
devices on my network connect** on (Settings → Connection)? Then check two
things in Windows:

1. Windows Settings → Network & internet → your Wi-Fi (or Ethernet) →
   **Network profile type** must be **Private network**.
2. Open the Start menu, type **Allow an app through Windows Firewall** and
   open it. Click **Change settings**, find **tessera.exe** in the list, tick
   **Private**, and click **OK**. (If you clicked Cancel in the firewall box
   earlier, this is how to undo it.)

**My phone can't connect away from home.**
Remote access isn't set up yet (step 4), the phone hasn't connected since it
was (open Tessera on it once at home), or the PC is asleep or off. Settings →
Connection shows whether the Cloudflare link is connected, and whether its
key needs renewing.

**The Tessera window says it can't connect.**
Restart the PC and sign in. If it still doesn't work, the reason is written
at the end of the file `%APPDATA%\tessera\logs\tessera.log` (paste that
into the Windows Run box, Windows key + R, and press Enter to open it), and
send it to the person who gave you Tessera.

## Removing Tessera

Windows Settings → Apps → Installed apps → **Tessera** → **Uninstall**. This
stops Tessera and removes it from the programs that start with Windows. Your
chats and settings stay in `%APPDATA%\tessera` in case you install it again;
delete that folder to remove them for good.
