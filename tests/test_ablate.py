"""Offline checks of experiment accounting and independent verifiers."""

import importlib.util
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("ablate", ROOT / "scripts" / "ablate.py")
ablate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ablate)


class AblationAccounting(unittest.TestCase):
    def test_all_six_scenarios_have_three_complete_alternating_pairs(self):
        cases = ablate.load_cases(ablate.DEFAULT_FIXTURES)
        trials = ablate.schedule(cases, 3)
        self.assertEqual(len(cases), 6)
        self.assertEqual(len(trials), 108)
        for case in cases:
            for pair, variants in ablate.PAIR_VARIANTS.items():
                selected = [trial["variant"] for trial in trials
                            if trial["case_id"] == case["id"] and trial["pair"] == pair]
                self.assertEqual(selected, [*variants, *reversed(variants), *variants])

    def test_unknown_usage_does_not_become_free_or_zero(self):
        known = ablate.extract_metrics({"metrics": {"model_calls": 2, "input_tokens": 8,
                                                     "output_tokens": 3, "total_tokens": 11,
                                                     "cost": None}})
        missing = ablate.extract_metrics(None)
        summed = ablate.sum_metrics([{"metrics": known}, {"metrics": missing}])
        self.assertTrue(all(value is None for value in summed.values()))
        self.assertIsNone(known["cost_usd"])
        self.assertEqual(known["total_tokens"], 11)

    def test_limited_live_schedule_is_six_baselines_and_eighteen_comparisons(self):
        cases = ablate.load_cases(ablate.DEFAULT_FIXTURES)
        self.assertEqual(len(ablate.schedule(cases, 3, baseline_only=True)), 6)
        independent = [case for case in cases if case["id"] == "independent_tasks"]
        long_context = [case for case in cases if case["id"] == "long_context"]
        trials = (ablate.schedule(independent, 3, ["jobs", "parallel"])
                  + ablate.schedule(long_context, 3, ["compaction"]))
        self.assertEqual(len(trials), 18)
        self.assertEqual({trial["case_id"] for trial in trials}, {"independent_tasks", "long_context"})
        first = ablate.schedule(long_context, 1, ["compaction"])
        remaining = ablate.schedule(long_context, 2, ["compaction"], start_repetition=2)
        self.assertEqual(first + remaining, ablate.schedule(long_context, 3, ["compaction"]))

    def test_summary_includes_failures_and_missing_metrics(self):
        records = [
            {"pair": "jobs", "variant": "multi_job", "passed": True,
             "wall_seconds": 1.0, "metrics": {"model_calls": 2, "cost_usd": None}},
            {"pair": "jobs", "variant": "multi_job", "passed": False,
             "wall_seconds": 3.0, "metrics": {"model_calls": None, "cost_usd": None}},
        ]
        group = ablate.summary(records)["groups"]["jobs/multi_job"]
        self.assertEqual((group["trials"], group["passed"], group["failed"]), (2, 1, 1))
        self.assertEqual(group["wall_seconds"], 4)
        self.assertIsNone(group["metrics"]["model_calls"])

    def test_broken_repair_fails_and_valid_repair_passes_external_verifier(self):
        case = ablate.load_cases(ablate.DEFAULT_FIXTURES)[0]
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory) / "workspace"
            ablate.prepare_workspace(ablate.DEFAULT_FIXTURES, case, workspace)
            self.assertFalse(ablate.verify_workspace(case, workspace, {})["passed"])
            (workspace / "totals.py").write_text(
                "def total(values):\n    return sum(value for value in values if value is not None)\n",
                encoding="utf-8",
            )
            self.assertTrue(ablate.verify_workspace(case, workspace, {})["passed"])


if __name__ == "__main__":
    unittest.main()
