# Contributing to Tessera

Thank you for helping. Issues, ideas, documentation fixes and pull requests
are all welcome. This page explains how to build and test Tessera, what a
good change looks like, and the small legal bits (the sign-off and the
licence of your contribution).

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).
**Security problems are reported privately**, never in a public issue: see
[SECURITY.md](SECURITY.md).

## What keeps Tessera what it is

- **Simple for everyone.** Plain words in the interface and the docs, safe
  defaults and presets rather than knobs, and it has to work on weak and old
  hardware without tuning.
- **Security first.** Secrets never reach a model or a log; least privilege
  everywhere; every loosening is something the user chooses, explained in
  plain words.
- **Tested.** Every change comes with tests; a change to the interface comes
  with an end-to-end check, on a desktop, a phone and a foldable, in light
  and dark.
- **Lean.** Match the code around you; comments say why. No heavy
  dependencies.

For anything bigger than a fix, open an issue first and describe what you
want to change, so we can agree on the approach before you spend time on it.

## Building

You need Rust 1.88 or newer (<https://rustup.rs>) and a C compiler, and
Node.js 22.12 or newer for the apps' interface. For the Linux desktop app on
Debian or Ubuntu:

```bash
sudo apt-get install libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev libxdo-dev libssl-dev
```

For the Android app: a JDK 17 or newer, the Android SDK and NDK, and
`rustup target add aarch64-linux-android`.

```bash
git clone https://github.com/KC-ctrl095/Tessera
cd tessera
(cd apps/tessera/ui && npm ci --ignore-scripts && npm run build)
TESSERA_WEB_APP="$PWD/apps/tessera/ui/dist" cargo build --release --locked -p tessera
```

This gives `target/release/tessera` with the browser app built in; without
`TESSERA_WEB_APP` the server has no browser app. `sh scripts/install.sh` does
the same and installs it.

The desktop and Android apps: [Building the apps](docs/GUIDE.md#building-the-apps).
How the parts fit together: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

When you try a development build, give it its own home so it never touches
your real Tessera: `TESSERA_HOME=$PWD/.dev-home target/release/tessera serve
--bind 127.0.0.1:7979` (the desktop app: its own `XDG_DATA_HOME` and
`XDG_CONFIG_HOME`).

## Testing

The same checks run in CI on every pull request:

```bash
cargo fmt --all --check
cargo clippy --workspace --exclude tessera-app --exclude tessera-background --all-targets -- -D warnings
cargo test --workspace --exclude tessera-app --exclude tessera-background
(cd apps/tessera/ui && npm run -s typecheck && npm run build)
node scripts/licenses.mjs --check                    # third-party licences (below)
sh scripts/test-install.sh                           # the installer, in a throwaway home
```

Three tests look at every process on the machine. If
`killing_a_job_stops_its_whole_tree`,
`observe_scans_proc_once_and_skips_held_marker_reads` or
`claude_close_stops_spawn_owned_setsid_descendant` says some processes could
not be checked, rerun them alone with `--test-threads=1` (CI does):
`cargo test -p tessera-core --lib -- --test-threads=1 killing_a_job_stops_its_whole_tree observe_scans_proc_once_and_skips_held_marker_reads claude_close_stops_spawn_owned_setsid_descendant`.

The interface has end-to-end suites that drive a headless Chromium against a
scripted fake server (`apps/tessera/ui/dev/mock-server.mjs`):

```bash
sh apps/tessera/ui/tests/e2e/run.sh                  # all suites
sh apps/tessera/ui/tests/e2e/run.sh notes tiles      # only these
```

They need Node 22 and a Chromium (`CHROMIUM=/path/to/chrome` picks one).
`VITE_PORT`, `MOCK_PORT`, `CDP_PORT` and `E2E_OUT` change the ports and
where screenshots go. More in [docs/TESTING.md](docs/TESTING.md).

A few things that are easy to miss:

- **Protocol changes** (`crates/proto`): new fields get serde defaults so
  older apps keep working; regenerate the interface's types with
  `TS_RS_EXPORT_DIR="$PWD/apps/tessera/ui/src/proto" cargo test -p tessera-proto --features ts`,
  and update [docs/PROTOCOL.md](docs/PROTOCOL.md).
- **The fake server** must behave like the real one for everything the
  interface relies on: change both.
- **The agent's base prompt has a size budget**, checked by
  `crates/core/tests/code.rs`. Keep prompt and tool-description changes
  small; detail belongs in the agent docs (`crates/core/docs/agent/`), which
  agents read on demand.
- **Tests never use real data or accounts**: throwaway homes, fake models
  and fake services. Tests that need a live model are `#[ignore]`d.

## Dependencies and other people's code

- Prefer no new dependency. When one is worth it, it must be under a
  permissive licence (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode) or, used
  unchanged, MPL-2.0. No GPL, LGPL or AGPL code in Tessera's programs.
- Don't copy code from elsewhere unless its licence allows it; say where it
  came from and keep its copyright notice.
- After changing `Cargo.lock` or `package-lock.json`, run
  `node scripts/licenses.mjs` and commit the updated THIRD_PARTY_LICENSES.txt
  and THIRD_PARTY_NOTICES.md.

## Using AI tools

Tessera itself is built with AI coding agents, so you're welcome to use
them. You are still the author of what you submit: read and understand it,
test it, and make sure it isn't copied from code you have no right to use.
The sign-off below is your promise about that, whoever or whatever typed the
code.

## Signing off your commits

Every commit must be signed off, certifying the Developer Certificate of
Origin below: that you wrote the change, or otherwise have the right to
submit it under Tessera's licence. Add the sign-off with `-s`:

```bash
git commit -s -m "Explain what changed and why"
```

which adds a line with your name and email address:

```
Signed-off-by: Your Name <you@example.com>
```

Use your real name or the name you are known by, and an email address that
reaches you. Your sign-off, like the rest of the commit, becomes a public
and permanent part of the project's history.

```
Developer Certificate of Origin
Version 1.1

Copyright (C) 2004, 2006 The Linux Foundation and its contributors.

Everyone is permitted to copy and distribute verbatim copies of this
license document, but changing it is not allowed.


Developer's Certificate of Origin 1.1

By making a contribution to this project, I certify that:

(a) The contribution was created in whole or in part by me and I
    have the right to submit it under the open source license
    indicated in the file; or

(b) The contribution is based upon previous work that, to the best
    of my knowledge, is covered under an appropriate open source
    license and I have the right under that license to submit that
    work with modifications, whether created in whole or in part
    by me, under the same open source license (unless I am
    permitted to submit under a different license), as indicated
    in the file; or

(c) The contribution was provided directly to me by some other
    person who certified (a), (b) or (c) and I have not modified
    it.

(d) I understand and agree that this project and the contribution
    are public and that a record of the contribution (including all
    personal information I submit with it, including my sign-off) is
    maintained indefinitely and may be redistributed consistent with
    this project or the open source license(s) involved.
```

## The licence of your contribution

Tessera is licensed under the [Apache License 2.0](LICENSE). Whatever you
contribute is licensed under the same licence, as its section 5 says, with
no extra terms; you keep the copyright to your work. There is no separate
contributor agreement to sign. Contributing doesn't give you rights to the
Tessera name or logo ([TRADEMARKS.md](TRADEMARKS.md)).
