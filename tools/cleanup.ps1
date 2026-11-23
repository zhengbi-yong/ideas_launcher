# Clean the build cache and the leftovers of the launcher.
#
# A copy of the launcher that was started with administrator rights keeps its
# executable open, and an ordinary program cannot end such a process — so the
# file stays in `target/release` and the directory fills up with stale copies.
#
# This script asks for the rights it needs and does the cleaning in one go.
#
# Usage:
#   pwsh -File tools/cleanup.ps1

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$target = Join-Path $root 'target'

# The cleaning needs rights that an ordinary program does not have, so it is
# done by a copy of this script that asks for them.
$self = $MyInvocation.MyCommand.Path
$worker = Join-Path $env:TEMP ("ideas-cleanup-" + [guid]::NewGuid().ToString('N').Substring(0, 8) + ".ps1")
$body = @"
`$ErrorActionPreference = 'SilentlyContinue'
Get-Process -Name 'ideas-launcher', 'IdeasLauncher' | Stop-Process -Force
Start-Sleep -Milliseconds 500
Get-ChildItem -Path '$target\release' -Filter 'ideas-launcher.stale-*.exe' | Remove-Item -Force
Get-ChildItem -Path '$target\release' -Filter '*.locked' | Remove-Item -Force
Get-ChildItem -Path '$target\release' -Filter 'ideas-launcher.running.exe' | Remove-Item -Force
"@
Set-Content -Path $worker -Value $body -Encoding UTF8

try {
  Write-Host "请在 UAC 弹窗点「是」…" -ForegroundColor Yellow
  Start-Process -FilePath "powershell.exe" `
      -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $worker) `
      -Verb RunAs -Wait
  Write-Host "清理完成" -ForegroundColor Green
}
catch {
  Write-Host "提权被取消,没有清理。`n可以直接删掉 target/release 里的 ideas-launcher.stale-*.exe。" -ForegroundColor Yellow
}
finally {
  Remove-Item $worker -Force -ErrorAction SilentlyContinue
}
