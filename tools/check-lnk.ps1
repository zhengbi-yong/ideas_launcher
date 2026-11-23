# Compare the shortcut parser with the shell.
#
# The parser in `crates/launcher-index/src/lnk.rs` is ours; the referee is
# `WScript.Shell`, which asks the shell itself. Every shortcut in the Start
# Menu of this machine is resolved by both, and the answers have to agree.
#
#   pwsh -File tools/check-lnk.ps1

param(
  # The folders that are walked.
  [string[]]$Folders = @(
    "$env:ProgramData\Microsoft\Windows\Start Menu\Programs",
    "$env:APPDATA\Microsoft\Windows\Start Menu\Programs"
  )
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$dump = Join-Path $root 'target\release\lnk-dump.exe'

if (-not (Test-Path $dump)) {
  throw "找不到 $dump —— 先跑 cargo build --release -p ideas-launcher --bin lnk-dump"
}

$lines = & $dump @Folders
$ours = @{}
foreach ($line in $lines) {
  $parts = $line -split "`t", 2
  if ($parts.Count -eq 2) { $ours[$parts[0]] = $parts[1] }
}

$shell = New-Object -ComObject WScript.Shell
$same = 0
$byShell = 0
$different = @()
$empty = @()

foreach ($path in $ours.Keys) {
  $theirs = ''
  try {
    $theirs = $shell.CreateShortcut($path).TargetPath
  } catch {
    $theirs = ''
  }
  $mine = $ours[$path]
  if (-not $mine) {
    $empty += $path
  } elseif ($mine.TrimEnd('\') -ieq $path.TrimEnd('\')) {
    # The shortcut is kept as it is: the shell resolves it, and so does the
    # icon lookup — that is the answer for a shortcut that points at a
    # shell folder instead of at a file.
    $byShell++
  } elseif ($mine.TrimEnd('\') -ieq $theirs.TrimEnd('\')) {
    $same++
  } else {
    $different += [pscustomobject]@{ Shortcut = $path; Ours = $mine; Shell = $theirs }
  }
}

Write-Host ("快捷方式总数 : {0}" -f $ours.Count)
Write-Host ("和外壳一致     : {0}" -f $same) -ForegroundColor Green
Write-Host ("交给外壳解析   : {0}" -f $byShell) -ForegroundColor Green
Write-Host ("解析不出来     : {0}" -f $empty.Count)
Write-Host ("和外壳不一致   : {0}" -f $different.Count) -ForegroundColor $(if ($different.Count) { 'Yellow' } else { 'Green' })

if ($different.Count) {
  Write-Host ''
  Write-Host '--- 不一致的(最多 20 条)---' -ForegroundColor Yellow
  $different | Select-Object -First 20 | Format-Table -AutoSize -Wrap
}
if ($empty.Count) {
  Write-Host ''
  Write-Host '--- 解析不出来的(最多 10 条)---' -ForegroundColor Yellow
  $empty | Select-Object -First 10 | ForEach-Object { Write-Host "  $_" }
}
