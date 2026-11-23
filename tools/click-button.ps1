param(
  # The process whose window holds the button.
  [Parameter(Mandatory = $true)]
  [int]$ProcessId,

  # The name of the button. A part of it is enough — "下一步" finds
  # "下一步(&N) >" as well.
  [Parameter(Mandatory = $true)]
  [string]$Name,

  # How long to wait for the button to show up.
  [int]$TimeoutSeconds = 90,

  # Only look at what is there, without pressing anything.
  [switch]$List
)

# Press a button of a native window.
#
# The mouse is a poor tool for this: the windows move, the screen scale
# changes, and the same button sits somewhere else every time. The
# accessibility of a window knows where its buttons are, whatever language
# they are written in — and the Chinese buttons of the installer are
# translated, so the text in the source is of no use here.

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

function Get-Window {
  param([int]$Target)
  $root = [System.Windows.Automation.AutomationElement]::RootElement
  $condition = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ProcessIdProperty, $Target)
  $found = $root.FindAll([System.Windows.Automation.TreeScope]::Children, $condition)
  foreach ($element in $found) {
    try {
      $current = $element.Current
      if ($current.Name -and $current.IsOffscreen -eq $false) {
        return $element
      }
    } catch { }
  }
  return $null
}

function Get-Buttons {
  param([System.Windows.Automation.AutomationElement]$Window)
  $condition = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
    [System.Windows.Automation.ControlType]::Button)
  $found = $Window.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)
  $list = @()
  foreach ($button in $found) {
    try {
      $list += [pscustomobject]@{
        Name = $button.Current.Name
        Enabled = $button.Current.IsEnabled
        Offscreen = $button.Current.IsOffscreen
        Element = $button
      }
    } catch { }
  }
  return $list
}

$deadline = (Get-Date).AddSeconds($TimeoutSeconds)
$pressed = $false
while ((Get-Date) -lt $deadline) {
  $window = Get-Window -Target $ProcessId
  if ($window) {
    $buttons = Get-Buttons -Window $window
    if ($List) {
      foreach ($button in $buttons) {
        "  [{0}] {1}{2}" -f $_.Enabled, $button.Name,
          $(if ($button.Offscreen) { ' (offscreen)' } else { '' })
      }
    }
    $target = $buttons | Where-Object {
      $_.Name -and $_.Name.Contains($Name) -and $_.Enabled -and -not $_.Offscreen
    } | Select-Object -First 1
    if ($target) {
      if (-not $List) {
        $pattern = $target.Element.GetCurrentPattern(
          [System.Windows.Automation.InvokePattern]::Pattern)
        $pattern.Invoke()
        "pressed: {0}" -f $target.Name
      }
      $pressed = $true
      break
    }
  }
  Start-Sleep -Milliseconds 400
}

if (-not $pressed -and -not $List) {
  "no button matching '{0}'" -f $Name
  exit 1
}
if (-not $pressed) { exit 2 }
exit 0
