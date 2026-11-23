param([string]$Text = "")

$src = @"
using System;
using System.Runtime.InteropServices;
using System.Threading;

public class TypeText {
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion U; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion {
    [FieldOffset(0)] public MOUSEINPUT mi;
    [FieldOffset(0)] public KEYBDINPUT ki;
  }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint n, INPUT[] p, int size);

  public static void Send(string s) {
    foreach (char c in s) {
      INPUT[] down = new INPUT[1];
      down[0].type = 1;
      down[0].U.ki = new KEYBDINPUT { wVk = 0, wScan = c, dwFlags = 0x0004, time = 0, dwExtraInfo = IntPtr.Zero };
      SendInput(1, down, Marshal.SizeOf(typeof(INPUT)));
      INPUT[] up = new INPUT[1];
      up[0].type = 1;
      up[0].U.ki = new KEYBDINPUT { wVk = 0, wScan = c, dwFlags = 0x0004 | 0x0002, time = 0, dwExtraInfo = IntPtr.Zero };
      SendInput(1, up, Marshal.SizeOf(typeof(INPUT)));
      Thread.Sleep(120);
    }
  }
}
"@
$path = Join-Path $env:TEMP "mavis-typetext.cs"
Set-Content -Path $path -Value $src -Encoding UTF8
if (-not ("TypeText" -as [type])) { Add-Type -Path $path }
[TypeText]::Send($Text)
"typed '$Text'"
