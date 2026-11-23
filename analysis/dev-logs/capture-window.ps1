param([int]$Hwnd = 0)

$src = @"
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
public class Cap {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int left, top, right, bottom; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);

  public static string Grab(IntPtr h, string path) {
    RECT r; GetWindowRect(h, out r);
    int w = r.right - r.left, ht = r.bottom - r.top;
    using (var bmp = new Bitmap(w, ht))
    using (var g = Graphics.FromImage(bmp)) {
      g.CopyFromScreen(r.left, r.top, 0, 0, new Size(w, ht));
      bmp.Save(path, ImageFormat.Png);
    }
    return string.Format("{0} ({1}x{2}) at {3},{4}", path, w, ht, r.left, r.top);
  }
}
"@
$path = Join-Path $env:TEMP "mavis-grab.cs"
Set-Content -Path $path -Value $src -Encoding UTF8
Add-Type -Path $path
Add-Type -AssemblyName System.Drawing

$out = Join-Path $env:TEMP ("fastlauncher-capture-" + (Get-Date -Format "HHmmss") + ".png")
[Cap]::Grab([IntPtr]$Hwnd, $out)
"visible=$([Cap]::IsWindowVisible([IntPtr]$Hwnd))"
$out
