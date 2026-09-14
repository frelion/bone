"""Pinned Harbor suite runner and auditable result summarizer for BONE."""

from __future__ import annotations

import argparse
import json
import os
import shlex
import shutil
import subprocess
import sys
import urllib.error
import urllib.request
from collections import Counter, defaultdict
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
SUITES_DIR = Path(__file__).resolve().parent / "suites"
RESULTS_DIR = Path(__file__).resolve().parent / "results"
REQUIRED_FIELDS = {
    "schema_version",
    "name",
    "description",
    "harbor_version",
    "dataset",
    "expected_task_count",
    "bone_version",
    "agent",
    "model",
    "attempts",
    "concurrency",
    "timeout_multiplier",
    "tasks",
}


class SuiteError(ValueError):
    """A suite manifest or result is invalid."""


def suite_path(name_or_path: str | Path) -> Path:
    candidate = Path(name_or_path)
    if candidate.is_file():
        return candidate.resolve()
    path = SUITES_DIR / f"{candidate.name.removesuffix('.json')}.json"
    if not path.is_file():
        available = ", ".join(p.stem for p in sorted(SUITES_DIR.glob("*.json")))
        raise SuiteError(f"unknown suite {name_or_path!s}; available: {available}")
    return path


def load_suite(name_or_path: str | Path) -> dict[str, Any]:
    path = suite_path(name_or_path)
    try:
        suite = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SuiteError(f"cannot read {path}: {error}") from error
    if not isinstance(suite, dict):
        raise SuiteError(f"{path}: manifest must be a JSON object")
    missing = REQUIRED_FIELDS - suite.keys()
    unknown = suite.keys() - REQUIRED_FIELDS
    if missing:
        raise SuiteError(f"{path}: missing fields: {', '.join(sorted(missing))}")
    if unknown:
        raise SuiteError(f"{path}: unknown fields: {', '.join(sorted(unknown))}")
    if suite["schema_version"] != 1:
        raise SuiteError(f"{path}: unsupported schema_version")
    if suite["name"] != path.stem:
        raise SuiteError(f"{path}: name must match filename")
    for field in (
        "description",
        "harbor_version",
        "dataset",
        "bone_version",
        "agent",
        "model",
    ):
        if not isinstance(suite[field], str) or not suite[field].strip():
            raise SuiteError(f"{path}: {field} must be a non-empty string")
    if suite["dataset"] != "terminal-bench@2.0":
        raise SuiteError(f"{path}: dataset must be pinned to terminal-bench@2.0")
    if suite["model"] != "openai/gpt-5.6-luna":
        raise SuiteError(f"{path}: model must be openai/gpt-5.6-luna")
    for field in ("expected_task_count", "attempts", "concurrency"):
        if (
            isinstance(suite[field], bool)
            or not isinstance(suite[field], int)
            or suite[field] < 1
        ):
            raise SuiteError(f"{path}: {field} must be a positive integer")
    multiplier = suite["timeout_multiplier"]
    if (
        isinstance(multiplier, bool)
        or not isinstance(multiplier, (int, float))
        or multiplier <= 0
    ):
        raise SuiteError(f"{path}: timeout_multiplier must be positive")
    tasks = suite["tasks"]
    if not isinstance(tasks, list) or not all(
        isinstance(task, str) and task for task in tasks
    ):
        raise SuiteError(f"{path}: tasks must be a list of non-empty strings")
    if len(tasks) != len(set(tasks)):
        raise SuiteError(f"{path}: tasks contains duplicates")
    if tasks and len(tasks) != suite["expected_task_count"]:
        raise SuiteError(f"{path}: expected_task_count does not match tasks")
    if not tasks and suite["expected_task_count"] != 89:
        raise SuiteError(
            f"{path}: an unfiltered Terminal-Bench 2 suite must expect 89 tasks"
        )
    return suite


def all_suite_names() -> list[str]:
    return [path.stem for path in sorted(SUITES_DIR.glob("*.json"))]


