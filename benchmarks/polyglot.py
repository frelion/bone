"""Run a small, pinned Aider Polyglot benchmark subset with BONE."""

from __future__ import annotations

import argparse
import hashlib
import json
import shlex
import shutil
import subprocess
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path

from benchmarks.behavior import IMAGE, auth_root, ensure_image, validate_binary
from benchmarks.profile import build_profile


ROOT = Path(__file__).resolve().parents[1]
RESULTS = ROOT / "benchmarks" / "results" / "polyglot"
SOURCE_URL = "https://github.com/Aider-AI/polyglot-benchmark.git"
SOURCE_COMMIT = "7e0611e77b54e2dea774cdc0aa00cf9f7ed6144f"
DEFAULT_TASKS = ("proverb", "grade-school", "phone-number", "robot-name", "wordy")


def run(
    command: list[str], *, check: bool = True, timeout: int | None = None
) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(
        command,
        cwd=ROOT,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=timeout,
    )
    if check and completed.returncode != 0:
        rendered = " ".join(shlex.quote(part) for part in command)
        raise RuntimeError(f"command failed ({completed.returncode}): {rendered}\n{completed.stdout}")
    return completed


def source_checkout(cache_root: Path) -> Path:
    checkout = cache_root / f"polyglot-benchmark-{SOURCE_COMMIT[:12]}"
    if checkout.is_dir():
        actual = run(["git", "-C", str(checkout), "rev-parse", "HEAD"]).stdout.strip()
        if actual != SOURCE_COMMIT:
            raise RuntimeError(f"cached Polyglot checkout has unexpected commit: {actual}")
        return checkout

    cache_root.mkdir(parents=True, exist_ok=True)
    partial = cache_root / f".polyglot-{uuid.uuid4().hex}.partial"
    try:
        run(["git", "clone", "--filter=blob:none", "--no-checkout", SOURCE_URL, str(partial)])
        run(["git", "-C", str(partial), "fetch", "--depth", "1", "origin", SOURCE_COMMIT])
        run(["git", "-C", str(partial), "checkout", "--detach", SOURCE_COMMIT])
        partial.rename(checkout)
    finally:
        if partial.exists():
            shutil.rmtree(partial)
    return checkout


