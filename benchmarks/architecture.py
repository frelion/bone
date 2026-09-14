"""Controlled Job architecture experiments using the existing Podman runner."""

from __future__ import annotations

import argparse
import hashlib
import json
import shlex
from datetime import datetime, timezone
from pathlib import Path

from benchmarks.behavior import Case, ensure_image, one_trial, validate_binary
from benchmarks.profile import build_profile, union_duration


ROOT = Path(__file__).resolve().parents[1]
MODES = ("single", "delegated", "auto")
FIXTURES = {
    "single": {
        "normalize.py": "def normalize(value):\n    raise NotImplementedError\n",
        "test_task.py": '''import unittest
from normalize import normalize

class Tests(unittest.TestCase):
    def test_normalize(self):
        for value, expected in [(" A  B ", "a b"), ("", ""), (" A\\tB\\n", "a b")]:
            self.assertEqual(normalize(value), expected)

if __name__ == "__main__":
    unittest.main()
''',
    },
    "independent": {
        "normalize.py": "def normalize(value):\n    raise NotImplementedError\n",
        "dedupe.py": "def dedupe(values):\n    raise NotImplementedError\n",
        "test_task.py": '''import unittest
from normalize import normalize
from dedupe import dedupe

class NormalizeTests(unittest.TestCase):
    def test_normalize(self):
        for value, expected in [(" A  B ", "a b"), ("", ""), (" A\\tB\\n", "a b")]:
            self.assertEqual(normalize(value), expected)

class DedupeTests(unittest.TestCase):
    def test_dedupe(self):
        for values, expected in [([3, 1, 3, 2, 1], [3, 1, 2]), ([], []), (["a", "a"], ["a"])]:
            self.assertEqual(dedupe(values), expected)

if __name__ == "__main__":
    unittest.main()
''',
    },
    "dependent": {
        "source.json": '{"records": [{"name": "Alpha", "amount": 3}, {"name": "Beta", "amount": 5}]}\n',
        "test_task.py": '''import json
import unittest
from pathlib import Path

class Tests(unittest.TestCase):
    def test_artifacts(self):
        records = json.loads(Path("source.json").read_text())["records"]
        normalized = json.loads(Path("normalized.json").read_text())
        self.assertEqual(normalized, [{"name": r["name"].lower(), "amount": r["amount"]} for r in records])
        expected = "\\n".join(f"{r['name']}:{r['amount']}" for r in normalized) + "\\n"
        self.assertEqual(Path("report.txt").read_text(), expected)

if __name__ == "__main__":
    unittest.main()
''',
    },
}
TASKS = {
    "single": "Implement normalize(value): lowercase and collapse all whitespace to single spaces, stripping ends.",
    "independent": "Implement normalize(value): lowercase and collapse whitespace to single spaces, stripping ends. Independently implement dedupe(values): return distinct hashable values preserving first appearance.",
    "dependent": "First transform source.json records into normalized.json, a JSON list with lowercase names and unchanged amounts in input order. Then read that generated normalized.json and produce report.txt with one name:amount line per record and a trailing newline. The report stage must consume the produced normalized.json.",
}


def make_case(workload: str, mode: str, timeout_seconds: int = 120) -> Case:
    files = FIXTURES[workload]
    setup = " && ".join(
        f"printf %s {shlex.quote(content)} > {shlex.quote(name)}"
        for name, content in files.items()
    )
    instructions = {
        "single": "Perform all work in your root Job. Do not delegate child Jobs.",
        "auto": "Choose your own Job decomposition, including whether delegation is useful.",
        "delegated": (
            "Use exactly two direct child Jobs and no grandchildren. Delegate both in one batch: one owns normalize.py, the other dedupe.py. Each child implements and verifies its own function. The parent integrates and verifies the combined result."
            if workload == "independent" else
            "Use exactly two direct child Jobs and no grandchildren. The first creates and verifies normalized.json. Wait for its completion and receive its evidence, then create the second to read normalized.json and create report.txt. The parent verifies the combined result."
        ),
    }
    # Keep the authoritative verifier outside the agent-controlled workspace.
    verifier = "python3 -c " + shlex.quote(files["test_task.py"])
    prompt = (
        "Work in /app. " + TASKS[workload] + "\n" + instructions[mode]
        + "\nInspect the supplied files. Do not modify test_task.py. Run python3 test_task.py before completing."
    )
    return Case(f"{workload}-{mode}", prompt, verifier, setup, timeout_seconds=timeout_seconds)


