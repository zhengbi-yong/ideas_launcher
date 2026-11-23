param(
  # The process whose window gets the wheel.
  [Parameter(Mandatory = $true)]
  [int]$ProcessId,

  # Where the wheel is turned, in physical screen pixels.
  [Parameter(Mandatory = $true)]
  [int]$X,

  [Parameter(Mandatory = $true)]
  [int]$Y,

  # How many notches. A negative number scrolls up.
  [int]$Notches = -3
)

# Turn the wheel over a window.
#
# The launcher scrolls its list with the wheel, and the scroll bar can be
# dragged. Both are checked with this.

$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Threading;

public class Wheel {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
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
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
  [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }

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
    Thread.Sleep(150);
    return ok;
  }

  /// Turn the wheel. One notch is 120.
  public static void Turn(int notches) {
    for (int i = 0; i < Math.Abs(notches); i++) {
      uint delta = notches > 0 ? 120u : unchecked((uint)-120);
      mouse_event(0x0800, 0, 0, delta, UIntPtr.Zero);
      Thread.Sleep(60);
    }
  }
}
"@

[Wheel]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null

$hwnd = [Wheel]::Biggest([uint32]$ProcessId)
if ($hwnd -eq [IntPtr]::Zero) { throw "找不到进程 $ProcessId 的可见窗口" }
[void][Wheel]::StealForeground($hwnd)
Start-Sleep -Milliseconds 400
[Wheel]::SetCursorPos($X, $Y) | Out-Null
Start-Sleep -Milliseconds 300
[Wheel]::Turn($Notches)
"turned the wheel $Notches notches at $X,$Y"
