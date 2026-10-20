# Run from any folder with Rust MSVC, Node 22 and NSIS available.
# Tauri merges tauri.windows.conf.json on Windows; the installer also takes
# windows/installer.conf.json, which bundles the two helpers built below.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path $PSScriptRoot -Parent
function Run([scriptblock] $Command) {
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "Release command failed with exit code $LASTEXITCODE" }
}
Push-Location $root
try {
    Push-Location apps/tessera/ui
    try {
        Run { npm ci --ignore-scripts --no-audit --no-fund }
        Run { npm run build }
    } finally { Pop-Location }
    $env:TESSERA_WEB_APP = Join-Path $root 'apps/tessera/ui/dist'
    $env:RUSTFLAGS = '-C target-feature=+crt-static'
    $target = 'x86_64-pc-windows-msvc'
    Run { cargo build --release --locked --target $target -p tessera -p tessera-service }
    $metadata = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
    $release = Join-Path $metadata.target_directory "$target/release"
    $resources = Join-Path $root 'apps/tessera/src-tauri/windows/resources'
    New-Item -ItemType Directory -Force $resources | Out-Null
    Copy-Item "$release/tessera.exe", "$release/tessera-service.exe" $resources
    $v = (Select-String -Path scripts/install.sh -Pattern '^TAURI_CLI=(.*)$').Matches[0].Groups[1].Value
    Run { cargo install tauri-cli --version $v --locked }
    Push-Location apps/tessera/src-tauri
    try { Run { cargo tauri build --target $target --bundles nsis --config windows/installer.conf.json -- --locked } }
    finally { Pop-Location }
    $setups = @(Get-ChildItem "$release/bundle/nsis/*-setup.exe")
    if ($setups.Count -ne 1) { throw 'Expected exactly one NSIS installer. Clean stale bundle output and retry.' }
    New-Item -ItemType Directory -Force out | Out-Null
    Copy-Item $setups[0].FullName out/tessera-desktop-windows-x86_64-setup.exe
    Write-Output 'Installer ready in out/tessera-desktop-windows-x86_64-setup.exe'
} finally { Pop-Location }
