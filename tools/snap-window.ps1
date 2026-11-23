param(
  [Parameter(Mandatory = $true)]
  [int]$ProcessId,

  [Parameter(Mandatory = $true)]
  [string]$Out,

  [double]$Settle = 3.0,

  # Leave the pointer where it is.
  #
  # The menu of the Windows shell is a window of its own: moving the pointer
  # away from it while the picture is taken would close it before it is
  # photographed.
  [switch]$KeepMouse
)

# Photograph a running launcher without touching it.
#
# The mouse is parked in the corner first: a pointer over the window changes
# the hovered row, and the rows move with the scroll. The picture would show a
# different window every time.

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Snap {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }

  public static IntPtr FindVisible(uint want) {
    IntPtr found = IntPtr.Zero;
    long best = 0;
    EnumWindows((h, l) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid == want && IsWindowVisible(h)) {
        RECT r; GetWindowRect(h, out r);
        long area = (long)(r.Right - r.Left) * (r.Bottom - r.Top);
        if (area > best) { best = area; found = h; }
      }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
"@

[Snap]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null

$hwnd = [Snap]::FindVisible([uint32]$ProcessId)
if ($hwnd -eq [IntPtr]::Zero) { throw "没有找到可见窗口" }

# Park the pointer where it cannot hover a row.
if (-not $KeepMouse) {
  [Snap]::SetCursorPos(4, 4) | Out-Null
}
Start-Sleep -Seconds $Settle

$rect = New-Object Snap+RECT
[Snap]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
$width = $rect.Right - $rect.Left
$height = $rect.Bottom - $rect.Top
Write-Host "窗口区域 : $($rect.Left),$($rect.Top) ${width}x${height}"

$bitmap = New-Object System.Drawing.Bitmap $width, $height
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, (New-Object System.Drawing.Size $width, $height))
$graphics.Dispose()
$bitmap.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$bitmap.Dispose()
Write-Host "截图     : $Out"
