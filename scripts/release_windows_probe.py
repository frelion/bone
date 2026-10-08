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
                      "llvmInstalledPath": str(llvm), "llvmInstalled": llvm.is_file(),
                      "powershell": shutil.which("powershell.exe"), "pwsh": shutil.which("pwsh.exe")}), flush=True)
    with tempfile.TemporaryDirectory(prefix="bone cwd probe ") as temporary:
        workspace = Path(temporary)
        (workspace / "cwd-evidence.txt").write_text("workspace evidence")
        script = r"""
[Console]::WriteLine(('PSHOME=' + $PSHOME + ';version=' + $PSVersionTable.PSVersion.ToString()))
[Console]::WriteLine(('nativeCwd=' + [Environment]::CurrentDirectory + ';scriptRoot=' + $PSScriptRoot))
[Console]::WriteLine(('location=' + (Get-Location).Path))
[IO.File]::WriteAllText($env:BONE_PROBE_MARKER, 'temporary cwd probe')
[Console]::WriteLine(('markerPath=' + [IO.Path]::GetFullPath($env:BONE_PROBE_MARKER)))
""".strip()
        (workspace / "probe.ps1").write_text(script, encoding="utf-8")
        for kind, cwd in (("normal", str(workspace)), ("verbatim", "\\\\?\\" + str(workspace))):
            commands = {
                "cmd": 'echo nativeCwd=%CD% & echo temporary cwd probe>"%BONE_PROBE_MARKER%" & for %F in ("%BONE_PROBE_MARKER%") do @echo markerPath=%~fF',
            }
            for shell_name in ("powershell.exe", "pwsh.exe"):
                if shutil.which(shell_name):
                    commands[shell_name + "-command"] = shell_name + ' -NoProfile -NonInteractive -Command "' + script.replace("\n", "; ") + '"'
            for shell_name in ("powershell.exe", "pwsh.exe"):
                if shutil.which(shell_name):
                    commands[shell_name + "-file"] = shell_name + f' -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "{workspace / "probe.ps1"}"'
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
                    timed_out = False
                    try:
                        stdout, stderr = process.communicate(timeout=min(5, remaining))
                    except subprocess.TimeoutExpired:
                        timed_out = True
                        # cmd's descendant can inherit the output pipes. Kill
                        # the whole short-lived diagnostic tree before draining.
                        try:
                            subprocess.run(["taskkill.exe", "/PID", str(process.pid), "/T", "/F"],
                                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=1)
                        except (OSError, subprocess.TimeoutExpired):
                            pass
                        process.kill()
                        try:
                            stdout, stderr = process.communicate(timeout=1)
                        except subprocess.TimeoutExpired:
                            stdout, stderr = b"", b""
                    result = {"cwdKind": kind, "cwdPassed": cwd, "mode": mode, "exit": process.returncode,
                              "timedOut": timed_out,
                              "stdout": stdout.decode("utf-8", errors="replace"),
                              "stderr": stderr.decode("utf-8", errors="replace"),
                              "workspaceMarker": (workspace / marker).exists()}
                except (OSError, subprocess.TimeoutExpired) as error:
                    result = {"cwdKind": kind, "cwdPassed": cwd, "mode": mode, "error": str(error)}
                print(json.dumps(result, ensure_ascii=True), flush=True)


if __name__ == "__main__":
    main()
