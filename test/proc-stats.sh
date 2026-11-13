#!/bin/sh
# プロセスの使用量を 1 行で出す（T-128。Windows の test/proc-stats.ps1 に当たる）
#   sh test/proc-stats.sh PID
# 出す値: rss（MB）、vsz（MB）、cpu（%）、threads（スレッド数）、children（子孫の数）
pid=$1
[ -z "$pid" ] && { echo "使い方: proc-stats.sh PID" >&2; exit 2; }
set -- $(ps -o rss=,vsz=,%cpu= -p "$pid" 2>/dev/null)
rss=$(( ${1:-0} / 1024 ))
vsz=$(( ${2:-0} / 1024 ))
cpu=${3:-0}
# スレッド数（ps -M はスレッドを 1 行ずつ出す。1 行目は見出し）
threads=$(ps -M "$pid" 2>/dev/null | sed 1d | wc -l | tr -d " ")
children=$(pgrep -P "$pid" 2>/dev/null | wc -l | tr -d " ")
echo "rss=${rss}MB vsz=${vsz}MB cpu=${cpu}% threads=${threads} children=${children}"
