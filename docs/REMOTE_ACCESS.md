# Reach Tessera from anywhere

At home, your phone and laptop reach Tessera over your Wi-Fi. Away from home
(on mobile data, at work, in a café) they can't: your Tessera server is
hidden inside your home network, and nothing on the internet leads to it.
**Remote access** gives it an address on the internet, such as
`https://tessera.example.com`, that your devices can use from anywhere.

It stays private. Only devices you paired can get in, and everything between
them and your server is end-to-end encrypted, so whatever carries the
traffic (Cloudflare, Tailscale, a proxy) can't read it.

You don't need remote access if you only use Tessera on the computer it runs
on, or only at home.

> **Prefer to be walked through it?** Ask in any Tessera chat: "help me set
> up remote access". The agent knows these steps and goes through them with
> you one at a time.

## Which way to choose

| | **Cloudflare** (recommended) | **Tailscale** | **Your own proxy or VPN** |
|---|---|---|---|
| Cost | Free, plus a domain name (about $10 a year) | Free | Whatever yours costs |
| You need | A free Cloudflare account and a domain | The Tailscale app on the server and on every device | To know how to run one |
| On your phone | Nothing extra: scan the code | Install Tailscale and keep it on | Depends |
| Ports to open on your router | None | None | Usually |
| Use Tessera in any web browser | Yes, if you turn it on | Only on devices with Tailscale | Yes, if you turn it on |

Most people should choose **Cloudflare**: Tessera sets everything up by
itself from one key you create, and phones need nothing extra. If you don't
want to buy a domain, choose **Tailscale**.

## Words you'll meet

- **Cloudflare**: a large internet company. Its free plan can carry traffic
  from the internet to a computer at home, without opening your router.
- **Domain**: a name on the internet that you rent by the year, like
  `example.com`. Tessera gets an address under it, like
  `tessera.example.com`.
- **Tunnel**: a connection your server opens *to* Cloudflare. Visitors
  reach your server through it, so nothing at home has to accept
  connections from the internet. The small program that keeps it open is
  called **cloudflared**; Tessera downloads and runs it for you.
- **API token**: a key that lets a program change things in your Cloudflare
  account. You create one for Tessera; it uses it once and forgets it.
- **Cloudflare Access**: a door in front of your address. Tessera sets it to
  let through only your paired Tessera apps, so strangers who find the
  address don't even reach your server.
- **Zero Trust**: the part of Cloudflare where Access lives. It has a free
  plan.

## Set it up with Cloudflare

It takes about 15 minutes the first time. You need a computer with a web
browser, and a payment card if you buy a domain (Cloudflare also asks for one
for the free Zero Trust plan, without charging it).

### Step 1. Create a Cloudflare account

Go to <https://dash.cloudflare.com/sign-up>, enter your email address and a
password, and confirm your email address from the message Cloudflare sends.
The free plan is all you need. Turning on two-step sign-in (My Profile →
Authentication) is a good idea: this account will guard your Tessera's door.

### Step 2. Get a domain on Cloudflare

**If you don't have a domain**, buy one from Cloudflare, which sells them at
cost (a `.com` is about $10 a year):

