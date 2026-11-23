# ============================================================ 警告
# 这个脚本会**全局注入按键**、抢占前台焦点,并且反复启动/结束目标程序。
# 运行时请不要使用电脑:它会打断你的输入和焦点。
# 只在无人值守的机器上跑。
# ============================================================
param(
  # How many rounds to measure per application.
  [int]$Rounds = 5,
  # "ours", "theirs" or "both".
  [string]$Which = "both"
)

# Two latencies that decide how a launcher feels, measured with the same
# method for both programs:
#
#   1. 进程启动 -> 热键可用 : start the program, press the hotkey until the
#      window shows up. How long until the launcher answers at all?
#   2. 常驻热键 -> 窗口出现 : with the program already running, press the
#      hotkey and time how long the window takes to appear.
#
# The hotkey is pressed again to put the window away again; both programs
# toggle on their hotkey.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$ours = Join-Path $root 'target\release\ideas-launcher.exe'
$theirs = Join-Path $env:LOCALAPPDATA 'FlowLauncher\app-2.1.4\Flow.Launcher.exe'

$src = @"
using System;
using System.Runtime.InteropServices;
using System.Threading;
public class Hk {
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion U; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion {
    [FieldOffset(0)] public MOUSEINPUT mi;
    [FieldOffset(0)] public KEYBDINPUT ki;
  }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint n, INPUT[] p, int size);
  public static void Key(ushort vk, bool up) {
    INPUT[] i = new INPUT[1];
    i[0].type = 1;
    i[0].U.ki = new KEYBDINPUT { wVk = vk, wScan = 0, dwFlags = (uint)(up ? 2 : 0), time = 0, dwExtraInfo = IntPtr.Zero };
    SendInput(1, i, Marshal.SizeOf(typeof(INPUT)));
  }
  public static void Combo(ushort mod, ushort key) {
    Key(mod, false); Thread.Sleep(30);
    Key(key, false); Thread.Sleep(30);
    Key(key, true);  Thread.Sleep(30);
    Key(mod, true);
  }
}
"@
$csPath = Join-Path $env:TEMP "mavis-hotkey2.cs"
Set-Content -Path $csPath -Value $src -Encoding UTF8
if (-not ("Hk" -as [type])) { Add-Type -Path $csPath }

Add-Type -Namespace Hk2 -Name Win -MemberDefinition @'
[StructLayout(LayoutKind.Sequential)] public struct RECT { public int left, top, right, bottom; }
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
[DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
'@

function Get-AppWindow([string]$processName, [string]$title) {
    $p = Get-Process -Name $processName -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $p) { return [IntPtr]::Zero }
    $found = & (Join-Path $PSScriptRoot 'enum-windows.ps1') -Pid_ $p.Id |
        Select-String ([regex]::Escape($title)) | Select-Object -First 1
    if (-not $found) { return [IntPtr]::Zero }
    return [IntPtr]([int](($found -join '') -replace '.*hwnd=(\d+).*', '$1'))
}

function Measure-Startup([string]$exe, [string]$processName, [string]$title, [ushort]$mod, [ushort]$key) {
    Get-Process -Name $processName -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 900

    $t0 = Get-Date
    $proc = Start-Process $exe -PassThru -WindowStyle Hidden
    $ready = $null
    while (((Get-Date) - $t0).TotalSeconds -lt 60) {
        # Press once and then watch: pressing again would toggle the window
        # away again.
        [Hk]::Combo($mod, $key)
        $deadline = (Get-Date).AddSeconds(2)
        while ((Get-Date) -lt $deadline) {
            $h = Get-AppWindow $processName $title
            if ($h -ne [IntPtr]::Zero -and [Hk2.Win]::IsWindowVisible($h)) {
                $ready = ((Get-Date) - $t0).TotalMilliseconds
                break
            }
            Start-Sleep -Milliseconds 5
        }
        if ($ready) { break }
        Start-Sleep -Milliseconds 100
    }
    return [pscustomobject]@{ Ready = $ready; Process = $proc }
}

