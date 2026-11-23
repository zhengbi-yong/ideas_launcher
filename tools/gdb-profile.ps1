param(
  # The process to look at.
  [Parameter(Mandatory = $true)]
  [int]$ProcessId,

  # How many times the stacks are to be read.
  [int]$Samples = 30,

  # Where the folded stacks are written.
  #
  # The format is the one of the flame graphs: one line per stack, the frames
  # from the outermost to the innermost, separated by semicolons, and the
  # number of samples that had this stack at the end.
  [string]$Out = 'folded-stacks.txt',

  # The debugger.
  [string]$Gdb = ''
)

# The poor man's profiler: attach to the running program, read the stacks of
# every thread, and let it go again. It is the fallback when the sampling
# profiler of the system needs privileges that are not there.
#
# The program has to be built with symbols — on Windows that means the GNU
# target, because the debugger cannot read the symbols of the other one:
#
#   subst X: <the source directory>
#   cargo build --release --target x86_64-pc-windows-gnu \
#       --config 'profile.release.strip=false' --config 'profile.release.debug=1'

$ErrorActionPreference = 'Stop'

if (-not $Gdb) {
  $found = Get-Command gdb.exe -ErrorAction SilentlyContinue
  if ($found) {
    $Gdb = $found.Source
  } else {
    $Gdb = Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Directory -Filter '*WinLibs*' -ErrorAction SilentlyContinue |
      ForEach-Object { Join-Path $_.FullName 'mingw64\bin\gdb.exe' } |
      Where-Object { Test-Path $_ } |
      Select-Object -First 1
  }
}
if (-not $Gdb -or -not (Test-Path $Gdb)) {
  throw '找不到 gdb.exe'
}
Write-Host "调试器: $Gdb"

# The stacks of the threads, and how often each of them was seen.
$stacks = @{}
$self = @{}

# Put one stack into the tables.
function Add-Sample([string[]]$frames) {
  if ($frames.Count -eq 0) { return }
  $key = $frames -join ';'
  if ($stacks.ContainsKey($key)) { $stacks[$key]++ } else { $stacks[$key] = 1 }
  $top = $frames[-1]
  if ($self.ContainsKey($top)) { $self[$top]++ } else { $self[$top] = 1 }
}

# The stacks of the threads, and how often each of them was seen.
$stacks = @{}
$self = @{}
for ($i = 0; $i -lt $Samples; $i++) {
  $output = & $Gdb -q -batch -p $ProcessId `
    -ex 'set pagination off' `
    -ex 'set confirm off' `
    -ex 'thread apply all bt 24' `
    -ex 'detach' 2>&1

  # The frames of one thread, from the innermost to the outermost.
  $frames = @()
  $take = $true
  $threads = 0
  foreach ($line in $output) {
    $text = $line.ToString()
    if ($text -match '^Thread\s+\d+') {
      # A new thread: the frames of the one before it are done.
      if ($frames.Count -gt 0 -and $take) {
        $threads++
        Add-Sample $frames
      }
      $frames = @()
      $take = $true
      continue
    }
    if ($text -match '^\s*#(\d+)\s+(0x[0-9a-f]+)\s+in\s+(.+?)\s*\(\)') {
      $name = $Matches[3]
      # The thread that the debugger itself put in the process is of no use:
      # it sits in a break point and says nothing about the program.
      if ($name -match 'DbgBreakPoint|DbgUiRemoteBreakin') { $take = $false }
      $frames = @($name) + $frames
    }
  }
  if ($frames.Count -gt 0 -and $take) {
    $threads++
    Add-Sample $frames
  }

  Write-Host ("  {0}/{1} 取到 {2} 个线程" -f ($i + 1), $Samples, $threads)
}

Write-Host ''
Write-Host '== 自顶的函数（自己花掉的时间）=='
$self.GetEnumerator() | Sort-Object Value -Descending | Select-Object -First 20 | ForEach-Object {
  Write-Host ('  {0,5}  {1}' -f $_.Value, $_.Key)
}

$lines = $stacks.GetEnumerator() | ForEach-Object { "$($_.Key) $($_.Value)" }
[System.IO.File]::WriteAllText($Out, (($lines -join "`n") + "`n"), (New-Object System.Text.UTF8Encoding($false)))
Write-Host ''
Write-Host "折叠栈已写入: $Out  （$($stacks.Count) 个不同的栈）"
