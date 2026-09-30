#!/usr/bin/env python3
"""Run alternating, reproducible CLI comparisons against external verifiers.

Python 3.9+, standard library only. No model requests occur without --run.
The script records every planned trial, including failures. Optional artifacts
retain fixture workspaces and CLI output; credential contents are never read.
"""

import argparse
import contextlib
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import uuid
from typing import Any, Dict, List, Optional


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_FIXTURES = ROOT / "fixtures" / "acceptance"
PAIR_VARIANTS = {
    "jobs": ("multi_job", "single_job"),
    "parallel": ("parallel", "serial"),
    "compaction": ("compaction", "no_compaction"),
}
METRIC_ALIASES = {
    "model_calls": ("model_calls", "calls"),
    "input_tokens": ("input_tokens", "prompt_tokens"),
    "output_tokens": ("output_tokens", "completion_tokens"),
    "total_tokens": ("total_tokens",),
    "cost_usd": ("cost_usd", "cost"),
}


def load_cases(fixtures: Path) -> List[Dict[str, Any]]:
    manifest = json.loads((fixtures / "cases.json").read_text(encoding="utf-8"))
    if manifest.get("schema_version") != 1:
        raise ValueError("unsupported fixture schema")
    cases = manifest["cases"]
    names = [case["id"] for case in cases]
    if len(names) != len(set(names)):
        raise ValueError("duplicate fixture id")
    return cases


def schedule(cases: List[Dict[str, Any]], repetitions: int,
             pairs: Optional[List[str]] = None, baseline_only: bool = False,
             start_repetition: int = 1) -> List[Dict[str, Any]]:
    trials = []
    for case in cases:
        if baseline_only:
            trials.append({"case_id": case["id"], "category": case["category"],
                           "pair": "baseline", "repetition": 1, "position": 1,
                           "variant": "baseline"})
            continue
        for pair, variants in PAIR_VARIANTS.items():
            if pairs is not None and pair not in pairs:
                continue
            for repetition in range(start_repetition - 1, start_repetition - 1 + repetitions):
                order = variants if repetition % 2 == 0 else tuple(reversed(variants))
                for position, variant in enumerate(order):
                    trials.append({
                        "case_id": case["id"], "category": case["category"],
                        "pair": pair, "repetition": repetition + 1,
                        "position": position + 1, "variant": variant,
                    })
    return trials


def flags_for(variant: str, max_parallel: int) -> List[str]:
    flags = ["--max-parallel", str(1 if variant == "serial" else max_parallel)]
    if variant == "single_job":
        flags.append("--single-job")
    if variant == "no_compaction":
        flags.append("--no-compaction")
    return flags


def prepare_workspace(fixtures: Path, case: Dict[str, Any], workspace: Path) -> None:
    shutil.copytree(fixtures / case["workspace"], workspace)
    if "long_context_lines" in case:
        count = int(case["long_context_lines"])
        rules = {
            11: "KEEP_RULE: render_report emits the exact header name,amount_cents followed by newline.",
            count // 2: "KEEP_RULE: sort rows by name ascending, preserving signed integer cents without currency conversion.",
            count - 12: "KEEP_RULE: each row ends with newline; an empty list emits only the header; do not rename render_report.",
        }
        lines = [rules.get(index, "ARCHIVE %05d: expired draft; ignore this archive entry." % index)
                 for index in range(count)]
        (workspace / "requirements.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_workspace(case: Dict[str, Any], workspace: Path,
                     original_hashes: Dict[str, str]) -> Dict[str, Any]:
    checks = []
    for index, check in enumerate(case["checks"]):
        passed = False
        if check["kind"] == "python":
            # The verifier is outside the agent's workspace and cannot be edited
            # to manufacture a pass. Load user code with its real module globals.
            source = "import runpy, sys\nsys.path.insert(0, sys.argv[1])\nnamespace = {}\n"
            for name in check["files"]:
                source += "namespace.update(runpy.run_path(sys.argv[1] + '/' + %r))\n" % name
            source += "exec(%r, namespace)\n" % check["code"]
            try:
                result = subprocess.run(
                    [sys.executable, "-B", "-c", source, str(workspace)], cwd=workspace,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20,
                )
                passed = result.returncode == 0
            except (OSError, subprocess.TimeoutExpired):
                passed = False
        elif check["kind"] == "json_fields":
            try:
                document = json.loads((workspace / check["path"]).read_text(encoding="utf-8"))
                fields = set(check["required"])
                types = {"event_id": "string", "occurred_at": "string", "amount_cents": "integer"}
                passed = (document.get("type") == "object"
                          and fields.issubset(document.get("required", []))
                          and all(document.get("properties", {}).get(name, {}).get("type") == types[name]
                                  for name in fields))
            except (OSError, ValueError, TypeError, AttributeError):
                passed = False
        else:
            raise ValueError("unknown verifier kind")
        checks.append({"check": index + 1, "kind": check["kind"], "passed": passed})

    for name, before in original_hashes.items():
        try:
            passed = digest(workspace / name) == before
        except OSError:
            passed = False
        checks.append({"check": "unchanged:" + name, "passed": passed})
    if "allowed_files" in case:
        actual = {str(path.relative_to(workspace)) for path in workspace.rglob("*") if path.is_file()}
        checks.append({"check": "allowed_files", "passed": actual.issubset(set(case["allowed_files"]))})
    return {"passed": all(check["passed"] for check in checks), "checks": checks}


