param(
  # The process whose window gets the keys.
  [Parameter(Mandatory = $true)]
  [int]$ProcessId,

  # The virtual key codes to press, for example 0x22 (page down).
  [int[]]$Key = @(0x22),

  # How many times each key is pressed.
  [int]$Repeat = 1,

  # The pause between two presses.
  [int]$Gap = 250
)

# Press virtual keys in the window of a process.
#
# The unicode sender of `send-text.ps1` cannot press a key like the page down,
# because it sends characters and not key codes.
#
# Windows does not let a program take the foreground away from another one.
# `AttachThreadInput` is the way around it: the two threads share their input
# queue for a moment, and then the foreground can be moved.

$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Threading;

public class PressKey {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion U; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion {
    [FieldOffset(0)] public MOUSEINPUT mi;
    [FieldOffset(0)] public KEYBDINPUT ki;
  }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint n, INPUT[] p, int size);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr SetFocus(IntPtr h);
  [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
  [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }

  /// The biggest visible window of a process.
  public static IntPtr Biggest(uint want) {
    IntPtr found = IntPtr.Zero; long best = 0;
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

  /// Move the foreground to a window, even from a program that does not own it.
  public static bool StealForeground(IntPtr hwnd) {
    IntPtr fg = GetForegroundWindow();
    uint fgThread = GetWindowThreadProcessId(fg, out _);
    uint myThread = GetCurrentThreadId();
    bool attached = false;
    if (fgThread != myThread) { attached = AttachThreadInput(myThread, fgThread, true); }
    BringWindowToTop(hwnd);
    bool ok = SetForegroundWindow(hwnd);
    SetFocus(hwnd);
    if (attached) { AttachThreadInput(myThread, fgThread, false); }
    return ok;
  }

  [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();

  public static void Tap(int vk) {
    INPUT[] down = new INPUT[1];
    down[0].type = 1;
    down[0].U.ki = new KEYBDINPUT { wVk = (ushort)vk, wScan = 0, dwFlags = 0, time = 0, dwExtraInfo = IntPtr.Zero };
    SendInput(1, down, Marshal.SizeOf(typeof(INPUT)));
    INPUT[] up = new INPUT[1];
    up[0].type = 1;
    up[0].U.ki = new KEYBDINPUT { wVk = (ushort)vk, wScan = 0, dwFlags = 0x0002, time = 0, dwExtraInfo = IntPtr.Zero };
    SendInput(1, up, Marshal.SizeOf(typeof(INPUT)));
  }
}
"@

[PressKey]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null

$hwnd = [PressKey]::Biggest([uint32]$ProcessId)
if ($hwnd -eq [IntPtr]::Zero) { throw "找不到进程 $ProcessId 的可见窗口" }
$ok = [PressKey]::StealForeground($hwnd)
Start-Sleep -Milliseconds 600
"foreground: $ok"

foreach ($vk in $Key) {
  for ($i = 0; $i -lt $Repeat; $i++) {
    [PressKey]::Tap($vk)
    Start-Sleep -Milliseconds $Gap
  }
}
"pressed $($Key -join ',') x$Repeat"
