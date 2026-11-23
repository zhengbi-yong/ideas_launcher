# ============================================================ 警告
# 这个脚本会**启动/结束**目标程序,并在 Flow Launcher 的窗口里注入热键。
# 它不会注入普通字符,但仍会抢占前台焦点。
# 运行时请不要使用电脑。只在无人值守的机器上跑。
# ============================================================
param(
  # How long each application is left alone before it is measured.
  [int]$SettleSeconds = 20,
  # How long the CPU is sampled.
  [int]$CpuSeconds = 10,
  # How many cold starts are measured per application.
  [int]$Starts = 3
)

# Compare IDEAS Launcher with Flow Launcher on the same machine, using the same
# method for both. The result is a Markdown table on the console.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$ours = Join-Path $root 'target\release\ideas-launcher.exe'
$theirs = Join-Path $env:LOCALAPPDATA 'FlowLauncher\app-2.1.4\Flow.Launcher.exe'
$oursLogDir = Join-Path $env:APPDATA 'IdeasLauncher\Logs'
$theirLogDir = Join-Path $env:APPDATA 'FlowLauncher\Logs'

function Stop-App([string]$name) {
    Get-Process -Name $name -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 600
}

function Measure-Resources([string]$name) {
    $p = Get-Process -Name $name -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $p) { return $null }
    $p.Refresh()

    $cpu0 = $p.TotalProcessorTime
    $t0 = Get-Date
    Start-Sleep -Seconds $CpuSeconds
    $p.Refresh()
    $cpu1 = $p.TotalProcessorTime
    $elapsed = ((Get-Date) - $t0).TotalSeconds
    $cores = [Environment]::ProcessorCount
    $cpu = [math]::Round((($cpu1 - $cpu0).TotalSeconds / $elapsed) * 100 / $cores, 3)

    [pscustomobject]@{
        WS_MB      = [math]::Round($p.WorkingSet64 / 1MB, 1)
        PM_MB      = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
        Threads    = $p.Threads.Count
        Handles    = $p.HandleCount
        Modules    = $p.Modules.Count
        CpuPercent = $cpu
    }
}

# The newest log file of a directory.
function Get-NewestLog([string]$dir, [string]$filter) {
    Get-ChildItem $dir -Recurse -Filter $filter -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
}

# Wait until the program writes its "I am ready" line, and return the
# millisecond value it reports.
function Measure-ColdStart([string]$exe, [string]$processName, [string]$logDir, [string]$logFilter, [string]$pattern) {
    Stop-App $processName
    $before = if (Test-Path $logDir) { (Get-ChildItem $logDir -Recurse -Filter $logFilter -ErrorAction SilentlyContinue | Measure-Object).Count } else { 0 }
    $proc = Start-Process $exe -PassThru -WindowStyle Hidden
    $deadline = (Get-Date).AddSeconds(90)
    $value = $null
    while ((Get-Date) -lt $deadline) {
        $file = Get-NewestLog $logDir $logFilter
        if ($file) {
            $line = Get-Content $file.FullName -Tail 60 -ErrorAction SilentlyContinue |
                Where-Object { $_ -match $pattern } | Select-Object -Last 1
            if ($line -and $line -match $pattern) {
                $value = [double]$Matches[1]
                break
            }
        }
        Start-Sleep -Milliseconds 40
    }
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 600
    return $value
}

Write-Host "== 1. 资源占用(静置后采样,CPU 取 $CpuSeconds 秒均值)==" -ForegroundColor Cyan

Stop-App 'ideas-launcher'; Stop-App 'IdeasLauncher'
Start-Process $ours -PassThru -WindowStyle Hidden | Out-Null
Start-Sleep -Seconds $SettleSeconds
$oursInfo = Measure-Resources 'ideas-launcher'

$theirsInfo = $null
if (Test-Path $theirs) {
    Stop-App 'Flow.Launcher'
    Start-Process $theirs -PassThru -WindowStyle Hidden | Out-Null
    Start-Sleep -Seconds ($SettleSeconds + 30)
    $theirsInfo = Measure-Resources 'Flow.Launcher'
}

