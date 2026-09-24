# Dev helpers for exercising Smowauncher without touching the keyboard.
#   . .\tools\devtools.ps1
#   Send-Keys @(0x5B)            # tap LWin
#   Save-Screen out.png
#   Get-SmowMemory

Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class SmowInput {
    [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Explicit, Size = 40)] struct INPUT { [FieldOffset(0)] public uint type; [FieldOffset(8)] public KEYBDINPUT ki; }
    [DllImport("user32.dll")] static extern uint SendInput(uint n, INPUT[] inputs, int size);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr h, System.Text.StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    static INPUT Key(ushort vk, bool up) {
        var i = new INPUT(); i.type = 1; i.ki.wVk = vk; i.ki.dwFlags = up ? 2u : 0u; return i;
    }
    static INPUT Char(char c, bool up) {
        var i = new INPUT(); i.type = 1; i.ki.wScan = c; i.ki.dwFlags = 4u | (up ? 2u : 0u); return i;
    }
    public static void Down(ushort vk) { SendInput(1, new[] { Key(vk, false) }, Marshal.SizeOf(typeof(INPUT))); }
    public static void Up(ushort vk) { SendInput(1, new[] { Key(vk, true) }, Marshal.SizeOf(typeof(INPUT))); }
    public static void Text(string s) {
        foreach (var c in s) { SendInput(2, new[] { Char(c, false), Char(c, true) }, Marshal.SizeOf(typeof(INPUT))); }
    }
    public static string Foreground() {
        var h = GetForegroundWindow(); var c = new System.Text.StringBuilder(256); var t = new System.Text.StringBuilder(256);
        GetClassName(h, c, 256); GetWindowText(h, t, 256); return c + " | " + t;
    }
}
"@
[SmowInput]::SetProcessDPIAware() | Out-Null

function Tap([int[]]$vks) { foreach ($k in $vks) { [SmowInput]::Down($k) }; [array]::Reverse($vks); foreach ($k in $vks) { [SmowInput]::Up($k) } }
function Type-Text([string]$s) { [SmowInput]::Text($s) }
function Get-Foreground { [SmowInput]::Foreground() }

function Save-Screen([string]$path, [int]$x = 0, [int]$y = 0, [int]$w = 0, [int]$h = 0) {
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    if ($w -eq 0) { $x = $b.X; $y = $b.Y; $w = $b.Width; $h = $b.Height }
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($x, $y, 0, 0, $bmp.Size)
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
}

function Get-SmowMemory {
    Get-Process smowauncher -ErrorAction SilentlyContinue | Select-Object Id,
        @{n = 'WorkingSetMB'; e = { [math]::Round($_.WorkingSet64 / 1MB, 1) } },
        @{n = 'PrivateMB'; e = { [math]::Round($_.PrivateMemorySize64 / 1MB, 1) } },
        @{n = 'Threads'; e = { $_.Threads.Count } },
        @{n = 'CPUsec'; e = { [math]::Round($_.CPU, 2) } }
}
Add-Type -AssemblyName System.Windows.Forms
