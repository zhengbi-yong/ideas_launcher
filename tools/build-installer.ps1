param(
  # Build the release binary and compile the installer.
  [switch]$SkipBuild,
  # Also install the result silently, for a smoke test.
  [switch]$Install,
  # Also compile a *verification* installer.
  #
  # It carries a different application id, so that installing it never touches
  # the copy the user actually runs — Inno Setup reads the application id to
  # decide whether it is looking at an upgrade of the same program.
  [switch]$Verify
)

# Build the Windows installer for IDEAS Launcher.
#
# The result is a single `IdeasLauncher-<version>-setup.exe` in `dist/`. It is
# a per-user installer: no administrator rights are needed, and the uninstaller
# shows up in the user's own list of installed programs.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root

try {
    $iscc = Join-Path $env:LOCALAPPDATA 'Programs\InnoSetup\ISCC.exe'
    if (-not (Test-Path $iscc)) {
        $iscc = (Get-Command iscc.exe -ErrorAction SilentlyContinue).Source
    }
    if (-not $iscc -or -not (Test-Path $iscc)) {
        throw "Inno Setup 的编译器找不到。请安装 Inno Setup 6.x,或把它加进 PATH。"
    }

    # The version of the program and the version of the installer have to be
    # the same: an installer that says 0.1.0 while the program says 1.0.0
    # confuses everybody who looks at it in the list of installed programs.
    $cargoVersion = (Select-String -Path 'Cargo.toml' -Pattern '^version = "([^"]+)"').Matches[0].Groups[1].Value
    $issVersion = (Select-String -Path 'packaging\IdeasLauncher.iss' -Pattern '#define MyAppVersion "([^"]+)"').Matches[0].Groups[1].Value
    if ($cargoVersion -ne $issVersion) {
        throw "版本号对不上:Cargo.toml 是 $cargoVersion,打包脚本是 $issVersion。"
    }
    Write-Host ("版本 {0}" -f $cargoVersion) -ForegroundColor Cyan

    if (-not $SkipBuild) {
        Write-Host "== 编译 release ==" -ForegroundColor Cyan
        # A running copy holds the executable open, and the linker cannot
        # replace a file that is in use. A copy that was started with
        # administrator rights cannot even be ended from here — its file is
        # moved aside instead, and the linker gets a free name.
        foreach ($name in @('ideas-launcher', 'IdeasLauncher')) {
            Get-Process -Name $name -ErrorAction SilentlyContinue | ForEach-Object {
                try {
                    Stop-Process -Id $_.Id -Force -ErrorAction Stop
                } catch {
                    Write-Host "  $($_.ProcessName) ($($_.Id)) 需要管理员权限,先改名绕开" -ForegroundColor Yellow
                }
            }
        }
        Start-Sleep -Milliseconds 700
        $exe = Join-Path $root 'target\release\ideas-launcher.exe'
        if (Test-Path $exe) {
            $stamp = Get-Date -Format 'HHmmss'
            try {
                Rename-Item -Path $exe -NewName "ideas-launcher.stale-$stamp.exe" -ErrorAction Stop
            } catch {
                Write-Host "  旧程序改名失败: $($_.Exception.Message)" -ForegroundColor Yellow
            }
        }
        cargo build --release --workspace
        if ($LASTEXITCODE -ne 0) { throw "cargo build 失败" }
    }

    $staging = Join-Path $root 'dist\staging'
    $dist = Join-Path $root 'dist'
    New-Item -ItemType Directory -Force -Path $staging | Out-Null

    Write-Host "== 收集文件 ==" -ForegroundColor Cyan
    Copy-Item "target\release\ideas-launcher.exe" (Join-Path $staging 'IdeasLauncher.exe') -Force
    # The service that holds the index of the whole machine. It sits next to
    # the launcher, because the launcher starts it with administrative rights
    # and it needs to find itself there.
    if (Test-Path "target\release\ideas-launcher-service.exe") {
        Copy-Item "target\release\ideas-launcher-service.exe" $staging -Force
    }
    Copy-Item "packaging\快速开始.txt" $staging -Force

    # The runtime helpers the external plugins import. They sit next to the
    # executable, because the launcher puts that directory on PYTHONPATH.
    $runtimeSource = Join-Path $root 'runtime'
    $runtimeStaging = Join-Path $staging 'runtime'
    if (Test-Path $runtimeSource) {
        if (Test-Path $runtimeStaging) { Remove-Item $runtimeStaging -Recurse -Force }
        Copy-Item $runtimeSource $runtimeStaging -Recurse -Force
        # The development-only scripts do not belong in an installation.
        Get-ChildItem $runtimeStaging -Recurse -Filter '__pycache__' -Directory -ErrorAction SilentlyContinue |
            Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
    }

    Get-ChildItem $staging | Select-Object Name, @{n='KB';e={[int]($_.Length/1KB)}} | Format-Table -AutoSize

    Write-Host "== 编译安装程序 ==" -ForegroundColor Cyan
    & $iscc "packaging\IdeasLauncher.iss"
    if ($LASTEXITCODE -ne 0) { throw "ISCC 失败" }

    $setup = Get-ChildItem $dist -Filter 'IdeasLauncher-*-setup.exe' |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $setup) { throw "没有生成安装程序" }
    Write-Host ("安装程序:{0}  ({1:N2} MiB)" -f $setup.FullName, ($setup.Length / 1MB)) -ForegroundColor Green

    if ($Verify) {
        Write-Host "== 编译验证用安装程序 ==" -ForegroundColor Cyan
        $verifyDir = Join-Path $dist 'verify'
        New-Item -ItemType Directory -Force -Path $verifyDir | Out-Null
        # A different application id, so that installing it is not read as an
        # upgrade of the real installation. The braces have to be doubled:
        # the preprocessor would read a single one as a constant reference.
        & $iscc '/DMyAppId={{7F3B2C1A-9D4E-4A6B-8C5F-2E1D0A9B8C7D}}' `
                '/DMyOutputBase=IdeasLauncher-verify-setup' `
                '/DMyOutputDir=..\dist\verify' `
                '/DMyGroupName=IdeasLauncherVerify' `
                "packaging\IdeasLauncher.iss"
        if ($LASTEXITCODE -ne 0) { throw "ISCC(验证) 失败" }
        Write-Host ("验证安装程序:{0}" -f (Join-Path $verifyDir 'IdeasLauncher-verify-setup.exe')) -ForegroundColor Green
    }

    if ($Install) {
        Write-Host "== 静默安装(冒烟测试)==" -ForegroundColor Cyan
        $p = Start-Process $setup.FullName -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART' -Wait -PassThru
        Write-Host "安装程序退出码:$($p.ExitCode)"
        $installed = Join-Path $env:LOCALAPPDATA 'Programs\IdeasLauncher\IdeasLauncher.exe'
        if (Test-Path $installed) {
            Write-Host "已安装到 $installed" -ForegroundColor Green
        } else {
            throw "安装后没有找到可执行文件"
        }
    }
}
finally {
    Pop-Location
}
