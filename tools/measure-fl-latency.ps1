# ============================================================ 警告
# 这个脚本会**全局注入按键**、抢占前台焦点,并且反复启动/结束目标程序。
# 运行时请不要使用电脑:它会打断你的输入和焦点。
# 只在无人值守的机器上跑。
# ============================================================
param(
  [int]$Rounds = 5,
  [string]$Text = "chrome",
  [string]$Out = "",
  # The process to measure; defaults to Flow Launcher.
  [string]$ProcessName = "Flow.Launcher",
  # The hotkey that brings the window up.
  [string]$HotkeyCombo = "alt+space",
  # How far below the window top the result list starts.
  [int]$ListTop = 70,
  # The title of the window to measure.
  [string]$WindowTitle = ''
)

# Measure the observable latency of Flow Launcher: the delay between the last
# keystroke and the moment the result list changes on screen.
#
# Flow Launcher does not log per-query timings, so the only honest way to
# compare is to watch the window: capture the result area in a tight loop,
# hash every frame, and record when the pixels change.

$src = @"
using System;
using System.Runtime.InteropServices;

public class Win {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int left, top, right, bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct BITMAPINFOHEADER {
    public uint biSize; public int biWidth; public int biHeight; public ushort biPlanes; public ushort biBitCount;
    public uint biCompression; public uint biSizeImage; public int biXPelsPerMeter; public int biYPelsPerMeter;
    public uint biClrUsed; public uint biClrImportant;
  }
  [StructLayout(LayoutKind.Sequential)] public struct BITMAPINFO {
    public BITMAPINFOHEADER bmiHeader;
    public uint bmiColors0; public uint bmiColors1; public uint bmiColors2;
  }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
  [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr hdc);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleDC(IntPtr hdc);
  [DllImport("gdi32.dll")] public static extern IntPtr SelectObject(IntPtr hdc, IntPtr obj);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateDIBSection(IntPtr hdc, ref BITMAPINFO bmi, uint usage, out IntPtr bits, IntPtr hSection, uint offset);
  [DllImport("gdi32.dll")] public static extern bool DeleteObject(IntPtr obj);
  [DllImport("gdi32.dll")] public static extern bool DeleteDC(IntPtr hdc);
  [DllImport("gdi32.dll")] public static extern bool BitBlt(IntPtr dest, int x, int y, int w, int h, IntPtr src, int sx, int sy, uint rop);

  static IntPtr memDC = IntPtr.Zero, bmp = IntPtr.Zero, bits = IntPtr.Zero;
  static int curW = 0, curH = 0;
  static byte[] buffer = null;
  public static uint LastHash = 0;
  public static long LastChangeTicks = 0;

  // Copy a rectangle of the screen into an off-screen bitmap and return a
  // hash of its pixels, so a caller can tell when the visible content
  // changed. Returns 0 when the capture failed.
  public static uint CaptureHash(int x, int y, int w, int hgt) {
    if (w <= 0 || hgt <= 0) return 0;
    try {
      if (memDC == IntPtr.Zero || curW != w || curH != hgt) {
        if (memDC != IntPtr.Zero) { DeleteObject(bmp); DeleteDC(memDC); memDC = IntPtr.Zero; }
        IntPtr screen = GetDC(IntPtr.Zero);
        memDC = CreateCompatibleDC(screen);
        ReleaseDC(IntPtr.Zero, screen);
        BITMAPINFO bi = new BITMAPINFO();
        bi.bmiHeader.biSize = (uint)Marshal.SizeOf(typeof(BITMAPINFOHEADER));
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -hgt;
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = 0;
        bmp = CreateDIBSection(memDC, ref bi, 0, out bits, IntPtr.Zero, 0);
        SelectObject(memDC, bmp);
        curW = w; curH = hgt;
      }
      IntPtr screenDC = GetDC(IntPtr.Zero);
      bool ok = BitBlt(memDC, 0, 0, w, hgt, screenDC, x, y, 0x00CC0020);
      ReleaseDC(IntPtr.Zero, screenDC);
      if (!ok) return 0;
      // Copy the pixels out in one go and hash them in managed code; doing it
      // with Marshal.ReadByte per pixel would dominate the measurement.
      int bytes = w * hgt * 4;
      if (buffer == null || buffer.Length < bytes) buffer = new byte[bytes];
      Marshal.Copy(bits, buffer, 0, bytes);
      uint hash = 2166136261u;
      for (int i = 0; i < bytes; i += 16) {
        hash ^= (uint)buffer[i];
        hash *= 16777619u;
      }
      if (hash != LastHash) {
        LastHash = hash;
        LastChangeTicks = DateTime.UtcNow.Ticks;
      }
      return hash;
    } catch { return 0; }
  }
}
"@
$path = Join-Path $env:TEMP "mavis-winlat.cs"
Set-Content -Path $path -Value $src -Encoding UTF8
if (-not ("Win" -as [type])) { Add-Type -Path $path }

