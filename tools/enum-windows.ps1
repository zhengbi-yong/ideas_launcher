param([int]$Pid_ = 0)

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.Collections.Generic;

public class EnumWin {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int left, top, right, bottom; }
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);

  public static List<string> List(uint target) {
    var outp = new List<string>();
    EnumWindows((h, l) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid == target) {
        RECT r; GetWindowRect(h, out r);
        var t = new StringBuilder(256); GetWindowTextW(h, t, 256);
        var c = new StringBuilder(256); GetClassNameW(h, c, 256);
        outp.Add(string.Format("hwnd={0} class={1} title='{2}' visible={3} rect={4},{5}..{6},{7} size={8}x{9}",
          h, c, t, IsWindowVisible(h), r.left, r.top, r.right, r.bottom, r.right-r.left, r.bottom-r.top));
      }
      return true;
    }, IntPtr.Zero);
    return outp;
  }
}
"@

foreach ($line in [EnumWin]::List([uint32]$Pid_)) { $line }
