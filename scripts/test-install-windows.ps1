# Offline tests: pwsh -NoProfile -File scripts/test-install-windows.ps1
# Runs on Linux too. Parse the whole installer but load only its two pure/file
# helpers: never run setup, a server, a registry command or the installer itself.
$ErrorActionPreference = 'Stop'
$script = Join-Path $PSScriptRoot 'install.ps1'
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($script, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw "Installer has $($errors.Count) parse errors" }
foreach ($name in 'Install-File', 'Test-StartupFromFolder') {
    $fn = $ast.FindAll({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq $name }, $true)
    if ($fn.Count -ne 1) { throw "Expected one $name" }
    . ([scriptblock]::Create($fn[0].Extent.Text))
}
$count = 0
function Check($condition, [string]$why) {
    if (-not $condition) { throw $why }
    $script:count++
}
$folder = 'C:\Users\Ann Lee\Tessera'
foreach ($line in @('"C:\Users\Ann Lee\Tessera\tessera-service.exe" --log "D:\a b\server.log"',
                    '"c:\users\ann lee\tessera\tessera-service.exe" --log x')) {
    Check (Test-StartupFromFolder $line $folder) 'Quoted startup entry must match its own folder'
}
Check (Test-StartupFromFolder 'C:\Tessera\tessera-service.exe --log x' 'C:\Tessera\') 'Unquoted path and trailing slash'
Check (Test-StartupFromFolder '"C:\Tools (new)\Tessera\tessera-service.exe" --log x' 'C:\Tools (new)\Tessera') 'Regex characters in a folder are literal'
foreach ($line in @('', 'C:\Other\tessera-service.exe --log x',
                    '"C:\Users\Ann Lee\Tessera\tessera-service.exe.old" --log x',
                    '"C:\Other\tessera-service.exe" --log "C:\Users\Ann Lee\Tessera\tessera-service.exe"',
                    '"C:\Users\Ann Lee\Tessera\tessera-service.exe"extra --log x',
                    '"C:\Users\Ann Lee\Tessera2\tessera-service.exe" --log x')) {
    Check (-not (Test-StartupFromFolder $line $folder)) 'A different startup entry must be left alone'
}
$temp = Join-Path ([IO.Path]::GetTempPath()) ('tessera-installer-test-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temp | Out-Null
try {
    $from = Join-Path $temp 'download.exe'; $to = Join-Path $temp 'installed.exe'
    [IO.File]::WriteAllText($from, 'version one')
    Install-File $from $to
    Check ([IO.File]::ReadAllText($to) -eq 'version one') 'First install'
    [IO.File]::WriteAllText($from, 'version two')
    Install-File $from $to
    Check ([IO.File]::ReadAllText($to) -eq 'version two') 'Update publishes the new bytes'
    Check ([IO.File]::ReadAllText("$to.old") -eq 'version one') 'Update retains the previous bytes'
    Install-File $from $to
    Check (@(Get-ChildItem -LiteralPath $temp -Filter 'installed.exe.new-*').Count -eq 0) 'Repeated update clears temporary bytes'
    $failed = $false
    try { Install-File (Join-Path $temp 'missing.exe') $to } catch { $failed = $true }
    Check $failed 'A failed copy is reported'
    Check ([IO.File]::ReadAllText($to) -eq 'version two') 'A failed copy leaves the installed program'
    function Copy-Item {
        param([string]$LiteralPath, [string]$Destination, [switch]$Force)
        [IO.File]::WriteAllText($Destination, 'partial')
        throw 'test incomplete copy'
    }
    try {
        $failed = $false
        try { Install-File $from $to } catch { $failed = $true }
        Check $failed 'An incomplete copy is reported'
        Check ([IO.File]::ReadAllText($to) -eq 'version two') 'Incomplete copy leaves the installed program'
        Check (@(Get-ChildItem -LiteralPath $temp -Filter 'installed.exe.new-*').Count -eq 0) 'Incomplete copy is removed'
    } finally { Remove-Item Function:\Copy-Item }
    # Make only the first publication fail. Run the actual helper and actual
    # filesystem operations for backup, restore and temporary-file cleanup.
    $script:moves = 0
    function Move-Item {
        param([string]$LiteralPath, [string]$Destination, [switch]$Force)
        $script:moves++
        if ($script:moves -eq 1) { throw 'test publication failure' }
        Microsoft.PowerShell.Management\Move-Item -LiteralPath $LiteralPath -Destination $Destination -Force:$Force
    }
    try {
        [IO.File]::WriteAllText($from, 'version three')
        $failed = $false
        try { Install-File $from $to } catch { $failed = $true }
        Check $failed 'Publication failure is reported'
        Check ([IO.File]::ReadAllText($to) -eq 'version two') 'Publication failure restores the old program'
        Check (@(Get-ChildItem -LiteralPath $temp -Filter 'installed.exe.new-*').Count -eq 0) 'Publication failure removes temporary bytes'
    } finally { Remove-Item Function:\Move-Item }
} finally { Remove-Item -LiteralPath $temp -Recurse -Force }
Write-Host "ALL OK: $count Windows installer helper checks (offline, no Windows runtime)"
