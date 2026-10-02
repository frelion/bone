"""Guard PTY evidence against old ANSI text and broken Unicode cell replay."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('tui_pty', Path(__file__).with_name('tui_pty.py'))
pty_acceptance = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pty_acceptance)


class ScreenEvidenceTest(unittest.TestCase):
    def replay(self, output):
        fixture = pty_acceptance.Fixture.__new__(pty_acceptance.Fixture)
        fixture.rows, fixture.cols = 3, 24
        fixture.output = bytearray(output.encode())
        return fixture

    def test_old_preview_cannot_pass_after_terminal_clears_it(self):
        fixture = self.replay('\x1b[1;1HOLD_PREVIEW\x1b[2J\x1b[1;1HNEW_RESULT')
        self.assertFalse(fixture.visible('OLD_PREVIEW'))
        self.assertTrue(fixture.visible('NEW_RESULT'))

    def test_combining_and_zwj_graphemes_preserve_real_cell_positions(self):
        fixture = self.replay('\x1b[1;1H中e\u0301👩\u200d💻尾\x1b[1;8HR')
        self.assertTrue(fixture.screen().splitlines()[0].startswith('中e\u0301👩\u200d💻尾R'))


if __name__ == '__main__':
    unittest.main()
