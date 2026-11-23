param(
  # The process whose window is measured.
  [Parameter(Mandatory = $true)]
  [int]$ProcessId
)

# Measure how long the launcher takes to disappear once it is no longer in
# front.
#
# The window is put in the front first, and then the front is given away.
# From that moment on the window is looked at until it is gone.

$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Threading;

public class Latency {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern IntPtr GetShellWindow();
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr SetFocus(IntPtr h);
  [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
  [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion U; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion {
    [FieldOffset(0)] public MOUSEINPUT mi;
    [FieldOffset(0)] public KEYBDINPUT ki;
  }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] p, int size);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }

  public static bool Visible(uint want) {
    bool found = false;
    EnumWindows((h, l) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid == want && IsWindowVisible(h)) {
        RECT r; GetWindowRect(h, out r);
        if (r.Right - r.Left > 200 && r.Bottom - r.Top > 100) found = true;
      }
      return true;
    }, IntPtr.Zero);
    return found;
  }

  /// The largest window of a process.
  public static IntPtr Largest(uint want) {
    IntPtr found = IntPtr.Zero;
    long best = 0;
    EnumWindows((h, l) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid == want && IsWindowVisible(h)) {
        RECT r; GetWindowRect(h, out r);
        long a = (long)(r.Right - r.Left) * (r.Bottom - r.Top);
        if (a > best) { best = a; found = h; }
      }
      return true;
    }, IntPtr.Zero);
    return found;
  }

  /// Take the front the way a program is allowed to: with a tap of the Alt
  /// key and the input queues of the two threads tied together.
  public static bool StealForeground(IntPtr hwnd) {
    INPUT[] alt = new INPUT[2];
    alt[0].type = 1;
    alt[0].U.ki = new KEYBDINPUT { wVk = 0x12, wScan = 0, dwFlags = 0, time = 0, dwExtraInfo = IntPtr.Zero };
    alt[1].type = 1;
    alt[1].U.ki = new KEYBDINPUT { wVk = 0x12, wScan = 0, dwFlags = 0x0002, time = 0, dwExtraInfo = IntPtr.Zero };
    SendInput(2, alt, Marshal.SizeOf(typeof(INPUT)));

    IntPtr fg = GetForegroundWindow();
    uint fgThread = GetWindowThreadProcessId(fg, out _);
    uint myThread = GetCurrentThreadId();
    bool attached = false;
    if (fgThread != myThread) { attached = AttachThreadInput(myThread, fgThread, true); }
    BringWindowToTop(hwnd);
    bool ok = SetForegroundWindow(hwnd);
    SetFocus(hwnd);
    if (attached) { AttachThreadInput(myThread, fgThread, false); }
    Thread.Sleep(120);
    return ok;
  }
}
"@

[Latency]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null

if (-not [Latency]::Visible([uint32]$ProcessId)) {
  Write-Host "窗口不在屏幕上,先把它唤出来"
  exit 1
}

# The launcher is put in the front first — that is the state the user sees.
[void][Latency]::StealForeground([Latency]::Largest([uint32]$ProcessId))
# The launcher needs a frame to notice that it is in front, or it will think
# that it never was in front and wait for its grace time.
Start-Sleep -Milliseconds 700

# The desktop takes the front. The clock starts after the switch, so that the
# work of the measuring script is not counted.
[void][Latency]::StealForeground([Latency]::GetShellWindow())
$start = [System.Diagnostics.Stopwatch]::StartNew()

$gone = $null
while ($start.ElapsedMilliseconds -lt 5000) {
  if (-not [Latency]::Visible([uint32]$ProcessId)) {
    $gone = $start.ElapsedMilliseconds
    break
  }
  Start-Sleep -Milliseconds 2
}
if ($null -eq $gone) {
  Write-Host "窗口五秒钟都没有消失"
} else {
  Write-Host "窗口在 $gone 毫秒后消失"
}