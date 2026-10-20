# Security policy

Tessera runs AI agents with real access to computers, accounts and
passwords, so we take security problems seriously and thank everyone who
reports one responsibly.

How Tessera protects you (the sandbox, end-to-end encryption, how passwords
are kept from models, what each setting loosens) is explained in
[docs/SECURITY.md](docs/SECURITY.md). This page is about reporting problems.

## Reporting a vulnerability

**Please don't report security problems in public issues, discussions or
chats.** Report them privately on GitHub: the repository's **Security** tab →
**Report a vulnerability** (https://github.com/KC-ctrl095/Tessera/security/advisories/new). Only the
maintainers see the report.

Please include:

- what the problem is and what someone could do with it;
- the Tessera version (`tessera --version`, or Settings → Help & tour) and
  the platform (Linux, Windows, macOS, Android, a web browser);
- the steps to reproduce it, ideally the smallest ones, and any settings it
  depends on;
- whether it is already known or published anywhere.

## What happens next

Tessera is a small open-source project, so these are aims rather than
promises:

- We confirm we received your report within **7 days**.
- We tell you within **30 days** whether we consider it a vulnerability and
  how we plan to fix it.
- We fix serious problems as quickly as we can, publish the fix in a new
  release, and describe the problem in the release notes or a security
  advisory once people have had a chance to update.
- We ask you to keep the problem private until the fix is out, or for **90
  days** from your report, whichever comes first. If you need more time or
  less, tell us and we'll agree on a date together.
- We credit you in the advisory, unless you prefer not to be named.

There is no bug bounty.

## Supported versions

Tessera is young (version 0.x). Security fixes go into the **latest
release** only; there are no fixes for older versions. Running the install
command again updates to the latest release.

| Version | Security fixes |
|---|---|
| The latest release | Yes |
| Anything older | No: update |

## What counts

In scope: the code in this repository, including the server, the apps,
`tessera node`, the installers, pairing and end-to-end encryption, the
command sandbox, the handling of saved passwords and keys, approvals, web
views and whiteboards, and the remote access Tessera sets up.

Especially welcome: anything that lets someone without a paired device in;
that lets a model, a web page or a file read a saved password or key; that
lets an agent's command get out of the sandbox, or act without an approval
Tessera says it asks for; that makes a machine do more than its own settings
allow; or that weakens the encryption between devices and the server.

Usually out of scope:

- problems in other companies' services or programs (Cloudflare, AI
  providers, browsers, Claude Code): please report those to them;
- what an agent does after you loosened a protection on purpose (the YOLO
  mode, the sandbox off, a machine's Full access), unless Tessera told you
  something different about what the setting does;
- a model being talked into something by text it read, when no protection
  was bypassed: AI models can be misled, and Tessera's job is to limit what
  that can lead to (if it gets past a protection, it is in scope);
- attacks that need someone who already controls your server, your user
  account or an unlocked paired device;
- missing best practices without a way to exploit them.

## Testing safely

Test only on Tessera installations you own, with throwaway accounts and
data. Don't access or change other people's data, don't disrupt others'
servers or the services Tessera connects to, and stop and report as soon as
you have found a problem. We won't take legal action against anyone who
researches and reports in good faith within these rules.
