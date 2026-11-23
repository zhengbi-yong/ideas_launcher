# 在窗口里滚一滚（滚轮）。
param(
  [int]$X,
  [int]$Y,
  [int]$Notches = -6
)
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Wheel {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, int data, IntPtr extra);
  public const uint WHEEL = 0x0800;
}
"@
[Wheel]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null
[Wheel]::SetCursorPos($X, $Y) | Out-Null
Start-Sleep -Milliseconds 250
for ($i = 0; $i -lt [Math]::Abs($Notches); $i++) {
  $delta = if ($Notches -lt 0) { -120 } else { 120 }
  [Wheel]::mouse_event([Wheel]::WHEEL, 0, 0, $delta, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 80
}
Write-Host "已在 $X,$Y 滚动 $Notches 格"
