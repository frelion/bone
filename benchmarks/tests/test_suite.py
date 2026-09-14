import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from benchmarks import suite as suite_module


class SuiteManifestTests(unittest.TestCase):
    def test_all_manifests_are_valid_and_have_expected_sizes(self) -> None:
        expected = {
            "terminal-bench-2-smoke": (3, 1),
            "terminal-bench-2-regression": (20, 3),
            "terminal-bench-2-full": (89, 1),
        }
        self.assertEqual(set(suite_module.all_suite_names()), set(expected))
        for name, (task_count, attempts) in expected.items():
            manifest = suite_module.load_suite(name)
            self.assertEqual(manifest["expected_task_count"], task_count)
            self.assertEqual(manifest["attempts"], attempts)

    def test_command_pins_every_reproducibility_boundary_without_a_secret(self) -> None:
        manifest = suite_module.load_suite("terminal-bench-2-smoke")
        with patch.dict(os.environ, {"OPENAI_API_KEY": "super-secret"}):
            command = suite_module.build_harbor_command(manifest)
        rendered = " ".join(command)
        self.assertIn("harbor==0.23.0", rendered)
        self.assertIn("terminal-bench@2.0", command)
        self.assertIn("openai/gpt-5.6-luna", command)
        self.assertIn("version=0.3.2", command)
        self.assertEqual(command.count("--include-task-name"), 3)
        self.assertNotIn("super-secret", rendered)
        self.assertNotIn("--agent-env", command)

    def test_full_suite_requests_all_89_tasks(self) -> None:
        command = suite_module.build_harbor_command(
            suite_module.load_suite("terminal-bench-2-full")
        )
        index = command.index("--n-tasks")
        self.assertEqual(command[index + 1], "89")
        self.assertNotIn("--include-task-name", command)

    def test_duplicate_task_is_rejected(self) -> None:
        manifest = suite_module.load_suite("terminal-bench-2-smoke")
        manifest["tasks"] = ["same", "same", "third"]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "temporary.json"
            manifest["name"] = "temporary"
            path.write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaisesRegex(suite_module.SuiteError, "duplicates"):
                suite_module.load_suite(path)


class SummaryTests(unittest.TestCase):
    def test_summary_keeps_failures_errors_metadata_and_lock(self) -> None:
        result = {
            "id": "job-1",
            "n_total_trials": 4,
            "started_at": "2026-09-14T00:00:00Z",
            "finished_at": "2026-09-14T00:03:00Z",
            "stats": {
                "n_input_tokens": 100,
                "n_cache_tokens": 20,
                "n_output_tokens": 30,
                "cost_usd": 0.12,
            },
            "trial_results": [
                self._trial("task-a", "attempt-1", 1, "completed"),
                self._trial("task-a", "attempt-2", 0, "failed"),
                self._trial("task-b", "attempt-1", None, None, "DockerError"),
            ],
        }
        with tempfile.TemporaryDirectory() as directory:
            job_dir = Path(directory)
            (job_dir / "result.json").write_text(json.dumps(result), encoding="utf-8")
            (job_dir / "lock.json").write_text(
                json.dumps({"harbor_version": "0.23.0", "tasks": ["sha256:abc"]}),
                encoding="utf-8",
            )
            manifest = suite_module.load_suite("terminal-bench-2-smoke")
            summary = suite_module.summarize_job(job_dir, manifest)
            metrics = summary["metrics"]
            self.assertEqual(metrics["observed_trials"], 3)
            self.assertEqual(metrics["scored_trials"], 2)
            self.assertEqual(metrics["passed_trials"], 1)
            self.assertEqual(metrics["scheduled_trials"], 4)
            self.assertAlmostEqual(metrics["all_scheduled_pass_rate"], 1 / 4)
            self.assertAlmostEqual(metrics["scored_pass_rate"], 1 / 2)
            self.assertEqual(metrics["observed_pass_at_k"], 1.0)
            self.assertEqual(metrics["categories"]["infrastructure_error"], 1)
            self.assertEqual(metrics["bone_statuses"], {"completed": 1, "failed": 1})
            self.assertEqual(summary["trials"][0]["duration_seconds"], 60.0)
            self.assertEqual(summary["harbor_lock"]["harbor_version"], "0.23.0")
            json_path, markdown_path = suite_module.write_summary(job_dir, summary)
            self.assertTrue(json_path.is_file())
            self.assertIn(
                "Infrastructure errors remain failures", markdown_path.read_text()
            )

    @staticmethod
    def _trial(
        task: str,
        attempt: str,
        reward: int | None,
        bone_status: str | None,
        exception: str | None = None,
    ) -> dict[str, object]:
        verifier = None if reward is None else {"rewards": {"reward": reward}}
        metadata = (
            {}
            if bone_status is None
            else {"bone": {"status": bone_status, "exit_code": 0}}
        )
        exception_info = None if exception is None else {"exception_type": exception}
        return {
            "task_name": task,
            "trial_name": attempt,
            "task_checksum": f"sha256:{task}",
            "started_at": "2026-09-14T00:00:00Z",
            "finished_at": "2026-09-14T00:01:00Z",
            "verifier_result": verifier,
            "agent_result": {"metadata": metadata},
            "exception_info": exception_info,
        }


if __name__ == "__main__":
    unittest.main()
