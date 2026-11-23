param(
    [string]$Text = "",
    [string]$Out = ""
)

$root = "C:\Users\Sisyphus\Documents\private\快速启动软件"
$p = Get-Process -Name "launcher-app" -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $p) { "launcher is not running"; exit 1 }

$line = (& "$root\tools\enum-windows.ps1" -Pid_ $p.Id | Select-String "Fast Launcher")
if (-not $line) { "launcher window not found"; exit 1 }

$visible = ($line -join '') -match 'visible=True'
"launcher visible before = $visible"

if ($visible) {
    # Hide first, then show again: showing always focuses and focuses the
    # search box, which is what the text injection needs.
    & "$root\tools\send-hotkey.ps1" -Combo "ctrl+space" | Out-Null
    Start-Sleep -Milliseconds 700
}
& "$root\tools\send-hotkey.ps1" -Combo "ctrl+space" | Out-Null
Start-Sleep -Milliseconds 1500

if ($Text -ne "") {
    & "$root\tools\send-text.ps1" -Text $Text
    Start-Sleep -Milliseconds 900
}

$line2 = (& "$root\tools\enum-windows.ps1" -Pid_ $p.Id | Select-String "Fast Launcher")
$line2
$visible2 = ($line2 -join '') -match 'visible=True'
"launcher visible after = $visible2"

if ($Out -ne "") {
    $s = "C:\Users\Sisyphus\.codex\skills\screenshot\scripts\take_screenshot.ps1"
    powershell -ExecutionPolicy Bypass -File $s -Mode temp -Region $Out 2>&1 | Select-Object -Last 1
}
