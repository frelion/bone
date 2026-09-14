"""Build an execution-efficiency profile from a BONE headless trajectory."""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any


def variant(event: dict[str, Any]) -> tuple[str, dict[str, Any]]:
    name, value = next(iter(event.items()))
    return name, value


def ref_key(reference: dict[str, Any]) -> str:
    return f"{reference['runtime']}:{reference['id']}"


def kind_name(kind: Any) -> str:
    if isinstance(kind, str):
        return kind.lower()
    if isinstance(kind, dict) and "Tool" in kind:
        return f"tool:{kind['Tool']['name']}"
    return "unknown"


def union_duration(intervals: list[tuple[int, int]]) -> int:
    if not intervals:
        return 0
    total = 0
    start, end = sorted(intervals)[0]
    for next_start, next_end in sorted(intervals)[1:]:
        if next_start <= end:
            end = max(end, next_end)
        else:
            total += end - start
            start, end = next_start, next_end
    return total + end - start


def successful_test_run(data: dict[str, Any]) -> bool:
    if data.get("tool") != "bash":
        return False
    result = data.get("outcome", {}).get("result", {}).get("Ok", {})
    if result.get("exit_code") != 0:
        return False
    output = f"{result.get('stdout', '')}\n{result.get('stderr', '')}"
    count = re.search(r"(?m)^Ran ([1-9][0-9]*) tests? in ", output)
    return count is not None and re.search(r"(?m)^OK(?: \([^\n]*\))?$", output) is not None


def build_profile(entries: list[dict[str, Any]]) -> dict[str, Any]:
    if not entries:
        raise ValueError("trajectory is empty")
    origin = entries[0]["occurred_at"]
    end = entries[-1]["occurred_at"]
    starts: dict[str, dict[str, Any]] = {}
    jobs: dict[str, dict[str, Any]] = {}
    calls: list[dict[str, Any]] = []
    read_paths: dict[str, set[int]] = defaultdict(set)
    last_successful_test: int | None = None
    final_job: int | None = None

    for entry in entries:
        name, data = variant(entry["event"])
        timestamp = entry["occurred_at"]
        if name == "JobCreated":
            key = ref_key(data["job"])
            owner = data["owner"]
            parent = owner.get("Job") if isinstance(owner, dict) else None
            jobs[key] = {
                "job": data["job"],
                "parent": parent,
                "goal": data["goal"],
                "created_ms": timestamp - origin,
                "finished_ms": None,
                "outcome": None,
            }
        elif name == "CallStarted":
            key = ref_key(data["call"])
            starts[key] = {
                "call": data["call"],
                "job": data.get("job"),
                "kind": kind_name(data["kind"]),
                "start": timestamp,
            }
        elif name == "CallFinished":
            key = ref_key(data["call"])
            started = starts.pop(key, None)
            if started is not None:
                calls.append(
                    {
                        "call": started["call"],
                        "job": started["job"],
                        "kind": started["kind"],
                        "started_ms": started["start"] - origin,
                        "finished_ms": timestamp - origin,
                        "duration_ms": timestamp - started["start"],
                        "error": data.get("error"),
                    }
                )
        elif name == "ToolFinished":
            call_ref = data.get("call")
            key = ref_key(call_ref) if call_ref is not None else None
            started = starts.pop(key, None) if key is not None else None
            if started is not None:
                result = data.get("outcome", {}).get("result", {})
                calls.append(
                    {
                        "call": started["call"],
                        "job": started["job"],
                        "kind": started["kind"],
                        "started_ms": started["start"] - origin,
                        "finished_ms": timestamp - origin,
                        "duration_ms": timestamp - started["start"],
                        "error": result.get("Err"),
                    }
                )
            result = data.get("outcome", {}).get("result", {}).get("Ok", {})
            if data.get("tool") == "read" and isinstance(result.get("path"), str):
                read_paths[result["path"]].add(data["job"]["id"])
            if successful_test_run(data):
                last_successful_test = timestamp
        elif name == "JobFinished":
            key = ref_key(data["job"])
            job = jobs.setdefault(
                key,
                {
                    "job": data["job"],
                    "parent": None,
                    "goal": None,
                    "created_ms": None,
                },
            )
            job["finished_ms"] = timestamp - origin
            job["outcome"] = data["outcome"]
            if job.get("parent") is None:
                final_job = timestamp

    intervals = [(call["started_ms"], call["finished_ms"]) for call in calls]
    totals = Counter()
    counts = Counter()
    for call in calls:
        totals[call["kind"]] += call["duration_ms"]
        counts[call["kind"]] += 1
    span = end - origin
    active = union_duration(intervals)
    duplicates = [
        {"path": path, "jobs": sorted(job_ids)}
        for path, job_ids in sorted(read_paths.items())
        if len(job_ids) > 1
    ]
    return {
        "schema_version": 1,
        "wall_duration_ms": span,
        "call_active_union_ms": active,
        "outside_call_ms": max(0, span - active),
        "call_counts": dict(sorted(counts.items())),
        "call_duration_ms": dict(sorted(totals.items())),
        "post_verification_ms": (
            final_job - last_successful_test
            if final_job is not None and last_successful_test is not None
            else None
        ),
        "duplicate_reads_across_jobs": duplicates,
        "jobs": sorted(jobs.values(), key=lambda job: job["job"]["id"]),
        "calls": sorted(calls, key=lambda call: call["started_ms"]),
        "unfinished_calls": list(starts.values()),
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trajectory", type=Path)
    parser.add_argument("--output", type=Path)
    return parser.parse_args()


def main() -> int:
    options = parse_args()
    entries = json.loads(options.trajectory.read_text(encoding="utf-8"))
    rendered = json.dumps(build_profile(entries), indent=2) + "\n"
    if options.output:
        options.output.write_text(rendered, encoding="utf-8")
    else:
        sys.stdout.write(rendered)
    return 0


if __name__ == "__main__":
    sys.exit(main())