def final_document(stdout: str) -> Optional[Dict[str, Any]]:
    try:
        whole = json.loads(stdout)
        if isinstance(whole, dict):
            return whole
    except ValueError:
        pass
    result = None
    for line in stdout.splitlines():
        try:
            item = json.loads(line)
        except ValueError:
            continue
        if isinstance(item, dict):
            result = item
    return result


def number(value: Any) -> Optional[float]:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return value if math.isfinite(value) and value >= 0 else None


def extract_metrics(document: Optional[Dict[str, Any]]) -> Dict[str, Optional[float]]:
    metrics = document.get("metrics", {}) if document else {}
    if not isinstance(metrics, dict):
        metrics = {}
    result = {}
    for field, aliases in METRIC_ALIASES.items():
        result[field] = next((number(metrics[key]) for key in aliases
                              if key in metrics and number(metrics[key]) is not None), None)
    return result


def sum_metrics(steps: List[Dict[str, Any]]) -> Dict[str, Optional[float]]:
    result = {}
    for field in METRIC_ALIASES:
        values = [step["metrics"].get(field) for step in steps]
        result[field] = sum(values) if values and all(value is not None for value in values) else None
    return result


def execute_trial(args: argparse.Namespace, trial: Dict[str, Any], case: Dict[str, Any],
                  fixtures: Path) -> Dict[str, Any]:
    record = dict(trial)
    record.update({"schema_version": 1, "trial_id": str(uuid.uuid4()), "session_id": None, "steps": []})
    started = time.monotonic()
    config_path = args.data_dir / "config.toml"
    configuration = digest(config_path) if config_path.exists() else None
    if configuration != args.configuration_sha256:
        record.update({"passed": False, "outcome": "configuration_changed", "wall_seconds": 0,
                       "metrics": extract_metrics(None),
                       "verification": {"passed": False, "checks": [{"check": "fixed_configuration", "passed": False}]}})
        return record
    environment = os.environ.copy()
    # A model override would silently bypass the fixed experiment profile.
    environment.pop("BONE_MODEL", None)
    artifacts = getattr(args, "artifacts_dir", None)
    if artifacts is not None:
        directory = artifacts / record["trial_id"]
        directory.mkdir(parents=True)
        record["artifacts_dir"] = str(directory)
        storage = contextlib.nullcontext(str(directory))
    else:
        storage = tempfile.TemporaryDirectory(prefix="bone-ablation-")
    with storage as directory:
        workspace = Path(directory) / "workspace"
        prepare_workspace(fixtures, case, workspace)
        hashes = {name: digest(workspace / name) for name in case.get("unchanged_files", [])}
        for index, prompt in enumerate(case["prompts"]):
            command = [str(args.bone), "run", prompt, "--workspace", str(workspace),
                       "--profile", args.profile,
                       "--data-dir", str(args.data_dir), "--json"]
            if record["session_id"] is not None:
                command.extend(["--session", record["session_id"]])
            command.extend(flags_for(trial["variant"], args.max_parallel))
            command.extend(["--max-calls", str(args.max_calls),
                            "--context-chars", str(args.context_chars),
                            "--timeout-seconds", str(int(args.timeout))])
            step_started = time.monotonic()
            document = None
            outcome = "launch_error"
            exit_code = None
            try:
                completed = subprocess.run(command, capture_output=True, text=True,
                                           timeout=args.timeout, cwd=workspace, env=environment)
                exit_code = completed.returncode
                document = final_document(completed.stdout)
                outcome = "completed" if exit_code == 0 and document else "cli_error"
                if artifacts is not None:
                    (Path(directory) / ("stdout.%d.json" % (index + 1))).write_text(completed.stdout, encoding="utf-8")
                    (Path(directory) / ("stderr.%d.txt" % (index + 1))).write_text(completed.stderr, encoding="utf-8")
            except subprocess.TimeoutExpired as error:
                outcome = "timeout"
                if artifacts is not None:
                    for name, value in (("stdout", error.stdout), ("stderr", error.stderr)):
                        if isinstance(value, bytes):
                            value = value.decode("utf-8", errors="replace")
                        (Path(directory) / ("%s.%d.txt" % (name, index + 1))).write_text(value or "", encoding="utf-8")
            except OSError:
                outcome = "launch_error"
            status = document.get("status") if document else None
            if not isinstance(status, str) or len(status) > 64:
                status = None
            returned_session = document.get("session_id") if document else None
            if isinstance(returned_session, str):
                try:
                    record["session_id"] = str(uuid.UUID(returned_session))
                except ValueError:
                    pass
            record["steps"].append({
                "step": index + 1, "outcome": outcome, "exit_code": exit_code,
                "status": status, "wall_seconds": time.monotonic() - step_started,
                "metrics": extract_metrics(document),
            })
            if outcome != "completed":
                break
        record["verification"] = verify_workspace(case, workspace, hashes)
        if artifacts is not None and record["session_id"] is not None:
            history = subprocess.run([str(args.bone), "history", record["session_id"],
                                      "--data-dir", str(args.data_dir), "--json"],
                                     capture_output=True, text=True, timeout=20, env=environment)
            (Path(directory) / "history.json").write_text(history.stdout, encoding="utf-8")
            try:
                events = json.loads(history.stdout)
                record["event_counts"] = {
                    "jobs": len({event["job_id"] for event in events if event.get("job_id")}),
                    "summaries": sum(event.get("kind") == "summary" for event in events),
                    "summary_model_calls": sum(event.get("kind") == "model_started"
                                               and event.get("data", {}).get("purpose") == "summary"
                                               for event in events),
                }
            except (ValueError, KeyError, TypeError):
                record["event_counts"] = None
    record["wall_seconds"] = time.monotonic() - started
    record["metrics"] = sum_metrics(record["steps"])
    record["passed"] = (len(record["steps"]) == len(case["prompts"])
                        and all(step["outcome"] == "completed" for step in record["steps"])
                        and all(step["status"] == "completed" for step in record["steps"])
                        and record["verification"]["passed"])
    return record


