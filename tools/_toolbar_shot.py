"""截图预览窗口的工具栏，可选在几个位置上悬停以看提示气泡。

用法：
    python tools/_toolbar_shot.py <exe> <sample> <out_prefix> [dx,dy ...]

`dx` 是从窗口右边缘往左量的距离，`dy` 是从窗口顶边往下量的距离；每给出一个
位置，就把指针挪过去、等提示弹出来，再截一张。第一张总是不悬停的全景。

注意：GetDIBits 交回来的是 BGRA，写 PNG 时要换回 RGBA，否则红蓝颠倒。
"""

import ctypes
import ctypes.wintypes as wt
import struct
import subprocess
import sys
import time
import zlib

user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)
user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))

SRCCOPY, DIB_RGB, BI_RGB = 0x00CC0020, 0, 0
MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP = 0x0002, 0x0004
INPUT_KEYBOARD, KEYEVENTF_KEYUP = 1, 0x0002
VK_CONTROL, VK_SHIFT, VK_P = 0x11, 0x10, 0x50


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [("wVk", wt.WORD), ("wScan", wt.WORD), ("dwFlags", wt.DWORD),
                ("time", wt.DWORD), ("dwExtraInfo", ctypes.POINTER(ctypes.c_ulong))]


class INPUT(ctypes.Structure):
    class _U(ctypes.Union):
        _fields_ = [("ki", KEYBDINPUT), ("padding", ctypes.c_byte * 32)]
    _anonymous_ = ("u",)
    _fields_ = [("type", wt.DWORD), ("u", _U)]


class HEAD(ctypes.Structure):
    _fields_ = [("biSize", wt.DWORD), ("biWidth", wt.LONG), ("biHeight", wt.LONG),
                ("biPlanes", wt.WORD), ("biBitCount", wt.WORD), ("biCompression", wt.DWORD),
                ("biSizeImage", wt.DWORD), ("biXPelsPerMeter", wt.LONG),
                ("biYPelsPerMeter", wt.LONG), ("biClrUsed", wt.DWORD),
                ("biClrImportant", wt.DWORD)]


def key(vk, up):
    event = INPUT(type=INPUT_KEYBOARD)
    event.ki = KEYBDINPUT(wVk=vk, wScan=0, dwFlags=KEYEVENTF_KEYUP if up else 0,
                          time=0, dwExtraInfo=None)
    user32.SendInput(1, ctypes.byref(event), ctypes.sizeof(INPUT))


def chunk(tag, data):
    body = tag + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)


def capture(path):
    width = user32.GetSystemMetrics(78)
    height = user32.GetSystemMetrics(79)
    left = user32.GetSystemMetrics(76)
    top = user32.GetSystemMetrics(77)
    screen = user32.GetDC(None)
    mem = gdi32.CreateCompatibleDC(screen)
    bmp = gdi32.CreateCompatibleBitmap(screen, width, height)
    gdi32.SelectObject(mem, bmp)
    gdi32.BitBlt(mem, 0, 0, width, height, screen, left, top, SRCCOPY)
    head = HEAD()
    head.biSize = ctypes.sizeof(HEAD)
    head.biWidth = width
    head.biHeight = -height
    head.biPlanes = 1
    head.biBitCount = 32
    head.biCompression = BI_RGB
    buf = ctypes.create_string_buffer(width * height * 4)
    gdi32.GetDIBits(mem, bmp, 0, height, buf, ctypes.byref(head), DIB_RGB)
    raw = buf.raw
    # BGRA -> RGBA，否则 PNG 里的红蓝是反的。
    rgba = bytearray(len(raw))
    rgba[0::4] = raw[2::4]
    rgba[1::4] = raw[1::4]
    rgba[2::4] = raw[0::4]
    rgba[3::4] = raw[3::4]
    rows = b"".join(b"\x00" + bytes(rgba[r * width * 4:(r + 1) * width * 4])
                    for r in range(height))
    with open(path, "wb") as out:
        out.write(b"\x89PNG\r\n\x1a\n"
                  + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
                  + chunk(b"IDAT", zlib.compress(rows, 1))
                  + chunk(b"IEND", b""))
    gdi32.DeleteObject(bmp)
    gdi32.DeleteDC(mem)
    user32.ReleaseDC(None, screen)


def window_of(part):
    found = []

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def visit(hwnd, _):
        size = user32.GetWindowTextLengthW(hwnd)
        if size:
            text = ctypes.create_unicode_buffer(size + 1)
            user32.GetWindowTextW(hwnd, text, size + 1)
            if part in text.value and user32.IsWindowVisible(hwnd):
                found.append(hwnd)
        return True

    user32.EnumWindows(visit, 0)
    return found[-1] if found else None


def main():
    exe, sample, prefix = sys.argv[1], sys.argv[2], sys.argv[3]
    spots = []
    for spec in sys.argv[4:]:
        dx, dy = spec.split(",")
        spots.append((int(dx), int(dy)))

    process = subprocess.Popen([exe, sample])
    time.sleep(5)
    main_window = window_of("PoemReader")
    if main_window is not None:
        # 点一下标题栏才抢得到前台：后台进程直接 SetForegroundWindow 会被
        # Windows 的前台锁拒掉，后面的按键就发给了别的窗口。
        place = wt.RECT()
        user32.GetWindowRect(main_window, ctypes.byref(place))
        user32.SetCursorPos(place.left + (place.right - place.left) // 2, place.top + 10)
        time.sleep(0.3)
        user32.mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0)
        time.sleep(0.1)
        user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)
        time.sleep(0.8)
    for vk in (VK_CONTROL, VK_SHIFT, VK_P):
        key(vk, False)
    time.sleep(0.1)
    key(VK_P, True)
    key(VK_SHIFT, True)
    key(VK_CONTROL, True)

    preview = None
    for _ in range(30):
        time.sleep(0.5)
        preview = window_of("打印预览")
        if preview is not None:
            break
    if preview is None:
        print("没有找到预览窗口")
        process.terminate()
        raise SystemExit(1)
    place = wt.RECT()
    user32.GetWindowRect(preview, ctypes.byref(place))
    left, top = place.left, place.top
    width, height = place.right - place.left, place.bottom - place.top
    print("预览窗口", left, top, width, height)

    user32.SetForegroundWindow(preview)
    time.sleep(0.8)
    # 指针先挪到窗口外，免得一进来就停在某个按钮上。
    user32.SetCursorPos(left - 40, top + 200)
    time.sleep(0.6)
    capture(prefix + "_base.png")
    print("base")

    for index, (dx, dy) in enumerate(spots):
        user32.SetCursorPos(left + width - dx, top + dy)
        time.sleep(1.5)  # 提示延时 700ms，留足余量
        capture(f"{prefix}_{index}.png")
        print("shot", index, "at", left + width - dx, top + dy)

    process.terminate()
    time.sleep(0.5)


if __name__ == "__main__":
    main()
