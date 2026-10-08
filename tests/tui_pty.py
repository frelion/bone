#!/usr/bin/env python3
"""Real PTY acceptance: python3 tests/tui_pty.py [--binary target/debug/bone].
Only synthetic credentials and a scripted loopback Responses endpoint are used.
"""
import argparse
from contextlib import closing
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


def production_fingerprint():
    digest = hashlib.sha256()
    paths = [ROOT/'Cargo.toml',ROOT/'Cargo.lock',*(ROOT/'src').rglob('*.rs')]
    for path in sorted(paths,key=lambda p:p.relative_to(ROOT).as_posix()):
        digest.update(path.relative_to(ROOT).as_posix().encode()+b'\0'+path.read_bytes()+b'\0')
    return digest.hexdigest()


def tool(name, arguments):
    return {"type": "function_call", "call_id": name + "-fixture", "name": name, "arguments": arguments}


class Fixture:
    def __init__(self, binary, turns, size=(30, 110), max_parallel=1, workspace_connections=False):
        self.directory = tempfile.TemporaryDirectory(prefix="bone-tui-pty-")
        self.root = Path(self.directory.name)
        self.data = self.root / "data"
        self.workspace = self.root / "workspace"
        self.data.mkdir()
        self.workspace.mkdir()
        self.requests = self.root / "requests.jsonl"
        self.servers = []
        self.request_routes = {'api_a':self.requests}
        def start_endpoint(label, endpoint_turns, requests):
            script = self.root / (label+'-turns.json')
            script.write_text(json.dumps({'turns':endpoint_turns}))
            server = subprocess.Popen([sys.executable,'-B',str(ROOT/'tests/scripted_responses.py'),
                '--script',str(script),'--requests',str(requests)],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
            self.servers.append(server)
            ready,_,_ = select.select([server.stdout],[],[],5)
            assert ready, 'fixture endpoint did not start: '+label
            return server,int(server.stdout.readline())
        self.server,port = start_endpoint('api-a',turns['api_a'] if workspace_connections else turns,self.requests)
        self.api_a_url = f'http://127.0.0.1:{port}/v1'
        if workspace_connections:
            self.requests_b = self.root/'requests-b.jsonl'
            _,port_b = start_endpoint('api-b',turns['api_b'],self.requests_b)
            self.api_b_url = f'http://127.0.0.1:{port_b}/v1'
            self.request_routes['api_b'] = self.requests_b
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
        if workspace_connections:
            # Native ChatGPT wire, but only a fixture endpoint and a cache we
            # authored. No Codex login source or host credential is read.
            with (self.data/'config.toml').open('a') as config:
                config.write(f'''\n[profiles.subscription_fixture]
reuse_codex_login = false
[profiles.subscription_fixture.model]
model = "arbitrary-subscription-fixture-model"
[profiles.subscription_fixture.model.config.openai]
api_key = ""
base_url = "{self.api_b_url}"
dialect = "chatgpt"
route = "Responses"
auth = "Bearer"
''')
            auth = self.data/'profiles/subscription_fixture/auth.json'
            auth.parent.mkdir(parents=True)
            auth.write_text(json.dumps({'access_token':'synthetic-subscription-only-secret-48117','expires_at':4102444800}))
            auth.chmod(0o600)
        self.master, self.slave = pty.openpty()
        self.original = termios.tcgetattr(self.slave)
        self.rows, self.cols = size
        self.resize(*size)
        env = {k: v for k, v in os.environ.items() if k not in ("BONE_MODEL", "CHATGPT_ACCESS_TOKEN", "OPENAI_API_KEY")}
        env.update(TERM="xterm-256color", BONE_TUI_FIXTURE_KEY="synthetic-local-only")
        if workspace_connections:
            env['BONE_TUI_FIXTURE_B_KEY'] = 'synthetic-second-route-secret-76113'
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
        self.max_parallel = max_parallel
        self.binary_sha256 = hashlib.sha256(binary.read_bytes()).hexdigest()
        self.source_fingerprint = production_fingerprint()
        self.harness_sha256 = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
        self.explicit_profile = None if workspace_connections else 'fixture'
        profile_args = ['--profile',self.explicit_profile] if self.explicit_profile else []
        self.proc = subprocess.Popen([str(binary), "--data-dir", str(self.data), *profile_args, "tui",
            "--workspace", str(self.workspace), "--max-parallel", str(max_parallel), "--max-calls", "16"],
            stdin=self.slave, stdout=self.slave, stderr=self.slave, env=env, start_new_session=True)
        self.output = bytearray()
        self.answered_queries = 0
        self.frames = []
        self.terminal_cursor = (0, 0)
        self.cursor_visible = False
        self.cell_styles = []

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
        styles = [[(None, None, ()) for _ in range(self.cols)] for _ in range(self.rows)]
        foreground = background = None
        attributes = set()
        cursor_visible = True
        row = col = 0
        def current_style():
            return foreground, background, tuple(sorted(attributes))
        def erase_span(y, start, end):
            if not 0 <= y < self.rows:
                return
            for x in range(max(0, start), min(end, self.cols)):
                # Erasing either half of a wide glyph removes the glyph itself.
                if grid[y][x] == "" and x > 0:
                    grid[y][x - 1] = " "
                    styles[y][x - 1] = current_style()
                if x + 1 < self.cols and grid[y][x + 1] == "":
                    grid[y][x + 1] = " "
                    styles[y][x + 1] = current_style()
                grid[y][x] = " "
                styles[y][x] = current_style()
        source = self.output.decode("utf-8", errors="replace")
        source = re.sub(r"\x1b\][^\x07]*(?:\x07|\x1b\\)", "", source)
        tokens = re.findall(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b.|[^\x1b]", source)
        for token in tokens:
            if token.startswith("\x1b["):
                args, op = token[2:-1], token[-1]
                if args == '?25' and op in 'hl':
                    cursor_visible = op == 'h'
                    continue
                if any(ch not in "0123456789;" for ch in args):
                    continue
                values = [int(v) if v else 0 for v in args.split(";")] if args else [0]
                n = values[0] or 1
                if op == 'm':
                    i = 0
                    names = {1:'bold',2:'dim',3:'italic',4:'underline',5:'blink',6:'blink',7:'reverse',8:'hidden',9:'strike'}
                    resets = {22:('bold','dim'),23:('italic',),24:('underline',),25:('blink',),27:('reverse',),28:('hidden',),29:('strike',)}
                    while i < len(values):
                        value = values[i]
                        if value == 0: foreground = background = None; attributes.clear()
                        elif value in names: attributes.add(names[value])
                        elif value in resets: attributes.difference_update(resets[value])
                        elif 30 <= value <= 37 or 90 <= value <= 97: foreground = ('ansi',value)
                        elif 40 <= value <= 47 or 100 <= value <= 107: background = ('ansi',value)
                        elif value == 39: foreground = None
                        elif value == 49: background = None
                        elif value in (38,48) and i + 2 < len(values):
                            if values[i + 1] == 5:
                                color = ('index',values[i + 2]); i += 2
                            elif values[i + 1] == 2 and i + 4 < len(values):
                                color = ('rgb',*values[i + 2:i + 5]); i += 4
                            else:
                                color = None
                            if value == 38: foreground = color
                            else: background = color
                        i += 1
                    continue
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
                    styles = [[current_style() for _ in range(self.cols)] for _ in range(self.rows)]
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
                    styles[row][col] = current_style()
                    if width == 2 and col + 1 < self.cols:
                        grid[row][col + 1] = ""
                        styles[row][col + 1] = current_style()
                    col += width
        self.terminal_cursor = (row, col)
        self.terminal_cells = grid
        self.cursor_visible = cursor_visible
        self.cell_styles = []
        for y, line in enumerate(styles):
            start = 0
            while start < self.cols:
                end = start + 1
                while end < self.cols and line[end] == line[start]: end += 1
                if line[start] != (None, None, ()):
                    self.cell_styles.append({'row':y,'start':start,'end':end,'fg':line[start][0],'bg':line[start][1],'attributes':line[start][2]})
                start = end
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
            "snapshot_session": snapshot['id'],
            "sessions": {key:{'paused':value.get('paused'),'unknown_writes':len(value.get('unknown_writes',{})),
                'job_states':{job_id:job.get('state') for job_id,job in value.get('jobs',{}).items()}}
                for key,value in self.session_states().items()},
            "cursor": list(self.terminal_cursor), "paused": snapshot.get("paused"),
            "cursor_visible": self.cursor_visible, "cell_styles": self.cell_styles,
            "cells": self.terminal_cells,
            "unknown_writes": len(snapshot.get("unknown_writes", {})),
            "request_routes": {label:len(self.route_calls(label)) for label in self.request_routes},
            "running_jobs": sum(str(job.get('state','')).lower() == 'running' for job in snapshot.get('jobs', {}).values()),
            "job_states": {key:job.get('state') for key,job in snapshot.get('jobs', {}).items()}})

    def evidence(self, directory, name, error=None):
        directory.mkdir(parents=True, exist_ok=True)
        document = {"scenario": name, "scope": getattr(self,'scenario_scope',"Real PTY; synthetic protocol fixture, no real model or personal credentials"),
            "status": 'OBSERVED' if getattr(self,'observation_mode',False) else ("FAIL" if error else "PASS"), "error": str(error) if error else None,
            "binary": str(self.binary), "binary_sha256_at_start": self.binary_sha256, "max_parallel": self.max_parallel,
            "source_fingerprint_at_start": self.source_fingerprint, "source_fingerprint_at_end": production_fingerprint(),
            "harness_sha256_at_start": self.harness_sha256, "harness_sha256_at_end": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            "no_color": 'NO_COLOR' in self.env,
            "checks": getattr(self,'acceptance_checks',None), "frames": self.frames}
        (directory / (name + ".json")).write_text(json.dumps(document, ensure_ascii=False, separators=(',', ':')))
        cards = "".join("<section><h2>" + html.escape(frame["step"]) + "</h2><p>Requests: " + str(frame["requests"]) +
            "; events: " + str(frame["events"]) + "; terminal: " + str(frame["size"][1]) + "×" + str(frame["size"][0]) +
            "; paused: " + str(frame["paused"]) + "; unknown writes: " + str(frame["unknown_writes"]) + "</p><pre>" + styled_frame(frame) + "</pre></section>" for frame in self.frames)
        page = '<!doctype html><meta charset="utf-8"><title>BONE PTY ' + html.escape(name) + '</title><style>body{font:16px system-ui;margin:32px;background:#f5f3ed;color:#172b36}pre{font:13px monospace;white-space:pre;background:#18232b;color:#e5edf2;padding:16px;overflow:auto}section{margin:24px 0}</style><h1>' + html.escape(name) + '</h1><p>' + html.escape(document["scope"]) + '</p><p>' + document["status"] + '</p>' + ('<pre>' + html.escape(str(error)) + '</pre>' if error else '') + cards
        (directory / (name + ".html")).write_text(page)

    def restart(self, session):
        assert self.proc.poll() is not None
        self.output.clear()
        self.answered_queries = 0
        profile_args = ['--profile',self.explicit_profile] if self.explicit_profile else []
        self.proc = subprocess.Popen([str(self.binary), "--data-dir", str(self.data), *profile_args, "tui",
            "--workspace", str(self.workspace), "--session", session, "--max-parallel", str(self.max_parallel), "--max-calls", "16"],
            stdin=self.slave, stdout=self.slave, stderr=self.slave, env=self.env, start_new_session=True)
        self.wait(lambda: b'\x1b[?1049h' in self.output, "reopened TUI startup")

    def saved_drafts(self):
        return [json.loads(path.read_text()) for path in (self.data / 'tui').glob('*.json')]

    def calls(self):
        return [call for label in self.request_routes for call in self.route_calls(label)]

    def route_calls(self, label):
        path = self.request_routes[label]
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def events(self, session=None):
        db = self.data / "sessions.sqlite3"
        if not db.exists():
            return []
        try:
            with closing(sqlite3.connect(db)) as connection:
                if session is None:
                    rows = connection.execute("SELECT payload FROM events ORDER BY sequence")
                else:
                    rows = connection.execute("SELECT payload FROM events WHERE session_id=? ORDER BY sequence",(session,))
                return [json.loads(row[0]) for row in rows]
        except sqlite3.OperationalError:
            return []

    def state(self, session=None):
        session = session or getattr(self,'observed_session',None)
        with closing(sqlite3.connect(self.data / "sessions.sqlite3")) as connection:
            if session is None:
                return json.loads(connection.execute("SELECT snapshot FROM sessions LIMIT 1").fetchone()[0])
            return json.loads(connection.execute('SELECT snapshot FROM sessions WHERE id=?',(session,)).fetchone()[0])

    def session_states(self):
        with closing(sqlite3.connect(self.data/'sessions.sqlite3')) as connection:
            return {row[0]:json.loads(row[1]) for row in connection.execute('SELECT id,snapshot FROM sessions')}

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
        for server in self.servers:
            server.kill()
            server.wait()
        os.close(self.master)
        os.close(self.slave)
        self.directory.cleanup()


