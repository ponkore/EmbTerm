# Child process run inside a ConPTY. Writes test VT sequences and logs what it observes.
import ctypes, sys, time
from ctypes import wintypes, byref

k = ctypes.WinDLL("kernel32", use_last_error=True)
k.CreateFileW.restype = wintypes.HANDLE
hout = k.CreateFileW("CONOUT$", 0xC0000000, 3, None, 3, 0, None)
hin = k.CreateFileW("CONIN$", 0xC0000000, 3, None, 3, 0, None)
log = open(sys.argv[1], "w", encoding="utf-8")


class COORD(ctypes.Structure):
    _fields_ = [("X", ctypes.c_short), ("Y", ctypes.c_short)]


class SMALL_RECT(ctypes.Structure):
    _fields_ = [("L", ctypes.c_short), ("T", ctypes.c_short), ("R", ctypes.c_short), ("B", ctypes.c_short)]


class CSBI(ctypes.Structure):
    _fields_ = [("size", COORD), ("cur", COORD), ("attr", wintypes.WORD), ("win", SMALL_RECT), ("max", COORD)]


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


def w(s):
    b = s.encode("utf-16-le")
    n = wintypes.DWORD()
    k.WriteConsoleW(hout, ctypes.c_wchar_p(s), len(b) // 2, byref(n), None)


def cur():
    c = CSBI()
    k.GetConsoleScreenBufferInfo(hout, byref(c))
    return c.cur.X, c.cur.Y


m = wintypes.DWORD()
k.GetConsoleMode(hout, byref(m))
log.write(f"initial out mode=0x{m.value:x}\n")
k.SetConsoleMode(hout, m.value | 0x4 | 0x8)

ESC = "\x1b"
ST = ESC + "\\"
tests = [
    ("OSC8", ESC + "]8;;https://example.com" + ST + "link" + ESC + "]8;;" + ST),
    ("OSC52", ESC + "]52;c;aGVsbG8=" + "\x07"),
    ("OSC9", ESC + "]9;hello-notify" + "\x07"),
    ("OSC777", ESC + "]777;notify;Title;Body" + "\x07"),
    ("OSC7", ESC + "]7;file://host/C:/Users" + ST),
    ("OSC1337", ESC + "]1337;File=inline=1;size=4:AAAA" + "\x07"),
    ("DEC2026", ESC + "[?2026h" + "sync" + ESC + "[?2026l"),
    ("APC_KITTY", ESC + "_Gf=24,s=1,v=1,a=T;AAAA" + ST),
    ("DCS_SIXEL", ESC + 'P0;0;0q"1;1;1;1#0;2;100;0;0#0!1~' + ST),
    ("KKP_PUSH", ESC + "[>1u"),
    ("KKP_POP", ESC + "[<u"),
    ("DECSCUSR", ESC + "[5 q"),
    ("SGR_UL", ESC + "[4:3m" + ESC + "[58:2::255:0:0m" + "curly" + ESC + "[0m"),
]
w("\r\n")
for name, seq in tests:
    x0, y0 = cur()
    w(f"<{name}>" + seq + f"</{name}>")
    x1, y1 = cur()
    log.write(f"{name}: cursor {x0},{y0} -> {x1},{y1} (expected dx={len(name)*2+5})\n")
    w("\r\n")

for ch in ["\u2460", "\u2605", "\u203b", "\u2500", "\u00b0", "\u03b1", "\u3042", "\U0001F600", "\u2764\ufe0f",
           "\U0001F468\u200d\U0001F469\u200d\U0001F467", "e\u0301"]:
    x0, y0 = cur()
    w(ch)
    x1, y1 = cur()
    log.write(f"width U+{'+'.join('%04X' % ord(c) for c in ch)}: dx={x1 - x0}\n")
    w("|\r\n")

log.flush()

# Phase 2: VT input mode
k.FlushConsoleInputBuffer(hin)
k.SetConsoleMode(hin, 0x200 | 0x80)  # ENABLE_VIRTUAL_TERMINAL_INPUT | EXTENDED_FLAGS (quick edit off)


def drain(label, secs):
    end = time.time() + secs
    rec = (INPUT_RECORD * 64)()
    n = wintypes.DWORD()
    out = []
    while time.time() < end:
        if k.WaitForSingleObject(hin, 100) != 0:
            continue
        if not k.ReadConsoleInputW(hin, rec, 64, byref(n)):
            break
        for i in range(n.value):
            r = rec[i]
            if r.EventType == 1:
                ke = r.Event.Key
                out.append(f"KEY down={ke.bKeyDown} vk=0x{ke.wVirtualKeyCode:x} sc=0x{ke.wVirtualScanCode:x} ch={ke.uChar!r} ctrl=0x{ke.dwControlKeyState:x}")
            elif r.EventType == 2:
                me = r.Event.Mouse
                out.append(f"MOUSE pos={me.pos.X},{me.pos.Y} btn=0x{me.dwButtonState:x} ctrl=0x{me.dwControlKeyState:x} flags=0x{me.dwEventFlags:x}")
            else:
                out.append(f"EVENT type={r.EventType}")
    log.write(f"--- {label}\n")
    # compress VT-mode chars into a string for readability
    s = "".join(o.split("ch=")[1].split(" ctrl")[0].strip("'") if o.startswith("KEY down=1 vk=0x0") else "" for o in out)
    log.write("chars(vk=0,down): " + repr(s) + "\n")
    for o in out:
        log.write(o + "\n")
    log.flush()


w(ESC + "[>0q" + ESC + "[c" + ESC + "[?2026$p" + ESC + "[>c" + ESC + "[?u" + ESC + "_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA" + ST + ESC + "[16t" + ESC + "]11;?" + ESC + "P$q q" + ST + ESC + "[?996n")
w("<READY-VT>")
drain("VT input mode", 3.0)

# Phase 3: console (non-VT) input with mouse
k.SetConsoleMode(hin, 0x10 | 0x80 | 0x8)  # MOUSE | EXTENDED_FLAGS | WINDOW
k.FlushConsoleInputBuffer(hin)
w("<READY-CON>")
drain("console input mode", 3.0)
w("<DONE>\r\n")
log.close()
