param(
  # The process whose window gets the drag.
  [Parameter(Mandatory = $true)]
  [int]$ProcessId,

  # Where the drag starts, in physical screen pixels.
  #
  # The names do not start with the same letters as the end of the drag:
  # PowerShell would read -Y as a short form of -ToY.
  [Parameter(Mandatory = $true)]
  [int]$AtX,

  [Parameter(Mandatory = $true)]
  [int]$AtY,

  # Where the drag ends.
  [Parameter(Mandatory = $true)]
  [int]$ToX,

  [Parameter(Mandatory = $true)]
  [int]$ToY
)

# Drag something in a window — the scroll bar, for instance.
#
# The pointer is pressed, moved in steps, and released, so that the program
# under it sees a real drag and not a jump.

$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Threading;

public class Drag {
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

  public static void Move(int x, int y) { SetCursorPos(x, y); }

  public static void Button(bool down) {
    INPUT[] input = new INPUT[1];
    input[0].type = 0;
    input[0].U.mi = new MOUSEINPUT { dx = 0, dy = 0, mouseData = 0, dwFlags = down ? 0x0002u : 0x0004u, time = 0, dwExtraInfo = IntPtr.Zero };
    SendInput(1, input, Marshal.SizeOf(typeof(INPUT)));
  }
}
"@

[Drag]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null

$hwnd = [Drag]::Biggest([uint32]$ProcessId)
if ($hwnd -eq [IntPtr]::Zero) { throw "找不到进程 $ProcessId 的可见窗口" }
[void][Drag]::StealForeground($hwnd)
Start-Sleep -Milliseconds 400

[Drag]::Move($AtX, $AtY) | Out-Null
Start-Sleep -Milliseconds 250
[Drag]::Button($true)
Start-Sleep -Milliseconds 120
$steps = 12
for ($i = 1; $i -le $steps; $i++) {
  $x = [int]($AtX + ($ToX - $AtX) * $i / $steps)
  $y = [int]($AtY + ($ToY - $AtY) * $i / $steps)
  [Drag]::Move($x, $y) | Out-Null
  Start-Sleep -Milliseconds 35
}
Start-Sleep -Milliseconds 150
[Drag]::Button($false)
"dragged from $AtX,$AtY to $ToX,$ToY"
