# 看一眼变化文件里都是些什么：名字、时间、有多少是重复的。
#
# 变化文件的格式：
#   0   "FSDL"
#   4   版本
#   8   基表号码
#   16  写下的时刻
#   24  条数
#   32  开始是每一条：
#       8  文件引用号      8  上级引用号     8  大小
#       4  建立时间        4  修改时间        4  访问时间
#       4  属性            1  是否已删除      1  大小时间是否已知
#       2  名字长度        n  名字

param(
    [string]$Profile = $env:USERPROFILE
)

$ErrorActionPreference = 'Stop'

$path = Join-Path $Profile 'AppData\Roaming\IdeasLauncher\Cache\fs-index.delta'
if (-not (Test-Path $path)) { Write-Host "没有变化文件: $path"; exit 1 }

$bytes = [System.IO.File]::ReadAllBytes($path)
$at = 0
function Take([int]$n) { $script:at += $n }
function U32 { $v = [System.BitConverter]::ToUInt32($bytes, $script:at); $script:at += 4; $v }
function U64 { $v = [System.BitConverter]::ToUInt64($bytes, $script:at); $script:at += 8; $v }
function I64 { $v = [System.BitConverter]::ToInt64($bytes, $script:at); $script:at += 8; $v }

$magic = [System.Text.Encoding]::ASCII.GetString($bytes, 0, 4)
$at = 4
$version = U32
$base = U64
$written = I64
$count = U64
Write-Host ("基表 {0}  写下于 {1}  条数 {2:N0}" -f $base, ([DateTimeOffset]::FromUnixTimeSeconds($written).LocalDateTime), $count)

$byName = @{}
$byExt = @{}
$deleted = 0
$recent = @{}
$old = 0
$now = [DateTimeOffset]::Now.ToUnixTimeSeconds()
$hours = @{}

for ($i = 0; $i -lt $count; $i++) {
    $frn = U64
    $parent = U64
    $size = U64
    $created = U32
    $modified = U32
    $accessed = U32
    $attributes = U32
    $del = $bytes[$at]; $at += 1
    $known = $bytes[$at]; $at += 1
    $len = [System.BitConverter]::ToUInt16($bytes, $at); $at += 2
    $name = [System.Text.Encoding]::UTF8.GetString($bytes, $at, $len); $at += $len
    if ($del -ne 0) { $deleted++ }
    $key = $name.ToLowerInvariant()
    if ($byName.ContainsKey($key)) { $byName[$key]++ } else { $byName[$key] = 1 }
    $ext = [System.IO.Path]::GetExtension($name).ToLowerInvariant()
    if ($ext -eq '') { $ext = '(无扩展名)' }
    if ($byExt.ContainsKey($ext)) { $byExt[$ext]++ } else { $byExt[$ext] = 1 }
    $age = ($now - $modified) / 3600
    if ($age -lt 1) { $recent['1 小时内'] = 1 + [int]$recent['1 小时内'] }
    elseif ($age -lt 6) { $recent['1–6 小时'] = 1 + [int]$recent['1–6 小时'] }
    elseif ($age -lt 24) { $recent['6–24 小时'] = 1 + [int]$recent['6–24 小时'] }
    elseif ($age -lt 168) { $recent['1–7 天'] = 1 + [int]$recent['1–7 天'] }
    else { $old++ }
}

Write-Host ""
Write-Host "按名字（重复的排在前面）:"
$byName.GetEnumerator() | Sort-Object Value -Descending | Select-Object -First 12 | ForEach-Object {
    "  {0,7:N0}  {1}" -f $_.Value, $_.Key
}
Write-Host ""
Write-Host "按扩展名:"
$byExt.GetEnumerator() | Sort-Object Value -Descending | Select-Object -First 12 | ForEach-Object {
    "  {0,7:N0}  {1}" -f $_.Value, $_.Key
}
Write-Host ""
Write-Host "按修改时间:"
$recent.GetEnumerator() | Sort-Object Name | ForEach-Object { "  {0,-10} {1,7:N0}" -f $_.Key, $_.Value }
Write-Host ("  {0,-10} {1,7:N0}" -f '更早', $old)
Write-Host ""
Write-Host ("已删除的 {0:N0} 条" -f $deleted)
