param(
  # The query to type into the search box.
  [string]$Query = '',

  # The file the screenshot is written to.
  [string]$Out = 'launcher.png',

  # How long to wait for the window to settle.
  [double]$Settle = 4.0,

  # Open the settings panel instead of the search box.
  [switch]$Settings,

  # Capture a launcher that is already running instead of starting one.
  #
  # The process id is the way to tell the instances apart: two launchers with
  # the same title are running side by side all the time (the development
  # build and the installed one), and a window is found by its process, not
  # by its title.
  [int]$ProcessId = 0,

  # The executable to start. Defaults to the release build.
  [string]$Exe = 'target\release\ideas-launcher.exe'
)

# Take a screenshot of the launcher window.
#
# The window is an OpenGL surface, so `PrintWindow` cannot read it — the pixels
# are copied from the screen instead. That means the window has to be on
# screen, and it is found by *process id*, because another instance with the
# same title may be running.
#
# The screen is scaled (150% here), so the process has to be marked as
# per-monitor DPI aware first; otherwise `GetWindowRect` reports scaled-down
# coordinates and the copy lands on the wrong part of the screen.

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class WindowCapture {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }

  /// The visible window of a process.
  public static IntPtr FindVisible(uint want) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid == want && IsWindowVisible(h)) { found = h; return false; }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
"@

[WindowCapture]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null

$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    $exePath = if (Test-Path $exe) { $exe } else { Join-Path $root $exe }
    if (-not (Test-Path $exePath)) { throw "找不到可执行文件:$exePath" }

    $arguments = @('--show', '--keep-open')
    if ($Settings) { $arguments += '--settings' }
    # The query is quoted: a query with a space ("file: input.mp4") would
    # otherwise be cut in two on the way to the program.
    if ($Query) { $arguments += @('--query', "`"$Query`"") }

    if ($ProcessId -gt 0) {
        # The launcher is already running: ask it to show its window and
        # photograph that.
        $target = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
        if (-not $target) { throw "进程 $ProcessId 没有在运行" }
        # A process that runs with more rights than this script hides its own
        # path: reading it needs the same rights. The installed launcher lives
        # in a fixed place, so fall back to that.
        $existing = $target.Path
        if (-not $existing) {
            $installed = Join-Path $env:LOCALAPPDATA 'Programs\IdeasLauncher\IdeasLauncher.exe'
            if (Test-Path $installed) { $existing = $installed }
        }
        if (-not $existing) { throw "不知道进程 $ProcessId 的可执行文件在哪" }
        Start-Process $existing -ArgumentList '--show', '--keep-open' | Out-Null
    }
    else {
        $process = Start-Process $exePath -ArgumentList $arguments -PassThru
    }

    try {
        Start-Sleep -Seconds $Settle
        $wanted = if ($ProcessId -gt 0) { [uint32]$ProcessId } else { [uint32]$process.Id }
        $hwnd = [WindowCapture]::FindVisible($wanted)
        if ($hwnd -eq [IntPtr]::Zero) { throw "没有找到可见窗口" }

        $rect = New-Object WindowCapture+RECT
        [WindowCapture]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
        $width = $rect.Right - $rect.Left
        $height = $rect.Bottom - $rect.Top
        Write-Host "窗口区域 : $($rect.Left),$($rect.Top) ${width}x${height}"

        $bitmap = New-Object System.Drawing.Bitmap $width, $height
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        $graphics.CopyFromScreen(
            $rect.Left, $rect.Top, 0, 0,
            (New-Object System.Drawing.Size $width, $height))
        $graphics.Dispose()
        $bitmap.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
        $bitmap.Dispose()
        Write-Host "截图     : $Out"
    }
    finally {
        # Only the launcher this script started is stopped. A launcher that
        # was already running belongs to the user.
        if ($ProcessId -eq 0 -and $process) {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        }
    }
}
finally {
    Pop-Location
}
