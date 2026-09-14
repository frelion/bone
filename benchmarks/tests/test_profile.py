import unittest

from benchmarks.profile import build_profile


RUNTIME = "00000000-0000-0000-0000-000000000001"


def entry(timestamp, event):
    return {"occurred_at": timestamp, "event": event}


class ProfileTests(unittest.TestCase):
    def test_profiles_overlapping_calls_jobs_verification_and_duplicate_reads(self):
        root = {"runtime": RUNTIME, "id": 1}
        child = {"runtime": RUNTIME, "id": 2}
        work = {"runtime": RUNTIME, "id": 4}
        tool = {"runtime": RUNTIME, "id": 5}
        trajectory = [
            entry(1_000, {"InputSubmitted": {}}),
            entry(1_010, {"JobCreated": {"job": root, "owner": "User", "goal": "root", "scope": "", "done_when": ""}}),
            entry(1_020, {"JobCreated": {"job": child, "owner": {"Job": root}, "goal": "child", "scope": "", "done_when": ""}}),
            entry(1_100, {"CallStarted": {"call": work, "job": root, "kind": "Work"}}),
            entry(1_120, {"CallStarted": {"call": tool, "job": child, "kind": {"Tool": {"name": "bash"}}}}),
            entry(1_200, {"CallFinished": {"call": work, "error": None, "external_effect": "None"}}),
            entry(1_250, {"ToolFinished": {"call": tool, "job": child, "tool": "bash", "outcome": {"result": {"Ok": {"exit_code": 0}}}}}),
            entry(1_251, {"ToolFinished": {"job": root, "tool": "read", "outcome": {"result": {"Ok": {"path": "a.py"}}}}}),
            entry(1_252, {"ToolFinished": {"job": child, "tool": "read", "outcome": {"result": {"Ok": {"path": "a.py"}}}}}),
            entry(1_260, {"ToolFinished": {"job": root, "tool": "bash", "outcome": {"result": {"Ok": {"exit_code": 0, "stdout": "Ran 2 tests in 0.1s\n\nOK\n", "stderr": ""}}}}}),
            entry(1_280, {"JobFinished": {"job": child, "outcome": "Completed"}}),
            entry(1_300, {"JobFinished": {"job": root, "outcome": "Completed"}}),
        ]

        profile = build_profile(trajectory)

        self.assertEqual(profile["wall_duration_ms"], 300)
        self.assertEqual(profile["call_active_union_ms"], 150)
        self.assertEqual(profile["outside_call_ms"], 150)
        self.assertEqual(profile["call_counts"], {"tool:bash": 1, "work": 1})
        self.assertEqual(profile["post_verification_ms"], 40)
        self.assertEqual(
            profile["duplicate_reads_across_jobs"],
            [{"path": "a.py", "jobs": [1, 2]}],
        )
        self.assertEqual(profile["jobs"][1]["parent"], root)


if __name__ == "__main__":
    unittest.main()
