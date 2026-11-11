# 子ウィンドウ（EmbTerm の子 HWND）にマウスのメッセージを順に PostMessage で送る（T-063 の自動確認用）。
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File post-mouse.ps1 <HWND（10 進）> "<動作>,<動作>,..."
#
# 動作は次のどれか。X Y はクライアント座標、D はホイールの量（120 が 1 ノッチ、正は上）
#   down:X:Y  up:X:Y  move:X:Y  rdown:X:Y  rup:X:Y  wheel:D:X:Y
#   sdown:X:Y sup:X:Y smove:X:Y（Shift を押した扱い）
#   cdown:X:Y cup:X:Y cmove:X:Y（Ctrl を押した扱い。ハイパーリンクの Ctrl+クリック。T-039）
#   sleep:MS
# 本物のマウスは動かさない。
param(
    [Parameter(Mandatory = $true)][long]$Hwnd,
    [Parameter(Mandatory = $true)][string]$Actions
)
$ErrorActionPreference = 'Stop'
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class EmbTermPost {
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
}
'@
$h = [IntPtr]$Hwnd
function Pt([int]$x, [int]$y) { [IntPtr](($y -shl 16) -bor ($x -band 0xffff)) }
foreach ($a in $Actions.Split(',')) {
    $p = $a.Split(':')
    switch ($p[0]) {
        'down'  { [void][EmbTermPost]::PostMessageW($h, 0x0201, [IntPtr]1, (Pt $p[1] $p[2])) }
        'up'    { [void][EmbTermPost]::PostMessageW($h, 0x0202, [IntPtr]0, (Pt $p[1] $p[2])) }
        'move'  { [void][EmbTermPost]::PostMessageW($h, 0x0200, [IntPtr]1, (Pt $p[1] $p[2])) }
        'rdown' { [void][EmbTermPost]::PostMessageW($h, 0x0204, [IntPtr]2, (Pt $p[1] $p[2])) }
        'rup'   { [void][EmbTermPost]::PostMessageW($h, 0x0205, [IntPtr]0, (Pt $p[1] $p[2])) }
        'sdown' { [void][EmbTermPost]::PostMessageW($h, 0x0201, [IntPtr]5, (Pt $p[1] $p[2])) }
        'sup'   { [void][EmbTermPost]::PostMessageW($h, 0x0202, [IntPtr]4, (Pt $p[1] $p[2])) }
        'smove' { [void][EmbTermPost]::PostMessageW($h, 0x0200, [IntPtr]5, (Pt $p[1] $p[2])) }
        'cdown' { [void][EmbTermPost]::PostMessageW($h, 0x0201, [IntPtr]9, (Pt $p[1] $p[2])) }
        'cup'   { [void][EmbTermPost]::PostMessageW($h, 0x0202, [IntPtr]8, (Pt $p[1] $p[2])) }
        'cmove' { [void][EmbTermPost]::PostMessageW($h, 0x0200, [IntPtr]9, (Pt $p[1] $p[2])) }
        'wheel' {
            $pt = New-Object EmbTermPost+POINT
            $pt.X = [int]$p[2]; $pt.Y = [int]$p[3]
            [void][EmbTermPost]::ClientToScreen($h, [ref]$pt)
            $w = [IntPtr]([int64]([int]$p[1] -shl 16))
            [void][EmbTermPost]::PostMessageW($h, 0x020A, $w, (Pt $pt.X $pt.Y))
        }
        'sleep' { Start-Sleep -Milliseconds ([int]$p[1]) }
        default { throw "知らない動作: $a" }
    }
    Start-Sleep -Milliseconds 15
}
Write-Output "posted $Hwnd $Actions"
