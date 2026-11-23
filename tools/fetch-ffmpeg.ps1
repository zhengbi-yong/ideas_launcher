# Fetch (or verify) the bundled FFmpeg runtime that the video preview needs.
#
# The FFmpeg files are LGPL binaries and are deliberately NOT in Git
# (~153 MB). This script either
#   1. verifies the files already in runtime\ffmpeg against the pinned
#      SHA-256 list below (default, no download), or
#   2. downloads a build you pass with -Url, extracts the nine files we
#      ship, and verifies the same hashes.
#
# The pinned hashes are the known-good set this project was built and
# measured with (FFmpeg shared build, avcodec 63). If you upgrade FFmpeg,
# update the hashes together and re-test video preview.
#
# Usage:
#   tools\fetch-ffmpeg.ps1                 # verify what is already there
#   tools\fetch-ffmpeg.ps1 -Url <zip>      # download + extract + verify
param(
    [string]$Url = "",
    [switch]$Force
)

$ErrorActionPreference = "Stop"
$dest = Join-Path $PSScriptRoot "..\runtime\ffmpeg"
$files = @{
    "avcodec-63.dll"     = "7cc2681a59070cd88e8b7e4271ccbd641427ee4183d8d88ffe4c966f2a042d6c"
    "avdevice-63.dll"    = "8f3f6bfa364f71845ba4e657d1d89fd8bedb39c0cdf0bcc03b4b364a4d606d84"
    "avfilter-12.dll"    = "9d414de2ee7dd576da7e777f9d5713b121f3b9fe70020b16fcc509c56e7f2d4e"
    "avformat-63.dll"    = "9f92fdc285456c4ca0ba60c89f4ce0e446aabe7b09783718b3fff81e5f23115d"
    "avutil-61.dll"      = "3c4b42ea0e2605b1804a927b159642f71ba3a773d9bb93c218e246f1f0c550d5"
    "ffmpeg.exe"         = "fa4f4bf78e08f1f0a74d07b82f66766b9bdf08f0bc53d91d2883a77cd01a7ed8"
    "ffprobe.exe"        = "068783cfd496dd992fb5ed23b3657d90a9ebf02fbd7df5a5c452cb9909878886"
    "swresample-7.dll"   = "18b2ae46cdee2fab407f73569426864ef776860cb6b688f89358b59496c77d2e"
    "swscale-10.dll"     = "63d0caa8f40a464402b4d61869a60998dba65c731a22236e522300b0537791bb"
}

function Test-Set {
    $missing = @()
    foreach ($name in $files.Keys) {
        $p = Join-Path $dest $name
        if (-not (Test-Path $p)) { $missing += $name; continue }
        $actual = (Get-FileHash -Algorithm SHA256 $p).Hash.ToLowerInvariant()
        if ($actual -ne $files[$name]) {
            throw "$name hash mismatch:`n  expected $($files[$name])`n  actual   $actual"
        }
    }
    return $missing
}

if ($Url -ne "" -or $Force) {
    if ($Url -eq "") { throw "Downloading requires -Url (a FFmpeg win64 shared build zip, e.g. from gyan.dev or BtbN releases)." }
    $zip = Join-Path $env:TEMP "ffmpeg-fetch.zip"
    Invoke-WebRequest -Uri $Url -OutFile $zip
    $tmp = Join-Path $env:TEMP ("ffmpeg-fetch-" + [guid]::NewGuid().ToString("N"))
    Expand-Archive -Path $zip -DestinationPath $tmp
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    foreach ($name in $files.Keys) {
        $src = Get-ChildItem -Path $tmp -Recurse -Filter $name | Select-Object -First 1
        if (-not $src) { throw "$name not found inside the archive" }
        Copy-Item $src.FullName (Join-Path $dest $name) -Force
    }
    Remove-Item -Recurse -Force $tmp, $zip
}

$missing = Test-Set
if ($missing.Count -eq 0) {
    Write-Host "OK: runtime\ffmpeg complete and every hash matches the pinned set."
} else {
    Write-Host "Missing (pass -Url to download): $($missing -join ', ')"
    exit 1
}
