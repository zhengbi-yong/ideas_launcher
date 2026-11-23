# 实机验证：新建的文件能不能立刻搜到。
#
# 1. 启动启动器并带上查询词（窗口会一直开着）
# 2. 新建一个文件
# 3. 监控把变化送进索引后，窗口里的查询自动重跑 —— 看日志里的结果数
param(
  [string]$Exe = 'C:\Users\Sisyphus\Documents\private\ideas_launcher\target\release\ideas-launcher.exe',
  [string]$Name = 'livewatch2026'
)

$ErrorActionPreference = 'Stop'
$logDir = Join-Path $env:APPDATA 'IdeasLauncher\Logs'

Get-Process -Name 'ideas-launcher', 'IdeasLauncher' -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 600
$log = (Get-ChildItem $logDir | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
$start = (Get-Item $log).Length

Write-Host "启动启动器（查询词：$Name）…"
Start-Process -FilePath $Exe -ArgumentList '--show', '--query', $Name, '--keep-open' | Out-Null
Start-Sleep -Seconds 3

$desktop = [Environment]::GetFolderPath('Desktop')
$file = Join-Path $desktop "$Name.txt"
Set-Content -LiteralPath $file -Value 'live index test' -Encoding UTF8
Write-Host "已创建 $file（等 4 秒让监控与重查发生）"
Start-Sleep -Seconds 4

Write-Host '--- 查询日志 ---'
Get-Content $log | Select-String -SimpleMatch "query `"$Name" | Select-Object -Last 4 | ForEach-Object { $_.Line }

Write-Host '--- 监控相关日志 ---'
Get-Content $log | Select-String 'walked through|changed|watched' | Select-Object -Last 4 | ForEach-Object { $_.Line }
