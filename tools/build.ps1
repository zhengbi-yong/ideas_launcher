# Build the release binary.
#
# The launcher holds its own executable open while it runs, and the linker
# cannot replace a file that is in use. This script therefore ends the running
# copies first — and if a copy was started with administrator rights, the
# file has to be moved aside, because such a process cannot be ended from an
# ordinary one.
#
# Usage:
#   pwsh -File tools/build.ps1
#   pwsh -File tools/build.ps1 -Clean     # also throw the build cache away

param(
  # Throw the build cache away before building.
  [switch]$Clean
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root

try {
  Write-Host "== 结束运行中的启动器 ==" -ForegroundColor Cyan
  foreach ($name in @('ideas-launcher', 'IdeasLauncher')) {
    Get-Process -Name $name -ErrorAction SilentlyContinue | ForEach-Object {
      try {
        Stop-Process -Id $_.Id -Force -ErrorAction Stop
        Write-Host "  结束 $($_.ProcessName) ($($_.Id))"
      } catch {
        Write-Host "  $($_.ProcessName) ($($_.Id)) 需要管理员权限,先改名绕开" -ForegroundColor Yellow
      }
    }
  }
  Start-Sleep -Milliseconds 700

  # A file that is in use can still be renamed — the process keeps its handle
  # to the old name, and the linker gets a free name to write to.
  $exe = Join-Path $root 'target\release\ideas-launcher.exe'
  if (Test-Path $exe) {
    $stamp = Get-Date -Format 'HHmmss'
    $aside = Join-Path $root "target\release\ideas-launcher.stale-$stamp.exe"
    try {
      Rename-Item -Path $exe -NewName (Split-Path -Leaf $aside) -ErrorAction Stop
      Write-Host "  旧程序改名为 $(Split-Path -Leaf $aside)"
    } catch {
      Write-Host "  旧程序改名失败: $($_.Exception.Message)" -ForegroundColor Yellow
    }
  }

  if ($Clean) {
    Write-Host "== 清理构建缓存 ==" -ForegroundColor Cyan
    cargo clean
  }

  Write-Host "== 编译 release ==" -ForegroundColor Cyan
  cargo build --release --workspace
  if ($LASTEXITCODE -ne 0) { throw "cargo build 失败" }

  # The files that are in the way are collected in one place, so that they can
  # be thrown away in one go.
  $stale = Get-ChildItem (Join-Path $root 'target\release') -Filter 'ideas-launcher.stale-*.exe' -ErrorAction SilentlyContinue
  if ($stale) {
    Write-Host "== 被改名的旧程序 ==" -ForegroundColor Cyan
    $stale | ForEach-Object { Write-Host "  $($_.Name)" }
  }
}
finally {
  Pop-Location
}
