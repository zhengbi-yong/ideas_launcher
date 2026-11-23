# 打开设置面板 → 点「索引」→ 截图。
#
# 坐标是从窗口的实际位置算出来的（先拍一张，拿窗口矩形，再点）。
param(
  [string]$Exe = 'C:\Users\Sisyphus\Documents\private\ideas_launcher\target\release\ideas-launcher.exe',
  [string]$Out = "$env:TEMP\settings-index.png",
  [int]$NavX = 43,
  [int]$NavY = 234
)
$ErrorActionPreference = 'Stop'
$root = 'C:\Users\Sisyphus\Documents\private\ideas_launcher'

Get-Process -Name 'ideas-launcher' -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 600
Start-Process -FilePath $Exe -ArgumentList '--settings', '--keep-open' | Out-Null
Start-Sleep -Seconds 4

$pid1 = (Get-Process -Name 'ideas-launcher' -ErrorAction SilentlyContinue | Select-Object -First 1).Id
$probe = "$env:TEMP\settings-probe.png"
$captured = & "$root\tools\capture-window.ps1" -ProcessId $pid1 -Out $probe -Settle 1 6>&1
$line = ($captured | Where-Object { $_ -match '窗口区域' }) -join ' '
Write-Host $line
$all = $captured -join ' '
if ($all -match '(-?\d+),(-?\d+) (\d+)x(\d+)') {
  $left = [int]$Matches[1]
  $top = [int]$Matches[2]
} else {
  throw '拿不到窗口位置'
}

& "$root\tools\click-window.ps1" -X ($left + $NavX) -Y ($top + $NavY)
Start-Sleep -Seconds 2
& "$root\tools\capture-window.ps1" -ProcessId $pid1 -Out $Out -Settle 1
