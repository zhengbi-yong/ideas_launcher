param()

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class HK {
  [DllImport("user32.dll", SetLastError=true)] public static extern bool RegisterHotKey(IntPtr h, int id, uint m, uint v);
  [DllImport("user32.dll", SetLastError=true)] public static extern bool UnregisterHotKey(IntPtr h, int id);
  [DllImport("kernel32.dll")] public static extern int GetLastError();
}
"@

$combos = @(
  @{ name = 'Alt + Space';            m = 1;      v = 0x20 },
  @{ name = 'Ctrl + Space';           m = 2;      v = 0x20 },
  @{ name = 'Ctrl + Shift + Space';   m = 2 + 4;  v = 0x20 },
  @{ name = 'Win + Space';            m = 8;      v = 0x20 },
  @{ name = 'Ctrl + Alt + Space';     m = 2 + 1;  v = 0x20 },
  @{ name = 'Alt + K';                m = 1;      v = 0x4B },
  @{ name = 'Ctrl + Alt + K';         m = 2 + 1;  v = 0x4B },
  @{ name = 'Ctrl + Shift + K';       m = 2 + 4;  v = 0x4B },
  @{ name = 'Ctrl + J';               m = 2;      v = 0x4A },
  @{ name = 'Ctrl + Shift + J';       m = 2 + 4;  v = 0x4A }
)

$id = 9000
foreach ($c in $combos) {
    $id++
    $ok = [HK]::RegisterHotKey([IntPtr]::Zero, $id, [uint32]$c.m, [uint32]$c.v)
    $err = [HK]::GetLastError()
    $status = if ($ok) { 'FREE' } else { "busy (err=$err)" }
    "{0,-24} {1}" -f $c.name, $status
    if ($ok) { [HK]::UnregisterHotKey([IntPtr]::Zero, $id) | Out-Null }
}
