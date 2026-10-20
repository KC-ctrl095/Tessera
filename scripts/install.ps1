# Install or update Tessera on Windows, for the current user only (no
# administrator rights). In PowerShell:
#
#   irm https://github.com/KC-ctrl095/Tessera/releases/latest/download/install.ps1 | iex
#
# From another server (a mirror, or a private Gitea), point it there first:
#
#   $env:TESSERA_REPO_URL = "http://gitea.example.com:3000/you/tessera"
#   $env:TESSERA_TOKEN = "<a read-only access token>"     # private repositories only
#   irm "$env:TESSERA_REPO_URL/releases/latest/download/install.ps1" | iex
#
# From a release zip you already have: unpack it and run, in that folder,
#
#   powershell -ExecutionPolicy Bypass -File .\install.ps1
#
# Options (as environment variables, since `irm | iex` takes no arguments;
# or as parameters when the script is run as a file):
#   TESSERA_REPO_URL   -Repo       the project's page (default: below)
#   TESSERA_VERSION    -Version    a release tag such as v0.2.0 (default: the latest)
#   TESSERA_DIR        -Dir        where to install (default: %LOCALAPPDATA%\Programs\Tessera)
#   TESSERA_TOKEN      -Token      an access token, for a private repository
#   TESSERA_ZIP        -Zip        install from this release zip instead of downloading
#   TESSERA_SOURCE=1   -Source     build from source with cargo instead
#   TESSERA_YES=1      -Yes        answer yes to every question
#   TESSERA_NO_DESKTOP=1 -NoDesktop leave out the desktop app
#
# What goes where:
#   %LOCALAPPDATA%\Programs\Tessera\tessera.exe          the server, the terminal app, `tessera setup`
#   %LOCALAPPDATA%\Programs\Tessera\tessera-service.exe  runs Tessera in the background (no window)
#   that folder is added to your PATH (for you only)
#   the desktop app, when the release has it             its own per-user installer
#   your data and settings                               %APPDATA%\tessera
# A download is checked against the release's SHA256SUMS before anything is
# installed. Then it offers `tessera setup`.
#
# Everything is in functions and runs from the last line, so a download cut
# short can't run half a script. It never closes your PowerShell window.

param(
    [string]$Repo = $env:TESSERA_REPO_URL,
    [string]$Version = $env:TESSERA_VERSION,
    [string]$Dir = $env:TESSERA_DIR,
    [string]$Token = $env:TESSERA_TOKEN,
    [string]$Zip = $env:TESSERA_ZIP,
    [switch]$Source = ($env:TESSERA_SOURCE -eq '1'),
    [switch]$Yes = ($env:TESSERA_YES -eq '1'),
    [switch]$NoDesktop = ($env:TESSERA_NO_DESKTOP -eq '1')
)

# The standalone copy of `repository` in Cargo.toml. Update it with
# node scripts/release-links.mjs --write. release-gitea.sh overrides it for a mirror.
$TesseraRepo = 'https://github.com/KC-ctrl095/Tessera'

$ZipName = 'tessera-windows-x86_64.zip'
$DesktopName = 'tessera-desktop-windows-x86_64-setup.exe'

function Say([string]$Text) { Write-Host "  $Text" }
function Ok([string]$Text) { Write-Host '  ok  ' -ForegroundColor Green -NoNewline; Write-Host $Text }
function Skip([string]$Text) { Write-Host '  --  ' -ForegroundColor DarkGray -NoNewline; Write-Host $Text }
function Warn([string]$Text) { Write-Host '  !!  ' -ForegroundColor Yellow -NoNewline; Write-Host $Text }

# A question with a yes default (Enter). Without a person to ask: yes.
function Ask([string]$Question) {
    if ($Yes -or -not (Test-Interactive)) { return $true }
    $a = Read-Host "  $Question [Y/n]"
    return ($a -eq '' -or $a -match '^(y|yes)$')
}

function Test-Interactive {
    try { return [Environment]::UserInteractive -and -not [Console]::IsInputRedirected } catch { return $false }
}

# (Windows PowerShell 5.1 refuses a User-Agent among -Headers: -UserAgent.)
function Get-Headers {
    $h = @{}
    if ($Token) { $h['Authorization'] = "token $Token" }
    return $h
}

function Get-File([string]$Url, [string]$Path) {
    Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Path -Headers (Get-Headers) -UserAgent 'tessera-installer'
}

