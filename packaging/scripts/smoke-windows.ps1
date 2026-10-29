# Smoke-test the Windows NSIS installer: install it silently into a throwaway directory, launch
# the installed app, check that the main window appears and that the app connects to the nuggetd
# sidecar installed next to it, then close the window the way a user would (WM_CLOSE), stop the
# daemon gracefully (CTRL_BREAK on its console) and uninstall silently.
# Usage: pwsh packaging/scripts/smoke-windows.ps1 path\to\Nugget_x.y.z_x64-setup.exe
#
# Needs an interactive desktop session (GitHub's windows-* runners have one) and the WebView2
# runtime. Everything runs against a throwaway NUGGET_HOME and install directory, never the
# user's own data.
#
# Environment:
#   SMOKE_ARTIFACTS          copy the logs (and a screenshot) here, pass or fail
#   SMOKE_CONNECT_TIMEOUT    seconds to wait for "connected to nuggetd" (default 90)
#   SMOKE_SPAWN_GRACE        seconds to wait for the app to start nuggetd itself (default 30)
param(
    [Parameter(Mandatory = $true)][string]$Installer
)
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $Installer -PathType Leaf)) {
    Write-Error "smoke: no such file: $Installer"
    exit 2
}
$Installer = (Resolve-Path -LiteralPath $Installer).Path

$connectTimeout = if ($env:SMOKE_CONNECT_TIMEOUT) { [int]$env:SMOKE_CONNECT_TIMEOUT } else { 90 }
$spawnGrace = if ($env:SMOKE_SPAWN_GRACE) { [int]$env:SMOKE_SPAWN_GRACE } else { 30 }
$installTimeout = 180
$windowTimeout = 60
$quitTimeout = 30
$daemonStopTimeout = 15

# ---- isolated environment -------------------------------------------------------------------

