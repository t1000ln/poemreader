"""Drive the preview: open it, pick a pages-per-sheet choice, screenshot.

Coordinates are passed as fractions of the window so a different DPI still
lands on the same control.
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


def click(x, y):
    user32.SetCursorPos(x, y)
    time.sleep(0.15)
    user32.mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0)
    time.sleep(0.05)
    user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)


def main():
    exe, document, shot = sys.argv[1], sys.argv[2], sys.argv[3]
    steps = sys.argv[4:]

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

    preview = window_of("PoemReader")
    if preview is None:
        print("no preview window")
        process.terminate()
        return
    left, top, width, height = rect_of(preview)
    print("preview rect:", left, top, width, height)
    user32.SetForegroundWindow(preview)
    time.sleep(0.8)
    # The first click on a background window only activates it, so press the
    # mouse once anywhere neutral to hand the window the focus for good.
    click(left + width // 2, top + height - 20)
    time.sleep(0.8)

    for step in steps:
        fx, fy = (float(part) for part in step.split(","))
        click(left + int(width * fx), top + int(height * fy))
        time.sleep(1.2)

    _capture.capture(shot)
    time.sleep(0.2)
    process.terminate()


main()
