param(
  # The process whose window gets the click.
  [Parameter(Mandatory = $true)]
  [int]$ProcessId,

  # Where to click, in physical screen pixels.
  [Parameter(Mandatory = $true)]
  [int]$X,

  [Parameter(Mandatory = $true)]
  [int]$Y,

  # The mouse button: "left" or "right".
  [ValidateSet('left', 'right')]
  [string]$Button = 'left',

  # Move the pointer only, without pressing a button.
  [switch]$Hover,

  # Point at a row of the result list instead of at a place on the screen.
  #
  # The row is counted from the top, starting at zero. The window moves to
  # wherever the pointer is, so a fixed place on the screen would hit a
  # different row every time.
  [int]$Row = -1
)

# Move the pointer to a place and click there.
#
# The point is in physical pixels, because that is what the screen uses. The
# window is found by its process, and the foreground is taken over the way
# `send-key.ps1` does it.

$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Threading;

public class ClickAt {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion U; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion {
    [FieldOffset(0)] public MOUSEINPUT mi;
    [FieldOffset(0)] public KEYBDINPUT ki;
  }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] p, int size);
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
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
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
    // Windows only lets a program take the foreground when the user has
    // "used" it recently. A tap of the Alt key is the oldest trick to say
    // "the keyboard is mine": after it, SetForegroundWindow is allowed.
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
    Thread.Sleep(150);
    return ok && GetForegroundWindow() == hwnd;
  }

  public static void Move(int x, int y) {
    SetCursorPos(x, y);
  }

  /// Press and release one mouse button.
  public static void Press(bool right) {
    uint down = right ? 0x0008u : 0x0002u;
    uint up = right ? 0x0010u : 0x0004u;
    INPUT[] input = new INPUT[1];
    input[0].type = 0;
    input[0].U.mi = new MOUSEINPUT { dx = 0, dy = 0, mouseData = 0, dwFlags = down, time = 0, dwExtraInfo = IntPtr.Zero };
    SendInput(1, input, Marshal.SizeOf(typeof(INPUT)));
    Thread.Sleep(80);
    input[0].U.mi.dwFlags = up;
    SendInput(1, input, Marshal.SizeOf(typeof(INPUT)));
  }
}
"@

[ClickAt]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null

$hwnd = [ClickAt]::Biggest([uint32]$ProcessId)
if ($hwnd -eq [IntPtr]::Zero) { throw "找不到进程 $ProcessId 的可见窗口" }

if ($Row -ge 0) {
  # Where the row sits on the screen.
  $rect = New-Object ClickAt+RECT
  [ClickAt]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
  # The list starts below the search box, and the rows are 62 points apart.
  $top = $rect.Top + [int]((78 + $Row * 62 + 29) * 1.5)
  $Y = $top
  Write-Host "第 $Row 行在屏幕 y=$Y(窗口顶边 $($rect.Top))"
}

[void][ClickAt]::StealForeground($hwnd)
Start-Sleep -Milliseconds 500

# Move first, so the window sees a hover before the click.
[ClickAt]::Move($X, $Y)
Start-Sleep -Milliseconds 400
if (-not $Hover) {
  [ClickAt]::Press($Button -eq 'right')
  Start-Sleep -Milliseconds 400
}
if ($Hover) { "hover at $X,$Y" } else { "$Button click at $X,$Y" }
