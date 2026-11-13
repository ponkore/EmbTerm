#!/bin/sh
# カーソル位置指定（CUP）と行の挿入・削除、スクロール領域（DECSTBM）
printf '\033[2J\033[H'
printf '\033[3;5Hrow3-col5'
printf '\033[5;1Hrow5'
printf '\033[2;4r'          # 2〜4 行をスクロール領域に
printf '\033[4;1H'
printf 'A\nB\nC\n'          # 領域の中でスクロールする
printf '\033[r'             # 領域を戻す
printf '\033[8;1Hdone\n'
