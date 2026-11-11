# 端末へ OSC・ベル・DECSCUSR を出す子プロセス（T-034 / T-035 / T-039 / T-040 の確認用。test/integration.el が使う）。
#
#   python -I osc-child.py <シーン> [引数...]
#
# シーン:
#   clip-write TEXT       OSC 52 で TEXT をクリップボードへ書く
#   clip-read LOG         OSC 52 の読み取り（?）を出し、1.5 秒の間に届いた入力（端末の答え）を LOG に書く
#   notify                OSC 9（本文だけ）と OSC 777（タイトルと本文）の通知
#   progress              OSC 9;4 の進捗（50% → 消す）
#   link ROW COL URI      画面を消して ROW 行 COL 桁（1 から）に OSC 8 のリンク「CLICK-HERE-LINK」を書く
#   bell N MS             BEL を N 回、MS ミリ秒ごとに出す
#   osc TEXT              TEXT（\e と \a を ESC と BEL に置き換える）をそのまま出す（DECSCUSR・OSC 11 など）
import base64
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

class KEY_EVENT_RECORD(ctypes.Structure):
    _fields_ = [("bKeyDown", wintypes.BOOL), ("wRepeatCount", wintypes.WORD), ("wVirtualKeyCode", wintypes.WORD),
                ("wVirtualScanCode", wintypes.WORD), ("uChar", wintypes.WCHAR), ("dwControlKeyState", wintypes.DWORD)]


class EVU(ctypes.Union):
    _fields_ = [("Key", KEY_EVENT_RECORD), ("pad", ctypes.c_byte * 16)]


class INPUT_RECORD(ctypes.Structure):
    _fields_ = [("EventType", wintypes.WORD), ("Event", EVU)]


k.ReadConsoleInputW.argtypes = [wintypes.HANDLE, ctypes.POINTER(INPUT_RECORD), wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]

ESC = "\x1b"
BEL = "\x07"
ST = ESC + "\\"


def w(s):
    n = wintypes.DWORD()
    k.WriteConsoleW(hout, s, len(s.encode("utf-16-le")) // 2, byref(n), None)


def set_out_vt():
    m = wintypes.DWORD()
    k.GetConsoleMode(hout, byref(m))
    k.SetConsoleMode(hout, m.value | 0x4)  # VT 処理


def main():
    set_out_vt()
    scene = sys.argv[1]
    if scene == "clip-write":
        data = base64.b64encode(sys.argv[2].encode("utf-8")).decode("ascii")
        w(f"{ESC}]52;c;{data}{BEL}")
        w("clip-write done\r\n")
    elif scene == "clip-read":
        log = open(sys.argv[2], "w", encoding="utf-8")
        k.SetConsoleMode(hin, 0x80 | 0x200)  # VT 入力（端末の答えをそのまま読む）
        w(f"{ESC}]52;c;?{BEL}")
        # ReadConsoleW は文字でないイベント（フォーカスの出入りなど）で待ち続けるので、ReadConsoleInputW で読む
        rec = (INPUT_RECORD * 256)()
        n = wintypes.DWORD()
        got = ""
        end = time.time() + 1.5
        while time.time() < end:
            if k.WaitForSingleObject(hin, 100) != 0:
                continue
            if not k.ReadConsoleInputW(hin, rec, 256, byref(n)):
                break
            for i in range(n.value):
                if rec[i].EventType == 1 and rec[i].Event.Key.bKeyDown and rec[i].Event.Key.uChar != "\x00":
                    got += rec[i].Event.Key.uChar
        log.write("RAW " + got.encode("unicode_escape").decode("ascii") + "\n")
        log.close()
        w("clip-read done\r\n")
    elif scene == "notify":
        w(f"{ESC}]9;osc9 body text{BEL}")
        w(f"{ESC}]777;notify;Claude Code;Claude is waiting for your input{BEL}")
        w("notify done\r\n")
    elif scene == "progress":
        w(f"{ESC}]9;4;1;50{BEL}")
        time.sleep(0.3)
        w(f"{ESC}]9;4;0{BEL}")
        w("progress done\r\n")
    elif scene == "link":
        row, col, uri = int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
        w(f"{ESC}[2J{ESC}[{row};{col}H{ESC}]8;;{uri}{ST}CLICK-HERE-LINK{ESC}]8;;{ST} plain-text")
        w(f"{ESC}[{row + 3};1H")
    elif scene == "bell":
        count, ms = int(sys.argv[2]), int(sys.argv[3])
        for _ in range(count):
            w(BEL)
            time.sleep(ms / 1000)
        w("bell done\r\n")
    elif scene == "osc":
        w(sys.argv[2].replace("\\e", ESC).replace("\\a", BEL))
    else:
        raise SystemExit(f"知らないシーン: {scene}")


main()
