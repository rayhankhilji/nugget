# Removes Heed's saved keys from Windows Credential Manager. Run by the NSIS uninstaller
# (uninstall-data.nsh) when the user ticks "Remove models and Heed's data"; also usable by hand:
#   powershell -NoProfile -ExecutionPolicy Bypass -File remove-nugget-keys.ps1
#
# Heed keeps every secret under the keychain service "app.heed.Heed" (nugget-core secrets.rs).
# keyring-rs stores each one as a generic credential whose target name is "<account>.<service>":
#   store-key-<16 hex>.app.heed.Heed   the store's database key, one per data folder
#   provider:<name>.app.heed.Heed      API keys
#   oauth:<source>.app.heed.Heed       account sign-ins (Gmail, Microsoft 365, ...)
#   password:<source>.app.heed.Heed    IMAP and CalDAV passwords
#
# A store key belongs to one data folder: "store-key-" + the first 16 hex digits of the SHA-256 of
# that folder's absolute path (nugget-store key.rs). Only the default folder's key is removed. Keys
# of other data folders (NUGGET_HOME, --home) are kept, because those stores are kept too and are
# unreadable without them.
#
# Exit code: the number of Heed credentials that should have been removed but are still there.

$ErrorActionPreference = 'Continue'
# Heed's keychain service, and the ones before the rename (left in place by the first launch).
$suffixes = @('.app.heed.Heed', '.dev.nugget.Nugget', '.nugget')

function Get-StoreKeyName([string]$dataDir) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $digest = $sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($dataDir))
    $hex = -join ($digest | ForEach-Object { $_.ToString('x2') })
    return 'store-key-' + $hex.Substring(0, 16)
}
# The store key's account, for the data folder now and the one before the rename.
$ownStoreKeys = @(
    (Get-StoreKeyName (Join-Path $env:LOCALAPPDATA 'heed\Heed\data')),
    (Get-StoreKeyName (Join-Path $env:LOCALAPPDATA 'Nugget\Nugget\data'))
)

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
        if ($name) {
            foreach ($suffix in $suffixes) {
                if ($name.EndsWith($suffix, [System.StringComparison]::OrdinalIgnoreCase)) {
                    $name
                    break
                }
            }
        }
    }
}

function Test-ShouldRemove([string]$name) {
    if ($name.StartsWith('store-key-', [System.StringComparison]::OrdinalIgnoreCase)) {
        foreach ($own in $ownStoreKeys) {
            foreach ($suffix in $suffixes) {
                if ([string]::Equals($name, $own + $suffix, [System.StringComparison]::OrdinalIgnoreCase)) {
                    return $true
                }
            }
        }
        return $false
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
