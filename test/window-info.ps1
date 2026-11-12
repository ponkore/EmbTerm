# ウィンドウ（ペインの子 HWND など）が見えているかと、親のクライアント座標での位置を出す（T-044 の自動確認用）。
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File window-info.ps1 <HWND（10 進）>...
#
# 1 行に 1 つ: `HWND 見えているか(1/0) 左 上 右 下`。ウィンドウが無ければ `HWND gone`
param([Parameter(ValueFromRemainingArguments = $true)][long[]]$Hwnds)
$ErrorActionPreference = 'Stop'
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class EmbTermWindowInfo {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr h);
    [DllImport("user32.dll")] public static extern int MapWindowPoints(IntPtr from, IntPtr to, ref RECT r, int n);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
'@
[void][EmbTermWindowInfo]::SetProcessDPIAware()
foreach ($n in $Hwnds) {
    $h = [IntPtr]$n
    if (-not [EmbTermWindowInfo]::IsWindow($h)) { Write-Output "$n gone"; continue }
    $r = New-Object EmbTermWindowInfo+RECT
    [void][EmbTermWindowInfo]::GetWindowRect($h, [ref]$r)
    [void][EmbTermWindowInfo]::MapWindowPoints([IntPtr]::Zero, [EmbTermWindowInfo]::GetParent($h), [ref]$r, 2)
    $v = if ([EmbTermWindowInfo]::IsWindowVisible($h)) { 1 } else { 0 }
    Write-Output "$n $v $($r.Left) $($r.Top) $($r.Right) $($r.Bottom)"
}
