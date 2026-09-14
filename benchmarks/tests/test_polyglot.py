import json
import tempfile
import unittest
from pathlib import Path

from benchmarks.polyglot import prepare_task


class PrepareTaskTests(unittest.TestCase):
    def test_separates_tests_from_the_agent_workspace(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            task = root / "python" / "exercises" / "practice" / "sample"
            (task / ".meta").mkdir(parents=True)
            (task / ".docs").mkdir()
            (task / ".meta" / "config.json").write_text(
                json.dumps({"files": {"solution": ["answer.py"], "test": ["answer_test.py"], "example": ["example.py"]}})
            )
            (task / ".docs" / "instructions.md").write_text("Solve it.")
            (task / "answer.py").write_text("pass\n")
            (task / "answer_test.py").write_text("# tests\n")
            (task / "example.py").write_text("# answer\n")
            trial = root / "trial"
            trial.mkdir()

            workspace, verifier, prompt, digests = prepare_task(root, "sample", trial)

            self.assertTrue((workspace / "answer.py").is_file())
            self.assertFalse((workspace / "answer_test.py").exists())
            self.assertFalse((workspace / "example.py").exists())
            self.assertTrue((verifier / "answer_test.py").is_file())
            self.assertEqual(set(digests), {"answer_test.py"})
            self.assertIn("answer.py", prompt)
            self.assertIn("/tests", prompt)


if __name__ == "__main__":
    unittest.main()
