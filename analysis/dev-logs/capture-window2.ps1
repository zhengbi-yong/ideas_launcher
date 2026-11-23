param([int]$Hwnd = 0)

$src = @"
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
public class Cap2 {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int left, top, right, bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct BITMAPINFOHEADER {
    public uint biSize; public int biWidth; public int biHeight; public ushort biPlanes;
    public ushort biBitCount; public uint biCompression; public uint biSizeImage;
    public int biXPelsPerMeter; public int biYPelsPerMeter; public uint biClrUsed; public uint biClrImportant;
  }
  [StructLayout(LayoutKind.Sequential)] public struct BITMAPINFO {
    public BITMAPINFOHEADER bmiHeader; public uint bmiColors0, bmiColors1, bmiColors2;
  }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern IntPtr GetWindowDC(IntPtr h);
  [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr hdc);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleDC(IntPtr hdc);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateDIBSection(IntPtr hdc, ref BITMAPINFO bmi, uint usage, out IntPtr bits, IntPtr sec, uint off);
  [DllImport("gdi32.dll")] public static extern IntPtr SelectObject(IntPtr hdc, IntPtr obj);
  [DllImport("gdi32.dll")] public static extern bool DeleteObject(IntPtr o);
  [DllImport("gdi32.dll")] public static extern bool DeleteDC(IntPtr hdc);

  // 2 = PW_RENDERFULLCONTENT, needed for composited / transparent windows.
  public static string Grab(IntPtr h, string path) {
    RECT r; GetWindowRect(h, out r);
    int w = r.right - r.left, ht = r.bottom - r.top;
    if (w <= 0 || ht <= 0) return "bad size";
    IntPtr winDc = GetWindowDC(h);
    IntPtr memDc = CreateCompatibleDC(winDc);
    var bmi = new BITMAPINFO();
    bmi.bmiHeader.biSize = 40; bmi.bmiHeader.biWidth = w; bmi.bmiHeader.biHeight = -ht;
    bmi.bmiHeader.biPlanes = 1; bmi.bmiHeader.biBitCount = 32; bmi.bmiHeader.biCompression = 0;
    IntPtr bits;
    IntPtr hbmp = CreateDIBSection(memDc, ref bmi, 0, out bits, IntPtr.Zero, 0);
    IntPtr old = SelectObject(memDc, hbmp);
    bool ok = PrintWindow(h, memDc, 2);
    SelectObject(memDc, old); DeleteObject(hbmp); DeleteDC(memDc); ReleaseDC(h, winDc);

    int stride = w * 4;
    byte[] raw = new byte[stride * ht];
    Marshal.Copy(bits, raw, 0, raw.Length);
    using (var bmp2 = new Bitmap(w, ht, stride, System.Drawing.Imaging.PixelFormat.Format32bppArgb))
    using (var g = Graphics.FromImage(bmp2)) {
      var data = bmp2.LockBits(new Rectangle(0, 0, w, ht), ImageLockMode.WriteOnly, System.Drawing.Imaging.PixelFormat.Format32bppArgb);
      Marshal.Copy(raw, 0, data.Scan0, raw.Length);
      bmp2.UnlockBits(data);
      bmp2.Save(path, ImageFormat.Png);
    }
    return string.Format("{0} ({1}x{2}) PrintWindow={3}", path, w, ht, ok);
  }
}
"@
$path = Join-Path $env:TEMP "mavis-cap2.cs"
Set-Content -Path $path -Value $src -Encoding UTF8
Add-Type -Path $path -ReferencedAssemblies System.Drawing.Common, System.Drawing.Primitives

$out = Join-Path $env:TEMP ("fastlauncher-win-" + (Get-Date -Format "HHmmss") + ".png")
[Cap2]::Grab([IntPtr]$Hwnd, $out)
