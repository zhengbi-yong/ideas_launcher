# 看一眼盘上的索引缓存：基表多大、什么时候写的、变化文件里有多少条变化。
#
# 变化文件的头上有五个数：
#
#   0..4   "FSDL" —— 文件的记号
#   4..8   版本
#   8..16  基表的号码 —— 变化是相对哪张表写的
#   16..24 变化写下的时刻
#   24..32 变化有多少条
#
# 基表的号码必须和基表自己的号码一样，不然这份变化就属于另一张表，
# 读回的时候会被丢掉 —— 也就是说，那些变化就白记了。

param(
    [string]$Profile = $env:USERPROFILE
)

$ErrorActionPreference = 'Stop'

$cache = Join-Path $Profile 'AppData\Roaming\IdeasLauncher\Cache'
$table = Join-Path $cache 'fs-index.bin'
$delta = Join-Path $cache 'fs-index.delta'

function Get-Number($path, [int64]$offset, [int]$bytes) {
    $stream = [System.IO.File]::OpenRead($path)
    try {
        $stream.Seek($offset, [System.IO.SeekOrigin]::Begin) | Out-Null
        $buffer = New-Object byte[] $bytes
        $read = $stream.Read($buffer, 0, $bytes)
        if ($read -ne $bytes) { return $null }
        if ($bytes -eq 4) { return [System.BitConverter]::ToUInt32($buffer, 0) }
        return [System.BitConverter]::ToUInt64($buffer, 0)
    } finally {
        $stream.Dispose()
    }
}

if (-not (Test-Path $table)) {
    Write-Host "没有基表: $table"
    exit 1
}

$tableStamp = Get-Number $table 8 8
$tableSize = (Get-Item $table).Length
$tableTime = (Get-Item $table).LastWriteTime

Write-Host "基表 fs-index.bin"
Write-Host ("  大小     {0:N2} MiB" -f ($tableSize / 1MB))
Write-Host ("  号码     {0}" -f $tableStamp)
Write-Host ("  写入于   {0}" -f $tableTime)

if (-not (Test-Path $delta)) {
    Write-Host ""
    Write-Host "没有变化文件 fs-index.delta —— 盘上的表就是最新的。"
    exit 0
}

$deltaItem = Get-Item $delta
$magic = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($delta)[0..3])
$version = Get-Number $delta 4 4
$baseStamp = Get-Number $delta 8 8
$writtenAt = Get-Number $delta 16 8
$count = Get-Number $delta 24 8

Write-Host ""
Write-Host "变化文件 fs-index.delta"
Write-Host ("  大小     {0:N2} MiB" -f ($deltaItem.Length / 1MB))
Write-Host ("  记号     {0}  版本 {1}" -f $magic, $version)
Write-Host ("  基表号码 {0}" -f $baseStamp)
Write-Host ("  写入于   {0}" -f ([DateTimeOffset]::FromUnixTimeSeconds([int64]$writtenAt).LocalDateTime))
Write-Host ("  变化条数 {0:N0}" -f $count)

if ($baseStamp -eq $tableStamp) {
    Write-Host ""
    Write-Host "对得上：这份变化是相对盘上这张表写的。"
} else {
    Write-Host ""
    Write-Host "对不上：这份变化属于另一张表（基表号码 {0}，变化里的 {1}）。" -f $tableStamp, $baseStamp
    Write-Host "读回的时候它会被丢掉 —— 那张表上的变化就白记了。"
    exit 2
}
