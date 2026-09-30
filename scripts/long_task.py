#!/usr/bin/env python3
"""A staged single-session dogfood gate; no requests without explicit --run."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "fixtures/long_task"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def snapshot(workspace):
    return {str(path.relative_to(workspace)): digest(path) for path in workspace.rglob("*") if path.is_file()}


def metrics(document):
    reported = document.get("metrics", {}) if document else {}
    return {key: reported.get(key) for key in ("model_calls", "input_tokens", "output_tokens", "total_tokens", "cost")}


def sum_metrics(records):
    result = {}
    for key in ("model_calls", "input_tokens", "output_tokens", "total_tokens", "cost"):
        values = [record["metrics"][key] for record in records]
        result[key] = sum(values) if values and all(isinstance(value, (int, float)) and not isinstance(value, bool) for value in values) else None
    return result


def capture(command, directory, name, environment, timeout):
    started = time.monotonic()
    try:
        result = subprocess.run(command, capture_output=True, text=True, env=environment, timeout=timeout)
        stdout, stderr, code = result.stdout, result.stderr, result.returncode
    except subprocess.TimeoutExpired as error:
        stdout, stderr, code = error.stdout or "", error.stderr or "", None
        if isinstance(stdout, bytes):
            stdout = stdout.decode(errors="replace")
        if isinstance(stderr, bytes):
            stderr = stderr.decode(errors="replace")
    except OSError as error:
        stdout, stderr, code = "", str(error), None
    (directory / (name + ".stdout")).write_text(stdout, encoding="utf-8")
    (directory / (name + ".stderr")).write_text(stderr, encoding="utf-8")
    try:
        document = json.loads(stdout)
    except ValueError:
        document = None
    return code, document, time.monotonic() - started


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true")
    parser.add_argument("--bone", type=Path, default=ROOT / "target/debug/bone")
    parser.add_argument("--data-dir", type=Path)
    parser.add_argument("--profile")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--context-chars", type=int, default=32000)
    parser.add_argument("--max-calls", type=int, default=48)
    parser.add_argument("--max-parallel", type=int, default=3)
    parser.add_argument("--timeout", type=int, default=300)
    args = parser.parse_args()
    task = json.loads((FIXTURE / "task.json").read_text())
    if not args.run:
        print(json.dumps({"executed": False, "rounds": task["rounds"]}, indent=2))
        return 0
    if args.data_dir is None or args.profile is None or args.output_dir is None:
        parser.error("--run requires --data-dir, --profile, and --output-dir")
    if min(args.max_calls, args.max_parallel, args.timeout) < 1 or args.context_chars < 1024:
        parser.error("execution limits must be positive; context-chars must be at least 1024")
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=False)
    workspace = output / "workspace"
    shutil.copytree(FIXTURE / "workspace", workspace)
    binary = args.bone.resolve()
    data = args.data_dir.resolve()
    fingerprint = {"binary_sha256": digest(binary), "config_sha256": digest(data / "config.toml"),
                   "task_sha256": digest(FIXTURE / "task.json"), "verifier_sha256": digest(FIXTURE / "verify.py")}
    environment = os.environ.copy()
    environment.pop("BONE_MODEL", None)
    session = None
    records, kernel_faults, quality_failures, coverage_failures, execution_failures = [], [], [], [], []
    with (output / "rounds.jsonl").open("x", encoding="utf-8") as stream:
        stream.write(json.dumps({"type": "experiment", "profile": args.profile, **fingerprint,
                                 "context_chars": args.context_chars, "max_calls": args.max_calls,
                                 "max_parallel": args.max_parallel, "timeout": args.timeout}) + "\n")
        stream.flush()
        for phase in task["rounds"]:
            index = phase["id"]
            directory = output / ("round-%02d" % index)
            directory.mkdir()
            before = snapshot(workspace)
            current = {"binary_sha256": digest(binary), "config_sha256": digest(data / "config.toml"),
                       "task_sha256": digest(FIXTURE / "task.json"), "verifier_sha256": digest(FIXTURE / "verify.py")}
            if current != fingerprint:
                execution_failures.append({"round": index, "kind": "experiment_configuration_changed"})
                break
            command = [str(binary), "run", phase["prompt"], "--workspace", str(workspace),
                       "--data-dir", str(data), "--profile", args.profile, "--json",
                       "--context-chars", str(args.context_chars), "--max-calls", str(args.max_calls),
                       "--max-parallel", str(args.max_parallel), "--timeout-seconds", str(args.timeout)]
            if session is not None:
                command.extend(["--session", session])
            code, document, seconds = capture(command, directory, "run", environment, args.timeout + 5)
            if not isinstance(document, dict):
                document = None
            if isinstance(document, dict) and isinstance(document.get("session_id"), str):
                if session is not None and document["session_id"] != session:
                    kernel_faults.append({"round": index, "kind": "session_changed"})
                session = document["session_id"]
            _, history, _ = capture([str(binary), "history", session or "invalid", "--data-dir", str(data), "--json"],
                                    directory, "history", environment, 20)
            if not isinstance(history, list):
                execution_failures.append({"round": index, "kind": "history_unavailable"})
                history = []
            ownership = [event.get("id") for event in history if event.get("kind", "").startswith(("model_", "tool_"))
                         and not all(event.get(key) for key in ("job_id", "call_id", "root_input"))]
            if ownership:
                kernel_faults.append({"round": index, "kind": "missing_effect_ownership", "event_ids": ownership})
            after = snapshot(workspace)
            current_input = document.get("input_id") if document else None
            started_writes = [event.get("id") for event in history
                              if event.get("root_input") == current_input and event.get("kind") == "tool_started"
                              and event.get("data", {}).get("tool_name") == "write_file"]
            no_write = not phase.get("no_write") or (before == after and not started_writes)
            if not no_write:
                quality_failures.append({"round": index, "kind": "model_violated_no_write"})
            _, check, _ = capture([sys.executable, "-B", str(FIXTURE / "verify.py"), "--workspace", str(workspace), "--phase", str(index)],
                                  directory, "verification", environment, 60)
            if index == 1:
                check = {"passed": no_write, "run": 0, "skipped": 9, "failures": [], "errors": []}
            if not isinstance(check, dict):
                execution_failures.append({"round": index, "kind": "verifier_unavailable"})
                check = {"passed": False, "unavailable": True}
            elif not check.get("passed"):
                quality_failures.append({"round": index, "kind": "independent_checks_failed"})
            text = document.get("text", "") if isinstance(document, dict) else ""
            pending_ok = all(term in text.lower() for term in phase.get("pending_terms", []))
            if not pending_ok:
                quality_failures.append({"round": index, "kind": "deferred_work_missing_from_explanation"})
            status = document.get("status") if isinstance(document, dict) else None
            control_ok = code == 0 and status == phase["expect_status"]
            if not control_ok:
                execution_failures.append({"round": index, "kind": "control_or_provider_failure", "status": status, "exit_code": code})
            if index == 7:
                _, states, _ = capture([str(binary), "sessions", "--data-dir", str(data), "--json"], directory, "sessions", environment, 20)
                saved = next((state for state in states if state.get("id") == session), None) if isinstance(states, list) else None
                if not isinstance(states, list) or saved is None:
                    execution_failures.append({"round": index, "kind": "saved_pause_state_unavailable"})
                elif status == "paused" and not saved.get("paused"):
                    kernel_faults.append({"round": index, "kind": "pause_not_persisted"})
            record = {"round": index, "session_id": session, "status": status, "exit_code": code,
                      "wall_seconds": seconds, "metrics": metrics(document), "verification": check,
                      "no_write_passed": no_write, "deferred_work_explanation_passed": pending_ok,
                      "write_file_started_ids": started_writes,
                      "event_counts": {"jobs": len({event.get("job_id") for event in history if event.get("job_id")}),
                                       "summaries": sum(event.get("kind") == "summary" for event in history)},
                      "completed": control_ok}
            records.append(record)
            stream.write(json.dumps(record, ensure_ascii=False) + "\n")
            stream.flush()
            print(json.dumps({"round": index, "status": status,
                              "verification_passed": bool(check and check.get("passed")),
                              "deferred_work_passed": pending_ok, "no_write_passed": no_write,
                              "passed": control_ok and bool(check and check.get("passed")) and pending_ok and no_write,
                              "summaries": record["event_counts"]["summaries"]}), flush=True)
            if session is None:
                break
    final_files = set(snapshot(workspace))
    allowed_files_ok = final_files.issubset(set(task["allowed_files"]))
    if not allowed_files_ok:
        quality_failures.append({"kind": "unexpected_workspace_files", "paths": sorted(final_files - set(task["allowed_files"]))})
    summaries = records[-1]["event_counts"]["summaries"] if records else 0
    if summaries < 2:
        coverage_failures.append({"kind": "fewer_than_two_actual_summaries", "observed": summaries})
    if len(records) < 12:
        coverage_failures.append({"kind": "fewer_than_twelve_rounds", "observed": len(records)})
    result = {"passed": not (kernel_faults or quality_failures or coverage_failures or execution_failures),
              "session_id": session, "rounds": len(records), "kernel_faults": kernel_faults,
              "quality_failures": quality_failures, "coverage_failures": coverage_failures,
              "execution_failures": execution_failures, "needs_review": bool(execution_failures),
              "metrics": sum_metrics(records), "wall_seconds": sum(record["wall_seconds"] for record in records),
              "final_quality_passed": bool(records and records[-1]["verification"].get("passed")),
              "allowed_files_passed": allowed_files_ok, **fingerprint}
    (output / "result.json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(result, ensure_ascii=False))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
