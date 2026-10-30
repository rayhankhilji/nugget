# Release rehearsal, Windows x64: the end-to-end checks, after prepare.sh has built and signed
# everything and the throwaway CA is in the Windows root store (workflow step, test only).
#
#   1. Wrong feed key: beta.json signed by a key the app doesn't trust is refused; nothing
#      installs.
#   2. Wrong payload key: a correctly signed feed whose payload signature is by the wrong key is
#      offered, but the download fails verification; nothing installs.
#   3. Update: with auto-install on, the OLD build (installed silently, per user, into a temp
#      directory) finds the signed update on the HTTPS localhost feed, downloads and verifies it,
#      and when its window is closed runs the NEW NSIS installer in passive mode and exits. The
#      installed nugget-desktop.exe must then carry the new version.
#
# Each launch runs like packaging/scripts/smoke-windows.ps1 (throwaway NUGGET_HOME and WebView2
# profile, NUGGET_TRAY=off, closed with WM_CLOSE) with a fresh settings.json in
# %APPDATA%\dev.nugget.desktop (removed at the end). Every wait is bounded (the app's first
# update check runs 60 s after launch). Results go to the evidence report, logs of every case to
# $EVIDENCE\logs\<case>\. Exits 1 if any check failed.
#
# Usage: pwsh e2e-windows.ps1 -RehearsalDir DIR -OldVersion V -NewVersion V -Port N
param(
    [Parameter(Mandatory = $true)][string]$RehearsalDir,
    [Parameter(Mandatory = $true)][string]$OldVersion,
    [Parameter(Mandatory = $true)][string]$NewVersion,
    [Parameter(Mandatory = $true)][int]$Port
)
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$scripts = $PSScriptRoot
$evidence = Join-Path $RehearsalDir 'evidence'
$results = Join-Path $evidence 'results.tsv'
$tls = Join-Path $RehearsalDir 'tls'
$checkTimeout = 240   # first check at 60 s, plus download time
$windowTimeout = 90
$quitTimeout = 90
$installTimeout = 300
$script:fails = 0

$old = @(Get-ChildItem -LiteralPath (Join-Path $RehearsalDir 'build\old') -Filter '*-setup.exe')
if ($old.Count -ne 1) { throw "expected one old installer, found $($old.Count)" }
$old = $old[0].FullName

