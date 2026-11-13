#!/bin/sh
# 画面いっぱいに色付きの文字を出す（T-128 の測定。セルごとに色を変える）
i=0
while [ $i -lt 120 ]; do
  j=0
  line=""
  while [ $j -lt 180 ]; do
    fg=$(( (i * 7 + j * 3) % 256 ))
    bg=$(( (i * 5 + j * 11) % 256 ))
    line="$line\033[38;5;${fg}m\033[48;5;${bg}mX"
    j=$((j + 1))
  done
  printf "$line\033[0m\n"
  i=$((i + 1))
done
