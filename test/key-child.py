# 端末から届いたキーと、端末の応答を記録する子プロセス（T-069 / T-070 / T-071 の確認用）。
#
#   python key-child.py <モード> <ログのパス>
#
# モード:
#   console   コンソールのアプリとして ReadConsoleInputW の KEY_EVENT を記録する（win32-input-mode の結果）
#   vt        ENABLE_VIRTUAL_TERMINAL_INPUT で読み、受けた文字列をそのまま記録する（レガシーの VT の結果）
#   vt-kkp    vt に加えて、起動時に CSI >5u（KKP）を求める。終わるときに CSI <u で戻す
#   query     VT 入力モードで端末への問い合わせ（CSI 6n、CSI 16t、CSI >0q、CSI c、OSC 11）を送り、応答を記録する
#   drop-kkp  vt-kkp のあと、CSI <u を出さずに落ちる（フラグが残ったまま子が死ぬ。安全策の確認）
#   syncout   同期出力を試す: CSI ?2026h を出し、途中の文字列を少しずつ書いて 300 ms 寝て CSI ?2026l で確定する
#   mouse     VT 入力モードで CSI ?1002;1006h（ボタンのドラッグ + SGR）を求めて、マウスの報告を記録する
#   scheme    VT 入力モードで CSI ?996n（配色の問い合わせ）を送り、mode 2031 を立てて、届いた答え・知らせを記録する（T-102）
#   altwheel  VT 入力モードで代替画面に入り、ホイールが矢印キーになるのを記録する（報告は無効のまま）
#
# ログは 1 行 1 イベントで、先頭に時刻（秒。time.time()）を付ける。送る側のログと時刻で突き合わせる。
# docs/research/conpty-probe/child.py を参考にした。
import ctypes
import sys
import time
from ctypes import wintypes, byref

k = ctypes.WinDLL("kernel32", use_last_error=True)
k.CreateFileW.restype = wintypes.HANDLE
k.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
k.GetConsoleMode.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
k.SetConsoleMode.argtypes = [wintypes.HANDLE, wintypes.DWORD]
k.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
k.WaitForSingleObject.restype = wintypes.DWORD
k.WriteConsoleW.argtypes = [wintypes.HANDLE, wintypes.LPCWSTR, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p]
k.ReadConsoleW.argtypes = [wintypes.HANDLE, ctypes.c_wchar_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p]
hout = k.CreateFileW("CONOUT$", 0xC0000000, 3, None, 3, 0, None)
hin = k.CreateFileW("CONIN$", 0xC0000000, 3, None, 3, 0, None)
mode = sys.argv[1]
log = open(sys.argv[2], "a", encoding="utf-8", buffering=1)


class COORD(ctypes.Structure):
    _fields_ = [("X", ctypes.c_short), ("Y", ctypes.c_short)]


class KEY_EVENT_RECORD(ctypes.Structure):
    _fields_ = [("bKeyDown", wintypes.BOOL), ("wRepeatCount", wintypes.WORD), ("wVirtualKeyCode", wintypes.WORD),
                ("wVirtualScanCode", wintypes.WORD), ("uChar", wintypes.WCHAR), ("dwControlKeyState", wintypes.DWORD)]


class MOUSE_EVENT_RECORD(ctypes.Structure):
    _fields_ = [("pos", COORD), ("dwButtonState", wintypes.DWORD), ("dwControlKeyState", wintypes.DWORD),
                ("dwEventFlags", wintypes.DWORD)]


class EVU(ctypes.Union):
    _fields_ = [("Key", KEY_EVENT_RECORD), ("Mouse", MOUSE_EVENT_RECORD), ("pad", ctypes.c_byte * 16)]


class INPUT_RECORD(ctypes.Structure):
    _fields_ = [("EventType", wintypes.WORD), ("Event", EVU)]


k.ReadConsoleInputW.argtypes = [wintypes.HANDLE, ctypes.POINTER(INPUT_RECORD), wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]


def stamp(line):
    log.write(f"{time.time():.3f} {line}\n")


