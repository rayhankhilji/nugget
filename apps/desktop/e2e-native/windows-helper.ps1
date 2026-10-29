# Windows helper for run.mjs: reads one command per line on stdin, answers one JSON line.
# Keys and clicks through SendInput, windows through EnumWindows, the target's text through UI
# Automation, screenshots through System.Drawing. DPI aware, so coordinates are physical pixels
# (the ones the overlay is placed in).
#
#   trigger                  double tap Right Ctrl
#   enter                    press Enter
#   click <x> <y>            left click at a screen point
#   windows <title>          visible top-level windows whose title is <title>, with their rects
#   foreground               the foreground window's title
#   activate <substring>     bring the first window whose title contains <substring> to the front
#   read <substring>         the text of the document/edit control in that window (UIA)
#   screenshot <file> [x y w h]
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing, System.Windows.Forms
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class NuggetE2E {
    [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Explicit)] public struct InputUnion { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
    [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion u; }
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    public delegate bool EnumProc(IntPtr hwnd, IntPtr lParam);

    [DllImport("user32.dll", SetLastError = true)] static extern uint SendInput(uint n, INPUT[] inputs, int size);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out RECT r);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd, int cmd);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();

    public static string Title(IntPtr hwnd) {
        var s = new StringBuilder(512);
        GetWindowText(hwnd, s, s.Capacity);
        return s.ToString();
    }

    [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent, EnumProc cb, IntPtr lParam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr SendMessage(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "SendMessageW")] static extern IntPtr SendMessageText(IntPtr hwnd, uint msg, IntPtr w, StringBuilder l);

    /// The text of the first Edit / RichEdit child window (classic and Windows 11 Notepad), read
    /// with WM_GETTEXT from the window itself; null when there is none.
    public static string ChildEditText(IntPtr parent) {
        string found = null;
        EnumChildWindows(parent, (h, l) => {
            var c = new StringBuilder(256);
            GetClassName(h, c, c.Capacity);
            var cls = c.ToString();
            if (cls == "Edit" || cls.StartsWith("RichEdit")) {
                int len = (int)SendMessage(h, 0x000E, IntPtr.Zero, IntPtr.Zero); // WM_GETTEXTLENGTH
                var s = new StringBuilder(len + 1);
                SendMessageText(h, 0x000D, (IntPtr)(len + 1), s); // WM_GETTEXT
                found = s.ToString();
                return false;
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static List<IntPtr> Windows() {
        var all = new List<IntPtr>();
        EnumWindows((h, l) => { all.Add(h); return true; }, IntPtr.Zero);
        return all;
    }

    static void Send(INPUT input) {
        var sent = SendInput(1, new[] { input }, Marshal.SizeOf(typeof(INPUT)));
        if (sent != 1) throw new Exception("SendInput was blocked (error " + Marshal.GetLastWin32Error() + ")");
        System.Threading.Thread.Sleep(25);
    }

    public static void Key(ushort vk, ushort scan, bool extended, bool down) {
        var i = new INPUT { type = 1 };
        i.u.ki.wVk = vk;
        i.u.ki.wScan = scan;
        i.u.ki.dwFlags = (extended ? 1u : 0u) | (down ? 0u : 2u);
        Send(i);
    }

    public static void Tap(ushort vk, ushort scan, bool extended) {
        Key(vk, scan, extended, true);
        Key(vk, scan, extended, false);
        System.Threading.Thread.Sleep(40);
    }

    public static void Click(int x, int y) {
        SetCursorPos(x, y);
        System.Threading.Thread.Sleep(50);
        foreach (var flag in new uint[] { 2, 4 }) {
            var i = new INPUT { type = 0 };
            i.u.mi.dwFlags = flag;
            Send(i);
        }
    }
}
'@
[NuggetE2E]::SetProcessDPIAware() | Out-Null

function Reply($obj) {
    [Console]::Out.WriteLine(($obj | ConvertTo-Json -Compress -Depth 5))
    [Console]::Out.Flush()
}

function Find-Window([string]$substring) {
    foreach ($h in [NuggetE2E]::Windows()) {
        if ([NuggetE2E]::IsWindowVisible($h) -and ([NuggetE2E]::Title($h)).Contains($substring)) { return $h }
    }
    return [IntPtr]::Zero
}

function Read-Text([IntPtr]$hwnd) {
    $A = [System.Windows.Automation.AutomationElement]
    $root = $A::FromHandle($hwnd)
    $seen = @()
    foreach ($type in @([System.Windows.Automation.ControlType]::Document, [System.Windows.Automation.ControlType]::Edit)) {
        $cond = New-Object System.Windows.Automation.PropertyCondition($A::ControlTypeProperty, $type)
        foreach ($el in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)) {
            $seen += "$($el.Current.ControlType.ProgrammaticName):$($el.Current.ClassName)"
            $t = Text-Of $el
            if ($null -ne $t) { return $t }
        }
    }
    # Else the Edit / RichEdit child window itself (classic Notepad's Edit shows up in UIA as a
    # pane without a text or value pattern).
    $t = [NuggetE2E]::ChildEditText($hwnd)
    if ($null -ne $t) { return $t }
    # Else the focused element, when it is in the same process (the insertion left focus there).
    $focused = $A::FocusedElement
    if ($null -ne $focused -and $focused.Current.ProcessId -eq $root.Current.ProcessId) {
        $seen += "focused $($focused.Current.ControlType.ProgrammaticName):$($focused.Current.ClassName)"
        $t = Text-Of $focused
        if ($null -ne $t) { return $t }
    }
    throw "no document or edit control with text in $($root.Current.ClassName) '$($root.Current.Name)' (seen: $($seen -join ', '))"
}

function Text-Of($el) {
    $p = $null
    if ($el.TryGetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern, [ref]$p)) {
        return $p.DocumentRange.GetText(-1)
    }
    $p = $null
    if ($el.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$p)) {
        return $p.Current.Value
    }
    return $null
}