# owner/name and the API of the server behind a project page (GitHub, or a
# Gitea/Forgejo).
function Get-LatestTag([string]$RepoUrl) {
    $u = [Uri]$RepoUrl
    $parts = $u.AbsolutePath.Trim('/').Split('/')
    if ($parts.Count -lt 2) { throw "$RepoUrl isn't a project's page (https://host/owner/name)." }
    $owner = $parts[0]; $name = $parts[1]
    if ($u.Host -eq 'github.com') {
        $api = "https://api.github.com/repos/$owner/$name/releases/latest"
    } else {
        $api = "$($u.GetLeftPart([UriPartial]::Authority))/api/v1/repos/$owner/$name/releases/latest"
    }
    try { $r = Invoke-RestMethod -UseBasicParsing -Uri $api -Headers (Get-Headers) -UserAgent 'tessera-installer' }
    catch { throw "no release found at $RepoUrl ($($_.Exception.Message))" }
    if (-not $r.tag_name) { throw "no release found at $RepoUrl" }
    return $r.tag_name
}

# The lines of SHA256SUMS ("<hash>  <name>" or "<hash> *<name>") as a table.
function Read-Sums([string]$Path) {
    $sums = @{}
    foreach ($line in Get-Content -LiteralPath $Path) {
        $f = $line.Trim() -split '\s+', 2
        if ($f.Count -eq 2) { $sums[$f[1].TrimStart('*')] = $f[0].ToLowerInvariant() }
    }
    return $sums
}

function Test-Sum([string]$Path, [hashtable]$Sums, [string]$Name) {
    if (-not $Sums.ContainsKey($Name)) { return $false }
    $got = (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
    return ($got -eq $Sums[$Name])
}

# Put `$From` in place of `$To`. A running program can't be overwritten on
# Windows but can be renamed: the old one steps aside (and goes next time).
function Install-File([string]$From, [string]$To) {
    $old = "$To.old"
    if (Test-Path -LiteralPath $old) { Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue }
    # Copy first: a failed copy (disk full, permissions) leaves the old
    # program in place. If publishing fails, put the previous one back.
    $next = "$To.new-" + [Guid]::NewGuid().ToString('N')
    try {
        Copy-Item -LiteralPath $From -Destination $next -Force
        if (Test-Path -LiteralPath $To) {
            Rename-Item -LiteralPath $To -NewName ([IO.Path]::GetFileName($old)) -Force
        }
        Move-Item -LiteralPath $next -Destination $To -Force
    } catch {
        if ((Test-Path -LiteralPath $old) -and -not (Test-Path -LiteralPath $To)) {
            Move-Item -LiteralPath $old -Destination $To -Force
        }
        throw
    } finally {
        Remove-Item -LiteralPath $next -Force -ErrorAction SilentlyContinue
    }
    # A zip unpacked in Explorer marks its files as downloaded; Windows would
    # then question tessera-service.exe at every sign-in. Installing it is
    # the user's choice, so the mark goes.
    try { Unblock-File -LiteralPath $To -ErrorAction Stop } catch {}
}

function Get-TesseraVersion([string]$Exe) {
    try {
        $v = & $Exe --version 2>$null
        if ($LASTEXITCODE -eq 0 -and $v) { return ($v | Select-Object -First 1).ToString().Trim() }
    } catch {}
    return ''
}

# Match the first program, not a substring of another install's path or
# a path mentioned in its arguments. The Rust launcher quotes only when needed.
function Test-StartupFromFolder([string]$Line, [string]$Folder) {
    $exe = $Folder.TrimEnd([char[]]'\/') + '\tessera-service.exe'
    $path = [regex]::Escape($exe)
    return ($Line -match ('^"' + $path + '"(?:\s|$)') -or $Line -match ('^' + $path + '(?:\s|$)'))
}

# The folder for the user's PATH (theirs only), now and for new windows.
function Add-ToPath([string]$Folder) {
    $user = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @()
    if ($user) { $parts = $user.Split(';') | Where-Object { $_ -ne '' } }
    if ($parts -notcontains $Folder) {
        [Environment]::SetEnvironmentVariable('Path', (($parts + $Folder) -join ';'), 'User')
        Ok "Added $Folder to your PATH (new terminals see it)"
    }
    if (($env:Path.Split(';')) -notcontains $Folder) { $env:Path = "$env:Path;$Folder" }
}

# Build from source (`$Checkout`: a copy of the source already here, else
# downloaded with git): needs Rust (rustup.rs) with the Visual Studio C++
# build tools.
function Build-FromSource([string]$RepoUrl, [string]$Tag, [string]$Work, [string]$Checkout) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw 'No ready-made build here, and building one needs Rust: install it from https://rustup.rs (with the Visual Studio C++ build tools it offers), then run this again.'
    }
    if ($Checkout) {
        $src = $Checkout
    } else {
        if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
            throw 'Building from source needs git: install Git for Windows (https://git-scm.com), then run this again.'
        }
        $src = Join-Path $Work 'src'
        $gitArgs = @('clone', '--quiet', '--depth', '1')
        if ($Tag) { $gitArgs += @('--branch', $Tag) }
        Say "Downloading the source from $RepoUrl"
        & git @gitArgs $RepoUrl $src
        if ($LASTEXITCODE -ne 0) { throw "Couldn't download the source from $RepoUrl." }
    }
    Say 'Building Tessera (this takes a few minutes the first time)'
    # The C runtime linked in, as in the ready-made build.
    $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = '-C target-feature=+crt-static'
    Push-Location $src
    try {
        & cargo build --release --locked -p tessera -p tessera-service
        if ($LASTEXITCODE -ne 0) { throw 'The build failed (see above).' }
    } finally { Pop-Location }
    $target = $env:CARGO_TARGET_DIR
    if (-not $target) { $target = Join-Path $src 'target' }
    $out = Join-Path $Work 'out'
    New-Item -ItemType Directory -Force -Path $out | Out-Null
    foreach ($exe in 'tessera.exe', 'tessera-service.exe') {
        Copy-Item -LiteralPath (Join-Path $target "release\$exe") -Destination $out
    }
    return $out
}