$work = Join-Path ([IO.Path]::GetTempPath()) ("nugget-smoke-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$installDir = Join-Path $work 'install'
$env:NUGGET_HOME = Join-Path $work 'nugget-home'
New-Item -ItemType Directory -Force -Path $work, $env:NUGGET_HOME | Out-Null
$env:NUGGET_TRAY = 'off' # without a tray, closing the main window quits the app
$env:NUGGET_LOG = 'info'
$env:NO_COLOR = '1'
# Keep WebView2's profile in the throwaway directory too (Tauri may set its own folder instead).
$env:WEBVIEW2_USER_DATA_FOLDER = Join-Path $work 'webview2'

$appLog = Join-Path $work 'app.log'
$appErr = Join-Path $work 'app.stderr.log'
$daemonLogDir = Join-Path $env:NUGGET_HOME 'data\logs' # crates/nuggetd/src/logging.rs
$screenshot = Join-Path $work 'screenshot.png'

$script:app = $null
$script:daemon = $null
$script:daemonExe = $null
$script:appAliveRequired = $false
$script:diagnosed = $false
$script:installed = $false
$script:uninstaller = $null
$startedBy = 'app'

$clock = [Diagnostics.Stopwatch]::StartNew()
function Elapsed([double]$since) { '{0:N1}' -f (($clock.Elapsed.TotalMilliseconds - $since) / 1000) }
function Now { $clock.Elapsed.TotalMilliseconds }
function Log([string]$msg) { Write-Host ("smoke [{0}s] {1}" -f (Elapsed 0), $msg) }

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class NuggetSmokeWin32 {
    delegate bool EnumProc(IntPtr hwnd, IntPtr lparam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr lparam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int max);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool PostMessage(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);

    // Top-level windows of a process: "hwnd<TAB>visible<TAB>title".
    public static List<string> Windows(int pid) {
        var found = new List<string>();
        EnumWindows((hwnd, _) => {
            uint owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner == (uint)pid) {
                var sb = new StringBuilder(512);
                GetWindowText(hwnd, sb, sb.Capacity);
                found.Add(hwnd.ToInt64() + "\t" + (IsWindowVisible(hwnd) ? 1 : 0) + "\t" + sb);
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    // What the title bar's close button ends up sending.
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

# Daily files named nuggetd.<YYYY-MM-DD>.log sort by date; the newest is the last.
function Daemon-LogFile {
    if (-not (Test-Path -LiteralPath $daemonLogDir)) { return $null }
    $f = Get-ChildItem -LiteralPath $daemonLogDir -Filter 'nuggetd*.log' -File | Sort-Object Name | Select-Object -Last 1
    if ($f) { return $f.FullName } else { return $null }
}

function Diagnostics {
    if ($script:diagnosed) { return }
    $script:diagnosed = $true
    Write-Host "================ app log ($appLog) ================"
    Write-Host (Read-Shared $appLog)
    Write-Host '================ app stderr ================'
    Write-Host (Read-Shared $appErr)
    Write-Host '================ daemon log ================'
    $f = Daemon-LogFile
    if ($f) { Write-Host (Read-Shared $f) } else { Write-Host "(none under $daemonLogDir)" }
    if ($script:app -and -not $script:app.HasExited) {
        Write-Host "================ windows of pid $($script:app.Id) ================"
        [NuggetSmokeWin32]::Windows($script:app.Id) | ForEach-Object { Write-Host $_ }
    }
    Write-Host '================ processes ================'
    Get-CimInstance Win32_Process | Where-Object { $_.Name -match 'nugget|Nugget|msedgewebview2' } |
        Format-Table ProcessId, ParentProcessId, Name, ExecutablePath -AutoSize | Out-String -Width 250 | Write-Host
    Write-Host '================ install dir ================'
    if (Test-Path -LiteralPath $installDir) { Get-ChildItem -LiteralPath $installDir -Recurse | Select-Object -First 60 FullName, Length | Format-Table | Out-String -Width 250 | Write-Host }
}

class SmokeFailure : Exception { SmokeFailure([string]$m) : base($m) {} }
function Fail([string]$msg) { throw [SmokeFailure]::new($msg) }

# Poll `cond` every 250 ms for up to `timeout` seconds. While the app is expected to be running,
# stop early if it exits.
function Wait-Until([int]$timeout, [string]$what, [scriptblock]$cond) {
    $deadline = (Now) + $timeout * 1000
    while (-not (& $cond)) {
        if ($script:appAliveRequired -and $script:app.HasExited) {
            Fail "the app exited (status $($script:app.ExitCode)) while waiting for: $what"
        }
        if ((Now) -ge $deadline) { return $false }
        Start-Sleep -Milliseconds 250
    }
    return $true
}

# nuggetd processes started from this run's install directory.
function Find-Daemons {
    if (-not $script:daemonExe) { return @() }
    @(Get-CimInstance Win32_Process -Filter "Name = 'nuggetd.exe'" | Where-Object { $_.ExecutablePath -eq $script:daemonExe })
}

function Main-Window {
    foreach ($w in [NuggetSmokeWin32]::Windows($script:app.Id)) {
        $parts = $w -split "`t", 3
        if ($parts[1] -eq '1' -and $parts[2] -eq 'Nugget') { return [long]$parts[0] }
    }
    return $null
}

function App-Logged([string]$text) { (Read-Shared $appLog).Contains($text) }
function Daemon-Logged([string]$text) { $f = Daemon-LogFile; $f -and (Read-Shared $f).Contains($text) }

# A graceful stop: CTRL_BREAK to the daemon's own process group (the app starts it with
# CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW, so it has a hidden console of its own; nuggetd
# shuts down on Ctrl+Break, crates/nuggetd/src/main.rs). Sending it means attaching to that
# console, which a process can do only after leaving its own, so it runs in a helper process.
function Send-CtrlBreak([int]$daemonPid) {
    $helper = Join-Path $work 'ctrl-break.ps1'
    $out = Join-Path $work 'ctrl-break.out'
    @'
param([int]$Target, [string]$Out)
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class NuggetCtrlBreak {
    [DllImport("kernel32.dll", SetLastError = true)] public static extern bool FreeConsole();
    [DllImport("kernel32.dll", SetLastError = true)] public static extern bool AttachConsole(uint pid);
    [DllImport("kernel32.dll", SetLastError = true)] public static extern bool SetConsoleCtrlHandler(IntPtr handler, bool add);
    [DllImport("kernel32.dll", SetLastError = true)] public static extern bool GenerateConsoleCtrlEvent(uint ev, uint group);
    public static string Send(uint pid) {
        FreeConsole();
        if (!AttachConsole(pid)) return "AttachConsole failed: error " + Marshal.GetLastWin32Error();
        SetConsoleCtrlHandler(IntPtr.Zero, true);
        if (!GenerateConsoleCtrlEvent(1, pid)) return "GenerateConsoleCtrlEvent failed: error " + Marshal.GetLastWin32Error();
        return "ok";
    }
}
"@
$r = [NuggetCtrlBreak]::Send([uint32]$Target)
[IO.File]::WriteAllText("$Out.tmp", $r)
Move-Item -LiteralPath "$Out.tmp" -Destination $Out
# Stay attached a moment: the daemon should be gone (cleanly) before this helper leaves.
Start-Sleep -Seconds 3
if ($r -eq 'ok') { exit 0 } else { exit 1 }
'@ | Set-Content -LiteralPath $helper -Encoding utf8
    $shell = (Get-Process -Id $PID).Path
    $p = Start-Process -FilePath $shell -ArgumentList '-NoProfile', '-NonInteractive', '-File', "`"$helper`"", $daemonPid, "`"$out`"" `
        -WindowStyle Hidden -PassThru
    $null = $p.Handle
    $sent = Wait-Until 30 'the CTRL_BREAK helper' { (Test-Path -LiteralPath $out) -or $p.HasExited }
    $result = if (Test-Path -LiteralPath $out) { Get-Content -LiteralPath $out -Raw } else { "(no result; helper status $($p.ExitCode))" }
    if (-not $sent -or $result -ne 'ok') { Fail "could not send CTRL_BREAK to nuggetd: $result" }
    Log 'CTRL_BREAK sent to the daemon''s process group'
}

function Cleanup([bool]$failed) {
    if ($failed) { Diagnostics }
    try { if ($script:app -and -not $script:app.HasExited) { Stop-Process -Id $script:app.Id -Force -ErrorAction SilentlyContinue } } catch {}
    foreach ($d in (Find-Daemons)) { Stop-Process -Id $d.ProcessId -Force -ErrorAction SilentlyContinue }
    if ($script:installed -and $script:uninstaller -and (Test-Path -LiteralPath $script:uninstaller)) {
        Start-Process -FilePath $script:uninstaller -ArgumentList '/S', "_?=$installDir" -Wait -ErrorAction SilentlyContinue | Out-Null
    }
    if ($env:SMOKE_ARTIFACTS) {
        New-Item -ItemType Directory -Force -Path $env:SMOKE_ARTIFACTS | Out-Null
        foreach ($f in @($appLog, $appErr, $screenshot, (Join-Path $work 'ctrl-break.out'))) {
            if (Test-Path -LiteralPath $f) { Copy-Item -LiteralPath $f -Destination $env:SMOKE_ARTIFACTS }
        }
        if (Test-Path -LiteralPath $daemonLogDir) {
            Get-ChildItem -LiteralPath $daemonLogDir -Filter 'nuggetd*.log' | ForEach-Object {
                [IO.File]::WriteAllText((Join-Path $env:SMOKE_ARTIFACTS $_.Name), (Read-Shared $_.FullName))
            }
        }
    }
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

$failed = $true
try {
    # ---- silent install ---------------------------------------------------------------------

    Log ("installer: $Installer ({0:N1} MiB)" -f ((Get-Item -LiteralPath $Installer).Length / 1MB))
    $tInstall = Now
    # NSIS: /S silent, /D= the install directory (must come last, unquoted).
    $inst = Start-Process -FilePath $Installer -ArgumentList '/S', "/D=$installDir" -PassThru
    $null = $inst.Handle
    if (-not $inst.WaitForExit($installTimeout * 1000)) { Fail "the installer did not finish within ${installTimeout}s" }
    $script:installed = $true
    if ($inst.ExitCode -ne 0) { Fail "the installer exited with status $($inst.ExitCode)" }
    $tInstalled = Elapsed $tInstall
    $exes = @(Get-ChildItem -LiteralPath $installDir -Filter '*.exe' -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -notin @('nuggetd.exe', 'uninstall.exe') })
    if ($exes.Count -ne 1) { Fail "expected one app executable in $installDir (does the installer honour /D=?), found $($exes.Count)" }
    $exe = $exes[0].FullName
    $script:daemonExe = Join-Path $installDir 'nuggetd.exe'
    $script:uninstaller = Join-Path $installDir 'uninstall.exe'
    if (-not (Test-Path -LiteralPath $script:daemonExe)) { Fail "no nuggetd sidecar at $($script:daemonExe)" }
    if (-not (Test-Path -LiteralPath $script:uninstaller)) { Fail "no uninstaller at $($script:uninstaller)" }
    Log "installed silently in ${tInstalled}s: $($exes[0].Name) + nuggetd.exe in $installDir"

    # ---- launch -----------------------------------------------------------------------------

    $tLaunch = Now
    # Redirected, so the app's log (stdout) lands in a file even though it is a GUI program.
    $script:app = Start-Process -FilePath $exe -WorkingDirectory $installDir -PassThru `
        -RedirectStandardOutput $appLog -RedirectStandardError $appErr
    $null = $script:app.Handle # keep the handle, so the exit code survives the process
    $script:appAliveRequired = $true
    Log "launched (pid $($script:app.Id))"

    if (-not (Wait-Until $windowTimeout 'the main window' { $null -ne (Main-Window) })) {
        Fail "no visible top-level window titled 'Nugget' within ${windowTimeout}s"
    }
    $tWindow = Elapsed $tLaunch
    $hwnd = Main-Window
    Log "window 'Nugget' is up (hwnd $hwnd) after ${tWindow}s"

    # ---- daemon: spawned by the app, connected ---------------------------------------------

    if (Wait-Until $spawnGrace 'the app to start nuggetd' { App-Logged 'starting nuggetd' }) {
        $line = ((Read-Shared $appLog) -split "`n" | Where-Object { $_.Contains('starting nuggetd') } | Select-Object -First 1)
        Log "the app is starting nuggetd: $($line -replace '^.*exe=', '')"
    } elseif (@(Find-Daemons).Count -gt 0) {
        $startedBy = 'already running'
    } else {
        Fail "the app did not log 'starting nuggetd' within ${spawnGrace}s"
    }

    if (-not (Wait-Until $connectTimeout 'connected to nuggetd' { App-Logged 'connected to nuggetd' })) {
        Fail "the app did not log 'connected to nuggetd' within ${connectTimeout}s"
    }
    $tConnect = Elapsed $tLaunch
    Log "app connected to nuggetd after ${tConnect}s"

    if (-not (Wait-Until 15 'a nuggetd process' { @(Find-Daemons).Count -gt 0 })) {
        Fail "no nuggetd process started from $($script:daemonExe)"
    }
    $script:daemon = @(Find-Daemons)[0]
    Log "nuggetd running (pid $($script:daemon.ProcessId), parent $($script:daemon.ParentProcessId))"

    # Logged once the daemon listens (crates/nuggetd/src/daemon.rs).
    if (-not (Wait-Until 15 "the daemon's ready line" { Daemon-Logged '"nuggetd ready"' })) {
        Fail "no 'nuggetd ready' line in the daemon log under $daemonLogDir"
    }
    Log "daemon log: $(Daemon-LogFile)"

    try {
        Add-Type -AssemblyName System.Windows.Forms, System.Drawing
        $b = [Windows.Forms.SystemInformation]::VirtualScreen
        $bmp = New-Object Drawing.Bitmap $b.Width, $b.Height
        $g = [Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen($b.Left, $b.Top, 0, 0, $bmp.Size)
        $bmp.Save($screenshot, [Drawing.Imaging.ImageFormat]::Png)
        $g.Dispose(); $bmp.Dispose()
        Log 'screenshot taken'
    } catch { Log "screenshot failed (ignored): $_" }

    # ---- quit -------------------------------------------------------------------------------

    $tQuit = Now
    if (-not [NuggetSmokeWin32]::Close($hwnd)) { Fail 'could not post WM_CLOSE to the window' }
    $script:appAliveRequired = $false
    if (-not $script:app.WaitForExit($quitTimeout * 1000)) {
        Fail "the app did not exit within ${quitTimeout}s of closing its window"
    }
    $script:app.WaitForExit()
    if ($script:app.ExitCode -ne 0) { Fail "the app exited with status $($script:app.ExitCode)" }
    $tExit = Elapsed $tQuit
    Log "app exited cleanly (status 0) ${tExit}s after the close"

    # The daemon is a background service and outlives the app by design.
    $daemonProc = Get-Process -Id $script:daemon.ProcessId -ErrorAction SilentlyContinue
    if (-not $daemonProc) { Fail 'nuggetd exited with the app; it should keep running' }
    $null = $daemonProc.Handle
    if ($daemonProc.WaitForExit(2000)) {
        Fail ('nuggetd exited (status 0x{0:X8}) within 2s of the app; it should keep running' -f $daemonProc.ExitCode)
    }
    $tTerm = Now
    Send-CtrlBreak $script:daemon.ProcessId
    if (-not $daemonProc.WaitForExit($daemonStopTimeout * 1000)) {
        Fail "nuggetd did not exit within ${daemonStopTimeout}s of CTRL_BREAK"
    }
    $daemonProc.WaitForExit()
    $tStop = Elapsed $tTerm
    $daemonStatus = $daemonProc.ExitCode
    # Buffered log lines are flushed on exit; "stopped" is the last line of a clean shutdown.
    if (-not (Daemon-Logged '"stopped"')) {
        $seen = if (Daemon-Logged 'console control signal received') { 'it logged the signal' } else { 'it never logged the signal' }
        Fail ("nuggetd exited (status 0x{0:X8}) without logging 'stopped' (unclean shutdown; {1})" -f $daemonStatus, $seen)
    }
    if ($daemonStatus -ne 0) { Fail "nuggetd exited with status $daemonStatus" }
    Log "nuggetd stopped cleanly ${tStop}s after CTRL_BREAK (status 0)"

    # ---- silent uninstall -------------------------------------------------------------------

    $tUninstall = Now
    # _?= runs the uninstaller in place and waits (otherwise it copies itself to %TEMP% and
    # returns at once); it then leaves itself and the directory, which we check and remove.
    $un = Start-Process -FilePath $script:uninstaller -ArgumentList '/S', "_?=$installDir" -PassThru
    $null = $un.Handle
    if (-not $un.WaitForExit($installTimeout * 1000)) { Fail "the uninstaller did not finish within ${installTimeout}s" }
    if ($un.ExitCode -ne 0) { Fail "the uninstaller exited with status $($un.ExitCode)" }
    $script:installed = $false
    $left = @(Get-ChildItem -LiteralPath $installDir -Recurse -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -ne 'uninstall.exe' })
    if ($left.Count -gt 0) { Fail "the uninstaller left files behind: $(($left | ForEach-Object Name) -join ', ')" }
    $tUninstalled = Elapsed $tUninstall
    Log "uninstalled silently in ${tUninstalled}s"

    $failed = $false
    Write-Host @"

PASS: Windows NSIS smoke test ($(Split-Path -Leaf $Installer))
  silent install             ${tInstalled}s
  window 'Nugget' shown      ${tWindow}s after launch
  connected to nuggetd       ${tConnect}s after launch (daemon started by: $startedBy)
  app exit after WM_CLOSE    ${tExit}s (status 0)
  nuggetd stop after BREAK   ${tStop}s
  silent uninstall           ${tUninstalled}s
  total                      $(Elapsed 0)s
"@
} catch {
    $msg = if ($_.Exception -is [SmokeFailure]) { $_.Exception.Message } else { "$($_.Exception.Message) at $($_.InvocationInfo.PositionMessage)" }
    Write-Host "smoke: FAIL: $msg"
    if ($env:GITHUB_ACTIONS) { Write-Host "::error::Windows NSIS smoke test: $msg" }
} finally {
    Cleanup $failed
}
if ($failed) { exit 1 }
exit 0
