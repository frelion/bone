#!/usr/bin/env python3
"""Real PTY acceptance: python3 tests/tui_pty.py [--binary target/debug/bone].
Only synthetic credentials and a scripted loopback Responses endpoint are used.
"""
import argparse
import html
import hashlib
import fcntl
import json
import copy
import uuid
import os
from pathlib import Path
import pty
import re
import select
import signal
import unicodedata
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[1]


def tool(name, arguments):
    return {"type": "function_call", "call_id": name + "-fixture", "name": name, "arguments": arguments}


class Fixture:
    def __init__(self, binary, turns, size=(30, 110)):
        self.directory = tempfile.TemporaryDirectory(prefix="bone-tui-pty-")
        self.root = Path(self.directory.name)
        self.data = self.root / "data"
        self.workspace = self.root / "workspace"
        self.data.mkdir()
        self.workspace.mkdir()
        self.requests = self.root / "requests.jsonl"
        script = self.root / "turns.json"
        script.write_text(json.dumps({"turns": turns}))
        self.server = subprocess.Popen([sys.executable, "-B", str(ROOT / "tests/scripted_responses.py"),
            "--script", str(script), "--requests", str(self.requests)], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        ready, _, _ = select.select([self.server.stdout], [], [], 5)
        assert ready, "fixture endpoint did not start"
        port = int(self.server.stdout.readline())
        (self.data / "config.toml").write_text(f'''default_profile = "fixture"
[profiles.fixture]
credential_env = "BONE_TUI_FIXTURE_KEY"
reuse_codex_login = false
[profiles.fixture.model]
model = "fixture"
[profiles.fixture.model.config.openai]
api_key = ""
base_url = "http://127.0.0.1:{port}/v1"
dialect = "openai"
route = "Responses"
auth = "Bearer"
''')
        self.master, self.slave = pty.openpty()
        self.original = termios.tcgetattr(self.slave)
        self.rows, self.cols = size
        self.resize(*size)
        env = {k: v for k, v in os.environ.items() if k not in ("BONE_MODEL", "CHATGPT_ACCESS_TOKEN", "OPENAI_API_KEY")}
        env.update(TERM="xterm-256color", BONE_TUI_FIXTURE_KEY="synthetic-local-only")
        editor = self.root / "fixture-editor"
        editor.write_text('#!/bin/sh\nprintf "%s" "EDITOR_REFERENCE_ONLY" > "$1"\n')
        editor.chmod(0o700)
        env["EDITOR"] = str(editor)
        env["VISUAL"] = str(editor)
        self.clipboard = self.root / "clipboard.txt"
        clipboard = self.root / "pbcopy"
        clipboard.write_text('#!/bin/sh\ncat > "$BONE_TUI_CLIPBOARD_FILE"\n')
        clipboard.chmod(0o700)
        env["BONE_TUI_CLIPBOARD_FILE"] = str(self.clipboard)
        env["PATH"] = str(self.root) + os.pathsep + env.get("PATH", "")
        self.env = env
        self.binary = binary
        self.binary_sha256 = hashlib.sha256(binary.read_bytes()).hexdigest()
        self.proc = subprocess.Popen([str(binary), "--data-dir", str(self.data), "--profile", "fixture", "tui",
            "--workspace", str(self.workspace), "--max-parallel", "1", "--max-calls", "16"],
            stdin=self.slave, stdout=self.slave, stderr=self.slave, env=env, start_new_session=True)
        self.output = bytearray()
        self.answered_queries = 0
        self.frames = []
        self.terminal_cursor = (0, 0)

    def resize(self, rows, cols):
        self.rows, self.cols = rows, cols
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        process = getattr(self, 'proc', None)
        if process is not None and process.poll() is None:
            process.send_signal(signal.SIGWINCH)

    def pump(self, seconds=.1):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            ready, _, _ = select.select([self.master], [], [], min(.05, max(0, deadline-time.monotonic())))
            if ready:
                try:
                    self.output.extend(os.read(self.master, 65536))
                    queries = self.output.count(b'\x1b[6n')
                    for _ in range(queries - self.answered_queries):
                        # A real terminal answers DSR; Terminal::reopen needs it after EDITOR.
                        os.write(self.master, b'\x1b[1;1R')
                    self.answered_queries = queries
                except OSError:
                    break

    def wait(self, predicate, label, timeout=15):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.pump()
            if predicate():
                return
            if self.proc.poll() is not None:
                raise AssertionError(f"{label}: process exited {self.proc.returncode}; tail={bytes(self.output[-1500:])!r}")
        events = self.events()
        recent = [{'kind': event.get('kind'), 'id': event.get('id')} for event in events[-6:]]
        raise AssertionError(f"timeout: {label}; event_count={len(events)}; recent={recent}; tail={bytes(self.output[-800:])!r}")

    def send(self, value):
        os.write(self.master, value.encode() if isinstance(value, str) else value)

    def visible(self, text):
        # Ratatui diff output uses cursor moves instead of literal spaces.
        return re.sub(r"\s+", "", text) in re.sub(r"\s+", "", self.screen())

    def screen(self):
        """Replay the VT operations emitted by ratatui, including cursor diff updates."""
        grid = [[" " for _ in range(self.cols)] for _ in range(self.rows)]
        row = col = 0
        def erase_span(y, start, end):
            if not 0 <= y < self.rows:
                return
            for x in range(max(0, start), min(end, self.cols)):
                # Erasing either half of a wide glyph removes the glyph itself.
                if grid[y][x] == "" and x > 0:
                    grid[y][x - 1] = " "
                if x + 1 < self.cols and grid[y][x + 1] == "":
                    grid[y][x + 1] = " "
                grid[y][x] = " "
        source = self.output.decode("utf-8", errors="replace")
        source = re.sub(r"\x1b\][^\x07]*(?:\x07|\x1b\\)", "", source)
        tokens = re.findall(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b.|[^\x1b]", source)
        for token in tokens:
            if token.startswith("\x1b["):
                args, op = token[2:-1], token[-1]
                if any(ch not in "0123456789;" for ch in args):
                    continue
                values = [int(v) if v else 0 for v in args.split(";")] if args else [0]
                n = values[0] or 1
                if op in "Hf":
                    row = max(0, values[0] - 1)
                    col = max(0, (values[1] if len(values) > 1 else 1) - 1)
                elif op == "A": row = max(0, row - n)
                elif op == "B": row = min(self.rows - 1, row + n)
                elif op == "C": col = min(self.cols - 1, col + n)
                elif op == "D": col = max(0, col - n)
                elif op == "E": row, col = min(self.rows - 1, row + n), 0
                elif op == "F": row, col = max(0, row - n), 0
                elif op == "G": col = n - 1
                elif op == "d": row = n - 1
                elif op == "J" and values[0] in (2, 3):
                    grid = [[" " for _ in range(self.cols)] for _ in range(self.rows)]
                elif op == "J" and values[0] == 0:
                    erase_span(row, col, self.cols)
                    for y in range(row + 1, self.rows): erase_span(y, 0, self.cols)
                elif op == "J" and values[0] == 1:
                    for y in range(row): erase_span(y, 0, self.cols)
                    erase_span(row, 0, col + 1)
                elif op == "K" and row < self.rows:
                    start, end = (0, self.cols) if values[0] == 2 else ((0, col + 1) if values[0] == 1 else (col, self.cols))
                    erase_span(row, start, end)
                elif op == "X": erase_span(row, col, col + n)
            elif token.startswith("\x1b"):
                continue
            elif token == "\r": col = 0
            elif token == "\n": row = min(self.rows - 1, row + 1)
            elif token == "\b": col = max(0, col - 1)
            elif token >= " " and row < self.rows and col < self.cols:
                previous = col - 1
                while previous >= 0 and grid[row][previous] == "":
                    previous -= 1
                joined = previous >= 0 and grid[row][previous].endswith("\u200d")
                zero_width = unicodedata.combining(token) or token in ("\u200d", "\ufe0e", "\ufe0f") or 0x1f3fb <= ord(token) <= 0x1f3ff
                if zero_width or joined:
                    if previous >= 0:
                        grid[row][previous] += token
                else:
                    width = 2 if unicodedata.east_asian_width(token) in "WF" else 1
                    erase_span(row, col, col + width)
                    grid[row][col] = token
                    if width == 2 and col + 1 < self.cols: grid[row][col + 1] = ""
                    col += width
        self.terminal_cursor = (row, col)
        return "\n".join("".join(line) for line in grid)

    def capture(self, label):
        self.pump(.08)
        # Layout evidence must come from the live alternate screen, never the
        # restored shell plus the application's exit command or stderr.
        if self.proc.poll() is not None or b'\x1b[?1049l' in self.output[-256:]:
            return
        screen = self.screen()
        snapshot = self.state()
        self.frames.append({"step": label, "screen": screen, "requests": len(self.calls()),
            "events": len(self.events()), "size": [self.rows, self.cols],
            "cursor": list(self.terminal_cursor), "paused": snapshot.get("paused"),
            "unknown_writes": len(snapshot.get("unknown_writes", {}))})

    def evidence(self, directory, name, error=None):
        directory.mkdir(parents=True, exist_ok=True)
        document = {"scenario": name, "scope": "Real PTY; synthetic protocol fixture, no real model or personal credentials",
            "status": "FAIL" if error else "PASS", "error": str(error) if error else None,
            "binary": str(self.binary), "binary_sha256_at_start": self.binary_sha256, "frames": self.frames}
        (directory / (name + ".json")).write_text(json.dumps(document, ensure_ascii=False, indent=2))
        cards = "".join("<section><h2>" + html.escape(frame["step"]) + "</h2><p>Requests: " + str(frame["requests"]) +
            "; events: " + str(frame["events"]) + "; terminal: " + str(frame["size"][1]) + "×" + str(frame["size"][0]) +
            "; paused: " + str(frame["paused"]) + "; unknown writes: " + str(frame["unknown_writes"]) + "</p><pre>" + html.escape(frame["screen"]) + "</pre></section>" for frame in self.frames)
        page = '<!doctype html><meta charset="utf-8"><title>BONE PTY ' + html.escape(name) + '</title><style>body{font:16px system-ui;margin:32px;background:#f5f3ed;color:#172b36}pre{font:13px monospace;white-space:pre;background:#18232b;color:#e5edf2;padding:16px;overflow:auto}section{margin:24px 0}</style><h1>' + html.escape(name) + '</h1><p>' + html.escape(document["scope"]) + '</p><p>' + document["status"] + '</p>' + ('<pre>' + html.escape(str(error)) + '</pre>' if error else '') + cards
        (directory / (name + ".html")).write_text(page)

    def restart(self, session):
        assert self.proc.poll() is not None
        self.output.clear()
        self.answered_queries = 0
        self.proc = subprocess.Popen([str(self.binary), "--data-dir", str(self.data), "--profile", "fixture", "tui",
            "--workspace", str(self.workspace), "--session", session, "--max-parallel", "1", "--max-calls", "16"],
            stdin=self.slave, stdout=self.slave, stderr=self.slave, env=self.env, start_new_session=True)
        self.wait(lambda: b'\x1b[?1049h' in self.output, "reopened TUI startup")

    def saved_drafts(self):
        return [json.loads(path.read_text()) for path in (self.data / 'tui').glob('*.json')]

    def calls(self):
        return [json.loads(line) for line in self.requests.read_text().splitlines()] if self.requests.exists() else []

    def events(self):
        db = self.data / "sessions.sqlite3"
        if not db.exists():
            return []
        try:
            with sqlite3.connect(db) as connection:
                return [json.loads(row[0]) for row in connection.execute("SELECT payload FROM events ORDER BY sequence")]
        except sqlite3.OperationalError:
            return []

    def state(self):
        with sqlite3.connect(self.data / "sessions.sqlite3") as connection:
            return json.loads(connection.execute("SELECT snapshot FROM sessions LIMIT 1").fetchone()[0])

    def quit(self, command=b'\x11'):
        self.capture("Before exit: live alternate screen")
        self.send(command)
        deadline = time.monotonic() + 5
        while self.proc.poll() is None and time.monotonic() < deadline:
            self.pump(.1)
        assert self.proc.poll() is not None, "quit did not exit within 5 seconds"
        self.pump()
        assert termios.tcgetattr(self.slave) == self.original, "terminal attributes were not restored"
        assert b'\x1b[?1049l' in self.output, "alternate screen was not exited"

    def close(self):
        if self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait()
        self.server.kill()
        self.server.wait()
        os.close(self.master)
        os.close(self.slave)
        self.directory.cleanup()


def run_case(binary, name, turns, action, size=(30, 110), evidence_dir=None):
    fixture = Fixture(binary, turns, size)
    try:
        fixture.wait(lambda: b'\x1b[?1049h' in fixture.output, "TUI startup")
        fixture.capture("Start: idle terminal")
        action(fixture)
        fixture.capture("Final terminal")
        if evidence_dir:
            fixture.evidence(evidence_dir, name)
        print("PASS", name)
    except Exception as error:
        fixture.capture("Failure: current terminal")
        if evidence_dir:
            fixture.evidence(evidence_dir, name, error)
        raise
    finally:
        fixture.close()


def verify_vt_replay():
    """Keep evidence replay honest for the operations used by ratatui diffs."""
    probe = object.__new__(Fixture)
    probe.rows, probe.cols = 3, 12
    cases = [
        ('中a\x1b[1;2HX', ' Xa'),  # Overwrite the right half of a CJK cell.
        ('中a\x1b[1;1HX', 'X a'),  # Overwrite its leading half.
        ('中中a\x1b[1;2H界', ' 界 a'),  # A new wide cell spans two old glyphs.
        ('中ab\x1b[1;2H\x1b[1X', '  ab'),  # Erase a continuation cell.
    ]
    for sequence, expected in cases:
        probe.output = bytearray(('\x1b[2J' + sequence).encode())
        assert probe.screen().splitlines()[0].rstrip() == expected, 'VT wide overwrite/erase replay corrupted layout evidence'
    probe.output = bytearray(b'one\x1b[2Ethree\x1b[1Ftwo')
    assert [line.rstrip() for line in probe.screen().splitlines()] == ['one', 'two', 'three'], 'VT E/F cursor replay corrupted layout evidence'


def paste_and_enter(f):
    f.send('\x1b[200~PASTE_ONE\nPASTE_TWO\x1b[201~')
    f.pump(.5)
    assert not f.calls(), "multiline paste executed before Enter"
    assert not any(e.get("kind") == "input" for e in f.events()), "paste was posted before Enter"
    f.send('\r')
    f.wait(lambda: any(e.get('kind') == 'model_message' and 'PASTE_ACCEPTED' in json.dumps(e) for e in f.events()), 'paste reply')
    assert 'PASTE_ONE' in json.dumps(f.calls()) and 'PASTE_TWO' in json.dumps(f.calls())
    f.resize(12, 44)
    f.pump(.2)
    f.resize(40, 140)
    f.pump(.2)
    f.quit()


def concurrent_input(f):
    f.send('INITIAL_TASK\r')
    f.wait(lambda: len(f.calls()) == 1, 'first model call')
    f.send('ADDED_CONSTRAINT\r')
    f.wait(lambda: 'ADDED_CONSTRAINT' in json.dumps(f.events()), 'input accepted while model runs')
    f.wait(lambda: len(f.calls()) >= 2, 'second request')
    assert 'ADDED_CONSTRAINT' in json.dumps(f.calls()[1:]), 'new constraint absent from follow-up model request'
    f.wait(lambda: f.visible('CONSTRAINT_ACCEPTED'), 'new constraint reply rendered')
    f.quit()


def pause_resume(f):
    f.send('PAUSE_TASK\r')
    f.wait(lambda: len(f.calls()) == 1, 'delayed call started')
    f.send(b'\x03')
    f.pump(.4)
    assert f.proc.poll() is None, 'Ctrl+C killed TUI'
    count = len(f.calls())
    f.pump(2.3)
    assert len(f.calls()) == count, 'paused TUI continued model requests'
    assert any(e.get('kind') == 'input' for e in f.events()), 'pause lost posted input'
    f.send(b'\x12')
    f.wait(lambda: len(f.calls()) > count or f.visible('PAUSE_COMPLETE'), 'resume produces progress')
    f.send('/stop\r')
    f.wait(lambda: f.state().get('paused'), '/stop persists paused state')
    f.send('/resume\r')
    f.wait(lambda: not f.state().get('paused'), '/resume clears paused state')
    f.quit()


def delegated_question(f):
    f.send('ROOT_TASK\r')
    f.wait(lambda: any(e.get('kind') == 'question' for e in f.events()), 'delegated question')
    question = next(e for e in f.events() if e.get('kind') == 'question')
    assert question.get('job_id'), 'question has no Job attribution'
    f.wait(lambda: 'Which output format' in f.screen(), 'delegated question visible in agent transcript')
    f.send('/reply ' + question['id'] + '\r')
    f.pump(.2)
    f.send('ANSWER_JSON\r')
    f.wait(lambda: f.visible('Root task done.'), 'explicit answer resumes delegated workflow')
    assert any(e.get('reply_to') == question['id'] and e.get('kind') == 'input' for e in f.events()), 'explicit answer not linked to question'
    tools = [e for e in f.events() if e.get('kind') in ('tool_call', 'tool_result')]
    assert tools and all(e.get('job_id') for e in tools), 'tool events lack Job attribution'
    before = len(f.events())
    f.send(b'\x1bOQ')  # F2 reveals the activity panel.
    f.pump(.2)
    f.send('\r')
    f.wait(lambda: f.visible('原文') or f.visible('结果'), 'activity detail opened')
    f.send('READ_ONLY_PROBE')
    f.pump(.2)
    assert len(f.events()) == before, 'activity detail posted input or changed state'
    f.send(b'\x1b')
    f.pump(.1)
    f.send(b'\x1b')
    f.pump(.1)
    f.quit()


def failure(f):
    f.send('FAIL_TASK\r')
    f.wait(lambda: len(f.calls()) >= 1, 'failure call')
    f.wait(lambda: any(e.get('kind') == 'error' for e in f.events()) or f.visible('失败'), 'failure visible before exit')
    assert f.proc.poll() is None, 'recoverable model failure terminated TUI'
    f.capture('Model HTTP failure remains actionable before exit')
    status_line = next((line for line in f.screen().splitlines() if '当前：' in line), '')
    assert '失败' in status_line, 'failure missing from primary status'
    assert '就绪' not in status_line, 'failed work presented as ready'
    f.quit()


def live_stream(f):
    f.send('STREAM_TASK\r')
    f.wait(lambda: 'LIVE_PREVIEW' in f.screen(), 'native live delta displayed')
    assert not any(e.get('kind') == 'model_message' for e in f.events()), 'preview only appeared after durable completion'
    f.wait(lambda: any(e.get('kind') == 'model_message' for e in f.events()), 'native final committed')
    f.wait(lambda: 'LIVE_PREVIEW' in f.screen(), 'final response rendered')
    f.quit()


def stale_stream(f):
    f.send('STREAM_OLD\r')
    f.wait(lambda: 'STALE_PREVIEW' in f.screen(), 'old preview visible')
    f.send('STREAM_NEW\r')
    f.wait(lambda: 'STREAM_NEW' in json.dumps(f.events()), 'new revision accepted')
    f.wait(lambda: 'STALE_PREVIEW' not in f.screen(), 'old revision preview removed')
    f.wait(lambda: f.visible('NEW_REVISION_DONE'), 'new revision result')
    f.quit()


def stopped_stream(f):
    f.send('STREAM_STOP\r')
    f.wait(lambda: 'STOP_PREVIEW' in f.screen(), 'preview before stop')
    f.send(b'\x03')
    f.wait(lambda: f.state().get('paused'), 'paused after native preview')
    f.wait(lambda: 'STOP_PREVIEW' not in f.screen(), 'cancelled preview cleared')
    f.pump(.5)
    assert not any(e.get('kind') == 'model_message' for e in f.events()), 'cancelled stream became authoritative'
    f.quit()


def command_draft_editor(f):
    f.send('/definitely_unknown_command\r')
    f.pump(.4)
    f.wait(lambda: any(d.get('draft') == '/definitely_unknown_command' for d in f.saved_drafts()), 'failed slash command retained exact saved draft')
    assert not f.calls() and not any(e.get('kind') == 'input' for e in f.events()), 'failed slash reached agent'
    f.send(b'\x01\x0b')  # Documented Ctrl+A/Ctrl+K clears the line.
    f.send(b'\x10')  # Ctrl+P opens palette.
    f.pump(.2)
    f.send(b'\x1b')
    f.pump(.2)
    f.send('/status\r')
    f.pump(.2)
    f.send(b'\x1b')
    f.pump(.2)
    f.send('/diff\r')
    f.pump(.2)
    f.send(b'\x1b')
    f.pump(.2)
    assert not f.calls() and not any(e.get('kind') == 'input' for e in f.events()), 'UI command created agent input'
    f.send(b'\x07')  # Ctrl+G invokes synthetic local editor.
    f.wait(lambda: 'EDITOR_REFERENCE_ONLY' in f.screen(), 'external editor returned draft')
    assert not f.calls(), 'external editor draft executed without Enter'
    session = f.state()['id']
    f.quit()
    drafts = list((f.data / 'tui').glob('*.json'))
    assert drafts and 'EDITOR_REFERENCE_ONLY' in drafts[0].read_text(), 'draft was not saved'
    f.restart(session)
    f.wait(lambda: 'EDITOR_REFERENCE_ONLY' in f.screen(), 'session draft restored')
    assert not f.calls(), 'restored draft auto-executed'
    f.send('\r')
    f.wait(lambda: f.visible('EDITOR_ACCEPTED'), 'restored editor draft submitted')
    assert 'EDITOR_REFERENCE_ONLY' in json.dumps(f.calls()), 'agent missing submitted editor draft'
    count = len(f.events())
    f.send('/older\r')
    f.wait(lambda: '更早会话原文' in f.screen() or '已载入最早对话' in f.screen() or '已经是最早的记录' in f.screen(), 'older records read-only view or earliest boundary')
    if '更早会话原文' in f.screen():
        f.send('READ_ONLY_OLDER_PROBE')
        f.pump(.2)
        f.send(b'\x1b')
        f.pump(.1)
    assert len(f.events()) == count, 'older history command changed agent state'
    f.quit()


def file_completion(f):
    (f.workspace / 'reference-unique.txt').write_text('FILE_CONTENT_MUST_NOT_AUTOEXECUTE')
    f.send('Read @reference-u')
    f.wait(lambda: 'reference-unique.txt' in f.screen(), 'inline file completion offers matching path')
    f.send('\t')
    f.pump(.2)
    assert '@reference-unique.txt' in f.screen(), 'file completion did not insert reference'
    assert not f.calls() and not any(e.get('kind') == 'input' for e in f.events()), 'completion submitted draft'
    f.send('\r')
    f.wait(lambda: f.visible('REFERENCE_ACCEPTED'), 'reference prompt submitted')
    assert '@reference-unique.txt' in json.dumps(f.calls()), 'reference missing from agent prompt'
    f.quit()


def session_commands(f):
    original = f.state()['id']
    f.send('/model definitely-missing-profile\r')
    f.pump(.3)
    f.wait(lambda: any(d.get('draft') == '/model definitely-missing-profile' for d in f.saved_drafts()), 'failed model selection retained exact draft')
    f.send(b'\x01\x0b')
    f.send('/model ollama:fixture-next\r')
    f.pump(.4)
    assert f.state()['paused'], 'model switch resumed work automatically'
    assert not f.calls(), 'model switch executed model request'
    f.send('/export\r')
    f.wait(lambda: bool(list(f.data.rglob('*.html')) + list(f.workspace.rglob('*.html'))), 'real HTML export')
    f.pump(.2)
    f.send(b'\x1b')
    f.pump(.1)
    f.send('/new\r')
    def session_ids():
        with sqlite3.connect(f.data / 'sessions.sqlite3') as connection:
            return [row[0] for row in connection.execute('SELECT id FROM sessions')]
    f.wait(lambda: len(session_ids()) == 2, 'new session created')
    assert original in session_ids()
    f.send(b'\x10')
    f.send('sessions\r')  # Palette explicitly executes this argument-taking command.
    f.wait(lambda: original in f.screen(), 'session picker exposes original UUID')
    f.send(original + '\r')
    f.wait(lambda: '会话已打开' in f.screen() or '会话已打开' in bytes(f.output).decode('utf-8', errors='replace'), 'session picker opens its selected session')
    f.send('/status\r')
    f.wait(lambda: f.visible('Session: ' + original), 'status verifies the actual reopened session UUID')
    f.capture('The reopened session identity is verified through its actual local status')
    f.send(b'\x1b')
    f.pump(.2)
    assert not f.calls() and not any(e.get('kind') == 'input' for e in f.events()), 'session command created agent inputs'
    f.quit()


def unicode_history_search(f):
    f.send('中文e\u0301👩‍💻测试')
    f.send(b'\x1b[D\x7f')  # Move before 试 and delete one CJK grapheme.
    f.send('\r')
    f.wait(lambda: f.visible('UNICODE_ACCEPTED'), 'Unicode editor input accepted')
    prompt = '中文e\u0301👩‍💻试'
    assert prompt in json.dumps(f.calls(), ensure_ascii=False), 'Unicode cursor edit corrupted graphemes'
    count = len(f.calls())
    f.send(b'\x1b[1;3A')  # Alt+Up recalls history; plain Up moves textarea cursor.
    def recalled():
        drafts = list((f.data / 'tui').glob('*.json'))
        return any(json.loads(path.read_text()).get('draft') == prompt for path in drafts)
    f.wait(recalled, 'history recalled exact Unicode draft')
    f.send(b'\x1b[1;3B')
    f.pump(.2)
    assert len(f.calls()) == count, 'history navigation submitted prompt'
    f.send(b'\x06')
    f.send('UNICODE_ACCEPTED')
    f.pump(.2)
    f.send(b'\x1b')
    f.pump(.2)
    assert len(f.calls()) == count, 'transcript search invoked agent'
    f.quit()


def interactive_editor(f):
    editor = f.root / 'fixture-editor'
    editor.write_text('#!/bin/sh\nprintf "INTERACTIVE_EDITOR_READY"\nIFS= read -r value\nprintf "%s" "$value" > "$1"\n')
    f.send(b'\x07')
    f.wait(lambda: f.visible('INTERACTIVE_EDITOR_READY'), 'external editor waiting on actual terminal input')
    f.send('EDITOR_TYPED_ON_TTY\r')
    def returned():
        return any(json.loads(path.read_text()).get('draft') == 'EDITOR_TYPED_ON_TTY' for path in (f.data / 'tui').glob('*.json'))
    f.wait(returned, 'interactive editor returned terminal input as saved draft')
    assert not f.calls() and not any(e.get('kind') == 'input' for e in f.events()), 'interactive editor autoexecuted input'
    f.send('\r')
    f.wait(lambda: f.visible('INTERACTIVE_EDITOR_ACCEPTED'), 'editor draft Enter accepted')
    assert 'EDITOR_TYPED_ON_TTY' in json.dumps(f.calls()), 'editor stdin was consumed by TUI event reader'
    f.quit()


def hanging_editor_signal(f):
    pid_file = f.root / 'editor.pid'
    editor = f.root / 'fixture-editor'
    editor.write_text('#!/bin/sh\necho $$ > "' + str(pid_file) + '"\nprintf "HANG_EDITOR_READY"\nexec sleep 60\n')
    f.send(b'\x07')
    f.wait(lambda: pid_file.exists() and f.visible('HANG_EDITOR_READY'), 'hanging editor running')
    pid = int(pid_file.read_text())
    signal_cleanup(f)
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return
        f.pump(.1)
    raise AssertionError('SIGTERM left external editor process running')


def signal_cleanup(f):
    f.capture('Before SIGTERM: live alternate screen')
    f.proc.send_signal(signal.SIGTERM)
    deadline = time.monotonic() + 5
    while f.proc.poll() is None and time.monotonic() < deadline:
        f.pump(.1)
    assert f.proc.poll() is not None, 'SIGTERM did not stop TUI'
    f.pump()
    assert termios.tcgetattr(f.slave) == f.original, 'SIGTERM left terminal raw'
    assert b'\x1b[?1049l' in f.output, 'SIGTERM left alternate screen'


def slash_inline(f):
    f.send('/')
    f.wait(lambda: '/new' in f.screen() and '/status' in f.screen(), 'slash candidates immediately visible', timeout=3)
    f.capture("Typing / shows commands without opening a separate panel")
    f.send('sta')
    f.wait(lambda: '/status' in f.screen(), 'slash candidates filter')
    f.send('\r')
    f.wait(lambda: any(d.get('draft', '').strip() == '/status' for d in f.saved_drafts()), 'Enter selects candidate into draft only')
    assert not f.calls() and not any(e.get('kind') == 'input' for e in f.events()), 'candidate Enter executed work'
    f.capture('Candidate Enter inserts /status without executing it')
    f.send(b'\x01\x0b')
    f.send('/sta')
    f.send('\t')
    f.pump(.2)
    assert not f.calls() and not any(e.get('kind') == 'input' for e in f.events()), 'Tab submitted command to agent'
    f.wait(lambda: any(d.get('draft', '').strip() == '/status' for d in f.saved_drafts()), 'Tab retains completed command draft')
    f.capture("Tab inserts /status; no model request")
    f.send('\r')
    f.pump(.3)
    assert not f.calls() and not any(e.get('kind') == 'input' for e in f.events()), 'status reached model'
    f.capture("Status opens through the completed command")
    f.send(b'\x1b')
    f.quit()



def multiline_undo(f):
    original = '第一行 e\u0301👩‍💻\n第二行 preserve cents'
    f.send(b'\x1b[200~' + original.encode() + b'\x1b[201~')
    f.wait(lambda: 'preserve cents' in f.screen(), 'multiline Unicode paste remains editable')
    assert not f.calls(), 'paste executed before Enter'
    f.capture("Unicode multiline paste remains a draft")
    f.send(b'\x1a')  # Ctrl+Z undoes the whole paste operation.
    f.wait(lambda: 'preserve cents' not in f.screen(), 'undo removes pasted operation')
    f.send(b'\x1bz')  # Alt+Z restores paste.
    f.wait(lambda: 'preserve cents' in f.screen(), 'redo restores multiline paste')
    f.send(b'\x1b[A\x05!')  # Visual Up then Ctrl+E inserts in first line.
    expected = '第一行 e\u0301👩‍💻!\n第二行 preserve cents'
    f.wait(lambda: any(d.get('draft') == expected for d in f.saved_drafts()), 'Up navigates multiline draft without recalling history')
    f.capture("Undo, redo and visual-line cursor editing preserve Unicode")
    f.send('\r')
    f.wait(lambda: 'MULTILINE_EDITOR_ACCEPTED' in f.screen(), 'edited multiline prompt submitted')
    assert any(e['kind'] == 'input' and e['data']['message']['content'][0]['text'] == expected for e in f.events()), 'editor changed graphemes or line boundaries'
    f.quit()

def live_shell(f):
    (f.workspace / 'module.py').write_text('def compute(): return 7\n')
    f.send('Inspect module.py and run its checks; keep terminal responsive.\r')
    f.wait(lambda: any(e.get('kind') == 'tool_started' and e.get('data', {}).get('tool_name') == 'shell' for e in f.events()), 'shell actually started')
    f.wait(lambda: 'LIVE_STDOUT_READY' in f.screen() and 'LIVE_STDERR_READY' in f.screen(), 'both live shell streams visible before exit', timeout=3)
    assert not (f.workspace / 'shell-finished').exists(), 'shell already finished before preview'
    assert not any(e.get('kind') == 'tool_result' and e.get('data', {}).get('tool_name') == 'shell' for e in f.events()), 'shell preview appeared after result'
    f.capture("Real shell still running; stdout and stderr visible")
    f.send('Preserve this follow-up draft')
    f.wait(lambda: 'Preserve this follow-up draft' in f.screen(), 'input responsive during running shell', timeout=3)
    f.send(b'\x1bOQ\x1b[F\r')  # F2 audit; End selects actual latest start event.
    f.wait(lambda: 'LIVE_STDOUT_READY' in f.screen(), 'running action evidence retains original command')
    f.send(b'\x19')
    f.wait(lambda: f.clipboard.exists() and 'LIVE_STDOUT_READY' in f.clipboard.read_text(), 'running action evidence copied completely')
    observed = f.clipboard.read_text()
    assert 'LIVE_STDOUT_READY' in observed and 'exit:' not in observed, 'running evidence invented a final tool result'
    assert 'ENGINEERING_CHECKS_COMPLETE' not in observed, 'evidence revealed a future fixture response'
    assert not any(e.get('kind') == 'tool_result' and e.get('data', {}).get('tool_name') == 'shell' for e in f.events()), 'evidence opened after shell completion'
    f.capture('Running evidence contains only the original action, before a final result exists')
    f.send(b'\x1b')
    f.pump(.2)
    f.send(b'\x1b')
    f.pump(.2)
    assert 'Preserve this follow-up draft' in f.screen(), 'audit return lost editor draft'
    f.send(b'\x1b[1;2D\x1b[1;2D')  # Shift+Left selects editor text before global stop.
    f.send(b'\x19')
    f.wait(lambda: f.clipboard.exists() and f.clipboard.read_text() == 'ft', 'Ctrl+Y proves the real editor selection exists before stopping')
    f.send(b'\x03')
    f.wait(lambda: f.state().get('paused'), 'Ctrl+C pauses running shell despite input selection', timeout=3)
    f.wait(lambda: bool(f.state().get('unknown_writes')), 'interrupted shell needs reconciliation')
    f.wait(lambda: any(d.get('draft') == 'Preserve this follow-up draft' for d in f.saved_drafts()), 'global pause preserves selected draft')
    f.capture("Paused real shell; unknown write retained")
    assert not (f.workspace / 'shell-finished').exists(), 'cancelled shell kept executing'
    f.send(b'\x01\x0b')
    draft = 'KEEP_FIRST_LINE\nKEEP_SECOND_LINE'
    f.send(b'\x1b[200~' + draft.encode() + b'\x1b[201~')
    f.send(b'\x1b[A\x01' + b'\x1b[C' * 5)  # First line, column five.
    f.wait(lambda: any(d.get('draft') == draft for d in f.saved_drafts()), 'multiline draft prepared for reconciliation')
    for cancel in (True, False):
        f.send(b'\x10')  # Command palette preserves editor text and cursor.
        f.send('reconcile\r')
        f.wait(lambda: '结果未知' in f.screen(), 'unknown write picker identifies interrupted shell')
        f.send('\r')
        f.wait(lambda: '核查表单' in f.screen(), 'reconciliation uses a separate form')
        f.send(b'\x04')
        f.wait(lambda: 'LIVE_STDOUT_READY' in f.screen(), 'reconcile evidence contains original shell command')
        f.capture('Reconciliation evidence reads the actual interrupted command')
        f.send(b'\x1b')
        f.pump(.2)
        assert '核查表单' in f.screen(), 'Esc from evidence did not return to its form'
        f.send(b'\x12')
        f.pump(.2)
        assert f.state().get('paused'), 'Ctrl+R resumed while reconciliation form was active'
        if cancel:
            f.send('DISCARD_THIS_RECONCILIATION')
            f.send(b'\x1b')
            f.pump(.2)
            # Close any restored picker parent, then edit the preserved cursor.
            f.send(b'\x1b')
            f.pump(.2)
            f.send('!')
            expected = 'KEEP_!FIRST_LINE\nKEEP_SECOND_LINE'
            f.wait(lambda: any(d.get('draft') == expected for d in f.saved_drafts()), 'cancel restores multiline draft and exact insertion cursor')
            assert f.state().get('unknown_writes'), 'cancel resolved an unknown write'
            assert '输入目标：新要求' in f.screen(), 'cancel changed the input target'
            f.capture('Cancel restores original multiline draft, target and insertion cursor')
        else:
            f.send('Checked workspace: shell-finished absent; process cancelled.\r')
    f.wait(lambda: not f.state().get('unknown_writes'), 'observed reconciliation recorded')
    assert f.state().get('paused'), 'reconcile automatically resumed work'
    assert len(f.calls()) == 2, 'reconciliation reissued model or shell work'
    f.capture("Recorded observation clears unknown write and keeps session paused")
    f.quit()


def reading_detail_copy(f):
    content = 'TOOL_BEGIN\n' + ''.join(f'engineering line {n}: original implementation evidence\n' for n in range(100)) + 'TOOL_FULL_END'
    (f.workspace / 'implementation.txt').write_text(content)
    f.send('Review implementation.txt and report.\r')
    f.wait(lambda: 'REVIEW_COMPLETE' in f.screen(), 'engineering response complete')
    count = len(f.calls())
    f.capture("Engineering transcript shows a collapsed tool alongside answer")
    f.send(b'\x1b[17~')  # F6 moves between input and conversation.
    f.send(b'\x1b[A')  # Select preceding read_file result.
    f.send('d')
    f.wait(lambda: 'TOOL_BEGIN' in f.screen(), 'selected tool opens complete result body')
    f.capture('Selected tool opens complete readable output')
    f.send('D')
    f.wait(lambda: 'kind: tool_result' in f.screen() and 'event:' in f.screen(), 'result opens the selected durable audit event')
    f.capture("Selected tool audit displays the durable event and its metadata")
    f.send(b'\x19')
    f.wait(lambda: f.clipboard.exists() and 'TOOL_FULL_END' in f.clipboard.read_text(), 'copy detail includes tail outside visible viewport')
    copied = f.clipboard.read_text()
    assert 'TOOL_BEGIN' in copied and 'engineering line 99' in copied, 'detail copy silently truncated tool body'
    assert len(f.calls()) == count, 'reading or copying invoked agent'
    f.capture("Full selected tool copied through fixture clipboard; no model request")
    f.send(b'\x1b')
    f.pump(.2)
    f.wait(lambda: 'TOOL_BEGIN' in f.screen() and 'tool_result' not in f.screen(), 'Esc returns from audit to readable result')
    f.capture('Esc from audit restores the readable result')
    f.send(b'\x1b')
    f.pump(.2)
    f.clipboard.unlink()
    f.send('y')  # Esc preserves conversation selection rather than switching to input.
    f.wait(lambda: f.clipboard.exists() and 'TOOL_FULL_END' in f.clipboard.read_text(), 'selected message copy preserves full original tool output')
    assert f.clipboard.read_text() == content, 'selected tool copy changed original newlines or added audit JSON'
    assert len(f.calls()) == count, 'selected message copy invoked agent'
    f.quit()


def persistent_search(f):
    f.send('ARCHIVE_REQUIRED_FLAG: preserve the original signed cents requirement.\r')
    f.wait(lambda: 'ARCHIVE_ACKNOWLEDGED' in f.screen(), 'original archived requirement answered')
    original = f.events()
    state = f.state()
    session = state['id']
    owner = state['focus']
    input_template = next(e for e in original if e['kind'] == 'input')
    model_template = next(e for e in original if e['kind'] == 'model_message')
    delivery_template = next(e for e in original if e['kind'] == 'delivery')
    f.quit()
    # Long-history setup copies already validated native event shapes. It does not
    # run 70 synthetic model turns merely to push the real first requirement out
    # of the startup page. The following browsing/search actions use a real PTY.
    with sqlite3.connect(f.data / 'sessions.sqlite3') as connection:
        snapshot = json.loads(connection.execute('SELECT snapshot FROM sessions WHERE id=?', (session,)).fetchone()[0])
        for n in range(70):
            user, response, delivery = (copy.deepcopy(template) for template in (input_template, model_template, delivery_template))
            for event in (user, response, delivery):
                event['id'] = str(uuid.uuid4())
            user['root_input'] = user['id']
            user['data']['message'] = {'role': 'user', 'content': [{'type': 'text', 'text': f'Later engineering request {n}'}]}
            response['reply_to'] = delivery['reply_to'] = user['id']
            response['root_input'] = delivery['root_input'] = user['id']
            response['data']['response']['choice'] = [{'type': 'text', 'text': f'Later engineering answer {n}'}]
            delivery['data']['response_event'] = response['id']
            for event in (user, response, delivery):
                connection.execute('INSERT INTO events (id,session_id,revision,payload,job_id) VALUES (?,?,?,?,?)', (event['id'],session,event['revision'],json.dumps(event),event.get('job_id')))
            snapshot['jobs'][owner]['history'].extend([user['id'], response['id']])
        connection.execute('UPDATE sessions SET snapshot=? WHERE id=?', (json.dumps(snapshot), session))
    f.restart(session)
    f.pump(.3)
    assert 'ARCHIVE_REQUIRED_FLAG' not in f.screen(), 'target was still in the loaded recent page'
    f.capture("Reopened long session; original requirement is outside startup page")
    before = len(f.events())
    f.send('/older\t\r')
    f.pump(.3)
    assert len(f.events()) == before, 'browsing older events mutated conversation'
    f.send('/search ARCHIVE_REQUIRED_FLAG\r')
    f.wait(lambda: f.visible('original signed cents'), 'persistent full-history search produces a real hit beyond its query title')
    f.capture("Full SQLite search finds original requirement after browsing older history")
    f.send('\r')
    f.wait(lambda: f.visible('搜索原文') and f.visible('original signed cents'), 'search selection opens the original body rather than leaving its picker')
    f.send(b'\x19')
    f.wait(lambda: f.clipboard.exists() and 'signed cents requirement.' in f.clipboard.read_text(), 'search detail preserves original text across terminal soft wrapping')
    assert len(f.events()) == before and len(f.calls()) == 1, 'search or opening hit invoked model/state mutation'
    f.capture("Selected history hit opens original requirement without executing work")
    f.send(b'\x1b')
    f.quit()


def explicit_question_target(f):
    f.send('Coordinate the two choices.\r')
    f.wait(lambda: len([e for e in f.events() if e['kind'] == 'question']) == 2, 'two pending questions')
    questions = [e for e in f.events() if e['kind'] == 'question']
    selected = next(e for e in questions if 'ROOT_FORMAT' in e['data']['question'])
    f.capture("Two pending questions remain independently addressable")
    f.send('/reply ' + selected['id'] + '\r')
    f.pump(.3)
    f.send('ROOT_ANSWER_JSON\r')
    f.wait(lambda: any(e['kind'] == 'input' and e.get('reply_to') == selected['id'] for e in f.events()), 'answer explicitly links chosen question')
    f.wait(lambda: 'ROOT_ANSWER_ACCEPTED' in f.screen(), 'selected question resumes its own work')
    f.capture("Explicit answer targets earlier root question, leaves child question pending")
    child = next(e for e in questions if e['id'] != selected['id'])
    assert not any(e['kind'] == 'input' and e.get('reply_to') == child['id'] for e in f.events()), 'another pending question was answered accidentally'
    f.send('/message\r')  # Explicitly cancel reply routing; Esc only closes layers.
    f.pump(.2)
    f.send('/latest\r')
    f.pump(.3)
    f.capture("Returning to latest history preserves cancelled reply routing")
    f.send('ORDINARY_NEW_REQUIREMENT\r')
    f.wait(lambda: any(e['kind'] == 'input' and 'ORDINARY_NEW_REQUIREMENT' in json.dumps(e) for e in f.events()), 'ordinary input accepted after cancelling reply mode')
    ordinary = next(e for e in f.events() if e['kind'] == 'input' and 'ORDINARY_NEW_REQUIREMENT' in json.dumps(e))
    assert ordinary.get('reply_to') not in {q['id'] for q in questions}, '/message left pending reply routing active'
    f.capture("Explicit new-message action cancels reply routing")
    f.quit()


def question_never_auto_targets(f):
    f.send('Start a question, but do not take over my draft.\r')
    f.wait(lambda: len(f.calls()) == 1, 'question call in progress')
    f.send('INDEPENDENT_DRAFT')
    f.wait(lambda: any(e['kind'] == 'question' for e in f.events()), 'single pending question reaches TUI')
    question = next(e for e in f.events() if e['kind'] == 'question')
    f.wait(lambda: any(d.get('draft') == 'INDEPENDENT_DRAFT' for d in f.saved_drafts()), 'question preserves independent draft')
    f.capture('A single new question does not commandeer the current draft')
    f.send('\r')
    f.wait(lambda: any(e['kind'] == 'input' and 'INDEPENDENT_DRAFT' in json.dumps(e) for e in f.events()), 'independent draft posted')
    event = next(e for e in f.events() if e['kind'] == 'input' and 'INDEPENDENT_DRAFT' in json.dumps(e))
    assert event.get('reply_to') != question['id'], 'a single question auto-bound independent input'
    f.wait(lambda: any(e['kind'] == 'model_message' and 'INDEPENDENT_ACCEPTED' in json.dumps(e) for e in f.events()), 'independent instruction receives a durable response')
    assert not any(e['kind'] == 'input' and e.get('reply_to') == question['id'] for e in f.events()), 'question was accidentally answered'
    f.quit()


def old_reader_and_delivery(f):
    f.send('OLD_INPUT_ANCHOR retain this original reading position.\r')
    f.wait(lambda: any(e['kind'] == 'delivery' for e in f.events()), 'first long delivery is durable')
    f.send('NEW_OUTPUT_TASK\r')
    f.wait(lambda: len(f.calls()) == 2, 'second reply is in progress')
    f.send(b'\x1b[17~\x1b[H')  # F6 and Home browse the old content.
    f.wait(lambda: 'OLD_INPUT_ANCHOR' in f.screen(), 'old content in viewport')
    f.capture('Reading old content while a new answer is still pending')
    f.send(b'\x1bOQ')
    f.pump(.2)
    f.capture('Audit opens from the existing old reading position')
    f.send(b'\x1b')
    f.pump(.2)
    assert 'OLD_INPUT_ANCHOR' in f.screen(), 'closing audit lost the original reading anchor'
    f.wait(lambda: sum(e['kind'] == 'delivery' for e in f.events()) == 2, 'new delivery committed while reading old content')
    f.pump(.3)
    assert 'OLD_INPUT_ANCHOR' in f.screen(), 'new output pulled the old reader to latest'
    assert re.search(r'未读|新消息|新内容|条新|回到最新|返回最新', f.screen()), 'new output provides no unread/latest entry'
    f.capture('New delivery preserves the old viewport and offers an unread entry')
    target = (40, 120) if f.cols == 80 else (24, 80)
    f.resize(*target)
    f.pump(.5)
    resized = f.screen().splitlines()
    assert resized[1] == '─' * f.cols and '阅读：' in resized[-1], 'application did not redraw at the real resized terminal geometry'
    assert 'OLD_INPUT_ANCHOR' in f.screen(), 'resize lost the old reading position'
    f.capture('Resize retains the same old reading anchor')
    calls = len(f.calls())
    events = len(f.events())
    f.send(b'\x1b[17~')  # Back to input, then jump to a real delivered answer.
    f.send('/delivery\r')
    f.wait(lambda: 'LATEST_DELIVERY_BEGIN' in f.screen(), 'delivery entry opens the latest complete answer')
    f.capture('Delivery entry opens the existing long answer directly')
    f.send(b'\x19')
    f.wait(lambda: f.clipboard.exists() and 'LATEST_DELIVERY_FULL_END' in f.clipboard.read_text(), 'full long delivery tail is copyable')
    assert len(f.calls()) == calls and len(f.events()) == events, 'delivery reading executed work'
    f.send(b'\x1b')
    f.pump(.2)
    f.quit()


def quiet_success_and_loud_failure(f):
    (f.workspace / 'success.txt').write_text('SUCCESS_READ_BODY_MUST_STAY_COLLAPSED\n')
    f.send('Run a successful file read, then a failed shell check.\r')
    f.wait(lambda: any(e['kind'] == 'tool_result' and e.get('data', {}).get('tool_name') == 'read_file' for e in f.events()), 'successful read is durable')
    f.wait(lambda: any(e['kind'] == 'tool_result' and e.get('data', {}).get('tool_name') == 'shell' for e in f.events()), 'failed shell is durable')
    f.pump(.3)  # Judge the stable durable summary after the live preview is removed.
    f.wait(lambda: 'CHECK_FAILURE_STDERR' in f.screen(), 'key failed-shell stderr visible in collapsed transcript')
    screen = f.screen()
    assert 'SUCCESS_READ_BODY_MUST_STAY_COLLAPSED' not in screen, 'successful tool printed body in default transcript'
    assert len([line for line in screen.splitlines() if 'read_file' in line or 'success.txt' in line]) == 1, 'successful read occupies multiple default rows'
    assert re.search(r'17|exit|退出', screen), 'failed shell omitted exit status'
    f.capture('Successful tool uses one row; failed shell exposes stderr and exit status')
    f.quit()


def reconcile_keeps_reply_target(f):
    f.send('Coordinate a question and a live worker.\r')
    f.wait(lambda: any(e['kind'] == 'question' for e in f.events()) and any(e['kind'] == 'tool_started' and e.get('data', {}).get('tool_name') == 'shell' for e in f.events()), 'question and live worker are both active')
    question = next(e for e in f.events() if e['kind'] == 'question')
    f.send('/reply ' + question['id'] + '\r')
    f.pump(.2)
    draft = 'ANSWER_FIRST_LINE\nANSWER_SECOND_LINE'
    f.send(b'\x1b[200~' + draft.encode() + b'\x1b[201~')
    f.send(b'\x1b[A\x01' + b'\x1b[C' * 7)
    f.send(b'\x03')
    f.wait(lambda: f.state().get('paused') and bool(f.state().get('unknown_writes')), 'worker interruption records unknown write')
    count = len(f.calls())
    before = len([e for e in f.events() if e['kind'] == 'input'])
    f.send(b'\x10')
    f.send('reconcile\r')
    f.wait(lambda: '结果未知' in f.screen(), 'unknown write list opened from reply draft')
    f.send('\r')
    f.wait(lambda: '核查表单' in f.screen(), 'separate reconciliation form opened')
    f.send('CANCEL_THIS_NOTE')
    f.send(b'\x04')
    f.wait(lambda: 'REPLY_WORKER_STARTED' in f.screen(), 'worker evidence opened')
    f.capture('Reconcile evidence overlays a separate note, retaining the original reply target')
    f.send(b'\x1b')
    f.pump(.2)
    f.send(b'\x1b')
    f.pump(.2)
    f.send('!')
    expected = 'ANSWER_!FIRST_LINE\nANSWER_SECOND_LINE'
    f.wait(lambda: any(d.get('draft') == expected for d in f.saved_drafts()), 'cancel restores reply draft at original insertion cursor')
    assert question['id'][:8] in next((line for line in f.screen().splitlines() if '输入目标：' in line), ''), 'reconciliation cancel lost the specific reply target'
    assert len(f.calls()) == count, 'read or cancel replayed model work'
    assert len([e for e in f.events() if e['kind'] == 'input']) == before, 'reconciliation cancel submitted reply draft'
    assert f.state().get('paused') and f.state().get('unknown_writes'), 'reconciliation cancel changed execution state'
    f.capture('Cancel restores exact reply target, multiline text and cursor; no input submitted')
    f.quit()


def exit_resume(f):
    session = f.state()['id']
    f.quit()
    # After restoring the alternate screen, verify the actual exit output only.
    tail = bytes(f.output).rsplit(b'\x1b[?1049l', 1)[-1].decode('utf-8', errors='replace')
    for part in ('bone', '--data-dir', str(f.data), '--profile', 'fixture', 'tui', '--session', session, '--workspace', str(f.workspace)):
        assert part in tail, f'exit resume command missing {part}'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/bone')
    parser.add_argument('--case', choices=['paste','concurrent','pause','question','failure','stream','stale','stream-stop','commands','completion','sessions','signal','unicode','interactive-editor','editor-signal','slash-inline','multiline-undo','shell-live','exit-resume','reading-detail','persistent-search','question-target','question-independent','reader-delivery','tool-density','reconcile-reply'])
    parser.add_argument("--evidence-dir", type=Path)
    parser.add_argument('--size', default='80x24', choices=['80x24', '120x40'], help='real terminal columns x rows')
    parser.add_argument('--keep-going', action='store_true', help='record every selected scenario, then fail if any failed')
    args = parser.parse_args()
    verify_vt_replay()
    cases = [
        ('slash-inline', [], slash_inline),
        ('multiline-undo', [{'contains':['第一行 e\u0301👩‍💻!', '第二行 preserve cents'], 'text':'MULTILINE_EDITOR_ACCEPTED'}], multiline_undo),
        ('shell-live', [
            {'output': [tool('read_file', {'path': 'module.py'})]},
            {'output': [tool('shell', {'command': 'printf "LIVE_STDOUT_READY\\n"; printf "LIVE_STDERR_READY\\n" >&2; while [ ! -e release-shell ]; do sleep 0.05; done; touch shell-finished', 'timeout_seconds': 15})]},
            {'text': 'ENGINEERING_CHECKS_COMPLETE'}], live_shell),
        ('exit-resume', [], exit_resume),
        ('reading-detail', [{'output': [tool('read_file', {'path': 'implementation.txt'})]}, {'text': 'REVIEW_COMPLETE'}], reading_detail_copy),
        ('persistent-search', [{'text': 'ARCHIVE_ACKNOWLEDGED'}], persistent_search),
        ('question-target', [
            {'match_job_title': 'Conversation', 'output': [tool('job_send', {'title': 'Child', 'message': 'CHILD_WORK'})]},
            {'match_job_title': 'Conversation', 'output': [tool('ask_user', {'question': 'ROOT_FORMAT: choose output'})]},
            {'match_job_title': 'Child', 'output': [tool('ask_user', {'question': 'CHILD_FORMAT: choose encoding'})]},
            {'match_job_title': 'Conversation', 'contains': ['ROOT_ANSWER_JSON'], 'text': 'ROOT_ANSWER_ACCEPTED'},
            {'text': 'ORDINARY_NEW_ACCEPTED'}], explicit_question_target),
        ('question-independent', [
            {'delay_seconds': .8, 'output': [tool('ask_user', {'question': 'INDEPENDENT_QUESTION: which format?'})]},
            {'contains': ['INDEPENDENT_DRAFT'], 'text': 'INDEPENDENT_ACCEPTED'},
            {'delay_seconds': 2, 'text': 'Original work retained alongside the independent requirement.'}], question_never_auto_targets),
        ('reader-delivery', [
            {'text': 'OLD_ANSWER_BEGIN\n' + ''.join(f'Original answer line {n}: retained engineering context.\n' for n in range(80)) + 'OLD_ANSWER_FULL_END'},
            {'delay_seconds': 2, 'contains': ['NEW_OUTPUT_TASK'], 'text': 'LATEST_DELIVERY_BEGIN\n' + ''.join(f'Current delivery line {n}: complete readable output.\n' for n in range(100)) + 'LATEST_DELIVERY_FULL_END'}], old_reader_and_delivery),
        ('tool-density', [
            {'output': [tool('read_file', {'path': 'success.txt'})]},
            {'output': [tool('shell', {'command': 'printf "Build step started\\nCHECK_FAILURE_STDERR: compilation failed\\nBuild step finished\\n" >&2; exit 17', 'timeout_seconds': 5})]},
            {'delay_seconds': 2, 'text': 'The check failed; fix CHECK_FAILURE_STDERR before continuing.'}], quiet_success_and_loud_failure),
        ('reconcile-reply', [
            {'match_job_title': 'Conversation', 'output': [tool('job_send', {'title': 'Worker', 'message': 'Start the worker command.'})]},
            {'match_job_title': 'Conversation', 'output': [tool('ask_user', {'question': 'REPLY_TARGET: which output format?'})]},
            {'match_job_title': 'Worker', 'output': [tool('shell', {'command': 'printf "REPLY_WORKER_STARTED\\n"; while [ ! -e release-worker ]; do sleep 0.05; done; touch reply-worker-finished', 'timeout_seconds': 15})]}], reconcile_keeps_reply_target),
        ('paste', [{'contains':['PASTE_ONE','PASTE_TWO'], 'text':'PASTE_ACCEPTED'}], paste_and_enter),
        ('concurrent', [{'delay_seconds':2,'text':'INITIAL_COMPLETE'}, {'contains':['ADDED_CONSTRAINT'],'text':'CONSTRAINT_ACCEPTED'}, {'delay_seconds':2,'text':'Initial work completed with the added constraint.'}], concurrent_input),
        ('pause', [{'delay_seconds':2,'text':'PAUSE_COMPLETE'}, {'text':'PAUSE_COMPLETE'}], pause_resume),
        ('question', [
            {'match_job_title':'Conversation','output':[tool('job_send',{'title':'Child','message':'CHILD_TASK'})]},
            {'match_job_title':'Conversation','output':[tool('job_wait',{'input_ids':'$input_ids'})]},
            {'match_job_title':'Child','output':[tool('ask_user',{'question':'Which output format should I use?'})]},
            {'match_job_title':'Child','contains':['ANSWER_JSON','answer_input'],'text':'Answer received.'},
            {'match_job_title':'Child','contains':['ANSWER_JSON','CHILD_TASK'],'text':'Child task done.'},
            {'match_job_title':'Conversation','contains':['Child task done.'],'text':'Root task done.'}], delegated_question),
        ('failure',[{'http_status':422}],failure),
        ('stream', [{'text':'LIVE_PREVIEW streamed suffix', 'delta_chunk_chars':12, 'event_delay_seconds':.12, 'event_delays':{'response.completed':2}}], live_stream),
        ('stale', [{'text':'STALE_PREVIEW old suffix', 'event_delays':{'response.completed':3}}, {'contains':['STREAM_NEW'], 'text':'NEW_REVISION_DONE'}, {'delay_seconds':2,'text':'Old work completed with the new revision.'}], stale_stream),
        ('stream-stop', [{'text':'STOP_PREVIEW cancelled suffix', 'event_delays':{'response.completed':3}}], stopped_stream),
        ('commands', [{'contains':['EDITOR_REFERENCE_ONLY'], 'text':'EDITOR_ACCEPTED'}], command_draft_editor),
        ('completion', [{'contains':['@reference-unique.txt'], 'text':'REFERENCE_ACCEPTED'}], file_completion),
        ('sessions', [], session_commands),
        ('signal', [], signal_cleanup),
        ('interactive-editor', [{'contains':['EDITOR_TYPED_ON_TTY'], 'text':'INTERACTIVE_EDITOR_ACCEPTED'}], interactive_editor),
        ('editor-signal', [], hanging_editor_signal),
        ('unicode', [{'contains':['中文e\u0301👩‍💻试'], 'text':'UNICODE_ACCEPTED'}], unicode_history_search),
    ]
    results = []
    for name, turns, action in cases:
        if args.case is None or args.case == name:
            cols, rows = (int(value) for value in args.size.split('x'))
            try:
                run_case(args.binary.resolve(), name, turns, action, size=(rows, cols), evidence_dir=args.evidence_dir)
                results.append({'scenario': name, 'status': 'PASS'})
            except Exception as error:
                results.append({'scenario': name, 'status': 'FAIL', 'error': str(error)})
                print('FAIL', name, str(error), flush=True)
                if not args.keep_going:
                    raise
    if args.evidence_dir:
        (args.evidence_dir / 'summary.json').write_text(json.dumps({'size': args.size, 'binary': str(args.binary.resolve()), 'results': results}, ensure_ascii=False, indent=2))
    if any(result['status'] == 'FAIL' for result in results):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
