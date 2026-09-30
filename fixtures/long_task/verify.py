"""Independent staged acceptance. This file remains outside the agent workspace."""
import argparse
import hashlib
import importlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


PHASE = 12
WORKSPACE = None


def event(identity="e-1", account="A", cents=125, timestamp="2026-10-01T12:00:00Z"):
    return {"id": identity, "account": account, "amount_cents": cents, "timestamp": timestamp}


class LedgerAcceptance(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="bone-ledger-check-")
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "nested" / "events.jsonl"
        self.storage = importlib.import_module("ledger.storage")
        self.service = importlib.import_module("ledger.service")

    def require(self, phase):
        if PHASE < phase:
            self.skipTest("future phase")

    def test_missing_path_and_normalized_exact_signed_cents(self):
        self.require(2)
        self.assertEqual(self.storage.read_events(self.path), [])
        self.assertTrue(self.service.add(self.path, event(" e-1 ", " 账户 ", -125)))
        self.assertEqual(self.storage.read_events(self.path), [event("e-1", "账户", -125)])
        for invalid in (event(cents=True), event(cents=1.5), event(account="  "), event(identity=""),
                        {key: value for key, value in event().items() if key != "timestamp"},
                        dict(event(), unexpected=True)):
            before = self.path.read_bytes()
            with self.assertRaises(ValueError):
                self.service.add(self.path, invalid)
            self.assertEqual(self.path.read_bytes(), before)

    def test_storage_direct_apis_enforce_the_same_schema(self):
        self.require(2)
        for invalid in (event(cents=True), dict(event(), extra="no"), {"id": "missing"}):
            with self.assertRaises(ValueError):
                self.storage.write_events(self.path, [event(), invalid])
            self.assertFalse(self.path.exists())
        self.storage.write_events(self.path, [event(" e-1 ", " A ", -5)])
        self.assertEqual(self.storage.read_events(self.path), [event("e-1", "A", -5)])
        before = self.path.read_bytes()
        with self.assertRaises(ValueError):
            self.storage.write_events(self.path, [event(cents=False)])
        self.assertEqual(self.path.read_bytes(), before)
        for invalid in (event(cents=True), dict(event(), extra="no"), {"id": "missing"}):
            self.path.write_text(json.dumps(invalid) + "\n", encoding="utf-8")
            with self.assertRaises(ValueError):
                self.storage.read_events(self.path)

    def test_duplicate_and_conflict_preserve_bytes(self):
        self.require(3)
        item = event()
        self.service.add(self.path, item)
        before = self.path.read_bytes()
        self.assertFalse(self.service.add(self.path, dict(item)))
        with self.assertRaises(ValueError):
            self.service.add(self.path, event(cents=126))
        self.assertEqual(self.path.read_bytes(), before)

    def test_batch_validation_is_all_or_nothing(self):
        self.require(4)
        with self.assertRaises(ValueError):
            self.service.add_many(self.path, [event(), event("bad", cents=True)])
        self.assertFalse(self.path.exists())
        self.assertEqual(self.service.add_many(self.path, []), 0)
        self.assertEqual(self.service.add_many(self.path, [event(), event(), event("e-2", cents=-100)]), 2)
        before = self.path.read_bytes()
        with self.assertRaises(ValueError):
            self.service.add_many(self.path, [event("e-3"), event(cents=999)])
        self.assertEqual(self.path.read_bytes(), before)

    def test_real_utc_timestamp_and_corrupt_storage(self):
        self.require(5)
        for invalid in ("yesterday", "2026-02-30T00:00:00Z", "2026-10-01T12:00:00+00:00", "2026-10-01", True):
            with self.assertRaises(ValueError):
                self.service.add(self.path, event(timestamp=invalid))
            self.assertFalse(self.path.exists())
        self.assertTrue(self.service.add(self.path, event(timestamp="2026-10-01T12:00:00.125Z")))
        before = self.path.read_bytes() + b'{"amount_cents":false}\n'
        self.path.write_bytes(before)
        with self.assertRaises(ValueError):
            self.storage.read_events(self.path)
        with self.assertRaises(ValueError):
            self.service.add(self.path, event("e-2"))
        self.assertEqual(self.path.read_bytes(), before)

    def test_csv_stability_quoting_and_read_only_input(self):
        self.require(8)
        exports = importlib.import_module("ledger.exports")
        self.service.add_many(self.path, [event(account="B", cents=-125), event("e-2", "A, Inc", 300), event("e-3", "B", 5), event("e-4", 'Z "Quote"', 7)])
        before = hashlib.sha256(self.path.read_bytes()).hexdigest()
        target = Path(self.temp.name) / "out" / "report.csv"
        self.assertEqual(exports.write_csv(self.path, target), 3)
        self.assertEqual(target.read_bytes(), b'account,balance_cents\n"A, Inc",300\nB,-120\n"Z ""Quote""",7\n')
        self.assertEqual(hashlib.sha256(self.path.read_bytes()).hexdigest(), before)
        self.assertEqual(exports.write_csv(Path(self.temp.name) / "absent", target), 0)
        self.assertEqual(target.read_bytes(), b"account,balance_cents\n")

    def test_balance_normalization_filter_and_no_mutation(self):
        self.require(9)
        self.assertEqual(self.service.balance(self.path), {})
        self.service.add_many(self.path, [event(account="B", cents=-5), event("e-2", "A", 2), event("e-3", "B", 1)])
        before = self.path.read_bytes()
        result = self.service.balance(self.path)
        self.assertEqual(list(result), ["A", "B"])
        self.assertEqual(result, {"A": 2, "B": -4})
        self.assertEqual(self.service.balance(self.path, " B "), {"B": -4})
        self.assertEqual(self.service.balance(self.path, "missing"), {})
        self.assertEqual(self.service.balance(self.path, ""), {})
        self.assertEqual(self.service.balance(self.path, "   "), {})
        self.assertEqual(self.path.read_bytes(), before)

    def cli(self, *arguments):
        environment = os.environ.copy()
        environment["PYTHONPATH"] = str(WORKSPACE)
        return subprocess.run([sys.executable, "-B", "-m", "ledger.cli", "--path", str(self.path), *arguments],
                              cwd=WORKSPACE, env=environment, capture_output=True, text=True, timeout=10)

    def test_cli_dry_run_survives_deferred_requirement(self):
        self.require(10)
        output = self.cli("add", "--event", json.dumps(event()), "--dry-run")
        self.assertEqual(output.returncode, 0, output.stderr)
        self.assertEqual(json.loads(output.stdout), {"added": True, "dry_run": True})
        self.assertFalse(self.path.exists())
        output = self.cli("add", "--event", json.dumps(event()))
        self.assertEqual(output.returncode, 0, output.stderr)
        self.assertEqual(json.loads(output.stdout), {"added": True})
        before = self.path.read_bytes()
        output = self.cli("add", "--event", json.dumps(event()), "--dry-run")
        self.assertEqual(json.loads(output.stdout), {"added": False, "dry_run": True})
        self.assertEqual(self.path.read_bytes(), before)
        self.assertEqual(json.loads(self.cli("balance").stdout), {"A": 125})
        destination = Path(self.temp.name) / "cli.csv"
        self.assertEqual(json.loads(self.cli("export-csv", "--output", str(destination)).stdout), {"rows": 1})

    def test_cli_errors_are_explicit_and_non_mutating(self):
        self.require(11)
        self.service.add(self.path, event())
        before = self.path.read_bytes()
        for raw in ("{invalid", json.dumps(event(cents=True)), json.dumps(event(cents=999))):
            output = self.cli("add", "--event", raw)
            self.assertEqual(output.returncode, 2)
            self.assertEqual(output.stdout, "")
            self.assertTrue(output.stderr.strip())
            self.assertNotIn("Traceback", output.stderr)
            self.assertEqual(self.path.read_bytes(), before)


def main():
    global PHASE, WORKSPACE
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--phase", type=int, default=12)
    args = parser.parse_args()
    PHASE, WORKSPACE = args.phase, args.workspace.resolve()
    sys.path.insert(0, str(WORKSPACE))
    stream = io.StringIO()
    result = unittest.TextTestRunner(stream=stream, verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(LedgerAcceptance))
    print(json.dumps({"passed": result.wasSuccessful(), "run": result.testsRun, "skipped": len(result.skipped),
                      "failures": [{"test": str(test), "trace": trace} for test, trace in result.failures],
                      "errors": [{"test": str(test), "trace": trace} for test, trace in result.errors]}))
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    raise SystemExit(main())
