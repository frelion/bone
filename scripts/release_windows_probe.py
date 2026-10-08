"""Temporary Unicode clipboard diagnosis: read only text this probe just wrote."""

import ctypes
from ctypes import wintypes
import hashlib
import json
import os
import shutil
import subprocess
import uuid


def own_clipboard_text(prefix):
    user = ctypes.WinDLL("user32", use_last_error=True)
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    user.OpenClipboard.argtypes, user.OpenClipboard.restype = [wintypes.HWND], wintypes.BOOL
    user.CloseClipboard.argtypes, user.CloseClipboard.restype = [], wintypes.BOOL
    user.GetClipboardData.argtypes, user.GetClipboardData.restype = [wintypes.UINT], wintypes.HANDLE
    kernel.GlobalLock.argtypes, kernel.GlobalLock.restype = [wintypes.HGLOBAL], ctypes.c_void_p
    kernel.GlobalUnlock.argtypes, kernel.GlobalUnlock.restype = [wintypes.HGLOBAL], wintypes.BOOL
    if not user.OpenClipboard(None):
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        handle = user.GetClipboardData(13)  # CF_UNICODETEXT
        if not handle:
            raise ctypes.WinError(ctypes.get_last_error())
        pointer = kernel.GlobalLock(handle)
        if not pointer:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            text = ctypes.wstring_at(pointer)
        finally:
            kernel.GlobalUnlock(handle)
    finally:
        user.CloseClipboard()
    # Permit a single visible BOM solely to identify our own test data. Python
    # prefix comparison is ordinal; an unexpected clipboard value is not logged.
    if not text.removeprefix("\ufeff").startswith(prefix):
        raise SystemExit("Clipboard did not contain this probe's exact nonce; refusing to log contents")
    return text


def main():
    if os.name != "nt":
        raise SystemExit("This diagnostic requires a native Windows runner")
    clip = shutil.which("clip.exe")
    if not clip:
        raise SystemExit("Native clip.exe is required")
    for with_bom in (True, False):
        nonce = uuid.uuid4().hex
        prefix = f"BONE clipboard probe {nonce}"
        expected = prefix + "\n中文第一行\n第二行 🦴🙂\n"
        payload = (b"\xff\xfe" if with_bom else b"") + expected.encode("utf-16-le")
        # Read no existing clipboard contents: write our unique nonce first.
        subprocess.run([clip], input=payload, check=True, timeout=5)
        actual = own_clipboard_text(prefix)
        print(json.dumps({"architecture": os.environ.get("PROCESSOR_ARCHITECTURE"),
                          "withBom": with_bom, "nonce": nonce, "writer": clip,
                          "characters": len(actual), "leadingBom": actual.startswith("\ufeff"),
                          "lfCount": actual.count("\n"), "crCount": actual.count("\r"),
                          "exactMatch": actual == expected,
                          "windowsLineEndingMatch": actual == expected.replace("\n", "\r\n"),
                          "textRepr": repr(actual),
                          "codepoints": [f"U+{ord(char):04X}" for char in actual],
                          "unicodeSha256": hashlib.sha256(actual.encode()).hexdigest()}), flush=True)


if __name__ == "__main__":
    main()