def analyze(entries: list[dict], workload: str, mode: str) -> dict:
    profile = build_profile(entries)
    roots = [j for j in profile["jobs"] if j["parent"] is None]
    children = [j for j in profile["jobs"] if j["parent"] is not None]
    root_refs = [j["job"] for j in roots]
    direct = all(j["parent"] in root_refs for j in children)
    ordered = sorted(children, key=lambda j: j["created_ms"])
    sequential = len(ordered) == 2 and ordered[0]["finished_ms"] is not None and ordered[1]["created_ms"] >= ordered[0]["finished_ms"]
    parent_calls = [c for c in profile["calls"] if c["kind"] == "work" and c["job"] in root_refs]
    creators = [
        next((c["call"] for c in reversed(parent_calls) if c["finished_ms"] <= j["created_ms"]), None)
        for j in ordered
    ]
    same_batch = len(creators) == 2 and creators[0] is not None and creators[0] == creators[1]
    structure_ok = len(roots) == 1 and (
        (mode == "single" and not children)
        or mode == "auto"
        or (mode == "delegated" and len(children) == 2 and direct and (sequential if workload == "dependent" else same_batch))
    )
    child_refs = [j["job"] for j in children]
    intervals = [(c["started_ms"], c["finished_ms"]) for c in profile["calls"] if c["kind"] == "work" and c["job"] in child_refs]
    profile.update({
        "structure_ok": structure_ok,
        "child_jobs": len(children),
        "child_model_overlap_ms": sum(end - start for start, end in intervals) - union_duration(intervals),
        "parent_work_calls": sum(c["kind"] == "work" and c["job"] in root_refs for c in profile["calls"]),
        "telemetry_limits": "Call durations include provider/network time. Tokens, retries, and individual WorkStep decisions are not exposed by public history. Duplicate reads only cover the read tool; bash reads are not detected.",
    })
    return profile


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--model", default="gpt-5.6-luna")
    parser.add_argument("--attempts", type=int, default=2)
    parser.add_argument("--workload", action="append", choices=list(FIXTURES))
    parser.add_argument("--mode", action="append", choices=MODES)
    parser.add_argument("--case", action="append", choices=[f"{w}-{m}" for w in FIXTURES for m in (("single",) if w == "single" else MODES)])
    parser.add_argument("--timeout-seconds", type=int, default=120)
    parser.add_argument("--results-dir", type=Path, default=ROOT / "benchmarks/results/architecture")
    args = parser.parse_args()
    if args.attempts < 1:
        parser.error("attempts must be positive")
    if args.timeout_seconds < 1:
        parser.error("timeout-seconds must be positive")
    selected = [
        (workload, mode)
        for workload in FIXTURES
        for mode in (("single",) if workload == "single" else MODES)
        if (not args.workload or workload in args.workload)
        and (not args.mode or mode in args.mode)
        and (not args.case or f"{workload}-{mode}" in args.case)
    ]
    if not selected:
        parser.error("the supplied filters select no conditions")
    validate_binary(args.binary)
    ensure_image()
    destination = args.results_dir / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    destination.mkdir(parents=True)
    metadata = {
        "model": args.model,
        "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
        "attempts": args.attempts,
        "timeout_seconds": args.timeout_seconds,
        "fixtures_sha256": hashlib.sha256(json.dumps(FIXTURES, sort_keys=True).encode()).hexdigest(),
    }
    (destination / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(destination, flush=True)
    results = []
    for attempt in range(1, args.attempts + 1):
        # Reverse mode order on alternate rounds to reduce a fixed order effect.
        for workload in FIXTURES:
            modes = ("single",) if workload == "single" else MODES[::1 if attempt % 2 else -1]
            for mode in modes:
                if (workload, mode) not in selected:
                    continue
                case = make_case(workload, mode, args.timeout_seconds)
                trial = destination / f"{case.name}-{attempt}"
                result = one_trial(args.binary, args.model, case, trial)
                result.update(workload=workload, mode=mode, attempt=attempt)
                native_result = trial / "bone-result.json"
                if not result["passed"] and native_result.is_file():
                    result["failure_summary"] = json.loads(native_result.read_text()).get("summary")
                trajectory = trial / "bone-trajectory.json"
                if trajectory.is_file():
                    try:
                        profile = analyze(json.loads(trajectory.read_text()), workload, mode)
                    except (ValueError, KeyError, TypeError) as error:
                        result["profile_error"] = f"{type(error).__name__}: {error}"
                    else:
                        (trial / "efficiency-profile.json").write_text(json.dumps(profile, indent=2) + "\n")
                        for key in ("wall_duration_ms", "structure_ok", "child_jobs", "child_model_overlap_ms", "parent_work_calls", "call_counts"):
                            result[key] = profile[key]
                results.append(result)
                (destination / "summary.json").write_text(json.dumps({**metadata, "trials": results}, indent=2) + "\n")
                print(json.dumps(result), flush=True)
    return 0 if all(r["passed"] and r.get("structure_ok") for r in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
