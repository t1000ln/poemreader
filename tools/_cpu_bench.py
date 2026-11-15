"""Compare CPU cost of a fixed workload across builds.

Workload: launch with a PDF, open the print preview (renders pages at print
resolution), let it settle, then read the process' user+kernel time.

Usage: python tools/_cpu_bench.py <exe> <file> [runs] [wait_seconds]
"""
import ctypes
import ctypes.wintypes as wt
import statistics
import subprocess
import sys
import time

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))

INPUT_KEYBOARD, KEYEVENTF_KEYUP = 1, 0x0002
VK_CONTROL, VK_SHIFT, VK_P = 0x11, 0x10, 0x50

PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
FILETIME = wt.FILETIME


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [("wVk", wt.WORD), ("wScan", wt.WORD), ("dwFlags", wt.DWORD),
                ("time", wt.DWORD), ("dwExtraInfo", ctypes.POINTER(ctypes.c_ulong))]


class INPUT(ctypes.Structure):
    class _U(ctypes.Union):
        _fields_ = [("ki", KEYBDINPUT), ("padding", ctypes.c_byte * 32)]

    _anonymous_ = ("u",)
    _fields_ = [("type", wt.DWORD), ("u", _U)]


def key(vk, up):
    event = INPUT(type=INPUT_KEYBOARD)
    event.ki = KEYBDINPUT(wVk=vk, wScan=0, dwFlags=KEYEVENTF_KEYUP if up else 0,
                          time=0, dwExtraInfo=None)
    user32.SendInput(1, ctypes.byref(event), ctypes.sizeof(INPUT))


def window_of(part):
    found = []

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def visit(hwnd, _):
        n = user32.GetWindowTextLengthW(hwnd)
        if n:
            buf = ctypes.create_unicode_buffer(n + 1)
            user32.GetWindowTextW(hwnd, buf, n + 1)
            if part in buf.value and user32.IsWindowVisible(hwnd):
                found.append(hwnd)
        return True

    user32.EnumWindows(visit, 0)
    return found[-1] if found else None


def cpu_seconds(pid):
    handle = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        return None
    creation, exit_, kernel, user = FILETIME(), FILETIME(), FILETIME(), FILETIME()
    ok = kernel32.GetProcessTimes(handle, ctypes.byref(creation), ctypes.byref(exit_),
                                 ctypes.byref(kernel), ctypes.byref(user))
    kernel32.CloseHandle(handle)
    if not ok:
        return None

    def to_s(ft):
        return ((ft.dwHighDateTime << 32) | ft.dwLowDateTime) / 1e7

    return to_s(kernel) + to_s(user)


def run_once(exe, sample, wait):
    process = subprocess.Popen([exe, sample])
    deadline = time.perf_counter() + 30
    while time.perf_counter() < deadline:
        if window_of("PoemReader"):
            break
        time.sleep(0.01)
    time.sleep(1.2)
    before = cpu_seconds(process.pid)
    # open the print preview, which rasterises every page for the preview strip
    for vk in (VK_CONTROL, VK_SHIFT, VK_P):
        key(vk, False)
    time.sleep(0.05)
    key(VK_P, True)
    key(VK_SHIFT, True)
    key(VK_CONTROL, True)
    time.sleep(wait)
    cpu = cpu_seconds(process.pid)
    opened = window_of("打印预览") is not None
    cpu = cpu - before
    process.terminate()
    process.wait()
    time.sleep(0.4)
    return cpu, opened


if __name__ == "__main__":
    exe, sample = sys.argv[1], sys.argv[2]
    runs = int(sys.argv[3]) if len(sys.argv) > 3 else 3
    wait = float(sys.argv[4]) if len(sys.argv) > 4 else 8.0
    results = [run_once(exe, sample, wait) for _ in range(runs)]
    samples = [r[0] for r in results]
    print("preview opened:", all(r[1] for r in results))
    print("preview cpu s (delta):", " ".join(f"{s:.2f}" for s in samples))
    print(f"median {statistics.median(samples):.2f} s")
