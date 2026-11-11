# 子ウィンドウ（EmbTerm の子 HWND など）に、左クリック（押して離す）を PostMessage で送る（T-028 の自動確認用）。
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File click-window.ps1 <HWND（10 進）> [X] [Y]
#
# X Y はクライアント座標（既定 20 20）。本物のマウスは動かさない。
param(
    [Parameter(Mandatory = $true)][long]$Hwnd,
    [int]$X = 20,
    [int]$Y = 20
)
$ErrorActionPreference = 'Stop'
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class EmbTermClick {
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint msg, IntPtr w, IntPtr l);
}
'@
$h = [IntPtr]$Hwnd
$l = [IntPtr](($Y -shl 16) -bor ($X -band 0xffff))
[void][EmbTermClick]::PostMessageW($h, 0x0201, [IntPtr]1, $l)   # WM_LBUTTONDOWN（MK_LBUTTON）
Start-Sleep -Milliseconds 50
[void][EmbTermClick]::PostMessageW($h, 0x0202, [IntPtr]0, $l)   # WM_LBUTTONUP
Write-Output "clicked $Hwnd ($X, $Y)"
