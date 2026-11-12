"""Kitty graphics を端末へ直接送る小さな確認（T-088）。

  python -I test/kitty-graphics.py SCENE [--no-wait] [行数]    # SCENE は下の scene_* の名前。送ったあと待つ

embterm.exe の `-- python -I test/kitty-graphics.py SCENE` として動かし、`--screenshot` の PNG を目で見る。
画像は Python の標準ライブラリだけでここで作る（リポジトリの外の画像は使わない）。
"""
import base64
import ctypes
import struct
import sys
import time
import zlib

# 再配布版の ConPTY が通す VT の出力を有効にする
kernel32 = ctypes.windll.kernel32
ENABLE_VT = 0x0004
DISABLE_AUTO_RETURN = 0x0008


def setup_console():
    h = kernel32.GetStdHandle(-11)
    mode = ctypes.c_uint32()
    kernel32.GetConsoleMode(h, ctypes.byref(mode))
    kernel32.SetConsoleMode(h, mode.value | ENABLE_VT | DISABLE_AUTO_RETURN)
    kernel32.SetConsoleOutputCP(65001)


def out(data):
    """バッファリングせずにバイト列を書く"""
    if isinstance(data, str):
        data = data.encode("utf-8")
    sys.stdout.buffer.write(data)
    sys.stdout.buffer.flush()


def apc(control, payload=None):
    """APC 1 つ。大きなデータは 4096 バイトごとに分ける（m=1 の続き）"""
    if payload is None:
        out(b"\x1b_G" + control.encode() + b"\x1b\\")
        return
    b64 = base64.standard_b64encode(payload)
    if len(b64) <= 4096:
        out(b"\x1b_G" + control.encode() + b";" + b64 + b"\x1b\\")
        return
    first = True
    while b64:
        chunk, b64 = b64[:4096], b64[4096:]
        more = 1 if b64 else 0
        ctl = f"{control},m={more}" if first else f"m={more}"
        out(b"\x1b_G" + ctl.encode() + b";" + chunk + b"\x1b\\")
        first = False