1. In the Cloudflare dashboard, open **Domain registration → Register
   domains** (or go to
   <https://dash.cloudflare.com/?to=/:account/registrar/register>).
2. Type a name you like and select **Search**.
3. Pick one that is available and select **Purchase**.
4. Fill in your contact details and payment, and select **Complete
   purchase**. Cloudflare emails you to confirm your address: do that, or the
   domain stops working after a while.

The domain is on Cloudflare straight away; there's nothing else to set up.

**If you already own a domain** elsewhere, add it to Cloudflare: in the
dashboard select **Add a domain** (or **Onboard a domain**), enter it, choose
the Free plan, and follow the steps. At the end Cloudflare shows two
*nameservers*; enter them at the company where you bought the domain, in
place of the ones there. It is ready when Cloudflare says the domain is
**Active** (often within an hour, sometimes up to a day).

### Step 3. Turn on Zero Trust (free)

Tessera puts Cloudflare Access in front of your address, and Access is part
of Zero Trust, which has to be turned on once per account:

1. In the Cloudflare dashboard, select **Zero Trust** in the menu on the left.
2. Choose a **team name** (any name; it isn't shown to anyone).
3. Choose the **Free** plan and complete the checkout. Cloudflare asks for
   payment details even for the free plan but doesn't charge you.

If your account already has Zero Trust, skip this step. If you forget it,
Tessera notices when you check the token in step 5 and says **Turn on Zero
Trust first**, with a link; turn it on, then select **Check again**.

### Step 4. Create the API token for Tessera

1. Open <https://dash.cloudflare.com/profile/api-tokens> (or: your profile
   icon at the top right → **My Profile** → **API Tokens**).
2. Select **Create Token**, then, at **Create Custom Token**, select **Get
   started**.
3. **Token name**: `Tessera`.
4. **Permissions**: add these five rows (select **+ Add more** for each new
   row). In each row pick the left box, then the middle one, then the right
   one:

   | | | |
   |---|---|---|
   | Account | Cloudflare Tunnel | Edit |
   | Account | Access: Apps and Policies | Edit |
   | Account | Access: Service Tokens | Edit |
   | Zone | DNS | Edit |
   | Zone | Zone | Read |

   (In Tessera's setup screen, **Copy the list** copies these rows.)
5. **Account Resources**: Include → your account.
6. **Zone Resources**: Include → Specific zone → the domain you'll use.
7. **TTL** (optional, recommended): set the end date to tomorrow, so the
   token stops working by itself.
8. Select **Continue to summary**, then **Create Token**.
9. Select **Copy** next to the token. Cloudflare shows it only this once.

The token can only change tunnels, Access and DNS for that one domain, and
Tessera never stores it.

### Step 5. Give the token to Tessera

**In the Tessera app** (desktop, phone or browser):

1. Open **Settings → Connection**. Under **Away from home**, select **Set it
   up with Cloudflare**.
2. Paste the token into **Cloudflare API token** and select **Check token**.
3. Pick your domain, and keep the suggested address (`tessera.` and your
   domain) or type another name under your domain.
4. Select **Set up remote access**. You see each step being done: the
   tunnel, the address, the Access door, downloading cloudflared, and waiting
   for the tunnel to connect.
5. When it says **Remote access is ready** and **Cloudflare tunnel
   connected**, you're done.

**In a terminal** on the server: run `tessera setup devices`, choose **At
home and away**, then **Cloudflare Tunnel (automatic)**, paste the token and
pick the domain. The terminal setup waits for the tunnel the same way.

**On Windows**, the steps are the same; [Tessera on Windows](WINDOWS.md)
covers what is specific to Windows, such as keeping the computer awake.

### Step 6. Delete the token (recommended)

Tessera has already forgotten it. In Cloudflare, go back to **My Profile →
API Tokens**, open the menu (⋯) next to **Tessera** and select **Delete**.
If you set a TTL in step 4, it also expires by itself.

### Step 7. Your devices pick it up by themselves

There's nothing to pair again. Phones and computers you paired **before**
setting up remote access learn the new address, and the key for the Access
door, from your server over their own encrypted connection: open Tessera on
each one **once while it's at home** (or anywhere it can still reach the
server). In **Settings → Connection** on that device, the server then lists
your `https://` address with a cloud icon. Devices you pair from now on get
it in their pairing code.

The same happens whenever this changes later: if you remove remote access,
change the address, or renew the key, your devices follow by themselves.
Addresses and headers you entered yourself stay yours: the server doesn't
reorder them or overwrite pasted keys. If you pasted an Access key yourself
and replace it in Cloudflare, update that pasted key in Settings → Connection
on the device too. Older apps didn't record whose headers they had saved,
so those existing keys are kept rather than deleted or replaced by guesswork.
(If you do pair a device again anyway, the app replaces its entry instead of
listing the server twice, and the old entry leaves Settings → Devices.)

### Step 8. Test it from your phone on mobile data

1. Turn **Wi-Fi off** on your phone, so it uses mobile data.
2. Open the Tessera app. It remembers the address that last worked and
   checks the other addresses alongside it, so an unreachable home address
   doesn't hold up the internet one.
3. Send a message in a chat. If it answers, remote access works.
4. To see which way it connected: **Settings → Connection**. The address
   marked **in use** should be your `https://` address, with a cloud icon
   (home addresses have a house icon).
5. Turn Wi-Fi back on.

If it doesn't connect, see [When something doesn't work](#when-something-doesnt-work).

### What Tessera created in your Cloudflare account

So nothing there surprises you later (don't change or delete these by hand;
use **Remove remote access** below):

- a **tunnel** named `tessera-…` (Zero Trust → Networks → Tunnels);
- a **DNS record** for your Tessera address, marked "Tessera tunnel";
- an **Access application** named `Tessera (your address)`, which lets in
  only the service token below;
- a **service token** named `Tessera apps (your address)`: the key your
  devices carry. It is valid for **one year**, and Tessera reminds you to
  renew it (see [Renewing the key](#renewing-the-key-once-a-year));
- if you turn on **Use Tessera in a web browser**: a second Access
  application for `your address/web`.

### Is this safe?

- **Nothing at home is opened.** The tunnel is a connection your server makes
  *out* to Cloudflare; your router keeps refusing connections from outside.
- **Strangers stop at Cloudflare's door.** Only requests carrying your
  devices' key get past Cloudflare Access; everyone else sees a Cloudflare
  page and never reaches your server.
- **Your devices still have to be paired**, and they check your server's
  key, so nobody can pretend to be your server.
- **Cloudflare can't read your chats.** Your devices and your server encrypt
  everything end to end; Cloudflare carries data it can't decrypt. It does
  see that connections happen, when, from which internet addresses, and how
  much data passes ([PRIVACY.md](../PRIVACY.md)).
- **The API token isn't kept**, so a stolen server can't change your
  Cloudflare account through it.

## Turn remote access off

**Cloudflare:**

1. Create a token as in [step 4](#step-4-create-the-api-token-for-tessera)
   (the same five permissions). Removing needs one too, because Tessera
   didn't keep the first.
2. In **Settings → Connection**, select **Remove remote access**, paste the
   token and select **Remove**.

Tessera stops the tunnel and deletes the tunnel, the DNS record, the Access
applications and the service token from your Cloudflare account. Your devices
then work at home only (they forget the address and key by themselves). Delete the token afterwards. (In a terminal:
`tessera setup devices` → **Remove the Cloudflare tunnel**.) Your domain and
Cloudflare account stay; cancel them in Cloudflare if you no longer want them.

**Tailscale or your own address:** in **Settings → Connection**, choose **I
use Tailscale or my own tunnel**, remove the address with its ✕ and select
**Save** (or run `tessera remote --clear`). Your devices forget it by
themselves the next time they connect.

## Tailscale (free, no domain)

Tailscale joins your devices into a private network of their own, wherever
they are. Only devices signed in to your Tailscale account can reach
Tessera.

1. Install Tailscale on the server and sign in: on Linux
   `curl -fsSL https://tailscale.com/install.sh | sh`, then
   `sudo tailscale up`; on Windows and macOS, the app from
   <https://tailscale.com/download>.
2. Install the Tailscale app on your phone and laptop, and sign in with the
   same account.
3. In **Settings → Connection**, turn on **Let other devices on my network
   connect**, then under **Away from home** choose **I use Tailscale or my own
   tunnel** and enter the server's Tailscale address, for example
   `http://my-server.tail1234.ts.net:7878`, and **Save**. (In a terminal:
   `tessera setup devices` → **Tailscale** finds the name by itself.)
   - If Tessera should listen only on its own computer, run
     `tailscale serve --bg 7878` on the server instead, and use
     `https://my-server.tail1234.ts.net`.
4. Pair your phone. A phone you paired before learns the new address by
   itself: open Tessera on it once while it's at home.

Keep Tailscale switched on on your phone: without it, the address doesn't
work away from home.

## Your own reverse proxy or VPN

For people who run their own. Choose **I use Tailscale or my own tunnel**
(terminal: **My own reverse proxy or tunnel**) and enter the HTTPS address
that forwards to Tessera (to `server.bind`, `127.0.0.1:7878` by default).
Only if your proxy needs extra headers (for example a Cloudflare Access
service token you set up yourself), add them under **More: extra headers**;
they travel in pairing codes, and paired devices pick up changes by
themselves. From the command line:
`tessera remote https://tessera.example.com -H Name=Value`. Devices send those
headers only to `https` addresses, never to the plain-`http` home address.

A proxy on the server's own computer **must say who is connecting** with
`X-Forwarded-For` (Caddy does by default; with nginx add
`proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;`). Without it,
everyone who comes through the proxy looks to Tessera as if they were on the
server itself, where plain (unencrypted) browser access is allowed.
`tessera remote <address>` checks this through the proxy and warns when it is
missing.

## Why not a free quick tunnel?

Cloudflare also offers "quick tunnels" (`cloudflared tunnel --url`, on
`trycloudflare.com`) that need no account and no domain. Tessera doesn't use
them: their address changes every time the server restarts, so your phone
would lose the server after every reboot and you'd have to pair it again,
and they have no Access door in front. A domain on Cloudflare, or Tailscale,
keeps its address and keeps strangers out.

## Use Tessera in a web browser

You can also use Tessera in an ordinary web browser (a work computer, a
borrowed laptop, a tablet without the app). It is **off by default**; turn it
on in **Settings → Connection → Web browsers** on one of your devices.

1. Turn on **Use Tessera in a web browser**. It asks first. If your remote
   address is the Cloudflare tunnel Tessera set up, Cloudflare lets only the
   apps through, so the first time it also asks for a Cloudflare API token
   with one permission (**Account · Access: Apps and Policies · Edit**; used
   once, never stored): Tessera then lets browsers reach `/web` on your
   address, and nothing else changes there.
2. Open the address it shows (`https://<your address>/web/`) in the browser.
3. On one of your devices, make a pairing code in **Settings → Devices** and
   type it in the browser. The browser shows up in Devices as a web browser;
   sign it out there whenever you like.

What to know:

- Only browsers you pair with a code from one of your devices get in, and
  everything between the browser and the server is end-to-end encrypted, as
  with the apps. The browser keeps a key of its own that no script can read
  out, and proves it holds it every time it connects: its saved login alone
  is useless anywhere else.
- While it's on, anyone who finds your address sees Tessera's sign-in page
  (`/web/`). Pairing there is rate-limited, by address too.
- The page must come from one of your server's own addresses: the remote
  addresses in **Settings → Connection** (or Tessera's tunnel). Behind a
  proxy of your own, add its https address there first.
- A browser is a little less protected than the apps: it loads Tessera's
  page from your server each time, so whoever runs the address in between
  (Cloudflare, or your own proxy) could change that page. Use it on
  computers you trust, and turn it off when you don't need it.
- It works only over https (your remote address), or in a browser on the
  server itself (`http://localhost:7878/web/`), never over plain http from
  elsewhere: a page that came unencrypted could have been changed on the
  way.
- Turning it off stops paired browsers at once and nothing is served at
  `/web` any more; they stay paired, so turning it on again lets them back
  in (sign them out in Devices to remove them for good).
- Visual whiteboards and HTML previews show in the apps, not in a web
  browser, and terminals open only from the apps.
- The server needs the web app: the installer builds it when Node.js is
  there. Without it the switch says so.

## Pairing a phone or computer

`tessera pair` (or **Settings → Devices → Add… → Pair a device → Get a code**
in an app) shows a QR code that holds the home and remote addresses, the
Access key if there is one, a one-time code and the server's key
fingerprint. In the Tessera app choose **Scan QR code**.

On a computer without a camera, use the link shown under the code: paste it
into the app's pairing screen, or run `tessera connect '<link>'` in another
terminal. Keep the link private: it holds the server's access keys. The code
in it works once and expires after 10 minutes.

A phone on mobile data, guest Wi-Fi or another network can only use the
remote address; the pairing screen warns when there isn't one or when the
tunnel isn't connected yet.

## When something doesn't work

### "This token can't see any domain"

The token's **Zone Resources** don't include your domain, or the domain
isn't **Active** in Cloudflare yet. Check the domain's status in the
Cloudflare dashboard, then create the token again with **Zone Resources:
Include → Specific zone → your domain**.

### "Turn on Zero Trust first"

Cloudflare didn't let Tessera use Access on your account. Almost always,
Zero Trust isn't turned on yet: do [step 3](#step-3-turn-on-zero-trust-free)
(**Open Zero Trust** in the message takes you there), then select **Check
again**. If Zero Trust is on, the token is missing **Access: Apps and
Policies · Edit** or **Access: Service Tokens · Edit**: make a new one as in
[step 4](#step-4-create-the-api-token-for-tessera). Tessera checks this before
it creates anything in your account.

### "Cloudflare refused: … Check the token has the permissions"

One of the five permissions is missing or set to Read instead of Edit.
Create a new token and compare it row by row with
[step 4](#step-4-create-the-api-token-for-tessera).

### "… is already in use in your DNS" or "an Access application already protects …"

That address is taken in your Cloudflare account. Pick another name (for
example `tessera2.example.com`), or delete the old record or application in
Cloudflare if you know you don't need it.

### "The Cloudflare tunnel isn't connected" (port 7844)

Setup and the pairing screen say why. The usual reason, on locked-down
servers and some hosting or work networks, is a firewall that blocks
outgoing connections to Cloudflare's tunnel port:

- Allow **outgoing TCP and UDP port 7844** to `region1.v2.argotunnel.com`
  and `region2.v2.argotunnel.com`.
- On the machine itself with ufw: `sudo ufw allow out 7844`. Elsewhere, ask
  whoever runs the network.
- Then select **Check again** (in a terminal: **Check the tunnel again** in
  `tessera setup devices`). The tunnel keeps retrying on its own, so it also
  connects without you once the port is open.

If the network can't be changed, use Tailscale instead: it finds its own
way out.

### It works on Wi-Fi but not on mobile data

- The phone hasn't connected since remote access was set up: open Tessera
  on it once at home, and it learns the address
  ([step 7](#step-7-your-devices-pick-it-up-by-themselves)). In **Settings →
  Connection** on the phone, a paired server lists its addresses; if there
  is no `https://` address with a cloud icon, that's the reason. (A phone
  with a Tessera app from before this existed: update the app, or pair it
  again.)
- The devices' key ran out: see [Renewing the key](#renewing-the-key-once-a-year).
- The tunnel isn't connected: look at **Settings → Connection** on another
  device (or the server) for the tunnel's line.
- The server is off, asleep or lost its internet connection. Remote access
  needs the server running. On a laptop or desktop, turn off sleep while it
  serves Tessera ([Windows](WINDOWS.md)).

### Opening the address in a browser shows a Cloudflare page, not Tessera

That's Cloudflare Access doing its job: only your Tessera apps carry the key.
To use Tessera in a browser, turn on
[Use Tessera in a web browser](#use-tessera-in-a-web-browser) and open
`https://<your address>/web/`.

### Renewing the key (once a year)

The key your devices carry through Cloudflare Access (the service token) is
valid for one year. From 30 days before it runs out, Tessera tells you: a
notification on your devices ("Remote access needs renewing", again at 7
days and on the last day), and a line in **Settings → Connection** with a
**Renew** button. If it ran out, the line says so ("Remote access stopped"),
and away from home your devices can't connect until you renew it.

Renewing gives the same key another year, so your devices keep working with
nothing to do on them:

1. Create a token as in [step 4](#step-4-create-the-api-token-for-tessera),
   with just one permission: **Account · Access: Service Tokens · Edit**.
2. In **Settings → Connection**, select **Renew**, paste the token and select
   **Renew**. Delete the token afterwards.

Or renew it in Cloudflare yourself: in the dashboard open **Zero Trust →
Access controls → Service credentials → Service Tokens**
(<https://one.dash.cloudflare.com/?to=/:account/access/service-auth/service-tokens>),
find **Tessera apps (your address)** and select **Refresh** (another year).
Then, in **Settings → Connection → Renew → Or renew it in Cloudflare
yourself**, select **I renewed it**, so Tessera knows the new date.

If remote access was set up with a Tessera from before it kept this date,
the line says "Renew it once" instead: renew it once, and the reminders
start.

### The domain stopped working

Domains are rented by the year: check in Cloudflare that it renewed (auto-
renew is on by default) and that you confirmed your contact email address.
