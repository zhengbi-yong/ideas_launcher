param(
  # The installer to test. Defaults to the verification installer, which
  # carries a different application id and therefore never touches the copy
  # the user actually runs.
  [string]$Installer,

  # Keep the temporary installation instead of removing it.
  [switch]$Keep
)

# Install the packaged launcher into a throw-away directory, check what landed
# there, and remove it again.
#
# The verification installer is used on purpose: with the normal application
# id, Inno Setup would read the test as an *upgrade* of the installation the
# user runs, and would uninstall it.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root 'dist'

if (-not $Installer) {
    $Installer = Join-Path $dist 'verify\IdeasLauncher-verify-setup.exe'
}
if (-not (Test-Path $Installer)) {
    throw "找不到安装程序:$Installer。先运行 tools\build-installer.ps1 -Verify。"
}

$work = Join-Path $dist 'verify-install'
$appDir = Join-Path $work 'app'
if (Test-Path $work) { Remove-Item $work -Recurse -Force }
New-Item -ItemType Directory -Force -Path $work | Out-Null

try {
    Write-Host "== 静默安装(验证用,不同的 AppId)==" -ForegroundColor Cyan
    $p = Start-Process $Installer `
        -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART','/TASKS=""',"/DIR=$appDir" `
        -Wait -PassThru
    if ($p.ExitCode -ne 0) { throw "安装程序退出码:$($p.ExitCode)" }

    $exe = Join-Path $appDir 'IdeasLauncher.exe'
    if (-not (Test-Path $exe)) { throw "安装后没有找到可执行文件" }

    $built = (Get-FileHash (Join-Path $root 'target\release\ideas-launcher.exe') -Algorithm SHA256).Hash
    $installed = (Get-FileHash $exe -Algorithm SHA256).Hash
    Write-Host "安装的 exe 与构建产物一致 : $($built -eq $installed)"

    foreach ($file in @(
            '快速开始.txt',
            'runtime\README.md',
            'runtime\python\python_plugin.py')) {
        Write-Host ("{0,-34} : {1}" -f $file, (Test-Path (Join-Path $appDir $file)))
    }

    Write-Host "`n== 卸载 ==" -ForegroundColor Cyan
    $u = Start-Process (Join-Path $appDir 'unins000.exe') `
        -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART' -Wait -PassThru
    Write-Host "卸载程序退出码:$($u.ExitCode)"
    Start-Sleep -Seconds 2
    Write-Host "卸载后残留 : $(Test-Path $appDir)"
}
finally {
    if (-not $Keep -and (Test-Path $work)) {
        Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}
