param(
    [string]$Combo = "ctrl+space",
    [string]$Text = ""
)

$src = @"
using System;
using System.Runtime.InteropServices;
using System.Threading;

public class Keys {
  // The INPUT union must be as large as MOUSEINPUT (32 bytes on x64), which
  // makes sizeof(INPUT) == 40. Windows rejects the call if cbSize is wrong.
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion U; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion {
    [FieldOffset(0)] public MOUSEINPUT mi;
    [FieldOffset(0)] public KEYBDINPUT ki;
    [FieldOffset(0)] public HARDWAREINPUT hi;
  }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct HARDWAREINPUT { public uint uMsg; public ushort wParamL; public ushort wParamH; }
  [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint n, INPUT[] p, int size);
  [DllImport("user32.dll")] public static extern short GetAsyncKeyState(int vkey);

  public static void Unicode(string s) {
    foreach (char c in s) {
      INPUT[] down = new INPUT[1];
      down[0].type = 1;
      down[0].U.ki = new KEYBDINPUT { wVk = 0, wScan = c, dwFlags = 0x0004, time = 0, dwExtraInfo = IntPtr.Zero };
      SendInput(1, down, Marshal.SizeOf(typeof(INPUT)));
      INPUT[] up = new INPUT[1];
      up[0].type = 1;
      up[0].U.ki = new KEYBDINPUT { wVk = 0, wScan = c, dwFlags = 0x0004 | 0x0002, time = 0, dwExtraInfo = IntPtr.Zero };
      SendInput(1, up, Marshal.SizeOf(typeof(INPUT)));
      Thread.Sleep(25);
    }
  }

  public static uint Key(ushort vk, bool up) {
    INPUT[] i = new INPUT[1];
    i[0].type = 1;
    i[0].U.ki = new KEYBDINPUT { wVk = vk, wScan = 0, dwFlags = (uint)(up ? 2 : 0), time = 0, dwExtraInfo = IntPtr.Zero };
    uint sent = SendInput(1, i, Marshal.SizeOf(typeof(INPUT)));
    return sent;
  }

  public static string Combo(ushort mod, ushort key) {
    uint a = Key(mod, false); Thread.Sleep(60);
    uint b = Key(key, false); Thread.Sleep(60);
    uint c = Key(key, true);  Thread.Sleep(60);
    uint d = Key(mod, true);
    return string.Format("sent={0},{1},{2},{3} sizeof={4}", a, b, c, d, Marshal.SizeOf(typeof(INPUT)));
  }
}
"@

$path = Join-Path $env:TEMP "mavis-sendkeys.cs"
Set-Content -Path $path -Value $src -Encoding UTF8
if (-not ("Keys" -as [type])) { Add-Type -Path $path }

$mods = @{ 'ctrl' = 0x11; 'alt' = 0x12; 'shift' = 0x10; 'win' = 0x5B }
$keys = @{ 'space' = 0x20; 'k' = 0x4B; 'j' = 0x4A; 'a' = 0x41 }

$parts = $Combo.ToLower() -split '\+'
$mod = 0xFFFF
$key = 0
foreach ($p in $parts) {
    $p = $p.Trim()
    if ($mods.ContainsKey($p)) { $mod = $mods[$p] }
    elseif ($keys.ContainsKey($p)) { $key = $keys[$p] }
}
if ($key -eq 0) { throw "unknown key in '$Combo'" }
if ($mod -eq 0xFFFF) { $mod = 0 }

[Keys]::Combo([uint16]$mod, [uint16]$key)
if ($Text -ne "") {
    Start-Sleep -Milliseconds 300
    [Keys]::Unicode($Text)
}
"sent $Combo $([string]$Text)"