def file_digest(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def prepare_task(source: Path, task: str, trial_dir: Path) -> tuple[Path, Path, str, dict[str, str]]:
    original = source / "python" / "exercises" / "practice" / task
    if not original.is_dir():
        raise RuntimeError(f"Polyglot task does not exist: {task}")

    workspace = trial_dir / "workspace"
    verifier = trial_dir / "verifier"
    shutil.copytree(original, workspace)
    verifier.mkdir()

    config = json.loads((workspace / ".meta" / "config.json").read_text(encoding="utf-8"))
    files = config.get("files", {})
    test_files = tuple(files.get("test", ()))
    example_files = tuple(files.get("example", ()))
    solution_files = tuple(files.get("solution", ()))
    if not test_files or not solution_files:
        raise RuntimeError(f"Polyglot task has an incomplete file manifest: {task}")

    test_digests: dict[str, str] = {}
    for relative in test_files:
        test_path = workspace / relative
        destination = verifier / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(test_path, destination)
        test_digests[relative] = file_digest(destination)
    for relative in example_files:
        example = workspace / relative
        if example.exists():
            example.unlink()

    instructions = ""
    for relative in (".docs/introduction.md", ".docs/instructions.md", ".docs/instructions.append.md"):
        document = workspace / relative
        if document.is_file():
            instructions += document.read_text(encoding="utf-8") + "\n"
    editable = " ".join(solution_files)
    prompt = (
        f"{instructions}\n####\n\n"
        f"Implement the exercise by modifying only these supplied solution files: {editable}.\n"
        "Do not change existing public function or class names. Use only the Python standard library.\n"
        "Run `cd /tests && PYTHONPATH=/app python3 -m unittest discover -v -p '*_test.py'` to verify the solution "
        "before completing the task. The tests are read-only.\n"
    )
    return workspace, verifier, prompt, test_digests


def one_trial(
    binary: Path,
    model: str,
    source: Path,
    task: str,
    trial_dir: Path,
    timeout_seconds: int,
) -> dict[str, object]:
    trial_dir.mkdir(parents=True, exist_ok=False)
    workspace, verifier, prompt_text, test_digests = prepare_task(source, task, trial_dir)
    prompt = trial_dir / "prompt.txt"
    prompt.write_text(prompt_text, encoding="utf-8")
    container = f"bone-polyglot-{uuid.uuid4().hex[:12]}"
    command = ["podman", "run", "--detach", "--name", container, "--workdir", "/app"]
    for mount in (
        f"{binary.resolve()}:/usr/local/bin/bone:ro",
        f"{auth_root().resolve()}:/bone-config/bone:rw",
        f"{workspace.resolve()}:/app:rw",
        f"{verifier.resolve()}:/tests:ro",
        f"{trial_dir.resolve()}:/artifacts:rw",
    ):
        command.extend(("--volume", mount))
    command.extend(("--env", "XDG_CONFIG_HOME=/bone-config", IMAGE))

    started = datetime.now(timezone.utc)
    try:
        run(command)
        agent = run(
            [
                "podman", "exec", container, "bone", "run",
                "--workspace", "/app", "--data-dir", "/tmp/bone-data",
                "--prompt-file", "/artifacts/prompt.txt", "--provider", "chatgpt",
                "--model", model, "--timeout-seconds", str(timeout_seconds),
                "--result", "/artifacts/bone-result.json",
                "--trajectory", "/artifacts/bone-trajectory.json",
            ],
            check=False,
            timeout=timeout_seconds + 90,
        )
        (trial_dir / "agent.log").write_text(agent.stdout, encoding="utf-8")
        result_path = trial_dir / "bone-result.json"
        native = json.loads(result_path.read_text(encoding="utf-8")) if result_path.is_file() else {}
        trajectory_path = trial_dir / "bone-trajectory.json"
        if trajectory_path.is_file():
            trajectory = json.loads(trajectory_path.read_text(encoding="utf-8"))
            try:
                profile = build_profile(trajectory)
            except ValueError:
                profile = None
            if profile is not None:
                (trial_dir / "efficiency-profile.json").write_text(
                    json.dumps(profile, indent=2) + "\n", encoding="utf-8"
                )
        verifier_run = run(
            [
                "podman", "exec", container, "bash", "-lc",
                "cd /tests && PYTHONPATH=/app python3 -m unittest discover -v -p '*_test.py'",
            ],
            check=False,
            timeout=120,
        )
        (trial_dir / "verifier.log").write_text(verifier_run.stdout, encoding="utf-8")
        tests_unchanged = all(file_digest(verifier / name) == digest for name, digest in test_digests.items())
        return {
            "task": task,
            "passed": verifier_run.returncode == 0 and tests_unchanged,
            "agent_completed": native.get("status") == "completed",
            "bone_status": native.get("status"),
            "bone_exit_code": native.get("exit_code", agent.returncode),
            "verifier_exit_code": verifier_run.returncode,
            "tests_unchanged": tests_unchanged,
            "duration_seconds": round((datetime.now(timezone.utc) - started).total_seconds(), 3),
        }
    except subprocess.TimeoutExpired as error:
        return {
            "task": task,
            "passed": False,
            "error": f"timeout after {error.timeout} seconds",
            "duration_seconds": round((datetime.now(timezone.utc) - started).total_seconds(), 3),
        }
    except Exception as error:
        return {
            "task": task,
            "passed": False,
            "error": f"{type(error).__name__}: {error}",
            "duration_seconds": round((datetime.now(timezone.utc) - started).total_seconds(), 3),
        }
    finally:
        run(["podman", "rm", "--force", container], check=False)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="native Linux BONE binary")
    parser.add_argument("--model", default="gpt-5.6-luna")
    parser.add_argument("--task", action="append", choices=DEFAULT_TASKS, dest="tasks")
    parser.add_argument("--attempts", type=int, default=1)
    parser.add_argument("--timeout-seconds", type=int, default=300)
    parser.add_argument("--results-dir", type=Path, default=RESULTS)
    return parser.parse_args()


def main() -> int:
    options = parse_args()
    if options.attempts < 1 or options.timeout_seconds < 1:
        raise SystemExit("--attempts and --timeout-seconds must be positive")
    validate_binary(options.binary)
    ensure_image()
    source = source_checkout(ROOT / "benchmarks" / "results" / ".cache")
    tasks = options.tasks or list(DEFAULT_TASKS)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    result_root = options.results_dir.resolve() / stamp
    result_root.mkdir(parents=True)

    outcomes: list[dict[str, object]] = []
    for task in tasks:
        for attempt in range(1, options.attempts + 1):
            outcome = one_trial(
                options.binary, options.model, source, task,
                result_root / f"{task}-{attempt}", options.timeout_seconds,
            )
            outcome["attempt"] = attempt
            outcomes.append(outcome)
            print(json.dumps(outcome, ensure_ascii=False), flush=True)

    passed = sum(bool(outcome["passed"]) for outcome in outcomes)
    summary = {
        "schema_version": 1,
        "suite": "aider-polyglot-python-smoke",
        "source_url": SOURCE_URL,
        "source_commit": SOURCE_COMMIT,
        "model": options.model,
        "tasks": tasks,
        "attempts": options.attempts,
        "passed": passed,
        "total": len(outcomes),
        "pass_rate": passed / len(outcomes),
        "outcomes": outcomes,
    }
    (result_root / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(result_root)
    return 0 if passed == len(outcomes) else 1


if __name__ == "__main__":
    sys.exit(main())
