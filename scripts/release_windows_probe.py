"""Temporary native clipboard diagnosis: read only the text this probe just wrote."""

import hashlib
import json
import os
import shutil
import subprocess
import uuid


def main():
    if os.name != "nt":
        raise SystemExit("This diagnostic requires a native Windows runner")
    clip, pwsh = shutil.which("clip.exe"), shutil.which("pwsh.exe")
    if not clip or not pwsh:
        raise SystemExit("Native clip.exe and PowerShell 7 are required for this diagnostic")
    nonce = uuid.uuid4().hex
    prefix = f"BONE clipboard probe {nonce}"
    expected = prefix + "\n中文第一行\n第二行 🦴🙂\n"
    payload = b"\xff\xfe" + expected.encode("utf-16-le")
    # No clipboard read occurs before this successful write of our own nonce.
    subprocess.run([clip], input=payload, check=True, timeout=5)
    script = r"""
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$value = Get-Clipboard -Raw
if (!$value.StartsWith($env:BONE_CLIPBOARD_PREFIX)) { exit 72 }
[Console]::Write($value)
"""
    result = subprocess.run(
        [pwsh, "-NoProfile", "-NonInteractive", "-Command", script],
        env=dict(os.environ, BONE_CLIPBOARD_PREFIX=prefix), stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True, timeout=10,
    )
    actual = result.stdout.decode("utf-8")
    if actual != expected:
        # Do not print clipboard contents. Only our expected nonce and hashes
        # are logged, including when the OS changes line endings.
        raise SystemExit(json.dumps({"error": "clipboard text did not round trip exactly",
                                     "nonce": nonce, "expectedCharacters": len(expected),
                                     "actualCharacters": len(actual),
                                     "expectedSha256": hashlib.sha256(expected.encode()).hexdigest(),
                                     "actualSha256": hashlib.sha256(result.stdout).hexdigest()}))
    print(json.dumps({"result": "PASS", "architecture": os.environ.get("PROCESSOR_ARCHITECTURE"),
                      "nonce": nonce, "characters": len(expected), "lineFeeds": expected.count("\n"),
                      "unicodeSha256": hashlib.sha256(result.stdout).hexdigest(),
                      "writer": clip, "reader": pwsh, "encoding": "UTF-16LE with BOM -> UTF-8"}))


if __name__ == "__main__":
    main()