function Install-Tessera {
    $ProgressPreference = 'SilentlyContinue'
    $ErrorActionPreference = 'Stop'
    # Windows PowerShell 5.1 may still default to old TLS versions.
    try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12 } catch {}

    Write-Host ''
    Write-Host '  Tessera' -ForegroundColor Yellow -NoNewline
    Write-Host '  installer for Windows: everything it needs, then a guided setup'
    Write-Host ''

    if ($PSVersionTable.PSVersion.Major -lt 5) { throw 'This needs PowerShell 5.1 or newer (part of Windows 10 and 11).' }
    if (-not [Environment]::Is64BitOperatingSystem) { throw 'Tessera needs 64-bit Windows.' }
    if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
        Warn 'This is an ARM computer: the x64 build runs through Windows'' emulation (a bit slower).'
    }

    if (-not $Repo) { $Repo = $TesseraRepo }
    $Repo = $Repo.TrimEnd('/')
    if ($Version -and -not $Version.StartsWith('v')) { $Version = "v$Version" }
    if (-not $Dir) { $Dir = Join-Path $env:LOCALAPPDATA 'Programs\Tessera' }
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null

    $work = Join-Path ([IO.Path]::GetTempPath()) ("tessera-install-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    try {
        $files = $null
        $sums = $null
        $base = $null
        $here = $PSScriptRoot
        # A copy of the source around this script (scripts\install.ps1):
        # build that, as install.sh does.
        $checkout = $null
        if ($here) {
            $top = Split-Path -Parent $here
            if ((Test-Path -LiteralPath (Join-Path $top 'Cargo.toml')) -and (Test-Path -LiteralPath (Join-Path $top 'crates\cli'))) { $checkout = $top }
        }
        # 1. The ready-made build: a zip given, the folder this script was
        #    unpacked into, or the release.
        if ($Zip) {
            if (-not (Test-Path -LiteralPath $Zip)) { throw "There is no file $Zip." }
            Expand-Archive -LiteralPath $Zip -DestinationPath (Join-Path $work 'x') -Force
            $files = Join-Path $work 'x'
            Ok "Unpacked $([IO.Path]::GetFileName($Zip))"
        } elseif ($checkout) {
            $files = Build-FromSource $Repo $Version $work $checkout
        } elseif ($here -and (Test-Path -LiteralPath (Join-Path $here 'tessera.exe'))) {
            $files = $here
            Ok "Using the programs next to this script ($here)"
        } elseif (-not $Source) {
            try {
                $tag = $Version
                if (-not $tag) { $tag = Get-LatestTag $Repo }
                $base = "$Repo/releases/download/$tag"
                $sumsFile = Join-Path $work 'SHA256SUMS'
                Get-File "$base/SHA256SUMS" $sumsFile
                $sums = Read-Sums $sumsFile
                if (-not $sums.ContainsKey($ZipName)) { throw "the release $tag has no Windows build" }
                $zipFile = Join-Path $work $ZipName
                Say "Downloading $ZipName ($tag)"
                Get-File "$base/$ZipName" $zipFile
                if (-not (Test-Sum $zipFile $sums $ZipName)) {
                    throw "CHECKSUM: $ZipName doesn't match the release's checksum, so nothing was installed. Run this again; if it happens again, please report it."
                }
                Expand-Archive -LiteralPath $zipFile -DestinationPath (Join-Path $work 'x') -Force
                $files = Join-Path $work 'x'
                Ok "Downloaded Tessera $tag (checksum verified)"
            } catch {
                if ($_.Exception.Message.StartsWith('CHECKSUM: ')) { throw $_.Exception.Message.Substring(10) }
                Skip "No ready-made build: $($_.Exception.Message)"
            }
        }
        # 2. Otherwise from source.
        if (-not $files) { $files = Build-FromSource $Repo $Version $work }

        $found = @{}
        foreach ($exe in 'tessera.exe', 'tessera-service.exe') {
            $f = Get-ChildItem -LiteralPath $files -Recurse -Filter $exe | Select-Object -First 1
            if (-not $f) { throw "The build has no $exe." }
            $found[$exe] = $f.FullName
        }
        $before = Get-TesseraVersion (Join-Path $Dir 'tessera.exe')
        $newVersion = Get-TesseraVersion $found['tessera.exe']
        if (-not $newVersion) { throw "The new tessera.exe doesn't start on this computer." }
        foreach ($exe in $found.Keys) { Install-File $found[$exe] (Join-Path $Dir $exe) }
        $was = ''
        if ($before -and $before -ne $newVersion) { $was = " (was $before)" }
        Ok "$newVersion in $Dir$was"
        Add-ToPath $Dir
        $tessera = Join-Path $Dir 'tessera.exe'

        # 3. The desktop app, when the release has one.
        $desktop = $null
        if (-not $NoDesktop) {
            if ($base -and $sums -and $sums.ContainsKey($DesktopName)) {
                if (Ask 'Install the desktop app too?') {
                    $setup = Join-Path $work $DesktopName
                    Say "Downloading $DesktopName"
                    Get-File "$base/$DesktopName" $setup
                    if (-not (Test-Sum $setup $sums $DesktopName)) {
                        throw "$DesktopName doesn't match the release's checksum, so it wasn't installed."
                    }
                    $desktop = $setup
                }
            } elseif ($files) {
                $d = Get-ChildItem -LiteralPath $files -Recurse -Filter '*-setup.exe' -ErrorAction SilentlyContinue | Select-Object -First 1
                if ($d -and (Ask 'Install the desktop app too?')) { $desktop = $d.FullName }
            }
            if ($desktop) {
                # The app's own installer, silent and per user (no administrator rights).
                $p = Start-Process -FilePath $desktop -ArgumentList '/S' -Wait -PassThru
                if ($p.ExitCode -eq 0) { Ok 'The desktop app (Start menu: Tessera)' } else { Warn "The desktop app's installer stopped with code $($p.ExitCode)." }
            } else {
                Skip 'Desktop app: not in this release (the terminal app works on its own)'
            }
        }

        # 4. A running background service picks up the new version.
        $run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
        $entries = Get-ItemProperty -Path $run -ErrorAction SilentlyContinue
        if ($entries -and $entries.PSObject.Properties['Tessera'] -and
            (Test-StartupFromFolder $entries.Tessera $Dir)) {
            & $tessera service install | Out-Null
            if ($LASTEXITCODE -eq 0) { Ok 'Restarted the background service with the new version' } else { Warn 'Restart the background service with: tessera service install' }
        }
        if ($entries -and $entries.PSObject.Properties['Tessera machine'] -and
            (Test-StartupFromFolder $entries.'Tessera machine' $Dir)) {
            & $tessera node service install | Out-Null
        }

        # 5. Web views use Edge (part of Windows) or Chrome: nothing to download.
        Ok 'Web views: they use Microsoft Edge or Google Chrome, already on this computer'

        Write-Host ''
        if ((Test-Interactive) -and -not $Yes) {
            if (Ask 'Set up Tessera now (models, devices, running in the background)?') {
                & $tessera setup
                return
            }
        }
        Say 'Next: run  tessera setup  (in a new terminal window, so it finds tessera).'
    } finally {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}

try {
    Install-Tessera
} catch {
    Write-Host ''
    Write-Host '  xx  ' -ForegroundColor Red -NoNewline
    Write-Host $_.Exception.Message
    Write-Host ''
}
