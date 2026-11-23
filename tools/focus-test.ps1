# ============================================================ 警告
# 这个脚本会**全局注入按键**、抢占前台焦点,并且反复启动/结束启动器。
# 运行时请不要使用电脑:它会打断你的输入和焦点。
# 只在无人值守的机器上跑。
# ============================================================
param(
  [string]$Text = "chrome",
  [int]$WaitMs = 900,
  [string]$Log = ""
)

# End-to-end focus / input probe.
#
# The launcher is started with its stderr redirected to a log file so that the
# egui event stream can be inspected afterwards. The probe itself must not
# create a visible console window, otherwise it would steal the foreground from
# the launcher and the launcher would hide itself again.

$root = "C:\Users\Sisyphus\Documents\private\快速启动软件"
$exe = "$root\target\release\ideas-launcher.exe"

Get-Process -Name "ideas-launcher" -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400

if ($Log -eq "") {
  $Log = Join-Path $env:TEMP "launcher-focus.log"
}
Remove-Item $Log -ErrorAction SilentlyContinue

$outLog = Join-Path $env:TEMP "launcher-focus.out.log"
$errLog = Join-Path $env:TEMP "launcher-focus.err.log"
Remove-Item $outLog, $errLog -ErrorAction SilentlyContinue

# A release build is a window program: it has no console, so the log goes to
# a file instead of to stderr.
$appLogDir = Join-Path $env:APPDATA "IdeasLauncher\Logs"

# `Start-Process` with a redirection target keeps the launcher completely
# detached from this shell, so the launcher can own the foreground window.
$proc = Start-Process -FilePath $exe -WorkingDirectory $root -PassThru `
  -RedirectStandardOutput $outLog -RedirectStandardError $errLog -WindowStyle Hidden

# Give the process time to register the hotkey and build its index.
Start-Sleep -Milliseconds 1500

$src = @"
using System;
using System.Runtime.InteropServices;
using System.Threading;
public class Probe {
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion U; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion {
    [FieldOffset(0)] public MOUSEINPUT mi;
    [FieldOffset(0)] public KEYBDINPUT ki;
  }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint n, INPUT[] p, int size);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern int GetWindowThreadProcessId(IntPtr h, out int pid);
  [DllImport("user32.dll")] public static extern IntPtr GetFocus();
  [DllImport("user32.dll")] public static extern int GetWindowLongW(IntPtr h, int i);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int left, top, right, bottom; }

  public static bool Visible(IntPtr h) { return (GetWindowLongW(h, -16) & 0x10000000) != 0; }
  public static void Key(ushort vk, bool up) {
    INPUT[] i = new INPUT[1];
    i[0].type = 1;
    i[0].U.ki = new KEYBDINPUT { wVk = vk, wScan = 0, dwFlags = (uint)(up ? 2 : 0), time = 0, dwExtraInfo = IntPtr.Zero };
    SendInput(1, i, Marshal.SizeOf(typeof(INPUT)));
  }
  public static void CtrlSpace() {
    Key(0x11, false); Thread.Sleep(30);
    Key(0x20, false); Thread.Sleep(30);
    Key(0x20, true);  Thread.Sleep(30);
    Key(0x11, true);
  }
  public static void Text(string s) {
    foreach (char c in s) {
      INPUT[] d = new INPUT[1];
      d[0].type = 1;
      d[0].U.ki = new KEYBDINPUT { wVk = 0, wScan = c, dwFlags = 0x0004, time = 0, dwExtraInfo = IntPtr.Zero };
      SendInput(1, d, Marshal.SizeOf(typeof(INPUT)));
      INPUT[] u = new INPUT[1];
      u[0].type = 1;
      u[0].U.ki = new KEYBDINPUT { wVk = 0, wScan = c, dwFlags = 0x0004 | 0x0002, time = 0, dwExtraInfo = IntPtr.Zero };
      SendInput(1, u, Marshal.SizeOf(typeof(INPUT)));
      Thread.Sleep(60);
    }
  }
}
"@
$path = Join-Path $env:TEMP "mavis-probe.cs"
Set-Content -Path $path -Value $src -Encoding UTF8
if (-not ("Probe" -as [type])) { Add-Type -Path $path }

# The launcher window is created by eframe; find it by window class + title.
$h = [IntPtr]::Zero
foreach ($w in (Get-Process -Id $proc.Id)) { }
$found = & "$root\tools\enum-windows.ps1" -Pid_ $proc.Id
$line = ($found | Select-String "IDEAS Launcher" | Select-Object -First 1)
if ($line) {
  $h = [IntPtr]([int](($line -join '') -replace '.*hwnd=(\d+).*', '$1'))
}
"launcher pid   : $($proc.Id)"
"launcher hwnd  : $h"
if ($h -eq [IntPtr]::Zero) { "no window found"; Stop-Process -Id $proc.Id -Force; exit 1 }

[Probe]::CtrlSpace()
Start-Sleep -Milliseconds $WaitMs

$fg = [Probe]::GetForegroundWindow()
$fgPid = 0
[void][Probe]::GetWindowThreadProcessId($fg, [ref]$fgPid)
"visible         : $([Probe]::Visible($h))"
"foreground hwnd : $fg  (launcher = $h)  match=$($fg -eq $h)"
"foreground proc : $((Get-Process -Id $fgPid -ErrorAction SilentlyContinue).ProcessName)"
"focused hwnd    : $([Probe]::GetFocus())"

if ($Text -ne "") {
  [Probe]::Text($Text)
  Start-Sleep -Milliseconds 700
}

$r = New-Object Probe+RECT
[void][Probe]::GetWindowRect($h, [ref]$r)
"rect            : $($r.left),$($r.top) .. $($r.right),$($r.bottom)"

"---- launcher log ----"
Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 300
# The release build writes to a log file, not to stderr.
Get-ChildItem $appLogDir -Filter "launcher-*.log" -ErrorAction SilentlyContinue |
  Sort-Object LastWriteTime -Descending | Select-Object -First 1 |
  ForEach-Object { Get-Content $_.FullName -Tail 40 }
if (Test-Path $errLog) { Get-Content $errLog -Tail 10 }
