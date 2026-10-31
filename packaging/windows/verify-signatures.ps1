# Check that every Windows file users run is Authenticode-signed, chains to a trusted root and is
# timestamped: the NSIS setup exe and the MSI, and inside them nugget-desktop.exe, nuggetd.exe and
# (NSIS) uninstall.exe. The NSIS bundle is installed silently into a temp directory and the MSI is
# extracted with an administrative install, so the copies checked are the ones users get. Tauri
# signs the uninstaller through NSIS `!uninstfinalize`, which does not stop the build if signing
# fails, so this is the check that catches an unsigned uninstall.exe.
#
# Usage: verify-signatures.ps1 -BundleDir target\<triple>\release\bundle [-Thumbprint <sha1>] [-LogDir <dir>]
#   -Thumbprint  also require this signer certificate (the dry-run proof's throwaway cert)
#   -LogDir      write each signtool output and summary.md here
# Appends a table to $GITHUB_STEP_SUMMARY when set. Exits 1 if any check fails.
#
# Used by .github/workflows/release.yml (real releases with Windows signing) and
# .github/workflows/windows-signing.yml (the dry-run proof).
#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$BundleDir,
    [string]$Thumbprint,
    [string]$LogDir
)
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$script:failures = 0
$script:summary = @('## Windows signatures', '')
function Check([string]$name, [bool]$ok) {
    if ($ok) { Write-Output "ok   $name" } else { Write-Output "::error::FAIL $name"; $script:failures++ }
    $script:summary += "- $(if ($ok) { 'ok' } else { '**FAIL**' }): $name"
}

$signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.*\x64\signtool.exe" -ErrorAction SilentlyContinue |
    Sort-Object { [version]$_.Directory.Parent.Name } | Select-Object -Last 1 -ExpandProperty FullName
if (-not $signtool) { Write-Output '::error::signtool.exe not found'; exit 1 }
Write-Output "signtool: $signtool"
if ($LogDir) { New-Item -ItemType Directory -Force -Path $LogDir | Out-Null }

function Verify([string]$file, [string]$tag) {
    $out = & $signtool verify /pa /all /v $file 2>&1 | Out-String -Width 4096
    $code = $LASTEXITCODE
    if ($LogDir) {
        "signtool verify /pa /all /v $file`n(exit $code)`n$out" | Out-File -FilePath (Join-Path $LogDir "$tag.txt")
    }
    [pscustomobject]@{ Code = $code; Out = $out }
}

$nsis = @(Get-ChildItem (Join-Path $BundleDir 'nsis') -Filter '*-setup.exe' -ErrorAction SilentlyContinue)
$msi = @(Get-ChildItem (Join-Path $BundleDir 'msi') -Filter '*.msi' -ErrorAction SilentlyContinue)
Check 'one NSIS installer and one MSI' ($nsis.Count -eq 1 -and $msi.Count -eq 1)
if ($script:failures -gt 0) { exit 1 }
$nsis = $nsis[0].FullName; $msi = $msi[0].FullName

$work = Join-Path ([IO.Path]::GetTempPath()) ('nugget-verify-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$inst = Join-Path $work 'nsis-install'
$ext = Join-Path $work 'msi-extract'
New-Item -ItemType Directory -Force -Path $work | Out-Null
$installed = $false
try {
    # NSIS: /S silent, /D= the install directory (must come last, unquoted).
    $p = Start-Process -FilePath $nsis -ArgumentList '/S', "/D=$inst" -Wait -PassThru
    $installed = $true
    Check 'NSIS silent install' ($p.ExitCode -eq 0)
    $p = Start-Process -FilePath msiexec.exe -ArgumentList '/a', "`"$msi`"", '/qn', "TARGETDIR=`"$ext`"" -Wait -PassThru
    Check 'MSI administrative extract' ($p.ExitCode -eq 0)

    $app = @(Get-ChildItem $inst -Filter *.exe -ErrorAction SilentlyContinue | Where-Object { $_.Name -notin 'nuggetd.exe', 'uninstall.exe' })
    $msiApp = @(Get-ChildItem $ext -Recurse -Filter *.exe -ErrorAction SilentlyContinue | Where-Object { $_.Name -ne 'nuggetd.exe' })
    $msiD = @(Get-ChildItem $ext -Recurse -Filter nuggetd.exe -ErrorAction SilentlyContinue)
    Check 'installed layout: one app exe, nuggetd.exe, uninstall.exe' ($app.Count -eq 1 -and
        (Test-Path "$inst\nuggetd.exe") -and (Test-Path "$inst\uninstall.exe"))
    Check 'MSI layout: one app exe, nuggetd.exe' ($msiApp.Count -eq 1 -and $msiD.Count -eq 1)

    $targets = [ordered]@{ 'NSIS setup' = $nsis; 'MSI' = $msi }
    if ($app.Count -eq 1) { $targets['NSIS-installed app'] = $app[0].FullName }
    foreach ($n in 'nuggetd.exe', 'uninstall.exe') { if (Test-Path "$inst\$n") { $targets["NSIS-installed $n"] = "$inst\$n" } }
    if ($msiApp.Count -eq 1) { $targets['MSI-extracted app'] = $msiApp[0].FullName }
    if ($msiD.Count -eq 1) { $targets['MSI-extracted nuggetd.exe'] = $msiD[0].FullName }

    $table = @('', '| File | verify /pa /all | timestamped | signer |', '|---|---|---|---|')
    foreach ($k in $targets.Keys) {
        $f = $targets[$k]
        $r = Verify $f ($k -replace '\W', '-')
        $ok = $r.Code -eq 0 -and $r.Out -match 'Successfully verified'
        $ts = $r.Out -match 'The signature is timestamped' -and $r.Out -notmatch 'is not timestamped'
        Check "$k ($(Split-Path $f -Leaf)) verifies" $ok
        Check "$k is timestamped" $ts
        $signer = 'any trusted'
        if ($Thumbprint) {
            $match = $r.Out -match [regex]::Escape($Thumbprint)
            Check "$k is signed by $Thumbprint" $match
            $signer = if ($match) { 'expected cert' } else { 'OTHER' }
        }
        $table += "| $k ``$(Split-Path $f -Leaf)`` | $(if ($ok) {'pass'} else {'FAIL'}) | $(if ($ts) {'yes'} else {'NO'}) | $signer |"
    }
    $script:summary += $table
} finally {
    if ($installed -and (Test-Path "$inst\uninstall.exe")) {
        $u = Start-Process -FilePath "$inst\uninstall.exe" -ArgumentList '/S', "_?=$inst" -Wait -PassThru
        Write-Output "uninstall exit $($u.ExitCode)"
    }
    Remove-Item -Recurse -Force -LiteralPath $work -ErrorAction SilentlyContinue
}

if ($env:GITHUB_STEP_SUMMARY) { $script:summary | Out-File -FilePath $env:GITHUB_STEP_SUMMARY -Append -Encoding utf8 }
if ($LogDir) { $script:summary | Out-File -FilePath (Join-Path $LogDir 'summary.md') -Encoding utf8 }
if ($script:failures -gt 0) { Write-Output "::error::$($script:failures) signature check(s) failed"; exit 1 }
exit 0
