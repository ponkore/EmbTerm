"""ホイールの行き先を確かめるための子プロセス（T-136）。

    python3 mouse-child.py mouse   マウスの報告（DEC 1000 + SGR 1006）を有効にする
    python3 mouse-child.py alt     代替画面（DEC 1049）に入る（マウスは見ない）

どちらも、受け取ったバイト列をそのまま読める形で 1 行ずつ画面に出す
（`embterm-module-pane-text` で読んで、何が届いたかを見る）。
"""

import os
import sys
import termios
import tty

mode = sys.argv[1] if len(sys.argv) > 1 else "mouse"
fd = 0
old = termios.tcgetattr(fd)
tty.setraw(fd)
if mode == "mouse":
    sys.stdout.write("\x1b[?1000h\x1b[?1006h")
elif mode == "alt":
    sys.stdout.write("\x1b[?1049h\x1b[H")
sys.stdout.write("READY " + mode + "\r\n")
sys.stdout.flush()
try:
    while True:
        b = os.read(fd, 1024)
        if not b:
            break
        # 1 文字でも q なら終わり
        if b == b"q":
            break
        sys.stdout.write("GOT " + repr(b)[2:-1] + "\r\n")
        sys.stdout.flush()
finally:
    if mode == "mouse":
        sys.stdout.write("\x1b[?1000l\x1b[?1006l")
    elif mode == "alt":
        sys.stdout.write("\x1b[?1049l")
    sys.stdout.flush()
    termios.tcsetattr(fd, termios.TCSADRAIN, old)
