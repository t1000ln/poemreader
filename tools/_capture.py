"""Launch the reader, press Ctrl+Shift+P and save a screenshot.

GDI + ctypes only: this machine refuses to load extra .NET types, so the PNG is
written by hand from a bottom-up 32-bit DIB.
"""
import ctypes
import ctypes.wintypes as wt
import struct
import sys
import time
import zlib

user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)

user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))

SRCCOPY = 0x00CC0020
DIB_RGB_COLORS = 0
BI_RGB = 0

INPUT_KEYBOARD = 1
KEYEVENTF_KEYUP = 0x0002
VK_CONTROL, VK_SHIFT, VK_P = 0x11, 0x10, 0x50


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", wt.DWORD), ("biWidth", wt.LONG), ("biHeight", wt.LONG),
        ("biPlanes", wt.WORD), ("biBitCount", wt.WORD), ("biCompression", wt.DWORD),
        ("biSizeImage", wt.DWORD), ("biXPelsPerMeter", wt.LONG),
        ("biYPelsPerMeter", wt.LONG), ("biClrUsed", wt.DWORD), ("biClrImportant", wt.DWORD),
    ]


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

    header = BITMAPINFOHEADER()
    header.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    header.biWidth = width
    header.biHeight = -height
    header.biPlanes = 1
    header.biBitCount = 32
    header.biCompression = BI_RGB

    buffer = ctypes.create_string_buffer(width * height * 4)
    gdi32.GetDIBits(memory, bitmap, 0, height, buffer, ctypes.byref(header), DIB_RGB_COLORS)

    rows = []
    for row in range(height):
        line = buffer.raw[row * width * 4:(row + 1) * width * 4]
        out = bytearray(width * 4)
        for column in range(width):
            blue, green, red = line[column * 4], line[column * 4 + 1], line[column * 4 + 2]
            out[column * 4:column * 4 + 4] = bytes((red, green, blue, 255))
        rows.append(b"\x00" + bytes(out))

    def chunk(tag, data):
        return (struct.pack(">I", len(data)) + tag + data
                + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(b"".join(rows), 6))
           + chunk(b"IEND", b""))
    with open(path, "wb") as file:
        file.write(png)

    gdi32.DeleteObject(bitmap)
    gdi32.DeleteDC(memory)
    user32.ReleaseDC(None, screen)


def main():
    exe, document, shot = sys.argv[1], sys.argv[2], sys.argv[3]
    import subprocess
    process = subprocess.Popen([exe, document])
    time.sleep(4)

    # Ctrl+Shift+P opens the print preview in its own window.
    key(VK_CONTROL, False)
    key(VK_SHIFT, False)
    key(VK_P, False)
    time.sleep(0.1)
    key(VK_P, True)
    key(VK_SHIFT, True)
    key(VK_CONTROL, True)
    time.sleep(4)

    capture(shot)
    time.sleep(0.2)
    process.terminate()


main()
