"""Native Windows console smoke test; standard library only, no Bitbucket access.

Uses a separate console so input injection and raw-mode checks cannot affect the
developer's terminal. See learn.microsoft.com/windows/console/console-handles.
"""

import ctypes as C
from ctypes import wintypes as W
from io import FileIO
import msvcrt
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import traceback


class Coord(C.Structure):
    _fields_ = [("x", W.SHORT), ("y", W.SHORT)]


class Rect(C.Structure):
    _fields_ = [("left", W.SHORT), ("top", W.SHORT), ("right", W.SHORT), ("bottom", W.SHORT)]


class BufferInfo(C.Structure):
    _fields_ = [("size", Coord), ("cursor", Coord), ("attributes", W.WORD),
                ("window", Rect), ("maximum", Coord)]


class KeyEvent(C.Structure):
    _fields_ = [("down", W.BOOL), ("repeat", W.WORD), ("virtual", W.WORD),
                ("scan", W.WORD), ("character", W.WCHAR), ("control", W.DWORD)]


class Event(C.Union):
    _fields_ = [("key", KeyEvent), ("padding", C.c_byte * 16)]


class InputRecord(C.Structure):
    _fields_ = [("kind", W.WORD), ("event", Event)]


kernel = C.WinDLL("kernel32", use_last_error=True)
for name, arguments in {
    "GetConsoleMode": [W.HANDLE, C.POINTER(W.DWORD)],
    "GetConsoleScreenBufferInfo": [W.HANDLE, C.POINTER(BufferInfo)],
    "ReadConsoleOutputCharacterW": [W.HANDLE, W.LPWSTR, W.DWORD, Coord, C.POINTER(W.DWORD)],
    "WriteConsoleOutputCharacterW": [W.HANDLE, W.LPCWSTR, W.DWORD, Coord, C.POINTER(W.DWORD)],
    "WriteConsoleInputW": [W.HANDLE, C.POINTER(InputRecord), W.DWORD, C.POINTER(W.DWORD)],
}.items():
    function = getattr(kernel, name)
    function.argtypes = arguments
    function.restype = W.BOOL


def check(result):
    if not result:
        raise C.WinError(C.get_last_error())


def mode(handle):
    result = W.DWORD()
    check(kernel.GetConsoleMode(handle, C.byref(result)))
    return result.value


def screen():
    # CONOUT$ opens the active buffer, including the alternate screen used by bb.
    # FileIO gives us an OS handle without Python's direction-only ConsoleIO wrapper.
    with FileIO("CONOUT$", "r+") as output:
        handle = msvcrt.get_osfhandle(output.fileno())
        info = BufferInfo()
        check(kernel.GetConsoleScreenBufferInfo(handle, C.byref(info)))
        length = info.size.x * (info.window.bottom - info.window.top + 1)
        result = C.create_unicode_buffer(length)
        count = W.DWORD()
        check(kernel.ReadConsoleOutputCharacterW(handle, result, length,
                                                Coord(0, info.window.top), C.byref(count)))
        return result[:count.value]


def wait_text(process, text):
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        content = screen()
        if text in content:
            return
        if process.poll() is not None:
            break
        time.sleep(0.03)
    raise AssertionError(f"Missing {text!r}; exit={process.poll()}; screen={content!r}")


def key(handle, character, virtual, control=0):
    for down in (True, False):
        record = InputRecord(1, Event(key=KeyEvent(down, 1, virtual, 0, character, control)))
        written = W.DWORD()
        check(kernel.WriteConsoleInputW(handle, C.byref(record), 1, C.byref(written)))


def exercise(binary, directory):
    env = {k: v for k, v in os.environ.items()
           if not k.startswith("BB_") and k not in ("BITBUCKET_REPO_FULL_NAME", "GIT_DIR", "GIT_WORK_TREE")}
    env["BB_CONFIG_DIR"] = str(directory / "config")
    with FileIO("CONIN$", "r+") as input_file, FileIO("CONOUT$", "r+") as output_file:
        input_handle = msvcrt.get_osfhandle(input_file.fileno())
        output_handle = msvcrt.get_osfhandle(output_file.fileno())
        original_mode = mode(input_handle)
        sentinel = "bb terminal restoration sentinel"
        count = W.DWORD()
        info = BufferInfo()
        check(kernel.GetConsoleScreenBufferInfo(output_handle, C.byref(info)))
        # Leave the original cursor row free for the Ctrl+C diagnostic after exit.
        check(kernel.WriteConsoleOutputCharacterW(output_handle, sentinel, len(sentinel),
                                                 Coord(0, info.window.bottom - 1), C.byref(count)))
        for interrupt in (False, True):
            with subprocess.Popen([binary, "tui", "--offline", "--workspace", "acme"],
                                  stdin=input_file, stdout=output_file, stderr=output_file,
                                  cwd=directory, env=env) as process:
                try:
                    wait_text(process, "Pipelines")
                    assert mode(input_handle) & 7 == 0, "Input did not enter raw mode"
                    key(input_handle, "?", 0xBF)
                    wait_text(process, "Keyboard help")
                    key(input_handle, "\x1b", 0x1B)
                    if interrupt:
                        key(input_handle, "\x03", 0x43, 0x0008)  # LEFT_CTRL_PRESSED
                    else:
                        key(input_handle, "q", 0x51)
                    assert process.wait(timeout=5) == (2 if interrupt else 0)
                    assert mode(input_handle) == original_mode, "Input mode was not restored"
                    assert sentinel in screen(), "Alternate screen was not restored"
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait(timeout=3)


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[2] == "--console":
        report = Path(sys.argv[3])
        try:
            exercise(sys.argv[1], report.parent)
            report.write_text("Windows terminal checks passed: help, quit, Ctrl+C, restoration\n", encoding="utf-8")
        except Exception:
            report.write_text(traceback.format_exc(), encoding="utf-8")
            sys.exit(1)
    else:
        with tempfile.TemporaryDirectory(prefix="bb-native-console-") as temporary:
            report = Path(temporary) / "result.txt"
            result = subprocess.run([sys.executable, __file__, str(Path(sys.argv[1]).resolve()),
                                     "--console", str(report)], timeout=35,
                                    creationflags=subprocess.CREATE_NEW_CONSOLE)
            sys.stdout.buffer.write(report.read_bytes() if report.exists() else b"Console test produced no report\n")
            sys.exit(result.returncode)
