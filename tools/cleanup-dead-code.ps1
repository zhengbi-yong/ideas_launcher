# 把服务里已经用不上的代码块删掉。
#
# 2026-11-09 那一次用它清掉了「手工拼安全描述符」那一套 —— 那条路走不通
# （见 analysis/RELEASE-GAP.md §1.22），代码留着只会骗人。
#
# 用法：先在下面的 $blocks 里写好 (起头标记, 结尾标记)，再跑这个脚本。
# 两端之间的内容会被删掉；标记是从上往下找的，所以顺序不影响。
#
# 注意：删完要重新编译，让编译器告诉你有没有漏掉引用。

$ErrorActionPreference = 'Stop'

$root = 'C:\Users\Sisyphus\Documents\private\ideas_launcher'
$file = Join-Path $root 'crates\launcher-service\src\winapi.rs'

# (起头标记, 结尾标记)
$blocks = @(
    # @('/// `SERVICE_STATUS_PROCESS`', '/// `SERVICE_TABLE_ENTRYW`'),
    # @('/// The security descriptor of the pipe.', '/// A string that lives for as long as the program does.')
)

if ($blocks.Count -eq 0) {
    Write-Host '没有要删的块 —— 在 $blocks 里写上(起头标记, 结尾标记)再跑。'
    exit 0
}

$text = [System.IO.File]::ReadAllText($file)
foreach ($pair in $blocks) {
    $start = $text.IndexOf($pair[0])
    if ($start -lt 0) { Write-Host "没找到: $($pair[0])"; continue }
    $end = $text.IndexOf($pair[1], $start)
    if ($end -lt 0) { Write-Host "没找到结尾: $($pair[1])"; continue }
    $text = $text.Remove($start, $end - $start)
    Write-Host "删掉: $($pair[0])"
}
[System.IO.File]::WriteAllText($file, $text, (New-Object System.Text.UTF8Encoding($false)))
Write-Host '好了 —— 重新编译看看有没有漏掉的引用。'
