import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from benchmarks.architecture import FIXTURES, analyze, make_case
from benchmarks.behavior import one_trial


class ArchitectureTests(unittest.TestCase):
    def test_result_survives_interrupted_container_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            trial = Path(directory) / "trial"

            def fake_run(command, **kwargs):
                if "--provider" in command:
                    (trial / "bone-result.json").write_text(json.dumps({"status": "completed", "exit_code": 0}))
                if command[:2] == ["podman", "rm"]:
                    raise KeyboardInterrupt
                return subprocess.CompletedProcess(command, 0, stdout="")

            with patch("benchmarks.behavior.run", side_effect=fake_run), patch("benchmarks.behavior.auth_root", return_value=Path(directory)):
                with self.assertRaises(KeyboardInterrupt):
                    one_trial(Path(directory) / "bone", "test", make_case("single", "single"), trial)
            self.assertTrue(json.loads((trial / "trial-result.json").read_text())["passed"])

    def test_verifiers_reject_stubs_and_accept_correct_implementations(self):
        solutions = {
            "normalize.py": "def normalize(value):\n    return ' '.join(value.lower().split())\n",
            "dedupe.py": "def dedupe(values):\n    return list(dict.fromkeys(values))\n",
            "normalized.json": '[{"name":"alpha","amount":3},{"name":"beta","amount":5}]',
            "report.txt": "alpha:3\nbeta:5\n",
        }
        for workload in FIXTURES:
            with self.subTest(workload=workload), tempfile.TemporaryDirectory() as directory:
                case = make_case(workload, "single")
                subprocess.run(["bash", "-c", case.setup], cwd=directory, check=True)
                failed = subprocess.run(["bash", "-c", case.verifier], cwd=directory, capture_output=True)
                self.assertNotEqual(failed.returncode, 0)
                for name, content in solutions.items():
                    Path(directory, name).write_text(content)
                # Changing the visible tests cannot weaken the independent verifier.
                Path(directory, "test_task.py").write_text("pass\n")
                passed = subprocess.run(["bash", "-c", case.verifier], cwd=directory, capture_output=True)
                self.assertEqual(passed.returncode, 0, passed.stderr)
                Path(directory, "normalize.py").write_text("def normalize(value):\n    return 'broken'\n")
                Path(directory, "report.txt").write_text("broken\n")
                failed = subprocess.run(["bash", "-c", case.verifier], cwd=directory, capture_output=True)
                self.assertNotEqual(failed.returncode, 0)

    def test_dependent_structure_rejects_early_second_child(self):
        root, first, second = [{"runtime": "r", "id": i} for i in (1, 2, 3)]

        def created(at, job, parent):
            return {"occurred_at": at, "event": {"JobCreated": {"job": job, "owner": {"Job": parent} if parent else "User", "goal": "test"}}}

        entries = [
            created(0, root, None), created(1, first, root), created(2, second, root),
            {"occurred_at": 3, "event": {"JobFinished": {"job": first, "outcome": "Completed"}}},
        ]
        self.assertFalse(analyze(entries, "dependent", "delegated")["structure_ok"])
        entries[2], entries[3] = entries[3], created(4, second, root)
        self.assertTrue(analyze(entries, "dependent", "delegated")["structure_ok"])
        self.assertFalse(analyze(entries, "independent", "single")["structure_ok"])

    def test_independent_children_must_come_from_one_parent_turn(self):
        root = {"runtime": "r", "id": 1}

        def event(at, name, data):
            return {"occurred_at": at, "event": {name: data}}

        def created(at, number):
            return event(at, "JobCreated", {"job": {"runtime": "r", "id": number}, "owner": "User" if number == 1 else {"Job": root}, "goal": "test"})

        def turn(start, end, number):
            call = {"runtime": "r", "id": number}
            return [event(start, "CallStarted", {"call": call, "job": root, "kind": "Work"}), event(end, "CallFinished", {"call": call, "error": None})]

        entries = [created(0, 1), *turn(1, 2, 1), created(3, 2), created(4, 3)]
        self.assertTrue(analyze(entries, "independent", "delegated")["structure_ok"])
        entries = [created(0, 1), *turn(1, 2, 1), created(3, 2), *turn(4, 5, 2), created(6, 3)]
        self.assertFalse(analyze(entries, "independent", "delegated")["structure_ok"])


if __name__ == "__main__":
    unittest.main()
