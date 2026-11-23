# 在窗口里点一下（绝对屏幕坐标，物理像素）。
param(
  [int]$X,
  [int]$Y
)
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Mouse {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, IntPtr extra);
  public const uint LEFTDOWN = 0x0002;
  public const uint LEFTUP = 0x0004;
}
"@
# 让这个进程也认得清 DPI，坐标才是物理像素。
[Mouse]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null
[Mouse]::SetCursorPos($X, $Y) | Out-Null
Start-Sleep -Milliseconds 300
[Mouse]::mouse_event([Mouse]::LEFTDOWN, 0, 0, 0, [IntPtr]::Zero)
Start-Sleep -Milliseconds 60
[Mouse]::mouse_event([Mouse]::LEFTUP, 0, 0, 0, [IntPtr]::Zero)
Write-Host "已点击 $X,$Y"
