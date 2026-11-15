"""Measure cold-start latency of a built exe: launch -> main window visible.

Usage: python tools/_startup_bench.py <exe> <file> [runs]
Prints the median and the individual samples in milliseconds.
"""
import ctypes
import ctypes.wintypes as wt
import statistics
import subprocess
import sys
import time

user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))


def visible_windows(part):
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
    return found


def bench(exe, sample, runs):
    times = []
    for _ in range(runs):
        start = time.perf_counter()
        process = subprocess.Popen([exe, sample])
        deadline = start + 30.0
        while time.perf_counter() < deadline:
            if visible_windows("PoemReader"):
                break
            time.sleep(0.005)
        else:
            process.terminate()
            raise SystemExit("window never appeared")
        times.append((time.perf_counter() - start) * 1000)
        # let the first paint settle before killing, so nothing is measured mid-init
        time.sleep(0.6)
        process.terminate()
        process.wait()
        time.sleep(0.5)
    return times


if __name__ == "__main__":
    exe, sample = sys.argv[1], sys.argv[2]
    runs = int(sys.argv[3]) if len(sys.argv) > 3 else 5
    samples = bench(exe, sample, runs)
    print("samples ms:", " ".join(f"{t:.0f}" for t in samples))
    print(f"median {statistics.median(samples):.0f} ms  min {min(samples):.0f} ms")