# Send the hotkey so the window is up.
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Threading;
public class Keys2 {
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion U; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion {
    [FieldOffset(0)] public MOUSEINPUT mi;
    [FieldOffset(0)] public KEYBDINPUT ki;
  }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint n, INPUT[] p, int size);
  public static uint Key(ushort vk, bool up) {
    INPUT[] i = new INPUT[1];
    i[0].type = 1;
    i[0].U.ki = new KEYBDINPUT { wVk = vk, wScan = 0, dwFlags = (uint)(up ? 2 : 0), time = 0, dwExtraInfo = IntPtr.Zero };
    return SendInput(1, i, Marshal.SizeOf(typeof(INPUT)));
  }
  public static string Combo(ushort mod, ushort key) {
    uint a = Key(mod, false); Thread.Sleep(40);
    uint b = Key(key, false); Thread.Sleep(40);
    uint c = Key(key, true);  Thread.Sleep(40);
    uint d = Key(mod, true);
    return string.Format("mod=0x{0:X} key=0x{1:X} sent={2},{3},{4},{5}", mod, key, a, b, c, d);
  }
  public static void Text(string s) {
    foreach (char c in s) {
      INPUT[] d = new INPUT[1];
      d[0].type = 1;
      d[0].U.ki = new KEYBDINPUT { wVk = 0, wScan = c, dwFlags = 0x0004, time = 0, dwExtraInfo = IntPtr.Zero };
      SendInput(1, d, Marshal.SizeOf(typeof(INPUT)));
      INPUT[] u = new INPUT[1];
      u[0].type = 1;
      u[0].U.ki = new KEYBDINPUT { wVk = 0, wScan = c, dwFlags = 0x0004 | 0x0002, time = 0, dwExtraInfo = IntPtr.Zero };
      SendInput(1, u, Marshal.SizeOf(typeof(INPUT)));
      Thread.Sleep(40);
    }
  }
}
"@


$comboMod = 0x12; $comboKey = 0x20
switch ($HotkeyCombo.ToLower()) {
  "alt+space"   { $comboMod = 0x12; $comboKey = 0x20 }
  "ctrl+space"  { $comboMod = 0x11; $comboKey = 0x20 }
  "win+space"   { $comboMod = 0x5B; $comboKey = 0x20 }
  "ctrl+alt+space" { $comboMod = 0x11; $comboKey = 0x20 }
}

$proc = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) { "$ProcessName is not running"; exit 1 }

# The window only exists once the hotkey has been pressed, so bring it up
# first and then pick up its handle.
"hotkey send: $([Keys2]::Combo($comboMod, $comboKey))"
Start-Sleep -Milliseconds 900

$hwnd = [IntPtr]::Zero
$found = & "$PSScriptRoot\enum-windows.ps1" -Pid_ $proc.Id
if ($WindowTitle -ne "") { $line = ($found | Select-String ([regex]::Escape($WindowTitle)) | Select-Object -First 1) } else { $line = ($found | Select-String "visible=True" | Select-Object -First 1) }
if ($line) {
  $hwnd = [IntPtr]([int](($line -join '') -replace '.*hwnd=(\d+).*', '$1'))
}
if ($hwnd -eq [IntPtr]::Zero) { "no window found"; exit 1 }

$r = New-Object Win+RECT
[void][Win]::GetWindowRect($hwnd, [ref]$r)
"$ProcessName hwnd=$hwnd"


$latencies = @()
for ($round = 1; $round -le $Rounds; $round++) {
  # Reset: make sure the window is closed, then open it again and let the
  # opening animation finish before the window rectangle is read.
  [Keys2]::Key(0x1B, $false); [Keys2]::Key(0x1B, $true)
  Start-Sleep -Milliseconds 300
  [Keys2]::Combo($comboMod, $comboKey)
  Start-Sleep -Milliseconds 1200

  # The result list starts below the search box.
  [void][Win]::GetWindowRect($hwnd, [ref]$r)
  $listX = $r.left + 10
  $listY = $r.top + $ListTop
  $listW = ($r.right - $r.left) - 20
  $listH = [Math]::Min(90, ($r.bottom - $r.top) - $ListTop)
  if ($round -eq 1) { "list region     : $listX,$listY ${listW}x${listH}" }

  # Let the window settle, then record the baseline hash.
  [Win]::CaptureHash($listX, $listY, $listW, $listH) | Out-Null
  $baseline = [Win]::LastHash
  Start-Sleep -Milliseconds 300

  # Type the query and note when the last keystroke was sent.
  [Keys2]::Text($Text)
  $lastKeyTicks = [DateTime]::UtcNow.Ticks

  # Watch the result area until it changes.
  $changeTicks = 0
  $deadline = [DateTime]::UtcNow.AddSeconds(3)
  while ([DateTime]::UtcNow -lt $deadline) {
    [Win]::CaptureHash($listX, $listY, $listW, $listH) | Out-Null
    if ([Win]::LastHash -ne $baseline -and [Win]::LastChangeTicks -gt $lastKeyTicks) {
      $changeTicks = [Win]::LastChangeTicks
      break
    }
    Start-Sleep -Milliseconds 2
  }

  if ($changeTicks -gt 0) {
    $ms = ($changeTicks - $lastKeyTicks) / 10000.0
    "round $round : $ms ms"
    $latencies += $ms
  } else {
    "round $round : no change seen"
  }

  [Keys2]::Key(0x1B, $false); [Keys2]::Key(0x1B, $true)   # escape
  Start-Sleep -Milliseconds 300
}

if ($latencies.Count -gt 0) {
  $s = $latencies | Sort-Object
  $p50 = $s[[Math]::Floor(($s.Count - 1) * 0.5)]
  $p90 = $s[[Math]::Floor(($s.Count - 1) * 0.9)]
  "p50 = $p50 ms   p90 = $p90 ms   n = $($s.Count)"
}
