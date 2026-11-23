param(
  # How long to wait before the picture is taken.
  [double]$Settle = 2.0,

  # Where the picture is written.
  [string]$Out = 'focus-away.png'
)

# Take the focus away from the launcher, so that it has to hide itself.
#
# The desktop window is used for it: it is always there, and it never asks
# anything of the user.

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Away {
  [DllImport("user32.dll")] public static extern IntPtr GetShellWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
}
"@

[Away]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null

# The desktop takes the focus.
$desktop = [Away]::GetShellWindow()
[void][Away]::SetForegroundWindow($desktop)
Write-Host "焦点交给了桌面"

Start-Sleep -Seconds $Settle

# Is the launcher still on the screen?
$visible = 0
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Vis {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  public static int CountOf(uint want) {
    int n = 0;
    EnumWindows((h, l) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid == want && IsWindowVisible(h)) {
        RECT r; GetWindowRect(h, out r);
        if (r.Right - r.Left > 200 && r.Bottom - r.Top > 100) n++;
      }
      return true;
    }, IntPtr.Zero);
    return n;
  }
}
"@
foreach ($proc in (Get-Process -Name 'ideas-launcher', 'IdeasLauncher' -ErrorAction SilentlyContinue)) {
  $n = [Vis]::CountOf([uint32]$proc.Id)
  Write-Host "进程 $($proc.Id): 可见窗口 $n 个"
}