def styled_frame(frame):
    """Render recorded VT cells/SGR; browser palette approximates terminal ANSI."""
    palette = ['#111111','#cc5555','#55bb66','#ddbb55','#6688dd','#bb66cc','#55bbcc','#dddddd',
        '#777777','#ff7777','#88dd99','#ffe077','#99bbff','#ee99ff','#88eeff','#ffffff']
    def color(value, default):
        if value is None: return default
        kind, *parts = value
        if kind == 'rgb': return '#%02x%02x%02x' % tuple(parts)
        if kind == 'ansi':
            code = parts[0]
            return palette[code - (90 if code >= 90 and code < 100 else 100 if code >= 100 else 40 if code >= 40 else 30) + (8 if code >= 90 else 0)]
        index = parts[0]
        if index < 16: return palette[index]
        if index >= 232: return '#%02x%02x%02x' % ((8 + (index - 232) * 10,) * 3)
        index -= 16; cube = [0,95,135,175,215,255]
        return '#%02x%02x%02x' % (cube[index // 36],cube[index // 6 % 6],cube[index % 6])
    cells = frame.get('cells')
    if cells is None: return html.escape(frame['screen'])
    styles = {}
    for run in frame.get('cell_styles',[]):
        for x in range(run['start'],run['end']): styles[(run['row'],x)] = run
    result = []
    for y, row in enumerate(cells):
        line = []
        x = 0
        while x < len(row):
            text = row[x]
            if not text:
                x += 1
                continue
            end = x + 1
            while end < len(row) and not row[end]: end += 1
            run = styles.get((y,x))
            cursor = frame.get('cursor') if frame.get('cursor_visible') else None
            # Only merge actual one-cell ASCII with identical recorded SGR.
            # Wide/combined glyphs retain their own recorded cell span.
            if end == x+1 and len(text)==1 and ord(text)<128 and cursor != [y,x]:
                while end < len(row) and len(row[end])==1 and ord(row[end])<128 and styles.get((y,end))==run and cursor != [y,end]:
                    text += row[end]
                    end += 1
            width = end - x
            attrs = run['attributes'] if run else ()
            fg = color(run['fg'] if run else None,'#e5edf2');bg = color(run['bg'] if run else None,'#18232b')
            if 'reverse' in attrs: fg,bg = bg,fg
            css = f'display:inline-block;flex:0 0 {width}ch;width:{width}ch;overflow:hidden;color:{fg};background:{bg}'
            if 'bold' in attrs: css += ';font-weight:700'
            if 'dim' in attrs: css += ';opacity:.7'
            if 'italic' in attrs: css += ';font-style:italic'
            if 'underline' in attrs: css += ';text-decoration:underline'
            if frame.get('cursor_visible') and frame.get('cursor') == [y,x]: css += ';outline:1px solid #f3f7ff;outline-offset:-1px'
            line.append('<span style="'+css+'">'+html.escape(text)+'</span>')
            x = end
        result.append('<span style="display:flex;white-space:pre;font-family:inherit;line-height:1.35">'+''.join(line)+'</span>')
    return ''.join(result)


def run_case(binary, name, turns, action, size=(30, 110), evidence_dir=None, observe=False):
    fixture = Fixture(binary, turns, size, max_parallel=2 if name in ('feedback-flow','workspace-flow') else 1,
        workspace_connections=name=='workspace-flow')
    fixture.observation_mode = observe
    try:
        fixture.wait(lambda: b'\x1b[?1049h' in fixture.output, "TUI startup")
        fixture.capture("Start: idle terminal")
        action(fixture)
        fixture.capture("Final terminal")
        if evidence_dir:
            fixture.evidence(evidence_dir, name)
        print("OBSERVED" if observe else "PASS", name)
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
    f.capture('Follow-up input is shown with a receipt while the earlier model runs')
    assert 'ADDED_CONSTRAINT' in f.screen() and re.search('已接收|已纳入',f.screen()), 'running input lacks visible original words and receipt'
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
    lines = f.screen().splitlines()
    input_top = next((i for i,line in enumerate(lines) if any(c in line for c in '┌╭╔') and '输入中' in line),None)
    status_line = lines[input_top-2] if input_top is not None and input_top >= 2 else ''
    assert '失败' in status_line, 'failure missing from primary status'
    assert '就绪' not in status_line, 'failed work presented as ready'
    f.quit()


def live_stream(f):
    f.send('STREAM_TASK\r')
    f.wait(lambda: 'LIVE_PREVIEW' in f.screen(), 'native live delta displayed')
    assert not any(e.get('kind') == 'model_message' for e in f.events()), 'preview only appeared after durable completion'
    f.capture('Native SSE preview before any durable model message')
    assert '未交付' in f.screen(), 'native streaming preview presented as delivered'
    assert not re.search(r'(?m)^\s*(?:你\s*·|Agent(?:\s*·|\s*$))',f.screen()), 'default stream shows user/Agent role headers'
    f.wait(lambda: any(e.get('kind') == 'model_message' for e in f.events()), 'native final committed')
    f.wait(lambda: 'LIVE_PREVIEW' in f.screen(), 'final response rendered')
    f.quit()


def feedback_flow(f):
    """One workflow: real input, model SSE, silent child shell and cancellation."""
    f.acceptance_checks = []
    def check(name, passed, observed):
        f.acceptance_checks.append({'name':name,'passed':bool(passed),'observed':observed})
    def frame(label):
        f.capture(label)
        return f.frames[-1]
    def spinner(capture):
        return ''.join(c for c in capture['screen'] if c in '⠋⠙⠹⠸⠼⠴⠦⠧')
    def editor_bounds(capture):
        cells = capture['cells']
        tops = []
        for y,row in enumerate(cells):
            for x,cell in enumerate(row):
                if cell not in ('┌','╭','╔'): continue
                right = next((j for j in range(x+1,len(row)) if row[j] in ('┐','╮','╗')),len(row))
                title = ''.join(row[x:right])
                if '输入中' in title or '草稿只读' in title: tops.append((y,x))
        if not tops: return None
        y,x = tops[-1]
        bottom = next((j for j in range(y+1,len(cells)) if cells[j][x] in ('└','╰','╚')),None)
        return (y,x,bottom) if bottom is not None else None
    def cursor_inside(capture):
        bounds = editor_bounds(capture)
        y,x = capture['cursor']
        return capture['cursor_visible'] and bounds is not None and bounds[0] < y < bounds[2] and bounds[1] < x < capture['size'][1]-1

    draft = '中文光标'*35 + 'FEEDBACK_DRAFT_TAIL'
    f.send('\x1b[200~'+draft+'\x1b[201~')
    f.pump(.15)
    f.send(b'\x01' + b'\x1b[C'*45 + b'!')
    expected = draft[:45]+'!'+draft[45:]
    f.wait(lambda:any(d.get('draft')==expected for d in f.saved_drafts()),'wrapped editing keys and insertion are fully processed')
    middle = frame('Chinese wrapped draft: middle character and visible editing cursor')
    y,x = middle['cursor']
    cursor_cell = middle['cells'][y][x] if 0 <= y < f.rows and 0 <= x < f.cols else None
    check('wrapped middle cursor remains inside framed editable input', cursor_inside(middle), {'cursor':middle['cursor'],'visible':middle['cursor_visible'],'bounds':editor_bounds(middle)})
    check('hardware cursor points to next actual Chinese character', cursor_cell == expected[46], {'cell':cursor_cell,'expected':expected[46]})
    f.send(b'\x05')
    f.pump(.25)
    end = frame('Chinese wrapped draft: end caret is visible')
    check('wrapped end cursor is visible inside input',cursor_inside(end),{'cursor':end['cursor'],'bounds':editor_bounds(end)})
    y,x = end['cursor']
    end_at_tail = 0 <= y < f.rows and 0 < x < f.cols and end['cells'][y][x-1] == expected[-1] and end['cells'][y][x] == ' '
    check('end caret follows the actual wrapped draft tail',end_at_tail,end['cursor'])
    check('send has not happened while draft is being edited',not f.calls() and not any(e['kind']=='input' for e in f.events()),len(f.calls()))
    f.send(b'\x1b[17~')
    read = frame('F6 reading: draft read only and hardware cursor hidden')
    check('reading focus has explicit read-only input and hides cursor','草稿只读' in read['screen'] and not read['cursor_visible'],{'cursor_visible':read['cursor_visible'],'read_only': '草稿只读' in read['screen']})
    f.send(b'\x1b[17~')
    edit = frame('F6 returns to editing: same end caret and editable frame')
    check('F6 restores editor position and explicit input focus',cursor_inside(edit) and edit['cursor']==end['cursor'] and '输入中' in edit['screen'],{'cursor':edit['cursor'],'prior':end['cursor']})
    f.send('\r')
    f.wait(lambda:len(f.calls())>=1,'silent model request starts')
    silent_a = frame('Sent input: exact original words, receipt and silent model A')
    f.pump(.23)
    silent_b = frame('Silent model B: spinner advances before any output')
    inputs = [e for e in f.events() if e['kind']=='input']
    sent_text = ''.join(part.get('text','') for part in inputs[0].get('data',{}).get('message',{}).get('content',[])) if inputs else None
    check('sent input is preserved exactly as a durable input',len(inputs)==1 and sent_text==expected,{'input_count':len(inputs),'sent_text':sent_text,'expected':expected})
    check('sent words and receipt are visible','FEEDBACK_DRAFT_TAIL' in silent_a['screen'] and bool(re.search('已接收|已纳入|已发送',silent_a['screen'])),silent_a['screen'])
    check('silent model gives changing activity feedback',bool(spinner(silent_a)) and spinner(silent_a)!=spinner(silent_b),[spinner(silent_a),spinner(silent_b)])
    f.wait(lambda:'FLOW_MODEL_PREVIEW' in f.screen(),'real SSE preview appears')
    preview = frame('Streaming model: live preview is explicitly not delivered')
    check('stream preview is visible before durable model completion',not any(e['kind']=='model_message' and 'FLOW_MODEL_PREVIEW' in json.dumps(e) for e in f.events()),preview['events'])
    check('stream preview is distinguished from final delivery',bool(re.search('未交付|输出中|生成中|接收输出',preview['screen'])),preview['screen'])
    f.wait(lambda:any(e['kind']=='model_message' and 'ROOT_BACKGROUND_DELIVERY' in json.dumps(e) for e in f.events()) and any(e['kind']=='tool_started' and e.get('data',{}).get('tool_name')=='shell' for e in f.events()),'original root waits for its silent child')
    # A parent cannot deliver its own outstanding assignment. A subsequent,
    # independent input can settle while the original child's shell still runs.
    f.send('FRONT_QUICK_REPLY\r')
    f.wait(lambda:any(e['kind']=='delivery' and any(i['kind']=='input' and i['id']==e.get('reply_to') and 'FRONT_QUICK_REPLY' in json.dumps(i) for i in f.events()) for e in f.events()),'independent foreground input delivered while old child runs')
    foreground = next(e for e in f.events() if e['kind']=='input' and 'FRONT_QUICK_REPLY' in json.dumps(e))
    delivered = next(e for e in f.events() if e['kind']=='delivery' and e.get('reply_to')==foreground['id'])
    check('independent foreground has its actual matching delivery',True,{'input_id':foreground['id'],'delivery_id':delivered['id'],'reply_to':delivered['reply_to']})
    f.wait(lambda:any(e['kind']=='model_message' and 'ROOT_CONTINUES_WAITING' in json.dumps(e) for e in f.events()),'parent returns to waiting for original child after independent delivery')
    shell_a = frame('Root has delivered, background shell is still silent and active A')
    f.pump(.23)
    shell_b = frame('Background silent shell B: root delivery must not hide activity')
    check('background shell really remains active after root delivery',shell_a['running_jobs']>0 and not (f.workspace/'FEEDBACK_WORKER_DONE').exists(),shell_a['job_states'])
    check('delivered input does not replace active execution status',bool(re.search('正在执行|执行命令|后台|运行中',shell_a['screen'])) and bool(spinner(shell_a)),shell_a['screen'])
    check('silent background shell spinner continues',bool(spinner(shell_a)) and spinner(shell_a)!=spinner(shell_b),[spinner(shell_a),spinner(shell_b)])
    for capture in (silent_a,silent_b,preview,shell_a,shell_b):
        check('default body has no user/Agent role header: '+capture['step'],not bool(re.search(r'(?m)^\s*(?:你\s*·|Agent(?:\s*·|\s*$))',capture['screen'])),capture['step'])
    f.send(b'\x03')
    f.wait(lambda:f.state().get('paused') and f.state().get('unknown_writes'),'pause records uncertain real shell write')
    paused_a = frame('Pause: spinner stops; uncertain writes require reconciliation')
    f.pump(.23)
    paused_b = frame('Paused B: no continued activity or claim all processes terminated')
    check('paused screen stops spinner',not spinner(paused_a) and not spinner(paused_b),[spinner(paused_a),spinner(paused_b)])
    check('pause reflects uncertainty without false global termination',bool(re.search('暂停|核查',paused_a['screen'])) and not bool(re.search('全部已停止|全部终止|所有进程已终止',paused_a['screen'])),{'paused':paused_a['paused'],'unknown_writes':paused_a['unknown_writes']})
    check('cancelled shell has no invented future output or worker delivery',not (f.workspace/'FEEDBACK_WORKER_DONE').exists() and not any('WORKER_FINAL_MUST_NOT_APPEAR' in json.dumps(e) for e in f.events()),len(f.events()))
    f.quit()
    failed = [c['name'] for c in f.acceptance_checks if not c['passed']]
    if failed and not f.observation_mode:
        raise AssertionError('; '.join(failed))


def workspace_flow(f):
    """One local protocol workflow; all credentials and both hosts are synthetic."""
    f.acceptance_checks = []
    def check(name, passed, observed=None):
        f.acceptance_checks.append({'name':name,'passed':bool(passed),'observed':observed})
        assert passed, name
    def menu(query):
        f.send(b'\x10')
        f.pump(.12)
        f.send(query+'\r')
    def field(value, next_label=None):
        f.send(b'\x01\x0b')
        f.send(value+'\r')
        if next_label: f.wait(lambda:f.visible('› '+next_label),'next native form field: '+next_label)
        else: f.pump(.15)
    def api_form():
        menu('connect')
        f.wait(lambda:f.visible('添加 API 连接'),'connection chooser')
        f.send('添加\r')
        f.wait(lambda:f.visible('API 模型'),'native provider picker')
        f.send('openai API 模型\r')  # Vendor plus visible detail, excluding other OpenAI-wire vendors.
        f.wait(lambda:f.visible('› 连接名称'),'API connection form')
    def config_string(section, key):
        # This fixture checks only basic quoted strings in its own config,
        # keeping the existing system-Python harness dependency free.
        current = ''
        for line in (f.data/'config.toml').read_text().splitlines():
            if line.startswith('['): current = line.strip()[1:-1]
            elif current == section:
                match = re.fullmatch(r'\s*'+re.escape(key)+r'\s*=\s*(".*")\s*',line)
                if match: return json.loads(match.group(1))
        return None
    def body(name, route, model):
        before = len(f.route_calls(route))
        def delivered():
            events = f.events(f.observed_session)
            inputs = {e['id']:e for e in events if e['kind']=='input'}
            return any(e['kind']=='delivery' and name in json.dumps(inputs.get(e.get('reply_to'),{})) for e in events)
        f.send(name+'\r')
        f.wait(delivered,'native delivery: '+name)
        requests = f.route_calls(route)[before:]
        models = [call['body'].get('model') for call in requests]
        check('actual '+route+' request uses '+model, model in models,{'route':route,'models':models})
    original = f.state()['id']
    f.observed_session = original
    f.send('WORKSPACE_SESSION_A\r')
    f.wait(lambda:any(e['kind']=='question' for e in f.events()),'real question from original session')
    question = next(e for e in f.events() if e['kind']=='question')
    f.wait(lambda:any(e['kind']=='delivery' and e.get('job_id')!=question.get('job_id') for e in f.events()),'child job settles independently')
    f.pump(.3)
    check('session list is not a job list',len(f.session_states())==1 and len(f.state()['jobs'])==2,{'sessions':1,'jobs':len(f.state()['jobs'])})
    # The legacy reader shares the asynchronous session-index path. Close it
    # immediately and type while its result can still arrive.
    f.send('/sessions\r')
    f.send(b'\x1b')
    f.pump(.12)
    f.send('ASYNC_DRAFT_NOT_STOLEN')
    f.wait(lambda:any(d.get('draft')=='ASYNC_DRAFT_NOT_STOLEN' for d in f.saved_drafts()),'input survives pending session index')
    f.pump(.4)
    f.capture('Session reader cancellation leaves native editing draft intact')
    check('async session result leaves editable draft intact',any(d.get('draft')=='ASYNC_DRAFT_NOT_STOLEN' for d in f.saved_drafts()) and f.cursor_visible)
    f.send(b'\x01\x0b')
    f.send('/reply '+question['id']+'\r')
    draft = '保留回复草稿_A\nTARGET_CURSOR_A'
    f.send('\x1b[200~'+draft+'\x1b[201~')
    f.send(b'\x1b[A\x01'+b'\x1b[C'*4)
    f.wait(lambda:any(d.get('draft')==draft and d.get('reply_to')==question['id'] for d in f.saved_drafts()),'specific question draft is saved')
    f.capture('Reply draft and its exact target before spatial focus changes')
    f.send(b'\x1b[1;2D')
    f.capture('Shift Left moves focus to session sidebar and hides editor caret')
    check('sidebar focus hides editor cursor',not f.cursor_visible)
    f.send(b'\x1b[1;2C!')
    edited = '保留回复!草稿_A\nTARGET_CURSOR_A'
    f.wait(lambda:any(d.get('draft')==edited and d.get('reply_to')==question['id'] for d in f.saved_drafts()),'Shift Right restores original editor insertion point')
    f.send(b'\x1b[1;2A')
    f.capture('Shift Up reads conversation with draft intact')
    check('conversation focus hides editing caret',not f.cursor_visible)
    f.send(b'\x1b[1;2B')
    f.capture('Shift Down restores editable reply draft')
    check('input focus has visible hardware caret',f.cursor_visible)
    before = len(f.calls())
    menu('new')
    f.wait(lambda:len(f.session_states())==2,'actual new SQLite session')
    second = next(s for s in f.session_states() if s!=original)
    f.observed_session = second
    check('session change saves and pauses the source',f.state(original)['paused'] and any(d.get('draft')==edited and d.get('reply_to')==question['id'] for d in f.saved_drafts()))
    check('new session and focus actions do not call models',len(f.calls())==before)
    f.send(b'\x1b[1;2B')
    body('WORKSPACE_API_A','api_a','fixture')
    # Current session is pinned first. In this two-session fixture, End
    # chooses the other session regardless of which is most recently active.
    f.pump(.3)
    f.send(b'\x1b[1;2D\x1b[F\r')
    f.wait(lambda:'保留回复!草稿_A' in f.screen(),'sidebar opens the original session')
    f.observed_session = original
    f.capture('Sidebar switches back to paused source: reply target and multiline draft remain')
    check('reopened session retains its exact target and readable question',any(d.get('draft')==edited and d.get('reply_to')==question['id'] for d in f.saved_drafts()) and f.visible('回复：WORKSPACE_REPLY_QUESTION'))
    f.send(b'\x1b[1;2D\x1b[F\r')
    f.wait(lambda:'API_A_ROUTE_CONFIRMED' in f.screen(),'sidebar returns to second session')
    f.observed_session = second
    f.send(b'\x1b[1;2B')
    f.send('FORM_DRAFT_PRESERVED')
    f.wait(lambda:any(d.get('draft')=='FORM_DRAFT_PRESERVED' for d in f.saved_drafts()),'form parent draft saved')
    original_config = (f.data/'config.toml').read_bytes()
    before = len(f.calls())
    api_form()
    field('fixture_b','模型名称')
    field('arbitrary-org/model:v2','API endpoint')
    field('not-an-absolute-url')
    f.wait(lambda:'未保存' in f.screen(),'invalid endpoint remains unsaved in form')
    f.capture('Invalid endpoint is a local form failure, preserving current connection and parent draft')
    check('invalid local endpoint preserves usable config',(f.data/'config.toml').read_bytes()==original_config and len(f.calls())==before)
    field(f.api_b_url,'API key')
    key = f.env['BONE_TUI_FIXTURE_B_KEY']
    f.send(key)
    f.pump(.2)
    f.capture('API key native textarea is masked and separate from the conversation draft')
    check('secret is masked and never becomes main draft',key not in f.screen() and not any(key in json.dumps(d) for d in f.saved_drafts()))
    f.send(b'\x1b')
    f.wait(lambda:'FORM_DRAFT_PRESERVED' in f.screen(),'cancel restores form parent editor')
    check('secret-step cancel preserves prior connection',(f.data/'config.toml').read_bytes()==original_config and len(f.calls())==before)
    api_form()
    field('fixture_b','模型名称')
    field('arbitrary-org/model:v2','API endpoint')
    field(f.api_b_url,'API key')
    f.send(key+'\r')
    f.wait(lambda:config_string('','default_profile')=='fixture_b','API B is durable default')
    f.capture('API B saved without any model probe; first Job must verify authentication')
    check('connection save makes no hidden model request or empty resumption',len(f.calls())==before and config_string('','default_profile')=='fixture_b' and f.visible('下次请求使用新模型') and not f.visible('Ctrl+R'))
    check('parent editor draft survives successful connection form',any(d.get('draft')=='FORM_DRAFT_PRESERVED' for d in f.saved_drafts()))
    f.send(b'\x01\x0b\x12')
    body('WORKSPACE_API_B','api_b','arbitrary-org/model:v2')
    menu('model')
    f.wait(lambda:f.visible('模型名称'),'single native model form')
    native = 'native/any:opaque-model-v9'
    field(native)
    f.wait(lambda:config_string('profiles.fixture_b.model','model')==native,'arbitrary native model is persisted')
    check('model form preserves API B endpoint',config_string('profiles.fixture_b.model.config.openai','base_url')==f.api_b_url)
    f.send(b'\x12')
    body('WORKSPACE_NATIVE_MODEL','api_b',native)
    f.quit()
    before_restart_vt = bytes(f.output)
    f.restart(second)
    f.observed_session = second
    f.capture('Restart without --profile retains default API B and arbitrary native model')
    f.send(b'\x12')
    body('WORKSPACE_RESTART_DEFAULT','api_b',native)
    f.send('/connect subscription_fixture\r')
    f.wait(lambda:config_string('','default_profile')=='subscription_fixture','synthetic subscription selected')
    f.send(b'\x12')
    body('WORKSPACE_SUBSCRIPTION','api_b','arbitrary-subscription-fixture-model')
    f.capture('Native ChatGPT protocol uses only authored OAuth cache and local endpoint')
    saved = (f.data/'config.toml').read_bytes()
    f.send('NARROW_PARENT_DRAFT')
    f.wait(lambda:any(d.get('draft')=='NARROW_PARENT_DRAFT' for d in f.saved_drafts()),'narrow parent draft saved')
    original_size = (f.rows,f.cols)
    f.resize(10,26)
    f.pump(.25)
    f.send(b'\x1b[1;2D')
    f.capture('Extreme narrow session popover uses the same session focus')
    f.send(b'\x1b')
    f.pump(.12)
    f.send(b'\x1b[1;2B')
    f.resize(*original_size)
    f.pump(.3)
    f.screen()
    check('narrow session popover returns to editing focus',f.cursor_visible)
    check('narrow session popover preserves configured model and draft',any(d.get('draft')=='NARROW_PARENT_DRAFT' for d in f.saved_drafts()) and (f.data/'config.toml').read_bytes()==saved and '[1;2B' not in f.screen())
    menu('model')
    f.wait(lambda:f.visible('模型名称'),'model modal opens from preserved draft')
    f.resize(10,26)
    f.pump(.25)
    f.capture('Extreme narrow model modal remains cancellable')
    event_count, request_count = len(f.events()), len(f.calls())
    f.send('\r')
    f.pump(.25)
    check('invisible narrow model field cannot submit',f.visible('扩大窗口') and (f.data/'config.toml').read_bytes()==saved and len(f.events())==event_count and len(f.calls())==request_count)
    f.capture('Narrow model form explains resizing and rejects invisible submission')
    f.send(b'\x1b')
    f.resize(*original_size)
    f.pump(.3)
    check('narrow model modal cancel preserves configured model and draft',(f.data/'config.toml').read_bytes()==saved and any(d.get('draft')=='NARROW_PARENT_DRAFT' for d in f.saved_drafts()))
    secrets = [key,'synthetic-subscription-only-secret-48117']
    complete_vt = before_restart_vt + bytes(f.output)
    check('secrets never reach VT output, input history or durable events',all(secret not in complete_vt.decode('utf-8',errors='replace') and secret not in json.dumps(f.events()) and not any(secret in json.dumps(d) for d in f.saved_drafts()) for secret in secrets))
    f.capture('Completed local connection/session workflow, parent draft preserved')
    f.quit()


def sidebar_flow(f):
    """Real key/mouse routing over authored history and one loopback delivery."""
    f.acceptance_checks = []
    def check(name, passed, observed=None):
        f.acceptance_checks.append({'name':name,'passed':bool(passed),'observed':observed})
        assert passed, name
    def sidebar_lines():
        f.screen()
        width = 30 if f.cols >= 100 else 26
        return [''.join(row[:width]) for row in f.terminal_cells]
    def locate(title):
        lines = sidebar_lines()
        return next((row for row,line in enumerate(lines) if title in line),None)
    def click(row):
        # SGR mouse uses one-based terminal coordinates. Click the text area,
        # avoiding the title/status separator and any scrollbar hit region.
        f.send(f'\x1b[<0;6;{row+1}M\x1b[<0;6;{row+1}m')
    def verify_session(session):
        # Verify the application's actual runtime ID through its local status
        # reader; sidebar titles themselves cannot prove routing correctness.
        f.send(b'\x1b[1;2B\x01\x0b')
        f.send('/status\r')
        f.wait(lambda:f.visible('Session: '+session),'runtime opens exact session '+session)
        status = f.screen()
        f.send(b'\x1b')
        f.pump(.12)
        return status
    original = f.state()['id']
    f.observed_session = original
    now = int(time.time()*1000)
    seeded = {}
    with closing(sqlite3.connect(f.data/'sessions.sqlite3')) as connection:
        template = f.state(original)
        for index in range(32,0,-1):
            session, job, event = (str(uuid.uuid4()) for _ in range(3))
            title = f'授权模块 · 收紧中文路径权限与历史配置，保留相同开头以检查辨识 · 任务{index:02}'
            snapshot = copy.deepcopy(template)
            snapshot.update(id=session,focus=job,revision=1,pending_inputs=[],paused=False,budgets={},unknown_writes={},jobs={job:{
                'id':job,'title':'Conversation','state':'Idle','inbox':[],
                'active_input':None,'history':[event],'summary':None,'wait_for':[],
                'current_call':None,'public_revision':1}})
            user = {'id':event,'session_id':session,'job_id':job,'call_id':None,
                'reply_to':None,'root_input':event,'kind':'input','revision':1,
                'data':{'source':'user','message':{'role':'user','content':[{'type':'text','text':title}]}},
                'timestamp':str(now-index*60000)}
            metadata = copy.deepcopy(user)
            metadata['data'].pop('message')
            connection.execute('INSERT INTO sessions(id,revision,snapshot) VALUES(?,?,?)',(session,1,json.dumps(snapshot)))
            connection.execute('INSERT INTO events(id,session_id,revision,payload,job_id,metadata) VALUES(?,?,?,?,?,?)',
                (event,session,1,json.dumps(user),job,json.dumps(metadata)))
            seeded[index] = session
        connection.commit()
    # Refresh once through the compatibility reader, then leave it before any
    # content editing. No model call or private host credential is involved.
    f.send('/sessions\r')
    f.wait(lambda:f.visible('授权模块'),'authored conversation index is loaded')
    f.send(b'\x1b')
    f.pump(.12)
    draft = '中文 e\u0301👩‍💻草稿'
    f.send(draft)
    f.send(b'\x1b[1;6D\x1b[1;6D\x19')
    f.wait(lambda:f.clipboard.exists() and f.clipboard.read_text()=='草稿','Ctrl Shift arrows create copyable Chinese selection')
    f.capture('Native Chinese selection: Ctrl Shift arrows copy the selected text')
    f.send(b'\x1b[1;2D')
    f.wait(lambda:bool(f.screen()) and not f.cursor_visible,'Shift Left enters sidebar')
    f.capture('Shift Left: current session and browsed candidates are separate')
    f.clipboard.unlink()
    f.send(b'\x1b[1;2C\x19')
    f.wait(lambda:f.clipboard.exists() and f.clipboard.read_text()=='草稿' and bool(f.screen()) and f.cursor_visible,'Shift Right restores editor focus and copyable selection')
    check('focus round trip preserves original selection',f.clipboard.read_text()=='草稿')
    f.send(b'\x1b[1;2A')
    f.wait(lambda:bool(f.screen()) and not f.cursor_visible,'Shift Up enters conversation')
    f.send(b'\x1b[1;2B')
    f.wait(lambda:bool(f.screen()) and f.cursor_visible,'Shift Down restores input')
    f.wait(lambda:any(d.get('draft')==draft for d in f.saved_drafts()),'focus navigation preserves saved draft')
    check('focus and selection do not submit model work',not f.calls() and len(f.events(original))==0)
    f.send(b'\x01\x0b/')
    f.wait(lambda:f.visible('/new') and f.visible('/model'),'inline slash candidates open')
    f.send(b'\x1b[1;2D')
    f.wait(lambda:bool(f.screen()) and not f.cursor_visible,'Shift Left closes inline completion and enters sidebar')
    check('inline slash layer closes when focus moves',not f.visible('/connect') and not f.calls() and len(f.events(original))==0)
    f.capture('Slash suggestions close on Shift Left without executing or changing the draft')
    f.send(b'\x1b[1;2C\x01\x0b')
    f.send('授权模块 · SIDEBAR_REFRESH 验证后台交付不会改变会话选择 · 任务00\r')
    f.wait(lambda:len(f.calls())==1,'single scripted background response is in flight')
    retained = '原会话未发送草稿'
    f.send(retained)
    f.wait(lambda:any(d.get('draft')==retained for d in f.saved_drafts()),'source draft is durable before session browsing')
    f.send(b'\x1b[1;2D')
    f.wait(lambda:locate('任务00') is not None,'current input title joins the sidebar index')
    f.send(b'\x1b[H\x1b[B')
    f.pump(.15)
    f.capture('Candidate 01 remains distinct from current 00 while a real response is pending')
    candidate_row, current_row = locate('任务01'), locate('任务00')
    width = 30 if f.cols >= 100 else 26
    bold_rows = {run['row'] for run in f.cell_styles if 'bold' in run['attributes'] and run['start']<width and run['end']>2}
    selected_rows = {run['row'] for run in f.cell_styles if 'reverse' in run['attributes'] and run['start']<width and run['end']>2}
    check('current identity survives long-title truncation',current_row is not None and current_row in bold_rows,
        {'current_row':current_row,'bold_rows':sorted(bold_rows)})
    check('candidate highlight is separate from current identity',
        candidate_row in selected_rows and candidate_row not in bold_rows and current_row in bold_rows and current_row not in selected_rows,
        {'candidate_row':candidate_row,'current_row':current_row,'bold_rows':sorted(bold_rows),'reverse_rows':sorted(selected_rows)})
    check('sidebar does not display revision as a turn count',not any(re.search(r'\d+\s*轮',line) for line in sidebar_lines()))
    f.wait(lambda:any(e['kind']=='delivery' for e in f.events(original)),'real model completion triggers asynchronous sidebar refresh')
    f.pump(.35)
    f.capture('Background delivery refresh preserves the browsed candidate')
    f.send('\r')
    f.wait(lambda:f.state(original)['paused'],'source session is paused by explicit switch')
    verify_session(seeded[1])
    f.observed_session = seeded[1]
    check('background refresh preserves candidate runtime identity',len(f.calls())==1)
    check('source draft is retained after opening candidate',any(d.get('draft')==retained for d in f.saved_drafts()))
    f.send(b'\x1b[1;2D\x1b[F')
    f.wait(lambda:locate('任务32') is not None,'End reaches off-screen session 32')
    check('session list scroll reaches beyond one viewport',locate('任务01') is None and locate('任务32') is not None)
    f.capture('A full session list scrolls to its oldest rows; common-prefix long titles retain distinct identifying suffixes')
    f.send(b'\x1b[H')
    f.wait(lambda:locate('任务02') is not None,'Home restores visible candidate 02')
    gap_row = locate('任务02') + 2
    assert all(cell in ('', ' ', '─') for cell in f.terminal_cells[gap_row][:width-1]), 'session separator contains information'
    click(gap_row)
    status = verify_session(seeded[1])
    check('clicking a separator preserves the exact runtime session',
        'Session: '+seeded[1] in status and len(f.calls())==1,
        {'clicked_zero_based_row':gap_row,'verified_session':seeded[1],'verification':'/status'})
    f.capture('Separator click keeps session 01; full runtime ID verified through /status')
    f.send(b'\x1b[1;2D\x1b[H')
    f.wait(lambda:locate('任务02') is not None,'candidate 02 remains available after separator click')
    row = locate('任务02')
    click(row)
    verify_session(seeded[2])
    f.observed_session = seeded[2]
    check('clicking a title opens its real session and pauses the previous one',f.state(seeded[1])['paused'] and len(f.calls())==1)
    f.capture('Single click on title opens exact session 02')
    f.send(b'\x1b[1;2D\x1b[H')
    f.wait(lambda:locate('任务03') is not None,'candidate 03 is visible')
    row = locate('任务03')
    click(row + 1)
    verify_session(seeded[3])
    f.observed_session = seeded[3]
    check('clicking the information line opens the same real session and pauses the previous one',f.state(seeded[2])['paused'] and len(f.calls())==1)
    f.capture('Single click on status/time line opens exact session 03')
    f.send('窄屏草稿保持')
    f.wait(lambda:any(d.get('draft')=='窄屏草稿保持' for d in f.saved_drafts()),'narrow-screen parent draft saved')
    original_size = (f.rows,f.cols)
    f.resize(24,42)
    f.pump(.2)
    check('narrow layout hides sidebar until requested',not any('会话' in line for line in f.screen().splitlines()[:2]))
    f.send(b'\x1b[1;2D')
    f.wait(lambda:f.visible('任务03') and not f.cursor_visible,'Shift Left explicitly opens narrow sidebar')
    f.capture('42-column terminal opens the same session sidebar explicitly')
    f.resize(*original_size)
    f.pump(.2)
    f.resize(24,42)
    f.pump(.2)
    f.send(b'\x1b')
    f.pump(.12)
    f.send('!')
    f.wait(lambda:any(d.get('draft')=='窄屏草稿保持!' for d in f.saved_drafts()),'resize round trip returns to original insertion cursor')
    check('narrow sidebar and resizing leave runtime and model count unchanged',len(f.calls())==1 and len(f.session_states())==33)
    f.resize(*original_size)
    f.pump(.2)
    f.capture('Escape after resizing restores the original editing draft without submission')
    f.quit()


def product_flow(f):
    """Natural whole-product journey, with authored sidebar data clearly separate from real Job calls."""
    f.scenario_scope = "Installed or candidate binary in a real PTY; actual Engine/Job calls to authored localhost Responses. The 32 sidebar conversations are mechanical state/title fixtures, not completed model tasks. No remote model or personal credentials."
    f.acceptance_checks = []
    def check(name, passed, observed=None):
        f.acceptance_checks.append({'name':name,'passed':bool(passed),'observed':observed})
        assert passed, name
    def menu(query):
        f.send(b'\x10'); f.pump(.12); f.send(query+'\r'); f.pump(.18)
    def clear():
        # Only the fixture's known one/two-line draft is cleared; production has no select-all shortcut.
        for _ in range(4): f.send(b'\x01\x0b\x7f')
        f.pump(.1)
    def saved(session):
        path = f.data/'tui'/(session+'.json')
        return json.loads(path.read_text()) if path.exists() else {}
    def main_has(text):
        f.screen()
        width=max(26,min(32,f.cols//4)) if f.cols>=80 else 0
        body='\n'.join(''.join(row[width:]) for row in f.terminal_cells)
        return re.sub(r'\s+','',text) in re.sub(r'\s+','',body)
    original = f.state()['id']
    f.observed_session = original
    titles = [
        ('收紧 OAuth 回调验证','question'), ('账单迁移 · 回滚前核查','unknown'),
        ('解释跨域请求为什么失败','idle'), ('修复桌面端输入法组合','paused'),
        ('给导入器补取消入口','ready'), ('阅读 PR 中重复的重试','waiting'),
        ('补齐 Linux 路径转义，保留旧参数兼容 · 终端输入','idle'),
        ('Refactor URL parser without changing public behavior','idle'),
        ('给分页查询添加稳定排序','idle'), ('修正中文搜索结果高亮','idle'),
        ('检查缓存失效边界','paused'), ('更新发布说明','idle'),
        ('保留登录后的跳转地址','question'), ('清理构建产物','idle'),
        ('对照新的错误码解释失败原因','idle'), ('调整窄屏表单','idle'),
        ('迁移用户偏好设置','idle'), ('复查工作区锁冲突','unknown'),
        ('追踪一次慢请求','idle'), ('阅读上传接口的调用关系','idle'),
        ('修复输入历史中的多行光标','idle'), ('让快捷键提示与实际动作一致','idle'),
        ('删除已经失效的兼容层','idle'), ('定位后台任务重复启动','paused'),
        ('确认代码修改没有覆盖另一任务','idle'), ('解释一次失败的编译','idle'),
        ('收缩模型配置边界','idle'), ('改善工具日志中的长路径','idle'),
        ('对比新旧索引结果','idle'), ('完善会话恢复说明','idle'),
        ('检查 Windows 换行符','idle'), ('梳理接入另一个 provider 的步骤','idle'),
    ]
    # These are mechanical conversation fixtures, never claimed as 32 completed model tasks.
    template = f.state(original)
    with closing(sqlite3.connect(f.data/'sessions.sqlite3')) as connection:
        for index, (title, attention) in reversed(list(enumerate(titles))):
            session, job, event, call = (str(uuid.uuid4()) for _ in range(4))
            snapshot = copy.deepcopy(template)
            snapshot.update(id=session,focus=job,revision=1,pending_inputs=[],paused=attention in ('paused','unknown'),budgets={},unknown_writes={},jobs={job:{
                'id':job,'title':'Conversation','state':{'question':'Waiting','waiting':'Waiting','ready':'Ready','paused':'Ready'}.get(attention,'Idle'),
                'inbox':[],'active_input':None,'history':[event],'summary':None,
                'wait_for':['fixture-dependency'] if attention=='waiting' else [],'current_call':None,'public_revision':1}})
            if attention=='unknown': snapshot['unknown_writes']={call:{'call_id':call,'job_id':job,'root_input':event,'tool_name':'shell'}}
            user={'id':event,'session_id':session,'job_id':job,'call_id':None,'reply_to':None,'root_input':event,'kind':'input','revision':1,
                'data':{'source':'user','message':{'role':'user','content':[{'type':'text','text':title}]}},'timestamp':str(int(time.time()*1000)-index*60000)}
            metadata=copy.deepcopy(user);metadata['data'].pop('message')
            connection.execute('INSERT INTO sessions(id,revision,snapshot) VALUES(?,?,?)',(session,1,json.dumps(snapshot)))
            connection.execute('INSERT INTO events(id,session_id,revision,payload,job_id,metadata) VALUES(?,?,?,?,?,?)',(event,session,1,json.dumps(user),job,json.dumps(metadata)))
            if attention=='question':
                question=copy.deepcopy(user);question.update(id=str(uuid.uuid4()),kind='question',call_id=call,reply_to=event,
                    data={'question':'是否保留现有跳转行为？','tool_key':call+'-key'})
                connection.execute('INSERT INTO events(id,session_id,revision,payload,job_id,metadata) VALUES(?,?,?,?,?,?)',
                    (question['id'],session,1,json.dumps(question),job,json.dumps(question)))
        connection.commit()
    f.send(b'\x1b[1;2D');f.wait(lambda:f.visible('收紧 OAuth'),'mixed authored sidebar index');f.send(b'\x1b[H')
    f.capture('Real terminal: current session is also the keyboard candidate')
    width=(f.cols//4 if f.cols>=80 else 0);width=max(26,min(32,width)) if width else f.cols
    sidebar=[''.join(row[:width]) for row in f.terminal_cells]
    current_row=next((row for row,line in enumerate(sidebar) if '新会话' in line),None)
    bold_rows={run['row'] for run in f.cell_styles if 'bold' in run['attributes'] and run['start']<width and run['end']>2}
    highlighted={run['row'] for run in f.cell_styles if 'reverse' in run['attributes'] and run['start']<width and run['end']>2}
    check('current title and keyboard candidate styles combine on the same session',
        current_row is not None and current_row in bold_rows and current_row in highlighted and not f.calls(),
        {'current_row':current_row,'bold_rows':sorted(bold_rows),'highlighted_rows':sorted(highlighted)})
    f.send(b'\x1b[B');f.capture('Real terminal: browse candidate, current session and mixed attention states')
    check('mixed titles expose distinct required attention words',all(f.visible(word) for word in ('核查','回复','暂停')))
    sidebar=[''.join(row[:width]) for row in f.terminal_cells]
    current_row=next((row for row,line in enumerate(sidebar) if '新会话' in line),None)
    candidate_row=next((row for row,line in enumerate(sidebar) if '收紧 OAuth' in line),None)
    candidate_time=time.strftime('%m/%d %H:%M',time.localtime(int(user['timestamp'])/1000))
    bold_rows={run['row'] for run in f.cell_styles if 'bold' in run['attributes'] and run['start']<width and run['end']>2}
    highlighted={run['row'] for run in f.cell_styles if 'reverse' in run['attributes'] and run['start']<width and run['end']>2}
    check('current identity and browsed candidate remain different without color',
        current_row is not None and current_row in bold_rows and current_row not in highlighted and candidate_row in highlighted and candidate_row not in bold_rows,
        {'current_row':current_row,'candidate_row':candidate_row,'bold_rows':sorted(bold_rows),'highlighted_rows':sorted(highlighted)})
    info=sidebar[candidate_row+1]
    dim_rows={run['row'] for run in f.cell_styles if 'dim' in run['attributes'] and run['start']<width and run['end']>2}
    check('session information shows its real status and persisted local update time',
        '回复' in info and candidate_time in info and candidate_row+1 in highlighted
        and candidate_row+1 not in bold_rows and candidate_row+2 not in highlighted
        and candidate_row+1 not in dim_rows and current_row+1 in dim_rows and candidate_row+2 in dim_rows,
        {'information_line':info,'expected_local_timestamp':candidate_time,'timestamp_source':'last authored persistent event','dim_rows':sorted(dim_rows)})
    check('authored sidebar list remains a session list',len(f.session_states())==33 and not f.calls())
    f.send(b'\x1b[1;2B');f.capture('Real terminal: input focus removes candidate highlight and retains the current title weight')
    bold_rows={run['row'] for run in f.cell_styles if 'bold' in run['attributes'] and run['start']<width and run['end']>2}
    highlighted={run['row'] for run in f.cell_styles if 'reverse' in run['attributes'] and run['start']<width and run['end']>2}
    check('leaving sidebar removes candidate highlight while preserving current identity without model work',
        current_row in bold_rows and candidate_row not in bold_rows and not highlighted and f.cursor_visible and not f.calls(),
        {'current_row':current_row,'candidate_row':candidate_row,'bold_rows':sorted(bold_rows),'highlighted_rows':sorted(highlighted),'requests':len(f.calls())})
    f.send('修复登录重定向：保持兼容行为，先确认失败回调的处理方式。\r')
    f.wait(lambda:len(f.calls())==1,'one actual local model request starts');f.capture('Actual Job: thinking without invented completed tasks')
    f.wait(lambda:any(e['kind']=='question' for e in f.events(original)),'actual question from a Job')
    question=next(e for e in f.events(original) if e['kind']=='question')
    new_draft='保持兼容行为。\n另外保留现有 cookie 名称。'
    f.send('\x1b[200~'+new_draft+'\x1b[201~')
    f.wait(lambda:saved(original).get('draft')==new_draft,'new requirement draft durable')
    menu('回复问题');f.send('\r');f.pump(.12)
    answer='保持原地址，并覆盖回归测试草稿'
    f.send(answer);f.send(b'\x1b[1;6D\x1b[1;6D\x19')
    f.wait(lambda:f.clipboard.exists() and f.clipboard.read_text()=='草稿','answer selection copies exact Chinese text')
    f.wait(lambda:saved(original).get('draft')==answer and saved(original).get('reply_to')==question['id'],'targeted answer durable')
    position=(saved(original).get('cursor'),saved(original).get('selection'))
    f.capture('Actual question: its selected answer and the separate unsent requirement')
    menu('new');f.wait(lambda:len(f.session_states())==34,'second real session created')
    # The new session is the only non-authored session without an input event.
    second=next(s for s in f.session_states() if s!=original and not f.events(s))
    f.observed_session=second
    f.send(b'\x1b[1;2D\x1b[H\x1b[B\r');f.pump(.25);f.send(b'\x1b[1;2B')
    f.wait(lambda:f.visible(answer),'source answer restores after session switch');f.observed_session=original
    check('session switch keeps answer target, caret and selection',saved(original).get('reply_to')==question['id'] and position==(saved(original).get('cursor'),saved(original).get('selection')))
    f.quit();f.restart(original);f.pump(.2);f.capture('Restart restores the actual targeted answer')
    f.clipboard.unlink();f.send(b'\x19');f.wait(lambda:f.clipboard.exists() and f.clipboard.read_text()=='草稿','restart restores exact input selection')
    menu('写新要求');f.wait(lambda:saved(original).get('draft')==new_draft,'inactive new requirement restores after switch and restart')
    check('all existing target drafts survive session switch and process restart',saved(original).get('draft')==new_draft)
    f.capture('Separate new requirement remains intact after returning from a question and restart')
    clear();f.send('/mo');f.pump(.15);f.send(b'\t');f.pump(.15)
    check('Tab completes a slash candidate without execution',f.visible('/model') and not f.visible('模型名称 · 服务商'))
    clear();f.send('/mo');f.pump(.15);f.send('\r');f.wait(lambda:f.visible('模型名称 · 服务商'),'Enter directly executes selected slash command')
    check('slash Enter opens its selected action in one step',f.visible('模型名称 · 服务商') and len(f.calls())==1)
    f.capture('Slash Enter directly opens model editing; parent input stays separate')
    f.send(b'\x1b');f.pump(.12);clear();f.send('点击编辑草稿');f.pump(.2);f.send(b'\x1b[1;2A');f.pump(.15)
    rows=f.screen().splitlines();row=next(index for index,line in enumerate(rows) if '点击编辑草稿' in line)
    f.send(f'\x1b[<0;{f.cols-8};{row+1}M\x1b[<0;{f.cols-8};{row+1}m');f.pump(.12);f.send('!')
    f.wait(lambda:saved(original).get('draft')=='点击编辑草稿!','clicking composer restores input focus without sending')
    f.capture('Clicking the visible composer permits editing without sending')
    check('click input edits only the draft',len(f.calls())==1 and f.visible('点击编辑草稿!'))
    clear();f.send('/not-a-real-command\r');f.wait(lambda:f.visible('未知命令'),'unknown command failure visible')
    f.capture('A real local command error keeps its draft and readable cause')
    f.pump(8.8);f.capture('Failure remains visible after the old eight-second expiry window')
    check('action failure remains until correction, rather than expiring',f.visible('未知命令') and f.visible('/not-a-real-command') and len(f.calls())==1)
    clear()
    menu('connect');f.wait(lambda:f.visible('添加 API 连接'),'connection chooser');f.send('添加\r')
    f.wait(lambda:f.visible('API 模型'),'native provider picker');f.send('openai API 模型\r')
    f.wait(lambda:f.visible('› 连接名称'),'API native form')
    for value,next_label in zip(('team_api','fixture-v2',f.api_a_url),('模型名称','API endpoint','API key')):
        f.send(b'\x01\x0b');f.send(value+'\r')
        f.wait(lambda:f.visible('› '+next_label) and bool(f.screen()) and f.cursor_visible,'next visible native form field: '+next_label)
    secret='synthetic-local-only-no-reuse-91557'
    f.send(secret);f.pump(.2);f.capture('Before saving: connection, model and endpoint remain visible beside a masked API key')
    check('final API step allows checking the actual connection before saving',all(f.visible(value) for value in ('team_api','fixture-v2',f.api_a_url)))
    check('API key does not enter terminal text or conversation',secret not in f.output.decode(errors='replace') and secret not in json.dumps(f.events()))
    # The parent draft is empty here. A former wide caret must not leave a
    # reversed trailing cell that looks like a second caret behind the form.
    main_start=max(26,min(32,f.cols//4)) if f.cols>=80 else 0
    main_rows=[''.join(row[main_start:]) for row in f.terminal_cells]
    draft_top=next(row for row,line in enumerate(main_rows) if '┌ 草稿 ' in line)
    draft_bottom=next(row for row in range(draft_top+1,len(main_rows)) if main_rows[row].startswith('└'))
    reversed_parent=[run for run in f.cell_styles if draft_top<run['row']<draft_bottom
        and run['start']>=main_start and 'reverse' in run['attributes']]
    check('inactive empty parent draft has no second reversed caret behind the API form',not reversed_parent,
        {'draft_rows':[draft_top+1,draft_bottom-1],'reverse_runs':reversed_parent})
    f.send(b'\x1b');f.pump(.12)
    # History runs in the existing empty B. A intentionally still owns an
    # unanswered work input; using A would also require scripting its resumption.
    f.send(b'\x1b[1;2D\x1b[H\x1b[B\r');f.pump(.25);f.send(b'\x1b[1;2B')
    f.observed_session=second
    original_ui=json.dumps(saved(original),ensure_ascii=False)
    check('separate history work leaves original question and its answer draft paused',f.state(original)['paused'] and answer in original_ui and question['id'] in original_ui)
    for index in range(12):
        marker='历史回归 '+str(index).zfill(2)+'：复查登录边界，保留会话约束。'
        f.send(marker+'\r')
        def delivered():
            events=f.events(second)
            inputs={e['id'] for e in events if e['kind']=='input' and marker in json.dumps(e.get('data',{}),ensure_ascii=False)}
            return any(e['kind']=='delivery' and e.get('reply_to') in inputs for e in events)
        f.wait(delivered,'actual local history turn '+str(index))
    f.quit();f.restart(second);f.pump(.2);f.capture('Twelve real local turns: reopening starts with a bounded recent window')
    state=f.state(second)
    check('completed Idle session keeps its core pause flag without offering empty resumption',state['paused'] and all(job['state']=='Idle' for job in state['jobs'].values())
        and not main_has('Ctrl+R') and not main_has('已暂停') and len(f.calls())==13)
    f.send(b'\x1b[1;2A')
    for _ in range(8):
        f.send(b'\x1b[H');f.pump(.15)
        if main_has('历史回归 00'): break
    f.capture('Natural reading reaches earlier persistent history without a hidden slash command')
    check('reading at the top loads older persisted conversation',main_has('历史回归 00'))
    check('navigation and historical loading make no new model requests',len(f.calls())==13,{'requests':len(f.calls()),'events':len(f.events(second))})
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
    f.send('/model ollama:fixture-next\r')
    f.pump(.3)
    f.wait(lambda: any(d.get('draft') == '/model ollama:fixture-next' for d in f.saved_drafts()), 'model cannot silently change provider; original command draft retained')
    f.send(b'\x01\x0b')
    f.send('/model openai:fixture-native-next\r')
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
        with closing(sqlite3.connect(f.data / 'sessions.sqlite3')) as connection:
            return [row[0] for row in connection.execute('SELECT id FROM sessions')]
    f.wait(lambda: len(session_ids()) == 2, 'new session created')
    assert original in session_ids()
    f.send('/sessions\r')  # Legacy alias stays parseable; sidebar is the normal entry.
    f.wait(lambda: f.visible('1 轮 · 已暂停'), 'session picker exposes meaningful source status')
    f.send('已暂停\r')
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
    f.wait(lambda: '/new' in f.screen() and '/connect' in f.screen(), 'five public slash candidates immediately visible', timeout=3)
    assert set(re.findall(r'/[a-z]+\b',f.screen())) == {'/new','/model','/connect','/help','/quit'}, 'public slash menu is not the five common entries'
    assert not re.search(r'/(?:status|sessions|audit|reconcile|details|export)\b',f.screen()), 'legacy operations leak into public slash candidates'
    f.capture("Typing / shows commands without opening a separate panel")
    f.send('he')
    f.wait(lambda: '/help' in f.screen(), 'slash candidates filter')
    f.send('\r')
    f.wait(lambda: f.visible('帮助') and f.visible('Shift'), 'Enter directly executes selected help action')
    assert not f.calls() and not any(e.get('kind') == 'input' for e in f.events()), 'help reached model'
    f.capture('Candidate Enter opens help in one step without model work')
    f.send(b'\x1b');f.pump(.12)
    f.send(b'\x01\x0b')
    f.send('/he')
    f.send('\t')
    f.pump(.2)
    assert not f.calls() and not any(e.get('kind') == 'input' for e in f.events()), 'Tab submitted command to agent'
    f.wait(lambda: any(d.get('draft', '').strip() == '/help' for d in f.saved_drafts()), 'Tab retains completed command draft')
    f.capture("Tab inserts /help; no model request")
    f.send('\r')
    f.wait(lambda:f.visible('帮助') and f.visible('Shift'),'completed public help command opens its reader')
    assert not f.calls() and not any(e.get('kind') == 'input' for e in f.events()), 'help reached model'
    f.capture("Help opens through the completed public command")
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
    f.send(b'\x1b[1;6D\x1b[1;6D')  # Ctrl+Shift+Left selects editor text before global stop.
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
        f.send('核查\r')  # Contextual action appears only for actual unknown writes.
        f.wait(lambda: '结果未知' in f.screen(), 'unknown write picker identifies interrupted shell')
        f.send('\r')
        f.wait(lambda: '核查表单' in f.screen(), 'reconciliation uses a separate form')
        f.send(b'\x04')
        f.wait(lambda:'核查证据' in f.screen(),'reconciliation evidence reader opens')
        if 'LIVE_STDOUT_READY' not in f.screen():
            f.send(b'\x1b[6~')  # Narrow viewport: read the actual parameter section.
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
            assert any(d.get('draft')==expected and d.get('reply_to') is None for d in f.saved_drafts()), 'cancel changed the durable input target'
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
    with closing(sqlite3.connect(f.data / 'sessions.sqlite3')) as connection:
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
    assert f.terminal_cells[1][-1] == '─' and '阅读：' in resized[-1], 'application did not redraw header/footer at the real resized terminal geometry'
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
    f.send('核查\r')
    f.wait(lambda: '结果未知' in f.screen(), 'unknown write list opened from reply draft')
    f.send('\r')
    f.wait(lambda: '核查表单' in f.screen(), 'separate reconciliation form opened')
    f.send('CANCEL_THIS_NOTE')
    f.send(b'\x04')
    f.wait(lambda:'核查证据' in f.screen(),'reply reconciliation evidence reader opens')
    if 'REPLY_WORKER_STARTED' not in f.screen():
        f.send(b'\x1b[6~')
    f.wait(lambda: 'REPLY_WORKER_STARTED' in f.screen(), 'worker evidence opened')
    f.capture('Reconcile evidence overlays a separate note, retaining the original reply target')
    f.send(b'\x1b')
    f.pump(.2)
    f.send(b'\x1b')
    f.pump(.2)
    f.send('!')
    expected = 'ANSWER_!FIRST_LINE\nANSWER_SECOND_LINE'
    f.wait(lambda: any(d.get('draft') == expected for d in f.saved_drafts()), 'cancel restores reply draft at original insertion cursor')
    assert any(d.get('draft')==expected and d.get('reply_to')==question['id'] for d in f.saved_drafts()), 'reconciliation cancel lost the saved specific reply target'
    assert f.visible('回复：'+question['data']['question']), 'readable reply question is not visible after reconciliation cancel'
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
    parser.add_argument('--case', choices=['paste','concurrent','pause','question','failure','stream','stale','stream-stop','commands','completion','sessions','signal','unicode','interactive-editor','editor-signal','slash-inline','multiline-undo','shell-live','exit-resume','reading-detail','persistent-search','question-target','question-independent','reader-delivery','tool-density','reconcile-reply','feedback-flow','workspace-flow','sidebar-flow','product-flow'])
    parser.add_argument("--evidence-dir", type=Path)
    parser.add_argument('--size', default='80x24', choices=['80x24', '120x40'], help='real terminal columns x rows')
    parser.add_argument('--keep-going', action='store_true', help='record every selected scenario, then fail if any failed')
    parser.add_argument('--observe-feedback', action='store_true', help='record old feedback-flow failures as observations, never as a passing gate')
    args = parser.parse_args()
    verify_vt_replay()
    cases = [
        ('product-flow', [{'delay_seconds':1.8,'output':[tool('ask_user',{'question':'失败回调是否继续返回原地址？'})]}]+[
            {'match_last_user_contains':'历史回归 '+str(index).zfill(2),'text':
                '第 '+str(index).zfill(2)+' 轮复查结果\n\n'
                '保留现有 cookie 名称；失败回调仍返回原地址。\n'
                '- 增加过期状态回归。\n- 下一轮检查请求取消边界。'} for index in range(12)],product_flow),
        ('sidebar-flow', [{'contains':['SIDEBAR_REFRESH'],'delay_seconds':4,'text':'SIDEBAR_BACKGROUND_DELIVERED'}],sidebar_flow),
        ('workspace-flow', {'api_a':[
            {'match_job_title':'Conversation','contains':['WORKSPACE_SESSION_A'],'output':[tool('job_send',{'title':'NOT_A_SESSION_JOB','message':'Prove jobs are not sessions.'})]},
            {'match_job_title':'Conversation','output':[tool('ask_user',{'question':'WORKSPACE_REPLY_QUESTION: choose a format.'})]},
            {'match_job_title':'NOT_A_SESSION_JOB','text':'CHILD_JOB_SETTLED'},
            {'contains':['WORKSPACE_API_A'],'text':'API_A_ROUTE_CONFIRMED'}], 'api_b':[
            {'contains':['WORKSPACE_API_B'],'text':'API_B_ROUTE_CONFIRMED'},
            {'contains':['WORKSPACE_NATIVE_MODEL'],'text':'NATIVE_MODEL_ROUTE_CONFIRMED'},
            {'contains':['WORKSPACE_RESTART_DEFAULT'],'text':'RESTART_DEFAULT_ROUTE_CONFIRMED'},
            {'contains':['WORKSPACE_SUBSCRIPTION'],'text':'SYNTHETIC_SUBSCRIPTION_ROUTE_CONFIRMED'}]},workspace_flow),
        ('feedback-flow', [
            {'match_job_title':'Conversation','delay_seconds':1.6,'output':[tool('job_send',{'title':'Worker','message':'Run the silent worker.'})]},
            {'match_job_title':'Conversation','text':'FLOW_MODEL_PREVIEW\nROOT_BACKGROUND_DELIVERY','delta_chunk_chars':14,'event_delay_seconds':.1,'event_delays':{'response.completed':1.6}},
            {'match_job_title':'Worker','output':[tool('shell',{'command':'while [ ! -e release-feedback-worker ]; do sleep 0.05; done; touch FEEDBACK_WORKER_DONE','timeout_seconds':30})]},
            {'match_job_title':'Conversation','contains':['FRONT_QUICK_REPLY'],'text':'INDEPENDENT_FOREGROUND_DELIVERED'},
            {'match_job_title':'Conversation','delay_seconds':.1,'text':'ROOT_CONTINUES_WAITING'},
            {'match_job_title':'Worker','text':'WORKER_FINAL_MUST_NOT_APPEAR'}],feedback_flow),
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
                observe = args.observe_feedback and name == 'feedback-flow'
                run_case(args.binary.resolve(), name, turns, action, size=(rows, cols), evidence_dir=args.evidence_dir, observe=observe)
                results.append({'scenario': name, 'status': 'OBSERVED' if observe else 'PASS'})
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
