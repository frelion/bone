#!/usr/bin/env python3
"""Build archive checks for the native GitHub release matrix; no credentials needed."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile


TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
)


def command(*args, env=None):
    return subprocess.check_output(args, text=True, env=env, timeout=60).strip()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def version(tag):
    package_version = tomllib.loads(Path("Cargo.toml").read_text())['package']['version']
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", tag) or tag != f"v{package_version}":
        raise ValueError(f"Tag {tag!r} does not match package version {package_version!r}")
    return package_version


def validate(args):
    preflight = not args.tag
    tag = args.tag or f"v{tomllib.loads(Path('Cargo.toml').read_text())['package']['version']}"
    package_version = version(tag)
    commit = command("git", "rev-parse", "HEAD")
    if not preflight and command("git", "rev-parse", f"refs/tags/{tag}^{{commit}}") != commit:
        raise ValueError("The checked out commit does not match the requested tag")
    output = f"tag={tag}\nversion={package_version}\ncommit={commit}\npreflight={str(preflight).lower()}\n"
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as stream:
            stream.write(output)
    print(output, end="")


def smoke(binary, package_version):
    with tempfile.TemporaryDirectory(prefix="bone-smoke-") as data:
        env = {key: value for key, value in os.environ.items() if not key.startswith("BONE_")}
        env["BONE_DATA_DIR"] = data
        executable = str(binary.resolve())
        require(command(executable, "--version", env=env) == f"bone {package_version}", "Binary version mismatch")
        require("Usage:" in command(executable, "--help", env=env), "Missing command help")
        require(json.loads(command(executable, "tools", "--json", env=env)), "Empty tool catalog")
        require(json.loads(command(executable, "providers", "--json", env=env)), "Empty provider catalog")
        require(tomllib.loads(command(executable, "config", env=env)), "Invalid default config")
        require(json.loads(command(executable, "sessions", "--json", env=env)) == [], "Smoke data was not isolated")
    print(f"PASS: {binary}: version, help, tools, providers, config, empty sessions")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def windows_imports(binary):
    tool = shutil.which("llvm-readobj")
    if not tool:
        installed = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "LLVM/bin/llvm-readobj.exe"
        require(installed.is_file(), "Native Windows runner is missing llvm-readobj for the PE dependency gate")
        tool = str(installed)
    imports = sorted(set(re.findall(r"Name:\s+(\S+\.dll)", command(tool, "--coff-imports", str(binary)), re.I)))
    require(imports, "Could not inspect PE imports")
    redistributable = [dll for dll in imports if re.match(r"(?:vcruntime|msvcp|msvcr\d|concrt|mfc\d|vcomp)", dll, re.I)]
    require(not redistributable, f"Windows release depends on VC redistributable DLLs: {redistributable}")
    print(f"PASS: PE dependency check: {', '.join(imports)}")
    return imports


def installation(target):
    if "windows" in target:
        return """BONE for Windows 11 / Windows Server 2025 or newer.
Extract this ZIP. In PowerShell run .\\bone.exe --version, then .\\bone.exe.
For a permanent install, copy bone.exe into a directory in your user PATH.
Use Windows Terminal for the interactive TUI. /new begins a conversation.
"""
    baseline = "macOS 15 or newer" if "apple" in target else "Linux (statically linked musl; glibc is not required)"
    return f"""BONE for {baseline}.
Extract this archive. Run ./bone --version, then ./bone.
For a permanent install:
  mkdir -p ~/.local/bin
  install -m 755 bone ~/.local/bin/bone
