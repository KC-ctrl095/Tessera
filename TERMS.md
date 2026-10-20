# Terms of use

*Last updated: 20 October 2026*

**In short.** Tessera is free, open-source software that you run yourself.
It comes as it is, with no warranty. Its AI agents act for you on real
computers, accounts and data, and they can make mistakes, so what they do is
your responsibility. The AI and internet services you connect to Tessera
have their own terms, which you must follow; in particular, a Claude
subscription may only be used through your own, unmodified Claude Code.
Tessera is not affiliated with any of those companies.

The rest of this page says the same thing in full.

## 1. What these terms cover

"Tessera" means the software in this repository: the server, the terminal
app, `tessera node`, the desktop, Android and web apps, the installers and
the documentation. "We" means the Tessera Authors, the people who wrote and
maintain it. "You" means anyone who installs, runs, changes or shares it.

Your rights to use, copy, change and share Tessera's code come from the
[Apache License 2.0](LICENSE). These terms don't take any of those rights
away and don't add conditions to the licence. They explain what Tessera is
and isn't, what we are and aren't responsible for, and the responsibilities
you have anyway, under the law and under other companies' terms, when you
use software like this. If anything here conflicts with the Apache License,
the licence wins.

We don't run Tessera for you. There is no Tessera account, no Tessera cloud
and no service of ours that your data passes through (see
[PRIVACY.md](PRIVACY.md)). If someone else runs Tessera for you, their terms
apply between you and them.

## 2. No warranty

TESSERA IS PROVIDED "AS IS" AND "AS AVAILABLE", WITHOUT WARRANTIES OR
CONDITIONS OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING WITHOUT LIMITATION ANY
WARRANTIES OR CONDITIONS OF TITLE, NON-INFRINGEMENT, MERCHANTABILITY, FITNESS
FOR A PARTICULAR PURPOSE, ACCURACY, SECURITY OR UNINTERRUPTED OR ERROR-FREE
OPERATION. This repeats section 7 of the Apache License.

In plain words: we try hard to make Tessera safe and reliable, but we don't
promise that it works, that it is secure, that it will keep working with
the services it connects to, or that we will fix problems or release
updates. You use it at your own risk.

## 3. Limitation of liability

TO THE FULLEST EXTENT ALLOWED BY LAW, IN NO EVENT WILL THE TESSERA AUTHORS OR
CONTRIBUTORS BE LIABLE TO YOU OR ANYONE ELSE FOR ANY DAMAGES OR LOSSES OF ANY
KIND, WHETHER DIRECT, INDIRECT, SPECIAL, INCIDENTAL, CONSEQUENTIAL OR
PUNITIVE, ARISING OUT OF OR RELATED TO TESSERA OR ITS USE, HOWEVER CAUSED AND
UNDER ANY THEORY OF LIABILITY (CONTRACT, TORT, NEGLIGENCE OR OTHERWISE), EVEN
IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGES. This repeats section 8 of the
Apache License.

This includes, for example: lost, changed or leaked data; damage to
computers, systems or accounts; charges from AI providers or any other
service; money spent or lost through actions an agent took; suspended or
closed accounts; missed deadlines; and anything an AI model said or did.

Where the law doesn't allow a limit like this, our liability is limited to
the smallest amount the law allows.

## 4. AI agents act on real systems: disclaimer

Tessera's agents don't just answer questions. Depending on what you allow,
they can:

- run commands and programs on your server and on the computers you add as
  machines, including commands that change or delete files;
- read, write and delete files, and change settings and code;
- browse the web, fill in forms, sign in to websites with the passwords you
  saved, send messages and emails, and post things in your name;
- use paid services and APIs, which can cost you money, and take actions
  with financial, legal or other real-world consequences;
- keep working without you, in scheduled jobs, bots and background tasks.

AI models make mistakes. They can misunderstand you, invent facts, act on
false information, carry out a harmful instruction hidden in a web page,
file or message they read, or do something you didn't expect. Tessera's
protections (the command sandbox, asking before risky actions, keeping
passwords and keys away from models, end-to-end encryption) reduce these
risks. They don't remove them, and some of them can be turned off: the
"YOLO" mode that stops the questions, turning the sandbox off, a machine's
"Full access", or running commands as an administrator all make mistakes
more costly. Those are your choices to make.

So:

- **You are responsible for what your agents do**, as if you had done it
  yourself, including actions they take while you aren't watching.
- Give agents only the access they need. Keep backups of anything you can't
  afford to lose. Review important actions before you approve them.
- Check the results of anything that matters. Don't rely on Tessera, or on
  any AI, for medical, legal, financial or safety decisions, or to control
  equipment, vehicles, or systems where a mistake could hurt someone or cause
  serious loss.
- The costs Tessera shows are estimates. Your providers' bills are what
  count.

## 5. Your accounts, computers and data

You are responsible for:

