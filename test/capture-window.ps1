# ウィンドウ（Emacs のフレームなど）を PNG に保存する（T-028 の自動確認用）。
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File capture-window.ps1 <HWND（10 進）> <出力の PNG>
#
# PrintWindow（PW_RENDERFULLCONTENT）で、子ウィンドウ（DXGI のフリップモデルのスワップチェーン）も含めて
# DWM が合成した内容を取る。ウィンドウが他のウィンドウの下にあっても取れる。
param(
    [Parameter(Mandatory = $true)][long]$Hwnd,
    [Parameter(Mandatory = $true)][string]$Out
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class EmbTermCapture {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
'@
[void][EmbTermCapture]::SetProcessDPIAware()
$h = [IntPtr]$Hwnd
$r = New-Object EmbTermCapture+RECT
if (-not [EmbTermCapture]::GetWindowRect($h, [ref]$r)) { throw "GetWindowRect に失敗: $Hwnd" }
$w = $r.Right - $r.Left
$ht = $r.Bottom - $r.Top
$bmp = New-Object System.Drawing.Bitmap $w, $ht
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
$ok = [EmbTermCapture]::PrintWindow($h, $hdc, 2)
$g.ReleaseHdc($hdc)
$g.Dispose()
if (-not $ok) { throw "PrintWindow に失敗: $Hwnd" }
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "$Out ${w}x${ht}"
