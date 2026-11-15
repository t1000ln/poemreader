"""Burst captures around an image drag: resting, held, and a timeline after release.

Lets the moments right after the button comes up be told apart: the drag layer
(the picture drawn by the GPU) against the re-rendered bitmap.
"""
import ctypes
import ctypes.wintypes as wt
import struct
import subprocess
import time
import zlib

user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)
user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))

MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP = 0x0002, 0x0004
INPUT_KEYBOARD, KEYEVENTF_KEYUP = 1, 0x0002
SRCCOPY, DIB_RGB, BI_RGB = 0x00CC0020, 0, 0
VK_CONTROL, VK_SHIFT, VK_P = 0x11, 0x10, 0x50


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", wt.WORD), ("wScan", wt.WORD), ("dwFlags", wt.DWORD),
        ("time", wt.DWORD), ("dwExtraInfo", ctypes.POINTER(ctypes.c_ulong)),
    ]


class INPUT(ctypes.Structure):
    class _U(ctypes.Union):
        _fields_ = [("ki", KEYBDINPUT), ("padding", ctypes.c_byte * 32)]
    _anonymous_ = ("u",)
    _fields_ = [("type", wt.DWORD), ("u", _U)]


class HEAD(ctypes.Structure):
    _fields_ = [
        ("biSize", wt.DWORD), ("biWidth", wt.LONG), ("biHeight", wt.LONG),
        ("biPlanes", wt.WORD), ("biBitCount", wt.WORD), ("biCompression", wt.DWORD),
        ("biSizeImage", wt.DWORD), ("biXPelsPerMeter", wt.LONG),
        ("biYPelsPerMeter", wt.LONG), ("biClrUsed", wt.DWORD), ("biClrImportant", wt.DWORD),
    ]


def key(vk, up):
    event = INPUT(type=INPUT_KEYBOARD)
    event.ki = KEYBDINPUT(wVk=vk, wScan=0, dwFlags=KEYEVENTF_KEYUP if up else 0,
                          time=0, dwExtraInfo=None)
    user32.SendInput(1, ctypes.byref(event), ctypes.sizeof(INPUT))


def capture(path):
    width = user32.GetSystemMetrics(78)
    height = user32.GetSystemMetrics(79)
    x = user32.GetSystemMetrics(76)
    y = user32.GetSystemMetrics(77)
    screen = user32.GetDC(None)
    memory = gdi32.CreateCompatibleDC(screen)
    bitmap = gdi32.CreateCompatibleBitmap(screen, width, height)
    gdi32.SelectObject(memory, bitmap)
    gdi32.BitBlt(memory, 0, 0, width, height, screen, x, y, SRCCOPY)
    header = HEAD()
    header.biSize = ctypes.sizeof(HEAD)
    header.biWidth = width
    header.biHeight = -height
    header.biPlanes = 1
    header.biBitCount = 32
    header.biCompression = BI_RGB
    buffer = ctypes.create_string_buffer(width * height * 4)
    gdi32.GetDIBits(memory, bitmap, 0, height, buffer, ctypes.byref(header), DIB_RGB)

    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    raw = b"".join(
        b"\x00" + buffer.raw[row * width * 4:(row + 1) * width * 4] for row in range(height)
    )
    open(path, "wb").write(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 1))
        + chunk(b"IEND", b"")
    )


def window_of(title_part):
    found = []

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def visit(hwnd, _):
        length = user32.GetWindowTextLengthW(hwnd)
        if length:
            buffer = ctypes.create_unicode_buffer(length + 1)
            user32.GetWindowTextW(hwnd, buffer, length + 1)
            if title_part in buffer.value and user32.IsWindowVisible(hwnd):
                found.append(hwnd)
        return True

    user32.EnumWindows(visit, 0)
    return found[-1] if found else None


def main():
    exe, document = sys.argv[1], sys.argv[2]
    process = subprocess.Popen([exe, document])
    time.sleep(5)

    for vk in (VK_CONTROL, VK_SHIFT, VK_P):
        key(vk, False)
    time.sleep(0.1)
    key(VK_P, True)
    key(VK_SHIFT, True)
    key(VK_CONTROL, True)
    time.sleep(6)

    preview = window_of("打印预览")
    if preview is None:
        print("no preview window")
        process.terminate()
        return
    place = wt.RECT()
    user32.GetWindowRect(preview, ctypes.byref(place))
    left, top = place.left, place.top
    width, height = place.right - place.left, place.bottom - place.top
    print("preview rect:", left, top, width, height)
    user32.SetForegroundWindow(preview)
    time.sleep(1.0)

    print("rest"); capture("_t_0_rest.png")
    x0, y0 = left + width // 2, top + height // 2
    user32.SetCursorPos(x0, y0)
    time.sleep(0.2)
    print("down"); user32.mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0)
    time.sleep(0.3)
    for step in range(1, 13):
        user32.SetCursorPos(x0 + 20 * step, y0 + 6 * step)
        time.sleep(0.05)
    time.sleep(0.6)
    print("held"); capture("_t_1_held.png")  # still holding, at the final position
    print("up"); user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)
    for delay, tag in ((0.15, "a"), (0.35, "b"), (0.7, "c"), (1.5, "d"), (4.0, "e")):
        time.sleep(delay)
        print("shot", tag); capture(f"_t_2_{tag}.png")
    process.terminate()
    print("done")


import sys

main()