def build_harbor_command(
    suite: dict[str, Any],
    *,
    job_name: str | None = None,
    jobs_dir: Path | None = None,
) -> list[str]:
    command = [
        "uvx",
        "--from",
        f"harbor=={suite['harbor_version']}",
        "harbor",
        "run",
        "--dataset",
        suite["dataset"],
        "--agent",
        suite["agent"],
        "--agent-kwarg",
        f"version={suite['bone_version']}",
        "--model",
        suite["model"],
        "--n-attempts",
        str(suite["attempts"]),
        "--n-concurrent",
        str(suite["concurrency"]),
        "--timeout-multiplier",
        str(suite["timeout_multiplier"]),
        "--jobs-dir",
        str((jobs_dir or RESULTS_DIR).resolve()),
    ]
    if job_name:
        command.extend(("--job-name", job_name))
    for task in suite["tasks"]:
        command.extend(("--include-task-name", task))
    if not suite["tasks"]:
        command.extend(("--n-tasks", str(suite["expected_task_count"])))
    return command


def command_environment() -> dict[str, str]:
    environment = os.environ.copy()
    prior = environment.get("PYTHONPATH")
    environment["PYTHONPATH"] = str(ROOT) if not prior else f"{ROOT}{os.pathsep}{prior}"
    return environment


def _run_capture(command: list[str]) -> tuple[bool, str]:
    try:
        result = subprocess.run(
            command,
            cwd=ROOT,
            env=command_environment(),
            check=False,
            capture_output=True,
            text=True,
        )
    except OSError as error:
        return False, str(error)
    detail = (result.stdout + result.stderr).strip()
    return result.returncode == 0, detail


def preflight(suite: dict[str, Any], *, network: bool = True) -> list[dict[str, Any]]:
    checks: list[dict[str, Any]] = []

    def record(name: str, passed: bool, detail: str) -> None:
        checks.append({"name": name, "passed": passed, "detail": detail})

    record("Python", sys.version_info >= (3, 11), sys.version.split()[0])
    uvx = shutil.which("uvx")
    record("uvx", uvx is not None, uvx or "not found")
    if uvx:
        passed, detail = _run_capture(
            [uvx, "--from", f"harbor=={suite['harbor_version']}", "harbor", "--version"]
        )
        record("Harbor", passed and suite["harbor_version"] in detail, detail)
    else:
        record("Harbor", False, "cannot verify without uvx")
    docker = shutil.which("docker")
    record("Docker CLI", docker is not None, docker or "not found")
    if docker:
        passed, detail = _run_capture(
            [docker, "info", "--format", "{{.ServerVersion}}"]
        )
        record("Docker daemon", passed, detail or "unavailable")
    else:
        record("Docker daemon", False, "cannot verify without Docker CLI")
    record(
        "OPENAI_API_KEY",
        bool(os.environ.get("OPENAI_API_KEY")),
        "set"
        if os.environ.get("OPENAI_API_KEY")
        else "not set (value is never printed)",
    )
    if network:
        asset = "bone-linux-x86_64"
        url = (
            "https://github.com/frelion/bone/releases/download/"
            f"v{suite['bone_version']}/{asset}"
        )
        try:
            request = urllib.request.Request(url, method="HEAD")
            with urllib.request.urlopen(request, timeout=15) as response:
                passed = 200 <= response.status < 400
                detail = f"HTTP {response.status}: {url}"
        except (OSError, urllib.error.URLError) as error:
            passed, detail = False, str(error)
        record("BONE release", passed, detail)
    return checks


def _primary_reward(rewards: Any) -> float | None:
    if not isinstance(rewards, dict) or not rewards:
        return None
    value = rewards.get("reward")
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        numeric = [
            item
            for item in rewards.values()
            if isinstance(item, (int, float)) and not isinstance(item, bool)
        ]
        if len(numeric) != 1:
            return None
        value = numeric[0]
    return float(value)


def _duration_seconds(trial: dict[str, Any]) -> float | None:
    try:
        started = datetime.fromisoformat(trial["started_at"].replace("Z", "+00:00"))
        finished = datetime.fromisoformat(trial["finished_at"].replace("Z", "+00:00"))
    except (AttributeError, KeyError, TypeError, ValueError):
        return None
    return (finished - started).total_seconds()


def _exception_name(trial: dict[str, Any]) -> str | None:
    info = trial.get("exception_info")
    if not isinstance(info, dict):
        return None
    return str(info.get("exception_type") or "UnknownError")


