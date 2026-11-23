param(
  [Parameter(Mandatory = $true)]
  [int]$ProcessId
)

# Look at the window of a process and report what the system thinks about it.
#
# The launcher measures the screen and positions itself in physical pixels, so
# it has to know whether the process is scaled by the system or scales itself.

$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Probe {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern int GetDpiForWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetThreadDpiAwarenessContext();
  [DllImport("user32.dll")] public static extern int GetAwarenessFromDpiAwarenessContext(IntPtr c);
  [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
  [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
  [DllImport("gdi32.dll")] public static extern int GetDeviceCaps(IntPtr dc, int index);
  [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
  [DllImport("user32.dll")] public static extern IntPtr MonitorFromPoint(POINT p, uint f);
  [DllImport("user32.dll")] public static extern bool GetMonitorInfo(IntPtr h, ref MONITORINFO mi);
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int x, y; }
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int left, top, right, bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct MONITORINFO { public uint cbSize; public RECT rcMonitor; public RECT rcWork; public uint dwFlags; }
}
"@

$proc = Get-Process -Id $ProcessId -ErrorAction Stop
$hwnd = $proc.MainWindowHandle
if ($hwnd -eq [IntPtr]::Zero) { throw "进程没有主窗口" }

$rect = New-Object Probe+RECT
[void][Probe]::GetWindowRect($hwnd, [ref]$rect)

$ctx = [Probe]::GetThreadDpiAwarenessContext()
$awareness = [Probe]::GetAwarenessFromDpiAwarenessContext($ctx)
$names = @{ 0 = 'DPI_AWARENESS_INVALID'; 1 = 'DPI_UNAWARE'; 2 = 'DPI_SYSTEM_AWARE'; 3 = 'DPI_PER_MONITOR_AWARE' }

$dc = [Probe]::GetDC([IntPtr]::Zero)
$LOGPIXELSELS = 88
$dpi = [Probe]::GetDeviceCaps($dc, $LOGPIXELSELS)
[void][Probe]::ReleaseDC([IntPtr]::Zero, $dc)

$mi = New-Object Probe+MONITORINFO
$mi.cbSize = [System.Runtime.InteropServices.Marshal]::SizeOf($mi)
$pt = New-Object Probe+POINT
[void][Probe]::GetCursorPos([ref]$pt)
$mon = [Probe]::MonitorFromPoint($pt, 2)
[void][Probe]::GetMonitorInfo($mon, [ref]$mi)

"hwnd              : $hwnd"
"window rect       : $($rect.left),$($rect.top) - $($rect.right),$($rect.bottom)"
"window size       : $($rect.right - $rect.left) x $($rect.bottom - $rect.top)  (physical)"
"GetDpiForWindow   : $([Probe]::GetDpiForWindow($hwnd))"
"awareness         : $($names[[int]$awareness]) ($awareness)"
"system LOGPIXELS  : $dpi"
"monitor rect      : $($mi.rcMonitor.left),$($mi.rcMonitor.top) - $($mi.rcMonitor.right),$($mi.rcMonitor.bottom)"
"work area         : $($mi.rcWork.left),$($mi.rcWork.top) - $($mi.rcWork.right),$($mi.rcWork.bottom)"