def w(s):
    n = wintypes.DWORD()
    k.WriteConsoleW(hout, s, len(s.encode("utf-16-le")) // 2, byref(n), None)


def set_out_vt():
    m = wintypes.DWORD()
    k.GetConsoleMode(hout, byref(m))
    k.SetConsoleMode(hout, m.value | 0x4 | 0x8)  # VT 処理 + DISABLE_NEWLINE_AUTO_RETURN


def read_console_input_loop(end):
    rec = (INPUT_RECORD * 64)()
    n = wintypes.DWORD()
    while time.time() < end:
        if k.WaitForSingleObject(hin, 100) != 0:
            continue
        if not k.ReadConsoleInputW(hin, rec, 64, byref(n)):
            break
        for i in range(n.value):
            r = rec[i]
            if r.EventType == 1:
                ke = r.Event.Key
                stamp(f"KEY down={int(ke.bKeyDown)} vk=0x{ke.wVirtualKeyCode:x} sc=0x{ke.wVirtualScanCode:x} "
                      f"ch=0x{ord(ke.uChar):04x} ctrl=0x{ke.dwControlKeyState:x} rep={ke.wRepeatCount}")
            elif r.EventType == 2:
                me = r.Event.Mouse
                stamp(f"MOUSE pos={me.pos.X},{me.pos.Y} btn=0x{me.dwButtonState:x} flags=0x{me.dwEventFlags:x}")
            elif r.EventType == 16:
                stamp(f"FOCUS {bytes(r.Event.pad)[:1]}")


def read_text_loop(end):
    buf = ctypes.create_unicode_buffer(4096)
    n = wintypes.DWORD()
    while time.time() < end:
        if k.WaitForSingleObject(hin, 100) != 0:
            continue
        if not k.ReadConsoleW(hin, buf, 4096, byref(n), None):
            break
        if n.value:
            text = buf[:n.value]
            stamp("RAW " + text.encode("unicode_escape").decode("ascii"))


def main():
    set_out_vt()
    # 入力モード: 既定の行入力などをすべて落とす（ENABLE_EXTENDED_FLAGS は残して QuickEdit を切る）
    if mode == "console":
        k.SetConsoleMode(hin, 0x80)
        stamp("mode console (ReadConsoleInputW)")
        w("console mode\r\n")
        read_console_input_loop(time.time() + float(sys.argv[3]) if len(sys.argv) > 3 else time.time() + 120)
        return
    # VT 入力
    k.SetConsoleMode(hin, 0x80 | 0x200)
    if mode in ("vt-kkp", "drop-kkp"):
        w("\x1b[>5u")
        stamp("sent CSI >5u")
    stamp(f"mode {mode}")
    w(f"{mode} mode\r\n")
    if mode == "query":
        for name, seq in [("CPR", "\x1b[6n"), ("CELL_PX", "\x1b[16t"), ("XTVERSION", "\x1b[>0q"), ("DA1", "\x1b[c"), ("TEXT_SIZE", "\x1b[18t")]:
            stamp(f"QUERY {name}")
            w(seq)
            read_text_loop(time.time() + 0.4)
        return
    if mode == "mouse":
        w("\x1b[?1002;1006h")
        read_text_loop(time.time() + (float(sys.argv[3]) if len(sys.argv) > 3 else 120))
        w("\x1b[?1002;1006l")
        return
    if mode == "scheme":
        # T-102: 配色を問い合わせ、mode 2031（配色が変わったら知らせる）を立てて、届いたものを記録する
        stamp("QUERY 996")
        w("\x1b[?996n\x1b[?2031h")
        read_text_loop(time.time() + (float(sys.argv[3]) if len(sys.argv) > 3 else 120))
        w("\x1b[?2031l")
        return
    if mode == "altwheel":
        w("\x1b[?1049hin alt screen\r\n")
        read_text_loop(time.time() + (float(sys.argv[3]) if len(sys.argv) > 3 else 120))
        w("\x1b[?1049l")
        return
    if mode == "syncout":
        w("before-sync\r\n")
        time.sleep(0.4)
        w("\x1b[?2026h")
        stamp("hold begin")
        for i in range(10):
            w(f"partial-{i} ")
            time.sleep(0.03)
        w("\r\n\x1b[?2026l")
        stamp("hold end")
        w("after-sync\r\n")
        time.sleep(1.5)
        return
    read_text_loop(time.time() + (float(sys.argv[3]) if len(sys.argv) > 3 else 120))
    if mode == "vt-kkp":
        w("\x1b[<u")


main()
