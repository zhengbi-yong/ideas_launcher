param([int]$Hwnd = 0)

$src = @"
using System;
using System.Runtime.InteropServices;
public class Top {
  [DllImport("user32.dll", SetLastError=true)] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
}
"@
$path = Join-Path $env:TEMP "mavis-top.cs"
Set-Content -Path $path -Value $src -Encoding UTF8
Add-Type -Path $path

$h = [IntPtr]$Hwnd
$TOPMOST = [IntPtr](-1)
$NOTOPMOST = [IntPtr](-2)
# SWP_NOMOVE(2) | SWP_NOSIZE(1) | SWP_SHOWWINDOW(0x40)
[void][Top]::SetWindowPos($h, $TOPMOST, 0, 0, 0, 0, 0x0002 -bor 0x0001 -bor 0x0040)
[void][Top]::BringWindowToTop($h)
[void][Top]::SetForegroundWindow($h)
Start-Sleep -Milliseconds 700
"visible=$([Top]::IsWindowVisible($h)) foreground=$([Top]::GetForegroundWindow()) mine=$Hwnd"