Add ~/.local/bin to PATH if it is not already present.
Use an interactive terminal for the TUI. /new begins a conversation.
"""


def package(args):
    package_version = version(args.tag)
    require(command("git", "rev-parse", "HEAD") == args.commit, "Build commit mismatch")
    require(args.target in TARGETS, "Unexpected release target")
    rustc = command("rustc", "--version", "--verbose")
    host = args.target.replace("-musl", "-gnu")
    require(f"host: {host}" in rustc.splitlines(), "Release builds must use the native CPU and OS")
    name = f"bone-{args.tag}-{args.target}"
    binary_name = "bone.exe" if "windows" in args.target else "bone"
    binary = Path("target") / args.target / "release" / binary_name
    imports = windows_imports(binary) if "windows" in args.target else None
    if "linux-musl" in args.target:
        require("INTERP" not in command("readelf", "--program-headers", str(binary)), "Linux release must be static")
    smoke(binary, package_version)
    dist = Path("dist")
    dist.mkdir(exist_ok=True)
    suffix = ".zip" if "windows" in args.target else ".tar.gz"
    archive = dist / (name + suffix)
    with tempfile.TemporaryDirectory(prefix="bone-package-") as temporary:
        stage = Path(temporary) / name
        stage.mkdir()
        shutil.copy2(binary, stage / binary_name)
        for document in ("LICENSE", "README.md"):
            shutil.copy2(document, stage / document)
        (stage / "INSTALL.txt").write_text(installation(args.target), encoding="utf-8")
        if suffix == ".zip":
            with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
                for file in sorted(stage.iterdir()):
                    bundle.write(file, f"{name}/{file.name}")
        else:
            with tarfile.open(archive, "w:gz") as bundle:
                bundle.add(stage, arcname=name)
        extracted = Path(temporary) / "extracted"
        if suffix == ".zip":
            with zipfile.ZipFile(archive) as bundle:
                bundle.extractall(extracted)
        else:
            with tarfile.open(archive) as bundle:
                bundle.extractall(extracted, filter="data")
        smoke(extracted / name / binary_name, package_version)
    evidence = {
        "version": package_version,
        "commit": args.commit,
        "target": args.target,
        "archive": archive.name,
        "sha256": digest(archive),
        "rustc": rustc,
        "rustflags": os.environ.get("RUSTFLAGS", ""),
        "windows_imports": imports,
        "features": "default",
        "test_command": f"cargo test --locked --target {args.target}",
        "smoke": ["version", "help", "tools", "providers", "config", "empty sessions"],
        "extracted_smoke_passed": True,
    }
    (dist / f"build-{args.target}.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print(f"Packaged {archive} ({evidence['sha256']})")


def aggregate(args):
    package_version = version(args.tag)
    dist = Path("dist")
    builds = [json.loads(path.read_text()) for path in sorted(dist.glob("build-*.json"))]
    require(sorted(build['target'] for build in builds) == sorted(TARGETS), "Missing or duplicate native build")
    for build in builds:
        suffix = ".zip" if "windows" in build['target'] else ".tar.gz"
        require(build['archive'] == f"bone-{args.tag}-{build['target']}{suffix}", "Unexpected archive name")
        require(build['commit'] == args.commit and build['version'] == package_version, "Build identity mismatch")
        require(build['extracted_smoke_passed'] and build['features'] == "default", "Extracted smoke failed")
        if "windows" in build['target']:
            require(build['windows_imports'], "Missing native PE dependency evidence")
            require("+crt-static" in build['rustflags'], "Windows build did not enable a static CRT")
        require(build['sha256'] == digest(dist / build['archive']), "Archive checksum mismatch")
    (dist / "release-builds.json").write_text(json.dumps(builds, indent=2) + "\n")
    for path in dist.glob("build-*.json"):
        path.unlink()
    files = sorted(path for path in dist.iterdir() if path.name != "SHA256SUMS")
    (dist / "SHA256SUMS").write_text("".join(f"{digest(path)}  {path.name}\n" for path in files))
    print(f"Verified {len(builds)} native builds; generated SHA256SUMS")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    for action in ("validate", "package", "aggregate"):
        options = commands.add_parser(action)
        options.add_argument("--tag", default="", required=action != "validate")
        if action != "validate":
            options.add_argument("--commit", required=True)
        if action == "package":
            options.add_argument("--target", required=True, choices=TARGETS)
    args = parser.parse_args()
    {"validate": validate, "package": package, "aggregate": aggregate}[args.action](args)


if __name__ == "__main__":
    main()
