param([int]$Pid_ = 0)

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class W33 {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int left, top, right, bottom; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
}
"@

if ($Pid_ -gt 0) {
    $p = Get-Process -Id $Pid_ -ErrorAction SilentlyContinue
    if ($p) {
        $h = [IntPtr]$p.MainWindowHandle
        $r = New-Object W33+RECT
        [W33]::GetWindowRect($h, [ref]$r) | Out-Null
        "app pid=$Pid_ hwnd=$($p.MainWindowHandle)"
        "visible=$([W33]::IsWindowVisible($h)) iconic=$([W33]::IsIconic($h))"
        "rect = $($r.left),$($r.top) .. $($r.right),$($r.bottom)  size=$($r.right-$r.left)x$($r.bottom-$r.top)"
        "screen = $([W33]::GetSystemMetrics(0))x$([W33]::GetSystemMetrics(1))"
        "foreground hwnd = $([W33]::GetForegroundWindow())  (app = $($p.MainWindowHandle))"
    }
}
