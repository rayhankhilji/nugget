# Write a Tauri config fragment that gives the MSI a numeric version when the app version is a
# prerelease (or has build metadata). Tauri's MSI bundler (convert_version in tauri-bundler 2.9)
# accepts only a purely numeric prerelease or build ("1.0.0-4", "1.0.0+4") and fails on
# "1.0.0-rc.1" or "1.0.0-beta.2"; NSIS copes on its own. The fragment sets
# bundle.windows.wix.version to MAJOR.MINOR.PATCH.N, N being the trailing number of the
# prerelease (else of the build metadata), 0 if there is none. Windows Installer ignores the
# fourth field when it compares versions for an upgrade, so 1.0.0-rc.1 (1.0.0.1) and 1.0.0
# (1.0.0) count as the same version and the final release still replaces the release candidate
# (Tauri's WiX template allows same-version upgrades and downgrades).
#
# Usage: wix-version.ps1 -Config apps/desktop/src-tauri/tauri.conf.json -Out <fragment.json>
# Always writes -Out: `{}` for a plain MAJOR.MINOR.PATCH version. Exits 1 on a version WiX
# can't represent (major or minor > 255, patch or N > 65535).
#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Config,
    [Parameter(Mandatory = $true)][string]$Out
)
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$version = [string](Get-Content -Raw -LiteralPath $Config | ConvertFrom-Json).version
$m = [regex]::Match($version, '^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+([0-9A-Za-z.-]+))?$')
if (-not $m.Success) { [Console]::Error.WriteLine("wix-version: not a semver version: '$version'"); exit 1 }
$major = [uint64]$m.Groups[1].Value; $minor = [uint64]$m.Groups[2].Value; $patch = [uint64]$m.Groups[3].Value
$pre = $m.Groups[4].Value; $build = $m.Groups[5].Value

if (-not $pre -and -not $build) {
    '{}' | Out-File -FilePath $Out -Encoding utf8NoBOM
    Write-Output "wix-version: $version is plain, no override"
    exit 0
}
$tail = if ($pre) { $pre } else { $build }
$n = 0
$t = [regex]::Match($tail, '(\d+)$')
if ($t.Success) { $n = [uint64]$t.Groups[1].Value }
if ($major -gt 255 -or $minor -gt 255 -or $patch -gt 65535 -or $n -gt 65535) {
    [Console]::Error.WriteLine("wix-version: $version can't be an MSI version (major/minor <= 255, patch and the prerelease number <= 65535)")
    exit 1
}
$wix = "$major.$minor.$patch.$n"
@{ bundle = @{ windows = @{ wix = @{ version = $wix } } } } | ConvertTo-Json -Depth 5 -Compress |
    Out-File -FilePath $Out -Encoding utf8NoBOM
Write-Output "wix-version: $version -> MSI version $wix"
exit 0
