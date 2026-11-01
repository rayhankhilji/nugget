# Check what the NSIS uninstaller removes, without a UI: seed Nugget's default data folders,
# a vault and a saved key, then uninstall silently with each command-line choice and look at what
# is left.
#
#   /S                          keeps everything
#   /S /REMOVEDATA              removes data, models, settings and saved keys; keeps the vault
#   /S /REMOVEDATA /REMOVEVAULT also removes %USERPROFILE%\Nugget Vault
#
# Usage: pwsh packaging/scripts/smoke-windows-uninstall.ps1 path\to\Nugget_x.y.z_x64-setup.exe
#
# It writes to the real per-user locations (%LOCALAPPDATA%\Nugget, %APPDATA%\Nugget,
# %USERPROFILE%\Nugget Vault, Credential Manager), so it refuses to run outside CI.
param(
    [Parameter(Mandatory = $true)][string]$Installer
)
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

if ($env:GITHUB_ACTIONS -ne 'true') { Write-Error 'smoke: this test writes to your real Nugget folders; CI only'; exit 2 }
$Installer = (Resolve-Path -LiteralPath $Installer).Path

$data = Join-Path $env:LOCALAPPDATA 'Nugget\Nugget'
$config = Join-Path $env:APPDATA 'Nugget\Nugget'
$vault = Join-Path $env:USERPROFILE 'Nugget Vault'
$installDir = Join-Path $env:LOCALAPPDATA 'Nugget'
$key = 'smoke-provider.dev.nugget.Nugget'

function Log([string]$m) { Write-Output "uninstall-smoke: $m" }
function Fail([string]$m) { Write-Output "::error::Windows uninstall smoke: $m"; throw $m }
function Has-Key { (cmdkey /list | Out-String).Contains($key) }

function Install {
    $p = Start-Process -FilePath $Installer -ArgumentList '/S' -PassThru -Wait
    if ($p.ExitCode -ne 0) { Fail "install exited with $($p.ExitCode)" }
    if (-not (Test-Path -LiteralPath (Join-Path $installDir 'uninstall.exe'))) { Fail "no uninstall.exe in $installDir" }
}

function Seed {
    foreach ($d in @("$data\data\models", "$data\data\logs", "$data\cache", $config, $vault)) {
        New-Item -ItemType Directory -Force -Path $d | Out-Null
    }
    Set-Content -LiteralPath "$data\data\models\model.gguf" -Value 'not a model'
    Set-Content -LiteralPath "$data\data\store.db" -Value 'not a store'
    Set-Content -LiteralPath "$config\config.toml" -Value '# smoke'
    Set-Content -LiteralPath "$vault\note.md" -Value '# my note'
    cmdkey /generic:$key /user:smoke /pass:smoke | Out-Null
    if (-not (Has-Key)) { Fail 'could not seed the Credential Manager entry' }
}

function Uninstall([string[]]$flags) {
    $un = Join-Path $installDir 'uninstall.exe'
    # _?= runs it in place and waits for it.
    $p = Start-Process -FilePath $un -ArgumentList (@('/S') + $flags + "_?=$installDir") -PassThru -Wait
    if ($p.ExitCode -ne 0) { Fail "uninstall $($flags -join ' ') exited with $($p.ExitCode)" }
    Remove-Item -LiteralPath $un -Force -ErrorAction SilentlyContinue
}

function Expect([string]$what, [bool]$present, [bool]$want) {
    if ($present -ne $want) { Fail "$what is $(if ($present) { 'still there' } else { 'gone' }); expected $(if ($want) { 'kept' } else { 'removed' })" }
    Log "  $what $(if ($want) { 'kept' } else { 'removed' })"
}

$cases = @(
    @{ flags = @(); data = $true; vault = $true },
    @{ flags = @('/REMOVEDATA'); data = $false; vault = $true },
    @{ flags = @('/REMOVEDATA', '/REMOVEVAULT'); data = $false; vault = $false }
)
try {
    foreach ($c in $cases) {
        Log "case: /S $($c.flags -join ' ')"
        Install
        Seed
        Uninstall $c.flags
        Expect 'models' (Test-Path -LiteralPath "$data\data\models\model.gguf") $c.data
        Expect 'store' (Test-Path -LiteralPath "$data\data\store.db") $c.data
        Expect 'config' (Test-Path -LiteralPath "$config\config.toml") $c.data
        Expect 'saved key' (Has-Key) $c.data
        Expect 'vault note' (Test-Path -LiteralPath "$vault\note.md") $c.vault
        # Start the next case from nothing.
        cmd /c rmdir /s /q "$data" 2>$null
        cmd /c rmdir /s /q "$config" 2>$null
        cmd /c rmdir /s /q "$vault" 2>$null
        cmdkey /delete:$key 2>$null | Out-Null
    }
    Log 'PASS: /S keeps everything; /REMOVEDATA removes data, models, settings and keys but not the vault; /REMOVEVAULT removes the vault too'
} finally {
    cmdkey /delete:$key 2>$null | Out-Null
}
