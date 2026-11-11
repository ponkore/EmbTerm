# Minimal ConPTY host: runs child.py inside a pseudoconsole, records raw output, answers queries,
# and injects test input. Usage: python harness.py inbox|bundled <conpty.dll path> <outprefix> [flags]
import ctypes, sys, threading, time, re, os
from ctypes import wintypes, byref

kind = sys.argv[1]
dllpath = sys.argv[2]
prefix = sys.argv[3]
flags = int(sys.argv[4], 0) if len(sys.argv) > 4 else 0
k = ctypes.WinDLL("kernel32", use_last_error=True)


class COORD(ctypes.Structure):
    _fields_ = [("X", ctypes.c_short), ("Y", ctypes.c_short)]


if kind == "inbox":
    Create = k.CreatePseudoConsole
    Close = k.ClosePseudoConsole
else:
    d = ctypes.WinDLL(dllpath)
    Create = d.ConptyCreatePseudoConsole
    Close = d.ConptyClosePseudoConsole
Create.argtypes = [COORD, wintypes.HANDLE, wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(ctypes.c_void_p)]
Create.restype = ctypes.c_long
Close.argtypes = [ctypes.c_void_p]

inR, inW, outR, outW = (wintypes.HANDLE() for _ in range(4))
k.CreatePipe(byref(inR), byref(inW), None, 0)
k.CreatePipe(byref(outR), byref(outW), None, 0)
hpc = ctypes.c_void_p()
hr = Create(COORD(80, 25), inR, outW, flags, byref(hpc))
print("create hr", hex(hr & 0xffffffff))
k.CloseHandle(inR)
k.CloseHandle(outW)


class STARTUPINFOW(ctypes.Structure):
    _fields_ = [("cb", wintypes.DWORD), ("lpReserved", wintypes.LPWSTR), ("lpDesktop", wintypes.LPWSTR),
                ("lpTitle", wintypes.LPWSTR), ("dwX", wintypes.DWORD), ("dwY", wintypes.DWORD),
                ("dwXSize", wintypes.DWORD), ("dwYSize", wintypes.DWORD), ("dwXCountChars", wintypes.DWORD),
                ("dwYCountChars", wintypes.DWORD), ("dwFillAttribute", wintypes.DWORD), ("dwFlags", wintypes.DWORD),
                ("wShowWindow", wintypes.WORD), ("cbReserved2", wintypes.WORD), ("lpReserved2", ctypes.c_void_p),
                ("hStdInput", wintypes.HANDLE), ("hStdOutput", wintypes.HANDLE), ("hStdError", wintypes.HANDLE)]


class STARTUPINFOEXW(ctypes.Structure):
    _fields_ = [("StartupInfo", STARTUPINFOW), ("lpAttributeList", ctypes.c_void_p)]


class PROCESS_INFORMATION(ctypes.Structure):
    _fields_ = [("hProcess", wintypes.HANDLE), ("hThread", wintypes.HANDLE), ("dwProcessId", wintypes.DWORD),
                ("dwThreadId", wintypes.DWORD)]


size = ctypes.c_size_t()
k.InitializeProcThreadAttributeList(None, 1, 0, byref(size))
attr = ctypes.create_string_buffer(size.value)
k.InitializeProcThreadAttributeList(attr, 1, 0, byref(size))
k.UpdateProcThreadAttribute.argtypes = [ctypes.c_void_p, wintypes.DWORD, ctypes.c_size_t, ctypes.c_void_p,
                                        ctypes.c_size_t, ctypes.c_void_p, ctypes.c_void_p]
ok = k.UpdateProcThreadAttribute(attr, 0, 0x00020016, hpc, ctypes.sizeof(ctypes.c_void_p), None, None)
si = STARTUPINFOEXW()
si.StartupInfo.cb = ctypes.sizeof(STARTUPINFOEXW)
si.StartupInfo.dwFlags = 0x100  # STARTF_USESTDHANDLES with null handles so no inheritance of ours
si.lpAttributeList = ctypes.cast(attr, ctypes.c_void_p)
pi = PROCESS_INFORMATION()
here = os.path.dirname(os.path.abspath(__file__))
cmd = f'"{sys.executable}" -I "{os.path.join(here, "child.py")}" "{prefix}.child.log"'
ok = k.CreateProcessW(None, ctypes.create_unicode_buffer(cmd), None, None, False, 0x00080000, None, None,
                      byref(si), byref(pi))
print("createprocess", ok, ctypes.get_last_error())

buf = bytearray()
lock = threading.Lock()


def send(s):
    b = s.encode("utf-8")
    n = wintypes.DWORD()
    k.WriteFile(inW, b, len(b), byref(n), None)


answers = [
    (b"\x1b[c", "\x1b[?62;4;22;28c"),
    (b"\x1b[6n", "\x1b[5;1R"),
    (b"\x1b[>0q", "\x1bP>|Harness(1.0)\x1b\\"),
    (b"\x1b[?2026$p", "\x1b[?2026;2$y"),
    (b"\x1b[>c", "\x1b[>1;10;0c"),
    (b"\x1b[?u", "\x1b[?0u"),
    (b"\x1b_Gi=31,", "\x1b_Gi=31;OK\x1b\\"),
    (b"\x1b[16t", "\x1b[6;20;10t"),
    (b"\x1b]11;?", "\x1b]11;rgb:1111/2222/3333\x1b\\"),
    (b"\x1bP$q q", "\x1bP1$r2 q\x1b\\"),
    (b"\x1b[?996n", "\x1b[?997;1n"),
]
seen = {q: 0 for q, _ in answers}
triggers = {b"<READY-VT>": False, b"<READY-CON>": False}
test_input = ("\x1b[65;30;97;1;0;1_\x1b[65;30;97;0;0;1_"  # win32-input-mode: 'a' down/up
              "\x1b[97;5u"  # kitty: Ctrl+a
              "\x1b[<0;5;5M\x1b[<0;5;5m"  # SGR mouse press/release at (5,5)
              "x"
              "\x1b[17;29;0;1;8;1_\x1b[67;46;3;1;8;1_\x1b[67;46;3;0;8;1_\x1b[17;29;0;0;0;1_")  # win32im Ctrl+C


def reader():
    tmp = ctypes.create_string_buffer(65536)
    n = wintypes.DWORD()
    while True:
        if not k.ReadFile(outR, tmp, 65536, byref(n), None) or n.value == 0:
            break
        with lock:
            buf.extend(tmp.raw[:n.value])
            for q, a in answers:
                c = buf.count(q)
                while seen[q] < c:
                    seen[q] += 1
                    send(a)
            for t in triggers:
                if not triggers[t] and t in buf:
                    triggers[t] = True
                    time.sleep(0.3)
                    send(test_input)


th = threading.Thread(target=reader, daemon=True)
th.start()
k.WaitForSingleObject(pi.hProcess, 30000)
time.sleep(0.5)
threading.Thread(target=lambda: Close(hpc), daemon=True).start()
th.join(5)
with open(prefix + ".out.bin", "wb") as f:
    f.write(bytes(buf))


def esc(b):
    s = b.decode("utf-8", "replace")
    return s.replace("\x1b", "\\e").replace("\x07", "\\a").replace("\r", "\\r").replace("\n", "\\n\n")


with open(prefix + ".out.txt", "w", encoding="utf-8") as f:
    f.write(esc(bytes(buf)))
print("done", len(buf))