def gradient_rgba(w, h):
    """左から右へ赤が増え、上から下へ緑が増える。不透明"""
    data = bytearray()
    for y in range(h):
        for x in range(w):
            data += bytes((x * 255 // max(w - 1, 1), y * 255 // max(h - 1, 1), 96, 255))
    return bytes(data)


def checker_rgba(w, h, block):
    """黄色の市松模様。空いたところは透明（下の文字が見える）"""
    data = bytearray()
    for y in range(h):
        for x in range(w):
            on = (x // block + y // block) % 2 == 0
            data += bytes((255, 220, 0, 255)) if on else bytes((0, 0, 0, 0))
    return bytes(data)


def png_rgba(w, h, rgba):
    """最小の PNG を作る（8 ビット RGBA）"""

    def chunk(tag, body):
        return struct.pack(">I", len(body)) + tag + body + struct.pack(">I", zlib.crc32(tag + body))

    raw = bytearray()
    for y in range(h):
        raw.append(0)
        raw += rgba[y * w * 4 : (y + 1) * w * 4]
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(bytes(raw))) + chunk(b"IEND", b"")


# 行・桁の結合文字（Kitty の rowcolumn-diacritics の最初の部分）
DIACRITICS = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486,
]


def placeholder_rows(image_id, rows, cols):
    """yazi と同じ Unicode placeholder（前景色の 24 ビットに画像の ID）の行の列を作る。各行は改行なしの 1 行分の文字列"""
    r, g, b = (image_id >> 16) & 255, (image_id >> 8) & 255, image_id & 255
    fg = f"\x1b[38;2;{r};{g};{b}m"
    out_rows = []
    for row in range(rows):
        s = fg
        for col in range(cols):
            s += chr(0x10EEEE) + chr(DIACRITICS[row]) + chr(DIACRITICS[col])
        s += "\x1b[39m"
        out_rows.append(s)
    return out_rows


def goto(row, col):
    out(f"\x1b[{row};{col}H")


def scene_basic():
    """PNG / RGB / RGBA の 3 枚を a=T で置く"""
    out("basic: PNG (a=T,f=100,t=d) RGB (f=24) RGBA (f=32) \r\n")
    goto(3, 1)
    png = png_rgba(120, 90, gradient_rgba(120, 90))
    apc("a=T,f=100,t=d,i=101,q=2", png)
    goto(3, 24)
    rgb = gradient_rgba(100, 80)
    rgb3 = bytes(b for i, b in enumerate(rgb) if i % 4 != 3)
    apc("a=T,f=24,s=100,v=80,i=102,q=2", rgb3)
    goto(3, 48)
    apc("a=T,f=32,s=100,v=80,i=103,q=2", checker_rgba(100, 80, 10))
    goto(14, 1)
    out("end of basic\r\n")


def scene_placeholder():
    """U=1 の仮想配置（Unicode placeholder）。8 桁 x 4 行"""
    img = png_rgba(160, 80, gradient_rgba(160, 80))
    apc("a=T,U=1,f=100,t=d,i=7,c=8,r=4,C=1,q=2", img)
    out("placeholder: 8 cols x 4 rows at row 2, then text\r\n")
    for r in placeholder_rows(7, 4, 8):
        out(r + "\r\n")
    out("end of placeholder\r\n")


def scene_delete():
    """2 枚を置いて、最初の 1 枚を a=d で消す"""
    out("delete: only the green one should remain\r\n")
    goto(3, 1)
    apc("a=T,f=32,s=80,v=60,i=11,q=2", bytes((255, 0, 0, 255)) * (80 * 60))
    goto(3, 30)
    apc("a=T,f=32,s=80,v=60,i=12,q=2", bytes((0, 255, 0, 255)) * (80 * 60))
    time.sleep(1.0)
    apc("a=d,d=I,i=11,q=2")
    goto(12, 1)
    out("end of delete\r\n")


def scene_source_z():
    """切り取り（x,y,w,h）・z-index。画像の上に文字 / 文字の上に画像"""
    out("source rect + z-index\r\n")
    goto(3, 1)
    apc("a=t,f=32,s=120,v=120,i=21,q=2", gradient_rgba(120, 120))
    # 右下 1/4 を切り取り、10 桁 x 5 行に拡大
    apc("a=p,i=21,x=60,y=60,w=60,h=60,c=10,r=5,q=2")
    goto(3, 15)
    # z<0: 文字の下
    apc("a=T,f=32,s=120,v=60,i=22,z=-1,q=2", checker_rgba(120, 60, 8))
    goto(3, 15)
    out("BELOW-TEXT-IMAGE")
    goto(9, 15)
    # z>=0: 文字の上
    out("ABOVE-TEXT-IMAGE")
    goto(9, 15)
    apc("a=T,f=32,s=120,v=60,i=23,z=1,q=2", checker_rgba(120, 60, 8))
    goto(14, 1)
    out("end of source_z\r\n")


def scene_scroll():
    """画像を置いたあとスクロールで流す（embterm.exe の --scroll-up で遡って確かめる）"""
    out("scroll: image at line 2, then lots of lines\r\n")
    apc("a=T,f=32,s=160,v=80,i=31,q=2", gradient_rgba(160, 80))
    out("\r\n")
    # 2 番目の引数の数が行数（既定は 50 行）
    n = next((int(a) for a in sys.argv[2:] if a.isdigit()), 50)
    for i in range(n):
        out(f"line {i}\r\n")


def png_big(w, h):
    """大きな PNG（8 ビット RGBA のグラデーション）。行ごとにまとめて作るので 4000x4000 でも数秒で済む"""
    row = bytearray(w * 4)
    row[0::4] = bytes(x * 255 // max(w - 1, 1) for x in range(w))
    row[2::4] = bytes((96,)) * w
    row[3::4] = bytes((255,)) * w
    raw = bytearray()
    for y in range(h):
        row[1::4] = bytes((y * 255 // max(h - 1, 1),)) * w
        raw.append(0)
        raw += row

    def chunk(tag, body):
        return struct.pack(">I", len(body)) + tag + body + struct.pack(">I", zlib.crc32(tag + body))

    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(bytes(raw), 6)) + chunk(b"IEND", b"")


def solid_rgba(w, h, rgb):
    return bytes((*rgb, 255)) * (w * h)


def scene_anim():
    """アニメーション（T-092）: 赤 → 緑 → 青を 300 ms ごとに繰り返す 80x60 の画像を 10 桁 x 4 行に置く"""
    out("anim: red -> green -> blue (300 ms each, loop)\r\n")
    w, h = 80, 60
    apc(f"a=t,f=32,s={w},v={h},i=301,q=2", solid_rgba(w, h, (220, 0, 0)))
    apc(f"a=f,f=32,s={w},v={h},i=301,z=300,q=2", solid_rgba(w, h, (0, 200, 0)))
    apc(f"a=f,f=32,s={w},v={h},i=301,z=300,q=2", solid_rgba(w, h, (0, 0, 220)))
    apc("a=a,i=301,r=1,z=300,q=2")
    goto(3, 1)
    apc("a=p,i=301,c=10,r=4,q=2")
    apc("a=a,i=301,s=3,v=1,q=2")
    goto(8, 1)
    out("end of anim\r\n")


def scene_big():
    """大きな PNG（既定 4000x4000。2 番目の引数で 1 辺の画素数）を 1 枚、40 桁 x 20 行に縮めて置く（T-096 のデコードの時間の測定）"""
    n = next((int(a) for a in sys.argv[2:] if a.isdigit()), 4000)
    t0 = time.perf_counter()
    png = png_big(n, n)
    out(f"big: {n}x{n} PNG {len(png)} bytes (made in {time.perf_counter() - t0:.1f} s)\r\n")
    t1 = time.perf_counter()
    apc("a=T,f=100,t=d,i=201,c=40,r=20,q=2", png)
    out(f"\r\nend of big (sent in {time.perf_counter() - t1:.1f} s)\r\n")


def main():
    setup_console()
    scene = sys.argv[1] if len(sys.argv) > 1 else "basic"
    fn = globals().get(f"scene_{scene}")
    if fn is None:
        sys.exit(f"シーンが無い: {scene}")
    fn()
    # 画面を保存して閉じるまで待つ（embterm.exe の --exit-after が閉じる）。--no-wait なら Emacs の中でシェルへ戻る
    if "--no-wait" not in sys.argv:
        time.sleep(60)


if __name__ == "__main__":
    main()
