param(
  [string]$ProcessName = "ideas-launcher",
  [int]$SettleSeconds = 25
)

# Measure the resident footprint of a running launcher once it has settled.

$proc = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) { "$ProcessName is not running"; exit 1 }

Write-Output "waiting ${SettleSeconds}s for the process to settle ..."
Start-Sleep -Seconds $SettleSeconds

$proc.Refresh()
$p = Get-Process -Id $proc.Id

# The CPU time is sampled over a window so the idle load is visible.
$cpu0 = $p.TotalProcessorTime
$t0 = Get-Date
Start-Sleep -Seconds 10
$proc.Refresh()
$cpu1 = $p.TotalProcessorTime
$t1 = Get-Date

$cores = [Environment]::ProcessorCount
$cpuPercent = [math]::Round((($cpu1 - $cpu0).TotalSeconds / ($t1 - $t0).TotalSeconds) * 100 / $cores, 3)

"process        : $($p.ProcessName) pid=$($p.Id)"
"working set    : {0:N1} MB" -f ($p.WorkingSet64 / 1MB)
"private memory : {0:N1} MB" -f ($p.PrivateMemorySize64 / 1MB)
"paged memory   : {0:N1} MB" -f ($p.PagedMemorySize64 / 1MB)
"virtual memory : {0:N1} MB" -f ($p.VirtualMemorySize64 / 1MB)
"threads        : $($p.Threads.Count)"
"handles        : $($p.HandleCount)"
"modules        : $($p.Modules.Count)"
"cpu (10s avg)  : $cpuPercent %  on $cores logical cores"
