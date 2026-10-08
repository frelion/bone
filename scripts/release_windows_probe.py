"""Temporary Windows-only cwd diagnosis. No Cargo, model or release operations."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import uuid


def main():
    if os.name != "nt":
        raise SystemExit("This diagnostic requires a native Windows runner")
    deadline = time.monotonic() + 30
    cmd = os.environ.get("COMSPEC", r"C:\Windows\System32\cmd.exe")
    llvm = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "LLVM/bin/llvm-readobj.exe"
    print(json.dumps({"architecture": os.environ.get("PROCESSOR_ARCHITECTURE"),
                      "llvmOnPath": shutil.which("llvm-readobj"),
                      "llvmInstalledPath": str(llvm), "llvmInstalled": llvm.is_file()}), flush=True)
    with tempfile.TemporaryDirectory(prefix="bone cwd probe ") as temporary:
        workspace = Path(temporary)
        (workspace / "cwd-evidence.txt").write_text("workspace evidence")
        script = r"""
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$info = [ordered]@{ location = (Get-Location).Path; nativeCwd = [Environment]::CurrentDirectory; scriptRoot = $PSScriptRoot; psVersion = $PSVersionTable.PSVersion.ToString(); psHome = $PSHOME; executable = (Get-Process -Id $PID).Path; evidencePresent = (Test-Path -LiteralPath 'cwd-evidence.txt'); marker = $env:BONE_PROBE_MARKER }
[Console]::WriteLine(($info | ConvertTo-Json -Compress))
Set-Content -LiteralPath $env:BONE_PROBE_MARKER -Value 'temporary cwd probe'
[Console]::WriteLine(('markerPath=' + (Get-Item -LiteralPath $env:BONE_PROBE_MARKER).FullName))
""".strip()
        parent = script + r"""

$env:BONE_PROBE_MARKER += '-child'
$child = Start-Process powershell.exe -ArgumentList '-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File','probe-child.ps1' -NoNewWindow -Wait -PassThru
[Console]::WriteLine(('childExit=' + $child.ExitCode))
"""
        (workspace / "probe.ps1").write_text(parent, encoding="utf-8")
        (workspace / "probe-child.ps1").write_text(script, encoding="utf-8")
        for kind, cwd in (("normal", str(workspace)), ("verbatim", "\\\\?\\" + str(workspace))):
            commands = {
                "cmd": 'echo nativeCwd=%CD% & echo temporary cwd probe>"%BONE_PROBE_MARKER%" & for %F in ("%BONE_PROBE_MARKER%") do @echo markerPath=%~fF',
                "ps-command": 'powershell.exe -NoProfile -NonInteractive -Command "' + script.replace("\n", "; ") + '"',
                "ps-file-relative": 'powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File probe.ps1',
                "ps-file-absolute": f'powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "{workspace / "probe.ps1"}"',
            }
            for mode, shell in commands.items():
                remaining = deadline - time.monotonic() - 3
                if remaining <= 0:
                    print(json.dumps({"cwdKind": kind, "mode": mode, "error": "30 second total deadline reached"}))
                    return
                marker = f"bone-cwd-{uuid.uuid4().hex}.txt"
                env = dict(os.environ, BONE_PROBE_MARKER=marker)
                # A command-line string preserves cmd's raw syntax, matching the
                # Rust shell launcher rather than Python's list quoting rules.
                line = f'"{cmd}" /D /S /C {shell}'
                try:
                    process = subprocess.Popen(
                        line, executable=cmd, cwd=cwd, env=env,
                        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    )
                    try:
                        stdout, stderr = process.communicate(timeout=remaining)
                    except subprocess.TimeoutExpired:
                        # cmd's descendant can inherit the output pipes. Kill
                        # the whole short-lived diagnostic tree before draining.
                        try:
                            subprocess.run(["taskkill.exe", "/PID", str(process.pid), "/T", "/F"],
                                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=1)
                        except (OSError, subprocess.TimeoutExpired):
                            pass
                        process.kill()
                        try:
                            process.communicate(timeout=1)
                        except subprocess.TimeoutExpired:
                            process.stdout.close()
                            process.stderr.close()
                        print(json.dumps({"cwdKind": kind, "mode": mode, "error": "30 second total deadline reached"}), flush=True)
                        return
                    result = {"cwdKind": kind, "cwdPassed": cwd, "mode": mode, "exit": process.returncode,
                              "stdout": stdout.decode("utf-8", errors="replace"),
                              "stderr": stderr.decode("utf-8", errors="replace"),
                              "workspaceMarker": (workspace / marker).exists(),
                              "workspaceChildMarker": (workspace / (marker + "-child")).exists()}
                except (OSError, subprocess.TimeoutExpired) as error:
                    result = {"cwdKind": kind, "cwdPassed": cwd, "mode": mode, "error": str(error)}
                print(json.dumps(result, ensure_ascii=True), flush=True)


if __name__ == "__main__":
    main()
