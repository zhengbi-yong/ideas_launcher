param(
  # The process whose keys should be released.
  [int]$ProcessId = 0
)

# Release every key that might be stuck down.
#
# A key that is held down makes the system repeat it forever, and a program
# that reads the keyboard then keeps receiving the same key. The only way out
# is to tell the system that the key is up again — for the keys of the
# keyboard, and for the characters that were injected as Unicode.

$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;

public class Release {
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion U; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion {
    [FieldOffset(0)] public MOUSEINPUT mi;
    [FieldOffset(0)] public KEYBDINPUT ki;
  }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }

  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] p, int size);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, IntPtr extra);

  public static void KeyUp(ushort vk) {
    INPUT[] input = new INPUT[1];
    input[0].type = 1;
    input[0].U.ki = new KEYBDINPUT { wVk = vk, wScan = 0, dwFlags = 0x0002u, time = 0, dwExtraInfo = IntPtr.Zero };
    SendInput(1, input, Marshal.SizeOf(typeof(INPUT)));
  }

  public static void CharUp(char c) {
    INPUT[] input = new INPUT[1];
    input[0].type = 1;
    input[0].U.ki = new KEYBDINPUT { wVk = 0, wScan = c, dwFlags = 0x0004u | 0x0002u, time = 0, dwExtraInfo = IntPtr.Zero };
    SendInput(1, input, Marshal.SizeOf(typeof(INPUT)));
  }
}
"@

for ($vk = 0; $vk -le 0xFF; $vk++) {
    [Release]::KeyUp([uint16]$vk)
    [Release]::keybd_event([byte]$vk, 0, 0x0002, [IntPtr]::Zero) | Out-Null
}
for ($c = 0x20; $c -le 0x7E; $c++) {
    [Release]::CharUp([char]$c)
}
"released"