# RUNNER_TEMP rather than %TEMP%, whose 8.3 short form (RUNNER~1) would not match the long
# executable paths the process list reports.
$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$work = Join-Path $tempRoot ('nugget-rehearsal-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$installDir = Join-Path $work 'install'
New-Item -ItemType Directory -Force -Path $work | Out-Null
$configDir = Join-Path $env:APPDATA 'dev.nugget.desktop'
$settingsFile = Join-Path $configDir 'settings.json'
$python = (Get-Command python -ErrorAction SilentlyContinue)
if (-not $python) { $python = Get-Command python3 }
$python = $python.Source

foreach ($v in 'HTTPS_PROXY', 'https_proxy', 'HTTP_PROXY', 'http_proxy', 'ALL_PROXY', 'all_proxy') {
    Remove-Item -LiteralPath "Env:$v" -ErrorAction SilentlyContinue
}
$env:NO_PROXY = 'localhost,127.0.0.1,::1'
$env:NUGGET_TRAY = 'off'
$env:NUGGET_LOG = 'info'
$env:NO_COLOR = '1'

$clock = [Diagnostics.Stopwatch]::StartNew()
function Log([string]$msg) { Write-Host ('rehearsal [{0:N1}s] {1}' -f $clock.Elapsed.TotalSeconds, $msg) }

function Record([string]$check, [string]$status, [string]$detail) {
    $detail = ($detail -replace "[`t`r`n]+", ' ')
    [IO.File]::AppendAllText($results, "$check`t$status`t$detail`n", (New-Object Text.UTF8Encoding($false)))
    Write-Host "rehearsal: [$status] ${check}: $detail"
    if ($status -eq 'FAIL') { $script:fails++ }
}

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class NuggetRehearsalWin32 {
    delegate bool EnumProc(IntPtr hwnd, IntPtr lparam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr lparam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int max);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool PostMessage(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);

    // The visible top-level window titled exactly `title` of process `pid`, or 0.
    public static long Find(int pid, string title) {
        long found = 0;
        EnumWindows((hwnd, _) => {
            uint owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner == (uint)pid && IsWindowVisible(hwnd)) {
                var sb = new StringBuilder(512);
                GetWindowText(hwnd, sb, sb.Capacity);
                if (sb.ToString() == title) { found = hwnd.ToInt64(); return false; }
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static bool Close(long hwnd) {
        const uint WM_CLOSE = 0x0010;
        return PostMessage(new IntPtr(hwnd), WM_CLOSE, IntPtr.Zero, IntPtr.Zero);
    }
}
'@

# Read a file another process is still writing.
function Read-Shared([string]$path) {
    if (-not (Test-Path -LiteralPath $path)) { return '' }
    try {
        $fs = [IO.File]::Open($path, 'Open', 'Read', 'ReadWrite, Delete')
        try { return (New-Object IO.StreamReader($fs)).ReadToEnd() } finally { $fs.Dispose() }
    } catch { return '' }
}

function Wait-Until([int]$timeout, [scriptblock]$cond) {
    $deadline = [DateTime]::UtcNow.AddSeconds($timeout)
    while (-not (& $cond)) {
        if ([DateTime]::UtcNow -ge $deadline) { return $false }
        Start-Sleep -Milliseconds 500
    }
    return $true
}

function Sha256([string]$path) { (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() }

function App-Exe { Join-Path $installDir 'nugget-desktop.exe' }
function Installed-Version {
    $exe = App-Exe
    if (-not (Test-Path -LiteralPath $exe)) { return '(not installed)' }
    # A fresh read each time: the file is replaced by the update.
    [Diagnostics.FileVersionInfo]::GetVersionInfo($exe).ProductVersion
}

# Processes started from the install directory (the app, nuggetd).
function Install-Processes {
    @(Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($installDir, [StringComparison]::OrdinalIgnoreCase) })
}
# A running NSIS installer (the updater starts the downloaded *-setup.exe in passive mode).
function Setup-Processes {
    @(Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and $_.ExecutablePath -like '*-setup.exe' })
}

function Stop-InstallProcesses {
    foreach ($p in (Install-Processes)) { Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue }
    $null = Wait-Until 30 { @(Install-Processes).Count -eq 0 }
}

# ---- feed server -------------------------------------------------------------------------------

$script:server = $null
function Start-Feed([string]$variant, [string]$caseDir) {
    Stop-Feed
    $ready = Join-Path $work 'server-ready'
    Remove-Item -LiteralPath $ready -ErrorAction SilentlyContinue
    $argList = @('-I', "`"$(Join-Path $scripts 'feed-server.py')`"",
        '--root', "`"$(Join-Path $RehearsalDir "serve\$variant")`"", '--port', $Port,
        '--cert', "`"$(Join-Path $tls 'server.crt')`"", '--key', "`"$(Join-Path $tls 'server.key')`"",
        '--log', "`"$(Join-Path $caseDir 'feed-access.log')`"", '--ready-file', "`"$ready`"")
    $script:server = Start-Process -FilePath $python -ArgumentList $argList -PassThru `
        -RedirectStandardError (Join-Path $caseDir 'feed-server.log') -RedirectStandardOutput (Join-Path $caseDir 'feed-server.out')
    if (-not (Wait-Until 20 { Test-Path -LiteralPath $ready })) { throw "the feed server did not start (see $caseDir\feed-server.log)" }
}
function Stop-Feed {
    if ($script:server -and -not $script:server.HasExited) {
        Stop-Process -Id $script:server.Id -Force -ErrorAction SilentlyContinue
        $null = $script:server.WaitForExit(10000) # the port must be free for the next case
    }
    $script:server = $null
}

# ---- one launch --------------------------------------------------------------------------------

$script:app = $null
$script:appLog = $null
$script:caseDir = $null

function New-Case([string]$name) {
    $script:caseDir = Join-Path $evidence "logs\$name"
    New-Item -ItemType Directory -Force -Path $script:caseDir | Out-Null
    $w = Join-Path $work $name
    $env:NUGGET_HOME = Join-Path $w 'nh'
    $env:WEBVIEW2_USER_DATA_FOLDER = Join-Path $w 'webview2'
    New-Item -ItemType Directory -Force -Path $env:NUGGET_HOME | Out-Null
    # Beta channel (where a prerelease is published), auto-install: download and verify in the
    # background, install at quit.
    New-Item -ItemType Directory -Force -Path $configDir | Out-Null
    [IO.File]::WriteAllText($settingsFile, '{"channel":"beta","auto_install":true}', (New-Object Text.UTF8Encoding($false)))
}

function Launch([string]$logName) {
    $script:appLog = Join-Path $script:caseDir $logName
    $script:app = Start-Process -FilePath (App-Exe) -WorkingDirectory $installDir -PassThru `
        -RedirectStandardOutput $script:appLog -RedirectStandardError ($script:appLog + '.stderr')
    $null = $script:app.Handle
    return (Wait-Until $windowTimeout { $script:app.HasExited -or [NuggetRehearsalWin32]::Find($script:app.Id, 'Nugget') -ne 0 }) -and -not $script:app.HasExited
}

function Logged([string]$text) { (Read-Shared $script:appLog).Contains($text) }
function Wait-Logged([int]$timeout, [string]$text) {
    $null = Wait-Until $timeout { (Logged $text) -or $script:app.HasExited }
    return (Logged $text)
}
function First-Line([string]$text) {
    $l = (Read-Shared $script:appLog) -split "`n" | Where-Object { $_.Contains($text) } | Select-Object -First 1
    if ($l) { $l.Trim() } else { '' }
}

# WM_CLOSE, as the title bar's close button sends; returns the exit code or 'timeout'.
function Close-App {
    $h = [NuggetRehearsalWin32]::Find($script:app.Id, 'Nugget')
    if ($h -ne 0) { $null = [NuggetRehearsalWin32]::Close($h) }
    if (-not $script:app.WaitForExit($quitTimeout * 1000)) { return 'timeout' }
    $script:app.WaitForExit()
    return $script:app.ExitCode
}

function End-Case {
    Stop-InstallProcesses
    $logs = Join-Path $env:NUGGET_HOME 'data\logs'
    if (Test-Path -LiteralPath $logs) {
        Get-ChildItem -LiteralPath $logs -Filter 'nuggetd*.log' | ForEach-Object {
            [IO.File]::WriteAllText((Join-Path $script:caseDir $_.Name), (Read-Shared $_.FullName))
        }
    }
    Stop-Feed
}

function Show-Log {
    Write-Host "---- app log ($script:appLog) ----"
    Write-Host (Read-Shared $script:appLog)
}

# ---- cases -------------------------------------------------------------------------------------

# minisign's error when a signature was made by another key: the only refusal these cases accept
# ("download or signature check failed" alone would also cover a 404 or a TLS error).
$keyMismatch = 'different key than the one provided'

function Line-Of([string]$text) {
    $lines = (Read-Shared $script:appLog) -split "`n"
    for ($i = 0; $i -lt $lines.Count; $i++) { if ($lines[$i].Contains($text)) { return $i + 1 } }
    return 0
}

function Served([string]$pattern) {
    (Read-Shared (Join-Path $script:caseDir 'feed-access.log')) -match ('"GET /' + $pattern + ' HTTP/1\.[01]" 200')
}

function Case-Negative([string]$name, [string]$label) {
    $c = "$label (Windows NSIS)"
    New-Case $name
    Start-Feed $name $script:caseDir
    Log "case $name"
    if (-not (Launch 'app.log')) { Record "${c}: old app starts" 'FAIL' "no window within ${windowTimeout}s"; Show-Log; End-Case; return }
    if ($name -eq 'wrong-feed-key') {
        $null = Wait-Logged $checkTimeout 'update check failed'
        $ok = (Logged 'update check failed') -and (Logged 'the update feed failed signature verification') -and
            (Logged $keyMismatch) -and (Served 'feed/beta\.json\.sig') -and -not (Logged 'update available')
        $want = "'update check failed ... the update feed failed signature verification ... $keyMismatch', beta.json.sig served, no update offered"
        $line = 'update check failed'
    } else {
        $null = Wait-Logged $checkTimeout 'update download failed'
        $avail = Line-Of 'update available'
        $dl = Line-Of 'update download failed'
        # Offered first, the payload really downloaded (HTTP 200), then refused for the key alone.
        $ok = $avail -gt 0 -and $dl -gt $avail -and (Served 'dist/[^ ]+-setup\.exe') -and
            (Logged 'download or signature check failed') -and (Logged $keyMismatch) -and -not (Logged 'update downloaded')
        $want = "'update available' (line $avail), then 'update download failed ... $keyMismatch' (line $dl), a 200 for the payload, and no 'update downloaded'"
        $line = 'update download failed'
    }
    if ($ok) {
        Record "${c}: refused" 'PASS' ((First-Line $line) -replace '^.*?(WARN|INFO) ', '')
    } else {
        Record "${c}: refused" 'FAIL' "within ${checkTimeout}s, need $want"
        Show-Log
    }
    $st = Close-App
    # An installer would have started by now.
    $setup = Wait-Until 10 { @(Setup-Processes).Count -gt 0 }
    $v = Installed-Version
    if ($v -eq $OldVersion -and -not $setup -and -not (Logged 'installing downloaded update')) {
        Record "${c}: nothing installed" 'PASS' "no installer started; nugget-desktop.exe still $v; exit status $st"
    } else {
        Record "${c}: nothing installed" 'FAIL' "installer started: $setup; nugget-desktop.exe is $v"
        foreach ($p in (Setup-Processes)) { Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue }
    }
    End-Case
}

function Case-Update {
    $c = 'Update (Windows NSIS)'
    New-Case 'update'
    Start-Feed 'good' $script:caseDir
    Log "case update: $OldVersion -> $NewVersion"
    if (-not (Launch 'app.log')) { Record "${c}: old app starts" 'FAIL' "no window within ${windowTimeout}s"; Show-Log; End-Case; return }
    if (Wait-Logged $checkTimeout 'update available') {
        Record "${c}: old app finds the update on the signed feed" 'PASS' ((First-Line 'update available') -replace '^.*?INFO ', '')
    } else {
        Record "${c}: old app finds the update on the signed feed" 'FAIL' "no 'update available' within ${checkTimeout}s"; Show-Log; End-Case; return
    }
    if (Wait-Logged $checkTimeout 'update downloaded; installs at quit') {
        Record "${c}: payload downloaded and its signature verified" 'PASS' "logged 'update downloaded; installs at quit'"
    } else {
        Record "${c}: payload downloaded and its signature verified" 'FAIL' "no 'update downloaded; installs at quit' within ${checkTimeout}s"; Show-Log; End-Case; return
    }
    $st = Close-App
    if ($st -eq 0 -and (Logged 'installing downloaded update at quit') -and -not (Logged 'update install at quit failed')) {
        Record "${c}: hands over to the installer when the window is closed" 'PASS' "logged 'installing downloaded update at quit'; exit status 0"
    } else {
        Record "${c}: hands over to the installer when the window is closed" 'FAIL' "exit status $st"; Show-Log
    }
    # The NSIS hooks stop nuggetd and the passive installer replaces the files. Wait for the
    # installer to appear and then to finish before looking at the result.
    $t = [Diagnostics.Stopwatch]::StartNew()
    $started = Wait-Until 60 { @(Setup-Processes).Count -gt 0 -or (Installed-Version) -eq $NewVersion }
    $finished = Wait-Until $installTimeout { @(Setup-Processes).Count -eq 0 }
    if (-not $finished) {
        Record "${c}: the passive installer finishes" 'FAIL' "still running after ${installTimeout}s"
    } else {
        Record "${c}: the passive installer finishes" 'PASS' ("installer seen: $started; done {0:N0}s after the window closed" -f $t.Elapsed.TotalSeconds)
    }
    if ((Installed-Version) -eq $NewVersion) {
        Record "${c}: installed nugget-desktop.exe has the new version" 'PASS' "ProductVersion $NewVersion (was $OldVersion)"
    } else {
        Record "${c}: installed nugget-desktop.exe has the new version" 'FAIL' "ProductVersion is $(Installed-Version)"
        Get-CimInstance Win32_Process | Where-Object { $_.Name -match 'nugget|Nugget|setup' } |
            Format-Table ProcessId, ParentProcessId, Name, ExecutablePath -AutoSize | Out-String -Width 250 | Write-Host
    }
    # The passive installer restarts the app when it finishes (Tauri's /R); informational.
    $relaunched = Wait-Until 30 { @(Install-Processes | Where-Object { $_.Name -eq 'nugget-desktop.exe' }).Count -gt 0 }
    Record "${c}: installer restarted the app" 'INFO' $(if ($relaunched) { 'nugget-desktop.exe running again from the install directory' } else { 'no restart seen within 30s' })
    $access = Read-Shared (Join-Path $script:caseDir 'feed-access.log')
    if ($access -match '"GET /feed/beta\.json HTTP/1\.[01]" 200' -and $access -match '"GET /feed/beta\.json\.sig HTTP/1\.[01]" 200' -and $access -match '"GET /dist/[^ ]+-setup\.exe HTTP/1\.[01]" 200') {
        Record "${c}: fetched the feed, its signature and the payload over HTTPS" 'PASS' 'see logs/update/feed-access.log'
    } else {
        Record "${c}: fetched the feed, its signature and the payload over HTTPS" 'FAIL' ($access -replace "`n", ' ')
    }
    # The sidecar the old app started was built with the throwaway manifest key.
    $logs = Join-Path $env:NUGGET_HOME 'data\logs'
    $dlog = if (Test-Path -LiteralPath $logs) { (Get-ChildItem -LiteralPath $logs -Filter 'nuggetd*.log' | ForEach-Object { Read-Shared $_.FullName }) -join "`n" } else { '' }
    if ($dlog.Contains('"nuggetd ready"') -and -not $dlog.Contains('bundled model manifest failed verification')) {
        Record 'Manifest: the bundled nuggetd.exe (started by the app) accepts the signed manifest' 'PASS' 'no verification failure in its log'
    } else {
        Record 'Manifest: the bundled nuggetd.exe (started by the app) accepts the signed manifest' 'FAIL' 'no ready line, or a verification failure'
    }
    End-Case
}

$failed = $true
$installed = $false
try {
    # ---- silent per-user install of the OLD build --------------------------------------------
    Log "installing $old into $installDir"
    $inst = Start-Process -FilePath $old -ArgumentList '/S', "/D=$installDir" -PassThru
    $null = $inst.Handle
    if (-not $inst.WaitForExit($installTimeout * 1000)) { throw "the installer did not finish within ${installTimeout}s" }
    $installed = $true
    if ($inst.ExitCode -ne 0) { throw "the installer exited with status $($inst.ExitCode)" }
    $v = Installed-Version
    if ($v -eq $OldVersion) { Record 'Old build installed silently (per user)' 'PASS' "nugget-desktop.exe $v in a temp directory" }
    else { Record 'Old build installed silently (per user)' 'FAIL' "nugget-desktop.exe reports $v"; throw 'install failed' }

    Case-Negative 'wrong-feed-key' 'Wrong feed key'
    Case-Negative 'wrong-payload-key' 'Wrong payload key'
    Case-Update
    $failed = $false
} catch {
    Record 'Windows rehearsal ran to the end' 'FAIL' "$($_.Exception.Message) at $($_.InvocationInfo.PositionMessage)"
    if ($script:appLog) { Show-Log }
} finally {
    Stop-Feed
    foreach ($p in (Setup-Processes)) { Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue }
    Stop-InstallProcesses
    $un = Join-Path $installDir 'uninstall.exe'
    if ($installed -and (Test-Path -LiteralPath $un)) {
        $p = Start-Process -FilePath $un -ArgumentList '/S', "_?=$installDir" -PassThru
        $null = $p.Handle
        $null = $p.WaitForExit(180 * 1000)
    }
    Remove-Item -LiteralPath $settingsFile -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
Log "done: $($script:fails) failed check(s)"
if ($failed -or $script:fails -gt 0) { exit 1 }
exit 0
