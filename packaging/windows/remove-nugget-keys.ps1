# Removes Heed's saved keys from Windows Credential Manager. Run by the NSIS uninstaller
# (uninstall-data.nsh) when the user ticks "Remove models and Heed's data"; also usable by hand:
#   powershell -NoProfile -ExecutionPolicy Bypass -File remove-nugget-keys.ps1
#
# Heed keeps every secret under the keychain service "dev.nugget.Nugget" (nugget-core secrets.rs).
# keyring-rs stores each one as a generic credential whose target name is "<account>.<service>":
#   store-key-<16 hex>.dev.nugget.Nugget   the store's database key, one per data folder
#   provider:<name>.dev.nugget.Nugget      API keys
#   oauth:<source>.dev.nugget.Nugget       account sign-ins (Gmail, Microsoft 365, ...)
#   password:<source>.dev.nugget.Nugget    IMAP and CalDAV passwords
#
# A store key belongs to one data folder: "store-key-" + the first 16 hex digits of the SHA-256 of
# that folder's absolute path (nugget-store key.rs). Only the default folder's key is removed. Keys
# of other data folders (NUGGET_HOME, --home) are kept, because those stores are kept too and are
# unreadable without them.
#
# Exit code: the number of Heed credentials that should have been removed but are still there.

$ErrorActionPreference = 'Continue'
$suffix = '.dev.nugget.Nugget'

$dataDir = Join-Path $env:LOCALAPPDATA 'Nugget\Nugget\data'
$sha = [System.Security.Cryptography.SHA256]::Create()
$digest = $sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($dataDir))
$hex = -join ($digest | ForEach-Object { $_.ToString('x2') })
$ownStoreKey = 'store-key-' + $hex.Substring(0, 16) + $suffix

# `cmdkey /list` prints "Target: LegacyGeneric:target=<name>" for generic credentials. The
# "target=" part is not translated; the second pattern covers an output without it.
function Get-NuggetTargets {
    foreach ($line in @(& cmdkey.exe /list)) {
        $name = $null
        if ($line -match 'target=(.+)$') {
            $name = $Matches[1].Trim()
        } elseif ($line -match ':\s+(\S.*)$') {
            $name = $Matches[1].Trim()
        }
        if ($name -and $name.EndsWith($suffix, [System.StringComparison]::OrdinalIgnoreCase)) {
            $name
        }
    }
}

function Test-ShouldRemove([string]$name) {
    if ($name.StartsWith('store-key-', [System.StringComparison]::OrdinalIgnoreCase)) {
        return [string]::Equals($name, $ownStoreKey, [System.StringComparison]::OrdinalIgnoreCase)
    }
    return $true
}

$removed = 0
$kept = 0
foreach ($name in @(Get-NuggetTargets | Sort-Object -Unique)) {
    if (-not (Test-ShouldRemove $name)) {
        Write-Output ('Kept ' + $name + ' (the key of another Heed data folder)')
        $kept++
        continue
    }
    & cmdkey.exe ('/delete:' + $name) | Out-Null
    if ($LASTEXITCODE -eq 0) {
        Write-Output ('Removed ' + $name)
        $removed++
    } else {
        Write-Output ('Could not remove ' + $name)
    }
}

$left = @(Get-NuggetTargets | Sort-Object -Unique | Where-Object { Test-ShouldRemove $_ }).Count
Write-Output ('Saved keys: ' + $removed + ' removed, ' + $kept + ' kept, ' + $left + ' could not be removed')
exit $left
