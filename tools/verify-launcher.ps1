param(
    [string]$Text = "chrome",
    [string]$Region = ""
)

$root = "C:\Users\Sisyphus\Documents\private\快速启动软件"

$src = @"
using System;
using System.Runtime.InteropServices;
public class Verify {
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion U; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion {
    [FieldOffset(0)] public MOUSEINPUT mi;
    [FieldOffset(0)] public KEYBDINPUT ki;
  }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint n, INPUT[] p, int size);
  [DllImport("user32.dll")] public static extern int GetWindowLongW(IntPtr h, int i);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
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
    Key(0x11, false); System.Threading.Thread.Sleep(25);
    Key(0x20, false); System.Threading.Thread.Sleep(25);
    Key(0x20, true);  System.Threading.Thread.Sleep(25);
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
      System.Threading.Thread.Sleep(25);
    }
  }
}
"@
$path = Join-Path $env:TEMP "mavis-verify.cs"
Set-Content -Path $path -Value $src -Encoding UTF8
if (-not ("Verify" -as [type])) { Add-Type -Path $path }

$p = Get-Process -Name "ideas-launcher" -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $p) { "launcher is not running"; exit 1 }
$line = (& "$root\tools\enum-windows.ps1" -Pid_ $p.Id | Select-String "IDEAS Launcher")
$h = [IntPtr]([int](($line -join '') -replace '.*hwnd=(\d+).*', '$1'))

if ([Verify]::Visible($h)) {
    [Verify]::CtrlSpace()
    Start-Sleep -Milliseconds 600
}
[Verify]::CtrlSpace()
Start-Sleep -Milliseconds 700
"visible after hotkey : $([Verify]::Visible($h))"
"foreground is launcher: $([Verify]::GetForegroundWindow() -eq $h)"

if ($Text -ne "") {
    [Verify]::Text($Text)
    Start-Sleep -Milliseconds 900
}

$r = New-Object Verify+RECT
[void][Verify]::GetWindowRect($h, [ref]$r)
"window rect          : $($r.left),$($r.top) .. $($r.right),$($r.bottom)"

if ($Region -ne "") {
    $s = "C:\Users\Sisyphus\.codex\skills\screenshot\scripts\take_screenshot.ps1"
    powershell -ExecutionPolicy Bypass -File $s -Mode temp -Region $Region 2>&1 | Select-Object -Last 1
}
