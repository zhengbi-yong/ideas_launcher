param([int]$Hwnd = 0)

$src = @"
using System;
using System.IO;
using System.Runtime.InteropServices;

public class Cap3 {
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

  public static string Grab(IntPtr h, string path) {
    RECT r; GetWindowRect(h, out r);
    int w = r.right - r.left, ht = r.bottom - r.top;
    if (w <= 0 || ht <= 0) return "bad size " + w + "x" + ht;

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

    // Write a 32-bit BMP by hand (no System.Drawing needed).
    using (var fs = new FileStream(path, FileMode.Create))
    using (var bw = new BinaryWriter(fs)) {
      bw.Write((byte)0x42); bw.Write((byte)0x4D);          // "BM"
      bw.Write(14 + 40 + raw.Length);                        // file size
      bw.Write((uint)0); bw.Write((uint)0);                  // reserved
      bw.Write(14 + 40);                                    // pixel data offset
      bw.Write((uint)40); bw.Write(w); bw.Write(ht);        // BITMAPINFOHEADER
      bw.Write((ushort)1); bw.Write((ushort)32); bw.Write((uint)0);
      bw.Write((uint)raw.Length);
      bw.Write((int)2835); bw.Write((int)2835);
      bw.Write((uint)0); bw.Write((uint)0);
      // BMP rows are bottom-up; our DIB is top-down, so write rows in reverse.
      for (int y = ht - 1; y >= 0; y--) bw.Write(raw, y * stride, stride);
    }
    return string.Format("{0} ({1}x{2}) PrintWindow={3}", path, w, ht, ok);
  }
}
"@
$path = Join-Path $env:TEMP "mavis-cap3.cs"
Set-Content -Path $path -Value $src -Encoding UTF8
Add-Type -Path $path

$out = Join-Path $env:TEMP ("fastlauncher-win-" + (Get-Date -Format "HHmmss") + ".bmp")
[Cap3]::Grab([IntPtr]$Hwnd, $out)
