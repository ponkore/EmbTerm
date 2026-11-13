#!/bin/sh
# 画面消去とカーソルの原点復帰（CSI 2J / CSI H）
printf 'before\n'
printf '\033[2J\033[H'
printf 'after\n'