def summarize_job(job_dir: Path, suite: dict[str, Any] | None = None) -> dict[str, Any]:
    result_path = job_dir / "result.json"
    try:
        result = json.loads(result_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SuiteError(f"cannot read Harbor result {result_path}: {error}") from error
    trials_raw = result.get("trial_results")
    if not isinstance(trials_raw, list):
        raise SuiteError(f"{result_path}: trial_results must be a list")

    trials: list[dict[str, Any]] = []
    task_passes: dict[str, list[bool]] = defaultdict(list)
    categories: Counter[str] = Counter()
    bone_statuses: Counter[str] = Counter()
    for raw in trials_raw:
        if not isinstance(raw, dict):
            raise SuiteError(f"{result_path}: invalid trial result")
        verifier = raw.get("verifier_result") or {}
        reward = _primary_reward(
            verifier.get("rewards") if isinstance(verifier, dict) else None
        )
        exception = _exception_name(raw)
        if exception:
            category = "infrastructure_error"
        elif reward is None:
            category = "missing_verifier_result"
        elif reward >= 1.0:
            category = "passed"
        else:
            category = "verifier_failed"
        categories[category] += 1
        task_name = str(raw.get("task_name") or "unknown")
        if reward is not None:
            task_passes[task_name].append(reward >= 1.0)
        agent_result = raw.get("agent_result") or {}
        metadata = (
            agent_result.get("metadata") if isinstance(agent_result, dict) else {}
        )
        bone = metadata.get("bone") if isinstance(metadata, dict) else {}
        bone_status = bone.get("status") if isinstance(bone, dict) else None
        if bone_status:
            bone_statuses[str(bone_status)] += 1
        trials.append(
            {
                "task_name": task_name,
                "trial_name": raw.get("trial_name"),
                "task_checksum": raw.get("task_checksum"),
                "reward": reward,
                "category": category,
                "exception_type": exception,
                "duration_seconds": _duration_seconds(raw),
                "bone": bone if isinstance(bone, dict) else {},
            }
        )

    total = len(trials)
    scored = categories["passed"] + categories["verifier_failed"]
    passed_tasks = sum(any(attempts) for attempts in task_passes.values())
    stats = result.get("stats") if isinstance(result.get("stats"), dict) else {}
    lock_path = job_dir / "lock.json"
    lock = None
    if lock_path.is_file():
        try:
            lock = json.loads(lock_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            lock = {"error": "lock.json could not be parsed"}
    scheduled = result.get("n_total_trials", total)
    if (
        isinstance(scheduled, bool)
        or not isinstance(scheduled, int)
        or scheduled < total
    ):
        scheduled = total
    summary = {
        "schema_version": 1,
        "suite": suite["name"] if suite else None,
        "job_dir": str(job_dir.resolve()),
        "job_id": result.get("id"),
        "started_at": result.get("started_at"),
        "finished_at": result.get("finished_at"),
        "configuration": suite,
        "metrics": {
            "scheduled_trials": scheduled,
            "observed_trials": total,
            "scored_trials": scored,
            "passed_trials": categories["passed"],
            "all_scheduled_pass_rate": (
                categories["passed"] / scheduled if scheduled else 0.0
            ),
            "scored_pass_rate": categories["passed"] / scored if scored else 0.0,
            "tasks_with_scored_attempts": len(task_passes),
            "tasks_passing_at_least_once": passed_tasks,
            "observed_pass_at_k": passed_tasks / len(task_passes)
            if task_passes
            else 0.0,
            "categories": dict(sorted(categories.items())),
            "bone_statuses": dict(sorted(bone_statuses.items())),
            "n_input_tokens": stats.get("n_input_tokens"),
            "n_cache_tokens": stats.get("n_cache_tokens"),
            "n_output_tokens": stats.get("n_output_tokens"),
            "cost_usd": stats.get("cost_usd"),
        },
        "trials": trials,
        "harbor_lock": lock,
    }
    return summary


def write_summary(job_dir: Path, summary: dict[str, Any]) -> tuple[Path, Path]:
    json_path = job_dir / "bone-summary.json"
    markdown_path = job_dir / "bone-summary.md"
    json_path.write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    metrics = summary["metrics"]
    categories = metrics["categories"]
    lines = [
        f"# BONE benchmark summary: {summary.get('suite') or 'unlabeled job'}",
        "",
        f"- Job: `{summary.get('job_id')}`",
        f"- Observed trials: {metrics['observed_trials']}",
        f"- Passed trials: {metrics['passed_trials']}",
        f"- All-trial pass rate: {metrics['all_scheduled_pass_rate']:.2%}",
        f"- Scored-only pass rate: {metrics['scored_pass_rate']:.2%}",
        f"- Observed pass@k: {metrics['observed_pass_at_k']:.2%}",
        f"- Infrastructure errors: {categories.get('infrastructure_error', 0)}",
        f"- Missing verifier results: {categories.get('missing_verifier_result', 0)}",
        f"- Input/output tokens: {metrics['n_input_tokens']} / {metrics['n_output_tokens']}",
        f"- Reported cost (USD): {metrics['cost_usd']}",
        "",
        "Infrastructure errors remain failures in the all-trial pass rate. The scored-only rate is shown separately for diagnosis.",
        "",
    ]
    markdown_path.write_text("\n".join(lines), encoding="utf-8")
    return json_path, markdown_path


def print_checks(checks: list[dict[str, Any]]) -> bool:
    for check in checks:
        marker = "PASS" if check["passed"] else "FAIL"
        print(f"[{marker}] {check['name']}: {check['detail']}")
    return all(check["passed"] for check in checks)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    validate = subparsers.add_parser("validate", help="validate suite manifests")
    validate.add_argument("suite", nargs="?")
    validate.add_argument("--all", action="store_true")
    validate.add_argument(
        "--harbor", action="store_true", help="ask pinned Harbor to resolve config"
    )
    preflight_parser = subparsers.add_parser(
        "preflight", help="check run prerequisites"
    )
    preflight_parser.add_argument("suite")
    preflight_parser.add_argument("--no-network", action="store_true")
    command_parser = subparsers.add_parser(
        "command", help="print the exact Harbor command"
    )
    command_parser.add_argument("suite")
    command_parser.add_argument("--job-name")
    run_parser = subparsers.add_parser("run", help="run a suite through pinned Harbor")
    run_parser.add_argument("suite")
    run_parser.add_argument("--job-name")
    run_parser.add_argument("--skip-preflight", action="store_true")
    summarize = subparsers.add_parser(
        "summarize", help="summarize a completed Harbor job"
    )
    summarize.add_argument("job_dir", type=Path)
    summarize.add_argument("--suite")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        if args.command == "validate":
            if args.all == bool(args.suite):
                raise SuiteError("choose exactly one of SUITE or --all")
            names = all_suite_names() if args.all else [args.suite]
            for name in names:
                suite = load_suite(name)
                print(f"valid: {suite['name']} ({suite['expected_task_count']} tasks)")
                if args.harbor:
                    command = build_harbor_command(suite) + ["--print-config"]
                    passed, detail = _run_capture(command)
                    if detail:
                        print(detail)
                    if not passed:
                        return 2
            return 0
        if args.command == "preflight":
            suite = load_suite(args.suite)
            return (
                0 if print_checks(preflight(suite, network=not args.no_network)) else 2
            )
        if args.command == "command":
            suite = load_suite(args.suite)
            print(shlex.join(build_harbor_command(suite, job_name=args.job_name)))
            return 0
        if args.command == "run":
            suite = load_suite(args.suite)
            if not args.skip_preflight and not print_checks(preflight(suite)):
                return 2
            RESULTS_DIR.mkdir(parents=True, exist_ok=True)
            timestamp = datetime.now(UTC).strftime("%Y%m%d-%H%M%S")
            job_name = args.job_name or f"{suite['name']}-{timestamp}"
            command = build_harbor_command(suite, job_name=job_name)
            print(f"running: {shlex.join(command)}", flush=True)
            return subprocess.call(command, cwd=ROOT, env=command_environment())
        if args.command == "summarize":
            suite = load_suite(args.suite) if args.suite else None
            summary = summarize_job(args.job_dir.resolve(), suite)
            json_path, markdown_path = write_summary(args.job_dir.resolve(), summary)
            print(json_path)
            print(markdown_path)
            return 0
    except SuiteError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