function Measure-Resident([string]$processName, [string]$title, [ushort]$mod, [ushort]$key) {
    $h = Get-AppWindow $processName $title
    if ($h -eq [IntPtr]::Zero) { return $null }

    $samples = @()
    for ($i = 0; $i -lt 8; $i++) {
        # Make sure the window is away.
        if ([Hk2.Win]::IsWindowVisible($h)) {
            [Hk]::Combo($mod, $key)
            Start-Sleep -Milliseconds 600
        }
        $t0 = Get-Date
        [Hk]::Combo($mod, $key)
        $appeared = $null
        $deadline = $t0.AddSeconds(3)
        while ((Get-Date) -lt $deadline) {
            if ([Hk2.Win]::IsWindowVisible($h)) { $appeared = ((Get-Date) - $t0).TotalMilliseconds; break }
            Start-Sleep -Milliseconds 1
        }
        if ($appeared -ne $null) { $samples += $appeared }
        Start-Sleep -Milliseconds 400
    }
    if ($samples.Count -eq 0) { return $null }
    $sorted = $samples | Sort-Object
    return [pscustomobject]@{
        P50 = [math]::Round($sorted[[math]::Floor(($sorted.Count - 1) * 0.5)], 1)
        Min = [math]::Round($sorted[0], 1)
    }
}

if ($Which -eq 'ours' -or $Which -eq 'both') {
    Write-Host "== IDEAS Launcher ==" -ForegroundColor Cyan

    # Which hotkey did the launcher actually take? It prefers Alt+Space and
    # falls back to Ctrl+Space when Flow Launcher already owns it.
    $logDir = Join-Path $env:APPDATA 'IdeasLauncher\Logs'
    Get-Process -Name 'ideas-launcher' -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 400
    Start-Process $ours -PassThru -WindowStyle Hidden | Out-Null
    Start-Sleep -Seconds 2
    $taken = Get-ChildItem $logDir -Filter 'ideas-launcher-*.log' -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    $line = if ($taken) { Get-Content $taken.FullName -Tail 40 | Where-Object { $_ -match 'global hotkey active' } | Select-Object -Last 1 } else { $null }
    $mod = 0x12; $key = 0x20
    if ($line -and $line -match 'Ctrl \+ Space') { $mod = 0x11 }
    Write-Host ("  生效的热键: {0}" -f ($line -replace '.*active: ', ''))

    $start = @()
    for ($i = 0; $i -lt $Rounds; $i++) {
        $r = Measure-Startup $ours 'ideas-launcher' 'IDEAS Launcher' $mod $key
        if ($r.Ready) { $start += $r.Ready; Write-Host ("  进程启动 -> 热键可用 : {0:N0} ms" -f $r.Ready) }
    }
    $resident = Measure-Resident 'ideas-launcher' 'IDEAS Launcher' $mod $key
    if ($resident) { Write-Host ("  常驻热键 -> 窗口出现 : p50 {0} ms (min {1} ms)" -f $resident.P50, $resident.Min) }
    Get-Process -Name 'ideas-launcher' -ErrorAction SilentlyContinue | Stop-Process -Force
}

if (($Which -eq 'theirs' -or $Which -eq 'both') -and (Test-Path $theirs)) {
    Write-Host "== Flow Launcher ==" -ForegroundColor Cyan
    $start = @()
    for ($i = 0; $i -lt $Rounds; $i++) {
        $r = Measure-Startup $theirs 'Flow.Launcher' 'Flow Launcher' 0x12 0x20
        if ($r.Ready) { $start += $r.Ready; Write-Host ("  进程启动 -> 热键可用 : {0:N0} ms" -f $r.Ready) }
    }
    $resident = Measure-Resident 'Flow.Launcher' 'Flow Launcher' 0x12 0x20
    if ($resident) { Write-Host ("  常驻热键 -> 窗口出现 : p50 {0} ms (min {1} ms)" -f $resident.P50, $resident.Min) }
    Get-Process -Name 'Flow.Launcher' -ErrorAction SilentlyContinue | Stop-Process -Force
}
