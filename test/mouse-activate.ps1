# EmbTerm の子 HWND の WM_MOUSEACTIVATE の戻り値を、フレームが前面かどうかと突き合わせて確かめる。
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File mouse-activate.ps1 <子 HWND（10 進）>
#
# 本物のマウスは動かさず、前面のウィンドウも変えない。出力は「foreground=<True|False> result=<戻り値>」。
# 期待: フレームが前面なら 3（MA_NOACTIVATE）、前面でなければ 1（MA_ACTIVATE）。
param([Parameter(Mandatory = $true)][long]$Hwnd)
$ErrorActionPreference = 'Stop'
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class EmbTermActivate {
    [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
}
'@
$h = [IntPtr]$Hwnd
$root = [EmbTermActivate]::GetAncestor($h, 2)  # GA_ROOT
$fg = [EmbTermActivate]::GetForegroundWindow() -eq $root
# wParam はトップレベルのウィンドウ、lParam は HTCLIENT(1) と WM_LBUTTONDOWN(0x201)
$r = [EmbTermActivate]::SendMessageW($h, 0x21, $root, [IntPtr]((0x201 -shl 16) -bor 1))
"foreground=$fg result=$r"
