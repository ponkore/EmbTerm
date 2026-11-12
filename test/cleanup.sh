#!/usr/bin/env bash
# 後始末（T-041）の自動確認。test/cleanup.el を emacs -Q で動かし、最後に Emacs を taskkill /F で止めて
# （kill-emacs-hook が走らない異常終了）、端末の子孫と OpenConsole.exe が残らないかを外から確かめる。
#   cargo xtask dist && bash test/cleanup.sh
set -u
cd "$(dirname "$0")/.."
EMACS=${EMACS:-C:/Apps/emacs/emacs-31.1/bin/emacs.exe}
OUT=target/t041
rm -f "$OUT/ready"
"$EMACS" -Q -l test/cleanup.el &
for _ in $(seq 1 60); do
  [ -f "$OUT/ready" ] && break
  sleep 1
done
if [ ! -f "$OUT/ready" ]; then
  echo "NG ready が来ない"; cat "$OUT/cleanup.log"; exit 1
fi
pid=$(cat "$OUT/ready")
pids=$(cat "$OUT/crash-pids.txt")
taskkill //F //PID "$pid" > /dev/null
sleep 4
ng=0
for p in $pids; do
  if tasklist //FI "PID eq $p" //NH | grep -q "$p"; then
    echo "NG 残った: $(tasklist //FI "PID eq $p" //NH | tr -s ' ')"
    ng=$((ng + 1))
  fi
done
echo "--- cleanup.log"
cat "$OUT/cleanup.log"
echo "--- 異常終了の後に残ったもの: $ng（$pids）"
grep -q '^\[[^]]*\] NG ' "$OUT/cleanup.log" && exit 1
[ "$ng" -eq 0 ]
