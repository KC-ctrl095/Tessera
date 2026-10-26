# Preparing a public release

This is the maintainer checklist, not installation instructions for users.

## Owner decisions before publishing

- Choose and publish the public repository. `workspace.package.repository` in `Cargo.toml` is the source of truth. The current placeholder returns 404. Nothing here chooses a replacement.
- Licence: Apache-2.0 (the owner's choice, October 2026). Before each release run `node scripts/licenses.mjs --check` so the third-party notices match the lockfiles.
- Keep the version in `Cargo.toml`, `apps/tessera/src-tauri/tauri.conf.json` and `apps/tessera/ui/package.json` in step. Tag that source as `v<version>`.
- Choose a public Android signing identity and back up its keystore and password offline. Keep the same key for updates. Set the repository secrets listed below. Never commit keys, passwords or signed-key configuration.
- Decide on Windows Authenticode signing before launch. The checked-in build makes a complete per-user installer, but does not set up a signing service or certificate. SmartScreen and Smart App Control can block an unsigned download. Do not tell users to disable those protections.

## One public project address

After changing `repository` in `Cargo.toml`, run these from the repository root.

```sh
node scripts/release-links.mjs --write
node scripts/release-links.mjs --check
```

The UI reads that same value through `apps/tessera/ui/src/lib/repo.ts`. Standalone installers and copyable Markdown commands have generated copies checked by CI. Gitea/Forgejo publishing also requires an explicit `--repo https://host/owner/name` or `TESSERA_RELEASE_REPO`. This supports a mirror without hard-coding a private server. Its packaged installers point at that chosen release.

## Required downloads

Both release paths refuse to publish a partial launch release.

| Download | Contents and installation |
| --- | --- |
| `tessera-linux-x86_64.tar.gz` | CLI and server with the built browser UI, glibc 2.28 or newer. `install.sh` selects this on x86_64 Linux |
| `tessera-linux-aarch64.tar.gz` | Same for ARM64 Linux |
| `tessera-desktop-linux-x86_64.tar.gz` | Desktop executable and icons. `install.sh` offers it on an x86_64 desktop with WebKitGTK 4.1 |
| `tessera-windows-x86_64.zip` | CLI, service helper, PowerShell installer and legal notices. For terminal/server-only use |
| `tessera-desktop-windows-x86_64-setup.exe` | Recommended Windows installer. Includes the desktop app, `tessera.exe` and `tessera-service.exe` together. No Rust or Node needed to install |
| `tessera-android-arm64.apk` | Signed release app for ARM64 devices, Android 7.0 / API 24 or newer. Connects to a server, not a server itself |
| `install.sh`, `install.ps1`, `SHA256SUMS` | Installers and checksums for every published artifact |

Platform archives also carry the licence and third-party notices. macOS CLI builds are additional, optional downloads. Neither path builds a Linux ARM64 desktop or a macOS desktop. Linux ARM64 users can use the browser UI or build the desktop from source. Android x86_64 and ARMv7 APKs are not launch downloads.

## GitHub Actions

Push the version tag, or run the Release workflow for an existing tag. Linux CLI jobs first build the UI and set `TESSERA_WEB_APP` when compiling the server. Linux desktop and Windows jobs are required. Windows runs `scripts/release-windows.ps1`, which builds the UI and both helpers, stages the helpers for the Windows-only Tauri resource map, then builds NSIS.

The Android job needs all three repository secrets. If any is absent it skips the build with a warning and the release goes out without an Android app (the release notes say so); there is never an unsigned or debug fallback, and the completeness checker then refuses any APK in the release (`TESSERA_RELEASE_ANDROID=false`). Set the secrets and run the workflow again for the same tag to add the APK.

| Secret | Value |
| --- | --- |
| `ANDROID_KEYSTORE_BASE64` | Base64 of the private release keystore |
| `ANDROID_KEYSTORE_PASSWORD` | Password for both keystore and key |
| `ANDROID_KEY_ALIAS` | Existing release key alias |

The runner writes the key and password into private temporary files and removes them after building. It generates Tauri's ignored Gradle/ProGuard files into a fresh Android project, then overlays the checked-in Android customizations. `scripts/android.sh` retains its source and compiled vendor/wry patch guards. The workflow installs SDK 37, build-tools 37.0.0, NDK 29.0.13846066 and Java 17, matching the checked-in SDK level. CI must be exercised with the real signing secrets before launch. This change has not itself produced a release APK or Windows installer.

Publishing requires successful Linux, desktop, Windows and Android jobs (the Android job succeeds without building when no signing key is set). After downloading artifacts, it generates `SHA256SUMS` and runs the completeness checker before attestations or any release upload.

## Local builds and Gitea / Forgejo

On Windows with Rust MSVC, Node 22 and NSIS available, run this from PowerShell.

```powershell
.\scripts\release-windows.ps1
```

The complete installer is written to `out/tessera-desktop-windows-x86_64-setup.exe`. The installer build passes `--config windows/installer.conf.json`, which bundles both helpers from the ignored `windows/resources` staging folder; without them staged it fails rather than creating a broken installer. Ordinary builds, `cargo clippy` and `cargo tauri dev` don't need the helpers. For Linux cross-builds, `release-gitea.sh` performs those staging steps and uses cargo-xwin. Use the version of tauri-cli recorded by `TAURI_CLI` in `scripts/install.sh`.

For a local public APK, first initialize Tauri Android on a full source checkout with the Android SDK/NDK and Java installed. Keep its generated Gradle and ProGuard files and the checked-in customizations. `android.sh` refuses to build if generated files or vendor guards are missing. Set these variables to files outside the repository, not to the local developer sideload key.

```sh
export TESSERA_ANDROID_PUBLIC_RELEASE=1
export TESSERA_KEYSTORE=/private/path/release.jks
export TESSERA_KEYSTORE_PASS_FILE=/private/path/release-password.txt
export TESSERA_KEY_ALIAS=your-release-alias
RUST_MIN_STACK=134217728 sh scripts/android.sh release aarch64
cp out/tessera-universal-release-aarch64.apk out/tessera-android-arm64.apk
```

Public mode requires an existing key and password file. It never generates a key. Store and key passwords must match. Before publishing, smoke the APK on a throwaway Android emulator/profile, confirm the app stays open and the crash log is empty. Increment the Android version code for updates, as derived from the Tauri version, and verify it with `aapt dump badging`.

On an x86_64 Linux build host with WebKitGTK development libraries, cargo-xwin, cargo-zigbuild, Zig, NSIS and the checker tools installed, run this from the same tagged source used for the APK.

```sh
sh scripts/release-gitea.sh v0.1.0 --repo https://host/owner/name \
  --android-apk out/tessera-android-arm64.apk --dry-run
# After checking out/release, repeat without --dry-run to publish.
```

This builds both Linux CLIs, Linux desktop and a complete Windows installer. It always requires the signed APK. `--skip-build` packages complete existing target outputs only, never a partial set. It does not silently pick an unrelated Windows installer from `out/`. Clean stale NSIS outputs first if more than one exists. `TESSERA_BUILD_WRAP` can apply the host's resource lock to each cargo command. No server is restarted by this release script.

## Check the exact downloads

Install Python 3, 7z with NSIS support and Android SDK build-tools. Add `apksigner` and `aapt` to PATH, then run this on the final artifacts folder.

```sh
python3 scripts/check-release.py out/release
python3 scripts/test-check-release.py
```

The checker requires every launch file, verifies all SHA-256 entries and rejects missing, extra or duplicate entries. It inspects both Linux CLIs, the Windows CLI and Linux desktop for bundled `index-*.js` UI names. It extracts NSIS without executing it and matches both root-level helpers byte for byte against the Windows zip. It verifies the APK signature using `apksigner`, rejects debug/developer certificates and a debuggable manifest, checks the app ID and looks for the UI bundle in the ARM64 native library. Set `TESSERA_ANDROID_CERT_SHA256` to an expected certificate fingerprint for an additional identity pin. The checker is a packaging gate, not proof that every program runs.

Use isolated profiles for runtime install testing, never a running Tessera installation.

```sh
sh scripts/test-install.sh
sh scripts/test-first-run.sh
```

Linux smoke tests cover source and binary install, bad checksums, source fallback, keeping settings on reinstall and systemd ownership. For Windows, test the desktop installer and `install.ps1 -NoDesktop` on a clean Windows account without Rust or Node. Check first-run sign-in, updates preserving settings, uninstall preserving data and Guard remaining opt-in. A Linux packaging check does not prove Windows runtime behavior.

Installers compare archives against the release's `SHA256SUMS`. This detects corruption, not independent publisher identity because both files come from the same release. Use HTTPS and check the publisher. A checksum mismatch stops installation instead of falling back. Public download links must remain marked unavailable until the owner publishes the repository and a complete release.