def summary(records: List[Dict[str, Any]]) -> Dict[str, Any]:
    groups = {}
    for record in records:
        key = record["pair"] + "/" + record["variant"]
        group = groups.setdefault(key, {"trials": 0, "passed": 0, "failed": 0,
                                        "wall_seconds": 0.0, "metrics": {}})
        group["trials"] += 1
        group["passed" if record["passed"] else "failed"] += 1
        group["wall_seconds"] += record["wall_seconds"]
        for field, value in record["metrics"].items():
            previous = group["metrics"].get(field, 0)
            group["metrics"][field] = previous + value if previous is not None and value is not None else None
    return {"schema_version": 1, "trials": len(records), "groups": groups,
            "note": "Metrics are summed only when every contributing trial reports them; unknown values remain null."}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="execute CLI trials (may make billable model requests)")
    parser.add_argument("--bone", type=Path, default=ROOT / "target" / "debug" / "bone")
    parser.add_argument("--fixtures", type=Path, default=DEFAULT_FIXTURES)
    parser.add_argument("--data-dir", type=Path)
    parser.add_argument("--profile")
    parser.add_argument("--case", action="append", dest="case_ids")
    parser.add_argument("--pair", action="append", choices=list(PAIR_VARIANTS))
    parser.add_argument("--baseline-only", action="store_true", help="run each selected case once with default capabilities")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--start-repetition", type=int, default=1, help="continue the same AB/BA ordering after an earlier pair")
    parser.add_argument("--max-parallel", type=int, default=4)
    parser.add_argument("--timeout", type=float, default=300)
    parser.add_argument("--max-calls", type=int, default=64)
    parser.add_argument("--context-chars", type=int, default=96000)
    parser.add_argument("--artifacts-dir", type=Path, help="retain fixture workspaces, raw CLI output, and event histories")
    parser.add_argument("--output", type=Path, default=ROOT / "ablation-results.jsonl")
    args = parser.parse_args()
    if min(args.repetitions, args.start_repetition, args.max_parallel, args.timeout, args.max_calls, args.context_chars) <= 0:
        parser.error("all execution budgets must be positive")
    if args.baseline_only and args.pair:
        parser.error("--baseline-only cannot be combined with --pair")
    fixtures = args.fixtures.resolve()
    cases = load_cases(fixtures)
    if args.case_ids:
        known = {case["id"] for case in cases}
        if set(args.case_ids) - known:
            parser.error("unknown case id")
        cases = [case for case in cases if case["id"] in args.case_ids]
    trials = schedule(cases, args.repetitions, args.pair, args.baseline_only, args.start_repetition)
    if not args.run:
        print(json.dumps({"executed": False, "trials": trials}, indent=2))
        return 0
    if args.profile is None or args.data_dir is None:
        parser.error("--run requires an explicit --profile and --data-dir")
    args.bone = args.bone.resolve()
    args.data_dir = args.data_dir.resolve()
    if args.artifacts_dir is not None:
        args.artifacts_dir = args.artifacts_dir.resolve()
    config_path = args.data_dir / "config.toml"
    args.configuration_sha256 = digest(config_path) if config_path.exists() else None
    args.binary_sha256 = digest(args.bone)
    cases_by_id = {case["id"]: case for case in cases}
    records = []
    args.output.parent.mkdir(parents=True, exist_ok=True)
    # Refuse to overwrite an earlier experiment. Every completed trial is flushed.
    with args.output.open("x", encoding="utf-8") as stream:
        stream.write(json.dumps({"type": "experiment", "schema_version": 1,
                                 "planned_trials": len(trials), "repetitions": args.repetitions,
                                 "start_repetition": args.start_repetition,
                                 "profile": args.profile, "max_parallel": args.max_parallel,
                                 "max_calls": args.max_calls, "context_chars": args.context_chars,
                                 "timeout_seconds": args.timeout,
                                 "binary_sha256": args.binary_sha256,
                                 "configuration_sha256": args.configuration_sha256,
                                 "fixture_manifest_sha256": digest(fixtures / "cases.json")}) + "\n")
        stream.flush()
        for trial in trials:
            record = execute_trial(args, trial, cases_by_id[trial["case_id"]], fixtures)
            records.append(record)
            stream.write(json.dumps(record, ensure_ascii=False) + "\n")
            stream.flush()
            print(json.dumps({"case_id": record["case_id"], "variant": record["variant"],
                              "repetition": record["repetition"], "passed": record["passed"]}))
        aggregate = summary(records)
        stream.write(json.dumps({"type": "summary", **aggregate}) + "\n")
        stream.flush()
    return 0 if all(record["passed"] for record in records) else 1


if __name__ == "__main__":
    raise SystemExit(main())
