"""性能の測定（T-054）の負荷を出す小さな道具。標準ライブラリだけで動く。

  python -I test/perf-cat.py gen DIR                       # DIR に big10m.txt（10 MB）と lines1m.txt（100 万行）を作る
  python -I test/perf-cat.py cat FILE RESULT LABEL [HOLD]  # `cmd /c type FILE` の所要時間を測って RESULT に 1 行足す。HOLD 秒待ってから終わる
  python -I test/perf-cat.py colors RESULT LABEL N [HOLD]  # 画面いっぱいの色付きの文字（セルごとに前景・背景の色を変える）を N 回描く
                                                           # 環境変数 PERF_COLORS=fg / bg で前景だけ・背景だけを変える

`cat` は端末に出し切るまでの時間を測る（ConPTY は端末が読まないと詰まるので、アプリの書き終わりが端末の処理の速さになる）。
同じものを embterm.exe・Emacs の中の EmbTerm・Windows Terminal の中で動かして見比べる。
RESULT の 1 行は `LABEL 秒 バイト数 行数`。
"""
import ctypes
import os
import random
import subprocess
import sys
import time

kernel32 = ctypes.windll.kernel32


def setup_console():
    """VT の出力を有効にし、出力のコードページを UTF-8 にする"""
    h = kernel32.GetStdHandle(-11)
    mode = ctypes.c_uint32()
    kernel32.GetConsoleMode(h, ctypes.byref(mode))
    kernel32.SetConsoleMode(h, mode.value | 0x0004)
    kernel32.SetConsoleOutputCP(65001)


def gen(d):
    os.makedirs(d, exist_ok=True)
    # 10 MB: 1 行 100 バイト（98 文字 + CRLF）の行を約 10.5 万行
    line = "".join(chr(0x21 + (i % 94)) for i in range(90))
    with open(os.path.join(d, "big10m.txt"), "wb") as f:
        n = 0
        i = 0
        while n < 10 * 1024 * 1024:
            b = f"{i:07d} {line}\r\n".encode()
            f.write(b)
            n += len(b)
            i += 1
    # 100 万行: 1 行 13 バイト（`line0000000` + CRLF）
    with open(os.path.join(d, "lines1m.txt"), "wb") as f:
        f.write(b"".join(f"line{i:07d}\r\n".encode() for i in range(1_000_000)))
    for name in ("big10m.txt", "lines1m.txt"):
        p = os.path.join(d, name)
        print(name, os.path.getsize(p))


def count_lines(path):
    with open(path, "rb") as f:
        return f.read().count(b"\n")


def record(result, label, secs, nbytes, nlines):
    with open(result, "a", encoding="utf-8") as f:
        f.write(f"{label} {secs:.3f} {nbytes} {nlines}\n")


def cat(path, result, label, hold):
    # cmd の type は `/` を引数の印と読むので、`\` 区切りの絶対パスにする
    path = os.path.abspath(path)
    nbytes = os.path.getsize(path)
    nlines = count_lines(path)
    t0 = time.perf_counter()
    subprocess.run(["cmd.exe", "/c", "type", path])
    secs = time.perf_counter() - t0
    record(result, label, secs, nbytes, nlines)
    print(f"\r\n[perf-cat] {label}: {secs:.3f} s ({nbytes} bytes, {nlines} lines)")
    time.sleep(hold)


def colors(result, label, frames, hold):
    setup_console()
    size = os.get_terminal_size()
    cols, rows = size.columns, size.lines
    rnd = random.Random(1)
    # PERF_COLORS=fg / bg で前景だけ・背景だけを変える（既定は両方）
    which = os.environ.get("PERF_COLORS", "both")
    t0 = time.perf_counter()
    nbytes = 0
    for _ in range(frames):
        parts = ["\x1b[H"]
        for y in range(rows):
            for x in range(cols):
                fg = f"38;5;{rnd.randrange(256)}" if which != "bg" else "39"
                bg = f"48;5;{rnd.randrange(256)}" if which != "fg" else "49"
                parts.append(f"\x1b[{fg};{bg}m{chr(0x41 + rnd.randrange(26))}")
            if y < rows - 1:
                parts.append("\r\n")
        parts.append("\x1b[0m")
        data = "".join(parts).encode()
        nbytes += len(data)
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()
    secs = time.perf_counter() - t0
    record(result, label, secs, nbytes, frames)
    sys.stdout.buffer.write(f"\x1b[0m\r\n[perf-cat] {label}: {frames} frames {cols}x{rows} {secs:.3f} s\r\n".encode())
    sys.stdout.buffer.flush()
    time.sleep(hold)


def main():
    a = sys.argv[1:]
    if not a:
        sys.exit(__doc__)
    if a[0] == "gen":
        gen(a[1])
    elif a[0] == "cat":
        cat(a[1], a[2], a[3], float(a[4]) if len(a) > 4 else 0.0)
    elif a[0] == "colors":
        colors(a[1], a[2], int(a[3]), float(a[4]) if len(a) > 4 else 0.0)
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
