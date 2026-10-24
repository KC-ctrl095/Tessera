## Install

**Linux and macOS** (server and terminal; on a Linux desktop it also offers the desktop app):

```bash
curl -fsSL https://github.com/KC-ctrl095/Tessera/releases/latest/download/install.sh | sh
```

**Windows 10 and 11:** download `tessera-desktop-windows-x86_64-setup.exe` below and run it.
Windows may say "unknown publisher": the installer isn't code-signed yet. For the server and
terminal only, in PowerShell:

```powershell
irm https://github.com/KC-ctrl095/Tessera/releases/latest/download/install.ps1 | iex
```

**Android:** on your phone, download `tessera-android-arm64.apk` below and open it. The app
connects to your Tessera server; it doesn't run one.

After installing, `tessera setup` walks you through choosing an AI and pairing your phone.
Run the install command again to update.

## Which file is which

| File | What it is |
| --- | --- |
| `tessera-desktop-windows-x86_64-setup.exe` | Windows desktop app (complete installer) |
| `tessera-windows-x86_64.zip` | Windows server and terminal |
| `tessera-linux-x86_64.tar.gz` | Linux server and terminal (Intel and AMD) |
| `tessera-linux-aarch64.tar.gz` | Linux server and terminal (ARM, such as a Raspberry Pi 4 or 5) |
| `tessera-desktop-linux-x86_64.tar.gz` | Linux desktop app |
| `tessera-macos-aarch64.tar.gz`, `tessera-macos-x86_64.tar.gz` | macOS server and terminal (when present) |
| `tessera-android-arm64.apk` | Android app |
| `install.sh`, `install.ps1` | The installers used above |
| `SHA256SUMS` | Checksums of every file |

The installers check each download against `SHA256SUMS`. Every file also carries a build
provenance record: `gh attestation verify <file> --repo KC-ctrl095/Tessera` shows it was built
here, from this release's source.
