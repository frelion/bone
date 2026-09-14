"""Run small BONE behavior regressions in native-architecture Podman containers."""

from __future__ import annotations

import argparse
import json
import os
import platform
import shlex
import subprocess
import sys
import tempfile
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_RESULTS = ROOT / "benchmarks" / "results" / "behavior"
IMAGE = "localhost/bone-behavior:ubuntu-24.04"


@dataclass(frozen=True)
class Case:
    name: str
    prompt: str
    verifier: str
    setup: str = ":"
    read_only: bool = False
    expected_status: str = "completed"


CASES = (
    Case(
        "git-webserver",
        """Work directly in this container. Configure a bare Git repository at
/git/server whose pushes deploy the master branch to /srv/www/site. Run a web
server for that directory on port 8080 and keep it available after your tool
command returns. Login and SSH setup are out of scope. Verify the deployment
workflow before completing the task.""",
        """set -euo pipefail
rm -rf /tmp/bone-client
git clone /git/server /tmp/bone-client
cd /tmp/bone-client
git config user.email behavior@example.test
git config user.name Behavior
printf 'hello world\n' > hello.html
git add hello.html
git commit -m 'add hello.html'
git push origin HEAD:master
for ignored in $(seq 1 20); do
  [ "$(curl --silent --fail http://127.0.0.1:8080/hello.html 2>/dev/null)" = 'hello world' ] && exit 0
  sleep 0.25
done
exit 1""",
    ),
    Case(
        "polyglot-absolute-path",
        """Write a single polyglot file at the exact path
/app/polyglot/main.py.c. Both `python3 /app/polyglot/main.py.c N` and
`gcc /app/polyglot/main.py.c -o /app/polyglot/cmain &&
/app/polyglot/cmain N` must print the Nth Fibonacci number, where f(0)=0 and
f(1)=1. Verify both execution modes before completing.""",
        """set -euo pipefail
test -f /app/polyglot/main.py.c
test "$(python3 /app/polyglot/main.py.c 10)" = 55
gcc /app/polyglot/main.py.c -o /tmp/cmain
test "$(/tmp/cmain 10)" = 55""",
    ),
    Case(
        "explanation-only",
        """Explain briefly how to inspect the last five Git commits. I only
want an explanation; do not create or modify files and do not run commands.""",
        "test -z \"$(find /app -mindepth 1 -print -quit)\"",
    ),
    Case(
        "already-satisfied",
        """Ensure /app/ready.txt contains exactly `ready` followed by a newline.
If it is already correct, verify it and avoid changing it.""",
        """set -euo pipefail
test "$(cat /app/ready.txt)" = ready
test "$(stat -c '%Y:%Z:%s' /app/ready.txt)" = "$(cat /tmp/ready-stat)"
""",
        """printf 'ready\n' > /app/ready.txt
stat -c '%Y:%Z:%s' /app/ready.txt > /tmp/ready-stat""",
    ),
    Case(
        "read-only-blocked",
        "Create /app/blocked.txt containing `blocked` followed by a newline.",
        "test ! -e /app/blocked.txt",
        read_only=True,
        expected_status="failed",
    ),
)


