"""Open a picture's print preview, drag the picture, and screenshot before/after.

Used to check that dragging moves the picture inside the page: the two shots
should show a different part of the same picture.
"""
import ctypes
import ctypes.wintypes as wt
import subprocess
import sys
import time

import _capture  # noqa: E402  (the screenshot helper next door)

user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))

MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004


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


def rect_of(hwnd):
    place = wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(place))
    return place.left, place.top, place.right - place.left, place.bottom - place.top


def drag(x0, y0, x1, y1, steps=12):
    user32.SetCursorPos(x0, y0)
    time.sleep(0.2)
    user32.mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0)
    time.sleep(0.2)
    for step in range(1, steps + 1):
        user32.SetCursorPos(
            int(x0 + (x1 - x0) * step / steps), int(y0 + (y1 - y0) * step / steps)
        )
        time.sleep(0.06)
    time.sleep(0.4)
    user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)


def main():
    exe, document, before, after = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]

    process = subprocess.Popen([exe, document])
    time.sleep(4)

    _capture.key(_capture.VK_CONTROL, False)
    _capture.key(_capture.VK_SHIFT, False)
    _capture.key(_capture.VK_P, False)
    time.sleep(0.1)
    _capture.key(_capture.VK_P, True)
    _capture.key(_capture.VK_SHIFT, True)
    _capture.key(_capture.VK_CONTROL, True)
    time.sleep(5)

    preview = window_of("打印预览")
    if preview is None:
        print("no preview window")
        process.terminate()
        return
    left, top, width, height = rect_of(preview)
    print("preview rect:", left, top, width, height)
    user32.SetForegroundWindow(preview)
    time.sleep(1.0)

    centre_x, centre_y = left + width // 2, top + height // 2
    _capture.capture(before)
    time.sleep(0.3)

    drag(centre_x, centre_y, centre_x + 220, centre_y + 60)
    time.sleep(3)

    _capture.capture(after)
    time.sleep(0.2)
    process.terminate()


main()