while ($true) {
    $line = [Console]::In.ReadLine()
    if ($null -eq $line) { break }
    $parts = $line.Split(' ', 2)
    $cmd = $parts[0]
    $arg = if ($parts.Count -gt 1) { $parts[1] } else { '' }
    try {
        switch ($cmd) {
            'trigger' {
                # Right Ctrl: VK_RCONTROL, scan code 0x1D, extended.
                [NuggetE2E]::Tap(0xA3, 0x1D, $true)
                [NuggetE2E]::Tap(0xA3, 0x1D, $true)
                Reply @{ ok = $true }
            }
            'enter' {
                [NuggetE2E]::Tap(0x0D, 0x1C, $false)
                Reply @{ ok = $true }
            }
            'click' {
                $xy = $arg.Split(' ')
                [NuggetE2E]::Click([int]$xy[0], [int]$xy[1])
                Reply @{ ok = $true }
            }
            'windows' {
                $found = @()
                foreach ($h in [NuggetE2E]::Windows()) {
                    if ([NuggetE2E]::Title($h) -ne $arg) { continue }
                    $r = New-Object NuggetE2E+RECT
                    [NuggetE2E]::GetWindowRect($h, [ref]$r) | Out-Null
                    $found += @{ id = [string]$h; title = $arg; visible = [NuggetE2E]::IsWindowVisible($h); x = $r.Left; y = $r.Top; w = $r.Right - $r.Left; h = $r.Bottom - $r.Top }
                }
                Reply @{ ok = $true; windows = @($found) }
            }
            'foreground' {
                Reply @{ ok = $true; title = [NuggetE2E]::Title([NuggetE2E]::GetForegroundWindow()) }
            }
            'activate' {
                $h = Find-Window $arg
                if ($h -eq [IntPtr]::Zero) { throw "no window titled *$arg*" }
                [NuggetE2E]::ShowWindow($h, 9) | Out-Null
                Reply @{ ok = [NuggetE2E]::SetForegroundWindow($h) }
            }
            'read' {
                # Every visible window with that title (Notepad can have more than one).
                $errors = @()
                $text = $null
                foreach ($h in [NuggetE2E]::Windows()) {
                    if (-not [NuggetE2E]::IsWindowVisible($h) -or -not ([NuggetE2E]::Title($h)).Contains($arg)) { continue }
                    try { $text = Read-Text $h; break } catch { $errors += $_.Exception.Message }
                }
                if ($null -eq $text) { throw "no window titled *$arg* has text: $($errors -join '; ')" }
                Reply @{ ok = $true; text = $text }
            }
            'screenshot' {
                $a = $arg.Split(' ')
                $file = $a[0]
                if ($a.Count -ge 5) {
                    $x = [int]$a[1]; $y = [int]$a[2]; $w = [int]$a[3]; $hh = [int]$a[4]
                } else {
                    $b = [System.Windows.Forms.SystemInformation]::VirtualScreen
                    $x = $b.Left; $y = $b.Top; $w = $b.Width; $hh = $b.Height
                }
                $bmp = New-Object System.Drawing.Bitmap $w, $hh
                $g = [System.Drawing.Graphics]::FromImage($bmp)
                $g.CopyFromScreen($x, $y, 0, 0, $bmp.Size)
                $bmp.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
                $g.Dispose(); $bmp.Dispose()
                Reply @{ ok = $true }
            }
            default { throw "unknown command $cmd" }
        }
    } catch {
        Reply @{ ok = $false; error = "$($_.Exception.Message)" }
    }
}
