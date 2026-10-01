#!/usr/bin/env python3
"""Rebuild the trial1 native request replay locally; never contact a provider."""
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile


def checked(path, expected):
    path = Path(path)
    actual = hashlib.sha256(path.read_bytes()).hexdigest()
    if actual != expected:
        raise SystemExit(f"Evidence hash differs: {path}")
    return path


def main():
    directory = Path(__file__).resolve().parent
    report = json.loads((directory / "trial1-context-diagnosis.json").read_text())
    source = checked(report["replay_source_path"], report["replay_source_sha256"])
    history = checked(report["history_path"], report["history_sha256"])
    checked(history.parent / "phase1-6-state.json", report["state_sha256"])
    for path, digest in report["frozen_native_source_hashes"].items():
        checked(path, digest)
    dependencies = directory.parents[2] / "target/debug/deps"
    with tempfile.TemporaryDirectory(prefix="bone-frozen-trial1-replay-") as temporary:
        executable = Path(temporary) / "probe"
        command = ["rustc", "--edition=2024", "-Awarnings", "-L",
                   f"dependency={dependencies}", str(source), "-o", str(executable)]
        for name in ("anyhow", "rig_core", "serde", "serde_json", "uuid", "sha2", "tokio", "libc"):
            libraries = list(dependencies.glob(f"lib{name}-*.rlib"))
            if not libraries:
                raise SystemExit(f"Missing cached dependency {name}: {dependencies}")
            library = max(libraries, key=lambda path: path.stat().st_mtime_ns)
            command.extend(("--extern", f"{name}={library}"))
        subprocess.run(command, check=True)
        result = subprocess.run([str(executable)], check=True, capture_output=True, text=True)
        rows = json.loads(result.stdout)
        projections = [row for row in rows if row["projected"]]
        expected = report["projection_requests"]
        assert len(projections) == len(expected) == 3
        for actual, previous in zip(projections, expected):
            for key in ("event_index", "overhead_chars", "history_budget", "raw_request_chars",
                        "system_message_chars", "work_request_chars", "repaired_request_chars",
                        "old_bone_preamble_present", "new_bone_preamble_present", "audit_unmodified"):
                assert actual[key] == previous[key], (key, actual[key], previous[key])
            assert actual["tools"] == previous["tools"]
        print(json.dumps({"verified_frozen_source_commit": report["frozen_source_commit"],
                          "replayed_requests": len(rows), "projection_requests": projections}, indent=2))


if __name__ == "__main__":
    main()
