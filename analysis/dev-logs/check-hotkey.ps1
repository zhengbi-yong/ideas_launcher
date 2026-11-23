param([int]$Pid_ = 0)

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class W32 {
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool RegisterHotKey(IntPtr h, int id, uint m, uint v);
  [DllImport("user32.dll")] public static extern int GetLastError();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
}
"@

if ($Pid_ -gt 0) {
    $p = Get-Process -Id $Pid_ -ErrorAction SilentlyContinue
    if ($p) {
        $h = [IntPtr]$p.MainWindowHandle
        "app pid=$Pid_ hwnd=$($p.MainWindowHandle) visible=$([W32]::IsWindowVisible($h))"
    } else {
        "app pid=$Pid_ not running"
    }
}

"--- try registering Alt+Space ourselves ---"
$ok = [W32]::RegisterHotKey([IntPtr]::Zero, 4242, 1, 0x20)
"RegisterHotKey(Alt+Space) ok=$ok err=$([W32]::GetLastError())  [1409 = ERROR_HOTKEY_ALREADY_REGISTERED]"
if ($ok) { [W32]::UnregisterHotKey([IntPtr]::Zero, 4242) | Out-Null }
