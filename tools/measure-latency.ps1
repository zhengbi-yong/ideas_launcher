param([int]$Rounds = 20)

$root = "C:\Users\Sisyphus\Documents\private\快速启动软件"

$src = @"
using System;
using System.Runtime.InteropServices;
public class Lat {
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion U; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion {
    [FieldOffset(0)] public MOUSEINPUT mi;
    [FieldOffset(0)] public KEYBDINPUT ki;
  }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint n, INPUT[] p, int size);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern int GetWindowLongW(IntPtr h, int i);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  public static bool StyleVisible(IntPtr h) { return (GetWindowLongW(h, -16) & 0x10000000) != 0; }

  public static void Key(ushort vk, bool up) {
    INPUT[] i = new INPUT[1];
    i[0].type = 1;
    i[0].U.ki = new KEYBDINPUT { wVk = vk, wScan = 0, dwFlags = (uint)(up ? 2 : 0), time = 0, dwExtraInfo = IntPtr.Zero };
    SendInput(1, i, Marshal.SizeOf(typeof(INPUT)));
  }
  public static void CtrlSpace() {
    Key(0x11, false); System.Threading.Thread.Sleep(30);
    Key(0x20, false); System.Threading.Thread.Sleep(30);
    Key(0x20, true);  System.Threading.Thread.Sleep(30);
    Key(0x11, true);
  }
}
"@
$path = Join-Path $env:TEMP "mavis-lat.cs"
Set-Content -Path $path -Value $src -Encoding UTF8
if (-not ("Lat" -as [type])) { Add-Type -Path $path }

$p = Get-Process -Name "ideas-launcher" -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $p) { "launcher is not running"; exit 1 }
$line = (& "$root\tools\enum-windows.ps1" -Pid_ $p.Id | Select-String "IDEAS Launcher")
$h = [IntPtr]([int](($line -join '') -replace '.*hwnd=(\d+).*', '$1'))

$samples = New-Object System.Collections.Generic.List[double]
$fgSamples = New-Object System.Collections.Generic.List[double]
for ($i = 0; $i -lt $Rounds; $i++) {
    if ([Lat]::StyleVisible($h)) {
        [Lat]::CtrlSpace()
        Start-Sleep -Milliseconds 350
    }

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    [Lat]::CtrlSpace()
    while (-not [Lat]::StyleVisible($h) -and $sw.Elapsed.TotalMilliseconds -lt 5000) {
        Start-Sleep -Milliseconds 1
    }
    $visible = [Lat]::StyleVisible($h)
    $sw.Stop()
    if ($visible) { $samples.Add($sw.Elapsed.TotalMilliseconds) }

    # Time until the window actually owns the foreground.
    $sw2 = [System.Diagnostics.Stopwatch]::StartNew()
    while ([Lat]::GetForegroundWindow() -ne $h -and $sw2.Elapsed.TotalMilliseconds -lt 3000) {
        Start-Sleep -Milliseconds 1
    }
    $sw2.Stop()
    if ([Lat]::GetForegroundWindow() -eq $h) { $fgSamples.Add($sw2.Elapsed.TotalMilliseconds) }

    Start-Sleep -Milliseconds 250
}

function Show($list, $name) {
    if ($list.Count -eq 0) { "$name : no samples"; return }
    $s = $list | Sort-Object
    "{0}  n={1}  min={2:N1}ms  median={3:N1}ms  p90={4:N1}ms  max={5:N1}ms" -f `
        $name, $s.Count, $s[0], $s[[int]($s.Count/2)], $s[[int]($s.Count*0.9)], $s[$s.Count-1]
}
Show $samples "hotkey -> visible"
Show $fgSamples "hotkey -> foreground"