Write-Host "== 2. 冷启动就绪时间(各程序自己报的日志)== " -ForegroundColor Cyan

$oursStarts = @()
$theirsStarts = @()
for ($i = 0; $i -lt $Starts; $i++) {
    $v = Measure-ColdStart $ours 'ideas-launcher' $oursLogDir 'ideas-launcher-*.log' 'launcher initialised in ([0-9.]+)ms'
    if ($v) { $oursStarts += $v; Write-Host ("  IDEAS Launcher    : {0:N0} ms" -f $v) }
    if (Test-Path $theirs) {
        $v = Measure-ColdStart $theirs 'Flow.Launcher' $theirLogDir 'Flow.Launcher.*.log' 'Startup cost <([0-9]+)ms>'
        if ($v) { $theirsStarts += $v; Write-Host ("  Flow Launcher   : {0:N0} ms" -f $v) }
    }
}

function Average($values) {
    if (-not $values -or $values.Count -eq 0) { return $null }
    return [math]::Round(($values | Measure-Object -Average).Average, 0)
}

Write-Host ""
Write-Host "== 对标结果 ==" -ForegroundColor Cyan
Write-Host ''
Write-Host '| 指标 | Flow Launcher 2.1.4 | IDEAS Launcher | 优势 |'
Write-Host '|---|---|---|---|'
if ($theirsInfo -and $oursInfo) {
    $ws = if ($theirsInfo.WS_MB -gt 0) { [math]::Round($theirsInfo.WS_MB / $oursInfo.WS_MB, 1) } else { 0 }
    $pm = if ($theirsInfo.PM_MB -gt 0) { [math]::Round($theirsInfo.PM_MB / $oursInfo.PM_MB, 1) } else { 0 }
    $hd = if ($oursInfo.Handles -gt 0) { [math]::Round($theirsInfo.Handles / $oursInfo.Handles, 1) } else { 0 }
    $md = if ($oursInfo.Modules -gt 0) { [math]::Round($theirsInfo.Modules / $oursInfo.Modules, 1) } else { 0 }
    Write-Host "| 工作集 WS | $($theirsInfo.WS_MB) MB | $($oursInfo.WS_MB) MB | $(if ($oursInfo.WS_MB -lt $theirsInfo.WS_MB) { ' ours' } else { ' theirs' }) |"
    Write-Host "| 私有内存 PM | $($theirsInfo.PM_MB) MB | $($oursInfo.PM_MB) MB | $(if ($oursInfo.PM_MB -lt $theirsInfo.PM_MB) { "$pm× 更少" } else { 'theirs' }) |"
    Write-Host "| 线程 | $($theirsInfo.Threads) | $($oursInfo.Threads) | $(if ($oursInfo.Threads -le $theirsInfo.Threads) { '持平/更少' } else { 'theirs' }) |"
    Write-Host "| 句柄 | $($theirsInfo.Handles) | $($oursInfo.Handles) | $(if ($oursInfo.Handles -lt $theirsInfo.Handles) { "$hd× 更少" } else { 'theirs' }) |"
    Write-Host "| 加载模块 | $($theirsInfo.Modules) | $($oursInfo.Modules) | $(if ($oursInfo.Modules -lt $theirsInfo.Modules) { "$md× 更少" } else { 'theirs' }) |"
    Write-Host "| 空闲 CPU | $($theirsInfo.CpuPercent) % | $($oursInfo.CpuPercent) % | $(if ($oursInfo.CpuPercent -lt $theirsInfo.CpuPercent) { 'ours' } else { 'theirs' }) |"
}
$oursStart = Average $oursStarts
$theirsStart = Average $theirsStarts
if ($theirsStart -and $oursStart) {
    Write-Host "| 冷启动就绪 | $theirsStart ms | $oursStart ms | $([math]::Round($theirsStart / $oursStart, 1))× 更快 |"
}