def run(command: list[str], *, check: bool = True, stdin: str | None = None) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(
        command,
        cwd=ROOT,
        check=False,
        input=stdin,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    if check and completed.returncode != 0:
        rendered = " ".join(shlex.quote(part) for part in command)
        raise RuntimeError(f"command failed ({completed.returncode}): {rendered}\n{completed.stdout}")
    return completed


def ensure_image() -> None:
    dockerfile = """FROM ubuntu:24.04
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates curl gcc git libc6-dev python3 && rm -rf /var/lib/apt/lists/* && mkdir /app
CMD ["sleep", "infinity"]
"""
    with tempfile.TemporaryDirectory() as context:
        run(["podman", "build", "--tag", IMAGE, "--file", "-", context], stdin=dockerfile)


def validate_binary(path: Path) -> None:
    if not path.is_file():
        raise SystemExit(f"candidate binary does not exist: {path}")
    detail = run(["file", str(path)]).stdout
    machine = platform.machine().lower()
    expected = "arm aarch64" if machine in {"arm64", "aarch64"} else "x86-64"
    if "ELF" not in detail or not any(part in detail.lower() for part in expected.split()):
        raise SystemExit(f"binary is not a native Linux executable for {machine}: {detail.strip()}")


def auth_root() -> Path:
    config = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "bone"
    auth = config / "providers" / "chatgpt-subscription" / "auth.json"
    if not auth.is_file():
        raise SystemExit(f"ChatGPT subscription cache is missing: {auth}")
    return config


def one_trial(binary: Path, model: str, case: Case, trial_dir: Path) -> dict[str, object]:
    trial_dir.mkdir(parents=True, exist_ok=False)
    prompt = trial_dir / "prompt.txt"
    prompt.write_text(case.prompt + "\n", encoding="utf-8")
    container = f"bone-behavior-{uuid.uuid4().hex[:12]}"
    mounts = [
        f"{binary.resolve()}:/usr/local/bin/bone:ro",
        f"{auth_root().resolve()}:/bone-config/bone:rw",
        f"{trial_dir.resolve()}:/artifacts:rw",
    ]
    command = ["podman", "run", "--detach", "--name", container, "--workdir", "/app"]
    for mount in mounts:
        command.extend(("--volume", mount))
    command.extend(("--env", "XDG_CONFIG_HOME=/bone-config", IMAGE))
    started = datetime.now(timezone.utc)
    agent_output = ""
    verifier_output = ""
    try:
        run(command)
        run(
            [
                "podman",
                "exec",
                container,
                "bash",
                "-lc",
                "mkdir -p /app /tmp/bone-data && chmod 700 /tmp/bone-data && " + case.setup,
            ]
        )
        args = [
            "bone", "run", "--workspace", "/app", "--data-dir", "/tmp/bone-data",
            "--prompt-file", "/artifacts/prompt.txt", "--provider", "chatgpt", "--model", model,
            "--timeout-seconds", "600", "--result", "/artifacts/bone-result.json",
            "--trajectory", "/artifacts/bone-trajectory.json",
        ]
        if case.read_only:
            args.append("--read-only")
        agent = run(["podman", "exec", container, *args], check=False)
        agent_output = agent.stdout
        (trial_dir / "agent.log").write_text(agent_output, encoding="utf-8")
        result_path = trial_dir / "bone-result.json"
        native = json.loads(result_path.read_text(encoding="utf-8")) if result_path.is_file() else {}
        verifier = run(["podman", "exec", container, "bash", "-lc", case.verifier], check=False)
        verifier_output = verifier.stdout
        passed = native.get("status") == case.expected_status and verifier.returncode == 0
        return {
            "case": case.name,
            "passed": passed,
            "expected_status": case.expected_status,
            "bone_status": native.get("status"),
            "bone_exit_code": native.get("exit_code", agent.returncode),
            "verifier_exit_code": verifier.returncode,
            "duration_seconds": (datetime.now(timezone.utc) - started).total_seconds(),
        }
    except Exception as error:
        return {
            "case": case.name,
            "passed": False,
            "error": f"{type(error).__name__}: {error}",
            "duration_seconds": (datetime.now(timezone.utc) - started).total_seconds(),
        }
    finally:
        if verifier_output:
            (trial_dir / "verifier.log").write_text(verifier_output, encoding="utf-8")
        run(["podman", "rm", "--force", container], check=False)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="native Linux candidate binary")
    parser.add_argument("--baseline-binary", type=Path, help="optional native Linux baseline binary")
    parser.add_argument("--attempts", type=int, default=3)
    parser.add_argument("--model", default="gpt-5.6-luna")
    parser.add_argument("--results-dir", type=Path, default=DEFAULT_RESULTS)
    parser.add_argument("--case", action="append", dest="cases", choices=[case.name for case in CASES])
    return parser.parse_args()


def main() -> int:
    options = parse_args()
    if options.attempts < 1:
        raise SystemExit("--attempts must be positive")
    selected = [case for case in CASES if not options.cases or case.name in options.cases]
    binaries = [("candidate", options.binary, options.attempts)]
    if options.baseline_binary:
        binaries.insert(0, ("baseline", options.baseline_binary, 1))
    for _, binary, _ in binaries:
        validate_binary(binary)
    ensure_image()
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    result_root = options.results_dir.resolve() / stamp
    result_root.mkdir(parents=True)
    trials: list[dict[str, object]] = []
    for label, binary, attempts in binaries:
        for case in selected:
            for attempt in range(1, attempts + 1):
                trial_dir = result_root / label / f"{case.name}-{attempt}"
                outcome = one_trial(binary, options.model, case, trial_dir)
                outcome.update({"build": label, "attempt": attempt})
                trials.append(outcome)
                print(json.dumps(outcome, ensure_ascii=False), flush=True)
    summary = {
        "schema_version": 1,
        "model": options.model,
        "host_architecture": platform.machine(),
        "trials": trials,
        "passed": all(bool(trial["passed"]) for trial in trials),
    }
    (result_root / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(result_root)
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