- everything done through your Tessera server, by you, by your agents,
  bots and scheduled jobs, and by anyone whose device you pair;
- keeping your devices, pairing codes, pairing links and server safe, and
  signing out devices you lose (Settings → Devices);
- the accounts, computers and data you connect to Tessera, and having the
  right to use them that way (for example, your employer's systems);
- personal information about other people that you or your agents handle
  with Tessera, and following the privacy laws that apply to it. If you run
  Tessera for other people (family, friends, a team), you are the one who
  keeps their data, and you should tell them what that means
  ([PRIVACY.md](PRIVACY.md)).

## 6. Other companies' services

Tessera connects to services you choose: AI model providers (such as
Anthropic, OpenAI or OpenRouter, or models you run yourself), Claude Code,
Cloudflare, Tailscale, search providers (SearXNG, Brave, Tavily, DuckDuckGo),
GitHub and Google (for downloads), and any website your agents visit. Each
has its own terms, usage policies, age limits and prices. By connecting one,
you agree to follow its terms, and you are responsible for your account with
it and for what it charges you. We can't promise that any of them allows
every way you might use Tessera, or that they won't change or stop what
Tessera relies on.

Some rules matter enough to spell out:

- **Claude subscriptions.** Anthropic's rules let Claude Pro and Max plans
  be used through Anthropic's own apps and Claude Code, not through other
  software that signs in on your behalf. Tessera's Claude plan option runs
  **your own, unmodified Claude Code**, signed in by you; Tessera never
  asks for, reads, stores or passes on your Claude login. Don't
  change Tessera, or use it, to take, copy, share or reuse Claude login
  credentials or tokens, to send other software's requests through a
  subscription, or to let other people use your plan: each person signs in
  to their own Claude Code with their own subscription. For anything else,
  use an Anthropic API key, under Anthropic's commercial terms and usage
  policy.
- **ChatGPT plans.** Tessera's ChatGPT option signs you in on OpenAI's own
  sign-in page, with the sign-in made for OpenAI's Codex, and then talks to
  OpenAI's Codex service with that login. Whether and how your plan may
  be used this way is decided by OpenAI's terms; check them and follow them.
  OpenAI may change or stop this at any time.
- **Websites.** Your agents must respect the terms of the websites they use.
  Tessera doesn't try to get past websites' bot checks or verification
  challenges; when a site asks for one, Tessera hands the page to you to
  complete yourself. Don't use Tessera to break a site's rules, for example
  to create fake accounts, send spam or collect data where that isn't
  allowed.
- **Cloudflare.** Remote access through Cloudflare creates a tunnel, a DNS
  name and Cloudflare Access settings in your own Cloudflare account, under
  Cloudflare's terms.

## 7. Acceptable use

Don't use Tessera to:

- break the law, or help someone else break it;
- get into computers, accounts or data you aren't allowed to use;
- write or spread malware, or attack or overload systems;
- harass, threaten, deceive, impersonate or spy on people, or collect their
  personal information without a lawful reason;
- infringe anyone's copyright, trademarks or other rights;
- create or share sexual content involving minors, or content that promotes
  violence or terrorism;
- break the usage policies of the AI models or services you connect, or get
  around their safety measures or limits.

These rules restate what the law and the services' own policies already
require of you. They are not extra conditions on the software licence.

## 8. Security, updates and support

We fix security problems we learn about as well as we can, and tell people
how to report them in [SECURITY.md](SECURITY.md). We don't promise support,
updates, or that any version will keep working. Keep your copy up to date
(run the install command again), and read the release notes.

## 9. Changed copies of Tessera

If you share a changed copy of Tessera, the Apache License asks you to say
what you changed and to keep its notices. You are responsible for your
changed copy and for what it does. Don't present it as the official Tessera
or as made or approved by us; [TRADEMARKS.md](TRADEMARKS.md) explains what
you may call it.

## 10. No affiliation

Tessera is an independent project. It is not affiliated with, endorsed by or
sponsored by Anthropic, OpenAI, Cloudflare, Google, Microsoft, Tailscale,
GitHub, Brave, Tavily, DuckDuckGo, SearXNG or any other company or project
it works with. Their names and products are trademarks of their owners and
appear here only to say what Tessera works with.

## 11. Claims against us

To the extent the law allows, if someone makes a claim against the Tessera
Authors or contributors because of how you used Tessera, what your agents
did, or your breaking the law or another service's terms, you agree to
cover the costs and losses that follow from it, including reasonable legal
fees.

## 12. Changes to these terms

We may update these terms. The current version is in the repository, and
the date at the top says when it last changed. Each release of Tessera
includes the terms that apply to it.

## 13. Contact

Nothing in these terms takes away rights that the law of the place you live
gives you and that can't be waived.

If a court finds part of these terms unenforceable, the rest still applies.
Not enforcing a part of them doesn't mean giving it up.

Questions about these terms: open an issue at https://github.com/KC-ctrl095/Tessera/issues.
