# プロセスの資源の使用量を 1 行で出す（T-054 の測定用）。
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File proc-stats.ps1 <PID>
#
# 出力: ws_mb（ワーキングセット）private_mb（プライベート）handles threads cpu_s（CPU 時間の累計）
#       gpu_dedicated_mb gpu_shared_mb（GPU Process Memory のカウンタ。取れなければ -1）
#       children（子孫プロセスの数）conhost（子孫のうち OpenConsole.exe / conhost.exe の数）children_ws_mb（子孫のワーキングセットの合計）
param([Parameter(Mandatory = $true)][int]$ProcessId)
$ErrorActionPreference = 'Stop'
$p = Get-Process -Id $ProcessId
$ws = [math]::Round($p.WorkingSet64 / 1MB, 1)
$priv = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
$cpu = [math]::Round($p.TotalProcessorTime.TotalSeconds, 2)
function GpuMb([string]$name) {
    try {
        $s = (Get-Counter "\GPU Process Memory(pid_$($ProcessId)_*)\$name" -ErrorAction Stop).CounterSamples
        return [math]::Round((($s | Measure-Object -Property CookedValue -Sum).Sum) / 1MB, 1)
    } catch { return -1 }
}
$ded = GpuMb 'Dedicated Usage'
$shared = GpuMb 'Shared Usage'
# 子孫プロセス（親子関係で辿る）
$all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name, WorkingSetSize
$front = @($ProcessId)
$desc = @()
while ($front.Count -gt 0) {
    $next = @()
    foreach ($id in $front) {
        foreach ($c in $all | Where-Object { $_.ParentProcessId -eq $id -and $_.ProcessId -ne $ProcessId }) {
            if (-not ($desc | Where-Object { $_.ProcessId -eq $c.ProcessId })) { $desc += $c; $next += $c.ProcessId }
        }
    }
    $front = $next
}
$conhost = @($desc | Where-Object { $_.Name -match '^(OpenConsole|conhost)\.exe$' }).Count
$cws = [math]::Round((($desc | Measure-Object -Property WorkingSetSize -Sum).Sum) / 1MB, 1)
Write-Output "ws_mb=$ws private_mb=$priv handles=$($p.HandleCount) threads=$($p.Threads.Count) cpu_s=$cpu gpu_dedicated_mb=$ded gpu_shared_mb=$shared children=$($desc.Count) conhost=$conhost children_ws_mb=$cws"
