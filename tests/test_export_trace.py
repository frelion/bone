import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest


class ExportTracePrivacyTest(unittest.TestCase):
    def test_shareable_export_omits_private_fields_and_preserves_visible_actions(self):
        root = Path(__file__).resolve().parents[1]
        with tempfile.TemporaryDirectory() as directory:
            database = Path(directory) / 'sessions.sqlite3'
            output = Path(directory) / 'trace.html'
            state = {'id': 's', 'workspace': '/project', 'jobs': {}, 'access_token': 'fixture-private-token'}
            event = {'id': 'e', 'session_id': 's', 'kind': 'model_message', 'timestamp': 1,
                     'data': {'response': {'choice': [
                         {'type': 'text', 'text': 'visible answer'},
                         {'type': 'reasoning', 'reasoning': [{'type': 'encrypted', 'data': 'fixture-private-reasoning'}]},
                         {'type': 'toolcall', 'id': 'call', 'function': {'name': 'read_file', 'arguments': {'path': 'Cargo.toml'}}}],
                         'headers': {'Authorization': 'fixture-private-auth'}}}}
            with sqlite3.connect(database) as connection:
                connection.execute('CREATE TABLE sessions (id TEXT, snapshot TEXT)')
                connection.execute('CREATE TABLE events (sequence INTEGER, session_id TEXT, payload TEXT)')
                connection.execute('INSERT INTO sessions VALUES (?, ?)', ('s', json.dumps(state)))
                connection.execute('INSERT INTO events VALUES (?, ?, ?)', (1, 's', json.dumps(event)))
            original = database.read_bytes()
            subprocess.run([sys.executable, str(root / 'scripts/export_trace.py'), '--database', str(database),
                            '--session', 's', '--output', str(output)], check=True, capture_output=True)
            html = output.read_text()
            for secret in ('fixture-private-token', 'fixture-private-reasoning', 'fixture-private-auth'):
                self.assertNotIn(secret, html)
            self.assertIn('visible answer', html)
            self.assertIn('read_file', html)
            self.assertIn('Cargo.toml', html)
            self.assertIn('source_events_sha256', html)
            self.assertIn('exported_events_sha256', html)
            self.assertEqual(database.read_bytes(), original)


if __name__ == '__main__':
    unittest.main()
