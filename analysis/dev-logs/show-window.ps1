param([int]$Hwnd = 0)

$src = @"
using System;
using System.Runtime.InteropServices;
public class Sh {
  [DllImport("user32.dll", SetLastError=true)] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll", SetLastError=true)] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern int GetWindowLongW(IntPtr h, int i);
  [DllImport("user32.dll")] public static extern int GetWindowLongPtrW(IntPtr h, int i);
  [DllImport("user32.dll")] public static extern int GetLastError();
}
"@
$path = Join-Path $env:TEMP "mavis-showwin.cs"
Set-Content -Path $path -Value $src -Encoding UTF8
Add-Type -Path $path

$h = [IntPtr]$Hwnd
"before: visible=$([Sh]::IsWindowVisible($h))"
$style = [Sh]::GetWindowLongPtrW($h, -16)   # GWL_STYLE
$ex    = [Sh]::GetWindowLongPtrW($h, -20)   # GWL_EXSTYLE
"GWL_STYLE   = 0x{0:X}" -f $style
"GWL_EXSTYLE = 0x{0:X}" -f $ex
"  WS_VISIBLE(0x{0:X}) = {1}" -f 0x10000000, (($style -band 0x10000000) -ne 0)
"  WS_EX_LAYERED(0x{0:X}) = {1}" -f 0x80000, (($ex -band 0x80000) -ne 0)
"  WS_EX_NOACTIVATE(0x{0:X}) = {1}" -f 0x08000000, (($ex -band 0x08000000) -ne 0)

$r = [Sh]::ShowWindow($h, 5)
"ShowWindow(SW_SHOW) -> $r  err=$([Sh]::GetLastError())"
Start-Sleep -Milliseconds 500
"after:  visible=$([Sh]::IsWindowVisible($h))"
[void][Sh]::SetForegroundWindow($h)
