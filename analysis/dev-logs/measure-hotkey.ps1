param([int]$Rounds = 10)

$root = "C:\Users\Sisyphus\Documents\private\快速启动软件"

$src = @"
using System;
using System.Runtime.InteropServices;
public class Hot {
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
}
"@
$path = Join-Path $env:TEMP "mavis-hot.cs"
Set-Content -Path $path -Value $src -Encoding UTF8
if (-not ("Hot" -as [type])) { Add-Type -Path $path }

$p = Get-Process -Name "launcher-app" -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $p) { "launcher is not running"; exit 1 }

$line = (& "$root\tools\enum-windows.ps1" -Pid_ $p.Id | Select-String "Fast Launcher")
$h = [IntPtr]([int](($line -join '') -replace '.*hwnd=(\d+).*', '$1'))
"window: $h"

$samples = New-Object System.Collections.Generic.List[double]
for ($i = 0; $i -lt $Rounds; $i++) {
    # Make sure it is hidden first.
    if ([Hot]::IsWindowVisible($h)) {
        & "$root\tools\send-hotkey.ps1" -Combo "ctrl+space" | Out-Null
        Start-Sleep -Milliseconds 400
    }

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & "$root\tools\send-hotkey.ps1" -Combo "ctrl+space" | Out-Null
    while (-not [Hot]::IsWindowVisible($h) -and $sw.ElapsedMilliseconds -lt 5000) {
        Start-Sleep -Milliseconds 2
    }
    $sw.Stop()
    if ([Hot]::IsWindowVisible($h)) { $samples.Add($sw.Elapsed.TotalMilliseconds) }
    Start-Sleep -Milliseconds 250
}

if ($samples.Count -eq 0) { "no samples collected"; exit 1 }
$s = $samples | Sort-Object
"hotkey -> window visible  n=$($samples.Count)"
"  min    = {0:N1} ms" -f $s[0]
"  median = {0:N1} ms" -f $s[[int]($s.Count / 2)]
"  max    = {0:N1} ms" -f $s[$s.Count - 1]
"  mean   = {0:N1} ms" -f (($samples | Measure-Object -Average).Average)
