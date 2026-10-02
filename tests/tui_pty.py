#!/usr/bin/env python3
"""Real PTY acceptance: python3 tests/tui_pty.py [--binary target/debug/bone].
Only synthetic credentials and a scripted loopback Responses endpoint are used.
"""
import argparse
import fcntl
import json
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
        self.env = env
        self.binary = binary
        self.proc = subprocess.Popen([str(binary), "--data-dir", str(self.data), "--profile", "fixture", "tui",
            "--workspace", str(self.workspace), "--max-parallel", "1", "--max-calls", "16"],
            stdin=self.slave, stdout=self.slave, stderr=self.slave, env=env, start_new_session=True)
        self.output = bytearray()
        self.answered_queries = 0

    def resize(self, rows, cols):
        self.rows, self.cols = rows, cols
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

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
        raise AssertionError(f"timeout: {label}; events={self.events()}; tail={bytes(self.output[-1500:])!r}")

    def send(self, value):
        os.write(self.master, value.encode() if isinstance(value, str) else value)

    def visible(self, text):
        # Ratatui diff output uses cursor moves instead of literal spaces.
        output = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", self.output.decode("utf-8", errors="replace"))
        return re.sub(r"\s+", "", text) in re.sub(r"\s+", "", output)

    def screen(self):
        """Replay the VT operations emitted by ratatui, including cursor diff updates."""
        grid = [[" " for _ in range(self.cols)] for _ in range(self.rows)]
        row = col = 0
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
                elif op == "G": col = n - 1
                elif op == "d": row = n - 1
                elif op == "J" and values[0] in (2, 3):
                    grid = [[" " for _ in range(self.cols)] for _ in range(self.rows)]
                elif op == "K" and row < self.rows:
                    start, end = (0, self.cols) if values[0] == 2 else ((0, col + 1) if values[0] == 1 else (col, self.cols))
                    for x in range(start, min(end, self.cols)): grid[row][x] = " "
            elif token.startswith("\x1b"):
                continue
            elif token == "\r": col = 0
            elif token == "\n": row = min(self.rows - 1, row + 1)
            elif token == "\b": col = max(0, col - 1)
            elif token >= " " and row < self.rows and col < self.cols:
                width = 0 if unicodedata.combining(token) else (2 if unicodedata.east_asian_width(token) in "WF" else 1)
                if width:
                    grid[row][col] = token
                    if width == 2 and col + 1 < self.cols: grid[row][col + 1] = ""
                    col += width
        return "\n".join("".join(line) for line in grid)

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


def run_case(binary, name, turns, action, size=(30, 110)):
    fixture = Fixture(binary, turns, size)
    try:
        fixture.wait(lambda: b'\x1b[?1049h' in fixture.output, "TUI startup")
        action(fixture)
        print("PASS", name)
    finally:
        fixture.close()


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
    f.send('ANSWER_JSON\r')
    f.wait(lambda: f.visible('Root task done.'), 'plain answer resumes delegated workflow')
    assert any(e.get('reply_to') == question['id'] and e.get('kind') == 'input' for e in f.events()), 'plain answer not linked to question'
    tools = [e for e in f.events() if e.get('kind') in ('tool_call', 'tool_result')]
    assert tools and all(e.get('job_id') for e in tools), 'tool events lack Job attribution'
    before = len(f.events())
    f.send(b'\x1bOQ')  # F2 reveals the activity panel.
    f.pump(.2)
    f.send('\t\t\r')
    f.wait(lambda: f.visible('行动原文（只读）'), 'activity detail opened')
    f.send('READ_ONLY_PROBE')
    f.pump(.2)
    assert len(f.events()) == before, 'activity detail posted input or changed state'
    f.send(b'\x1b')
    f.pump(.1)
    f.send(b'\x1b')
    f.pump(.1)
    f.quit(b'/quit\r')


def failure(f):
    f.send('FAIL_TASK\r')
    f.wait(lambda: len(f.calls()) >= 1, 'failure call')
    f.pump(1)
    if f.proc.poll() is None:
        f.quit()
    else:
        assert termios.tcgetattr(f.slave) == f.original, 'failure exit left terminal raw'
        assert b'\x1b[?1049l' in f.output, 'failure exit left alternate screen'


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
    f.send(b'\x15')  # Ctrl+U clears line.
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
    f.wait(lambda: '更早会话原文' in f.screen() or '已经是最早的记录' in f.screen(), 'older records read-only view or earliest boundary')
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
    f.send('\t')
    f.wait(lambda: 'reference-unique.txt' in f.screen(), 'file picker offers matching path')
    f.send('\r')
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
    f.send(b'\x15')
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
    f.send('/sessions\r')
    f.wait(lambda: original in f.screen(), 'session picker exposes original UUID')
    f.send(original + '\r')
    f.wait(lambda: original[:8] in f.screen() and original not in f.screen(), 'session picker reopened original session')
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
    f.send(b'\x1b[A')
    def recalled():
        drafts = list((f.data / 'tui').glob('*.json'))
        return any(json.loads(path.read_text()).get('draft') == prompt for path in drafts)
    f.wait(recalled, 'history recalled exact Unicode draft')
    f.send(b'\x1b[B')
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
    f.proc.send_signal(signal.SIGTERM)
    deadline = time.monotonic() + 5
    while f.proc.poll() is None and time.monotonic() < deadline:
        f.pump(.1)
    assert f.proc.poll() is not None, 'SIGTERM did not stop TUI'
    f.pump()
    assert termios.tcgetattr(f.slave) == f.original, 'SIGTERM left terminal raw'
    assert b'\x1b[?1049l' in f.output, 'SIGTERM left alternate screen'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/bone')
    parser.add_argument('--case', choices=['paste','concurrent','pause','question','failure','stream','stale','stream-stop','commands','completion','sessions','signal','unicode','interactive-editor','editor-signal'])
    args = parser.parse_args()
    cases = [
        ('paste', [{'contains':['PASTE_ONE','PASTE_TWO'], 'text':'PASTE_ACCEPTED'}], paste_and_enter),
        ('concurrent', [{'delay_seconds':2,'text':'INITIAL_COMPLETE'}, {'contains':['ADDED_CONSTRAINT'],'text':'CONSTRAINT_ACCEPTED'}], concurrent_input),
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
        ('stale', [{'text':'STALE_PREVIEW old suffix', 'event_delays':{'response.completed':3}}, {'contains':['STREAM_NEW'], 'text':'NEW_REVISION_DONE'}], stale_stream),
        ('stream-stop', [{'text':'STOP_PREVIEW cancelled suffix', 'event_delays':{'response.completed':3}}], stopped_stream),
        ('commands', [{'contains':['EDITOR_REFERENCE_ONLY'], 'text':'EDITOR_ACCEPTED'}], command_draft_editor),
        ('completion', [{'contains':['@reference-unique.txt'], 'text':'REFERENCE_ACCEPTED'}], file_completion),
        ('sessions', [], session_commands),
        ('signal', [], signal_cleanup),
        ('interactive-editor', [{'contains':['EDITOR_TYPED_ON_TTY'], 'text':'INTERACTIVE_EDITOR_ACCEPTED'}], interactive_editor),
        ('editor-signal', [], hanging_editor_signal),
        ('unicode', [{'contains':['中文e\u0301👩‍💻试'], 'text':'UNICODE_ACCEPTED'}], unicode_history_search),
    ]
    for name, turns, action in cases:
        if args.case is None or args.case == name:
            run_case(args.binary.resolve(), name, turns, action, size=(20,80) if name == 'failure' else (30,110))


if __name__ == '__main__':
    main()
