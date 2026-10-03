#!/usr/bin/env python3
"""Reopen the completed recorded session; no new inference and no credential copy.
The original harness stopped at an obsolete footer assertion after delivery.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import pty
import subprocess
import sys
import termios
import time

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[3]
spec=importlib.util.spec_from_file_location('live_runner',ROOT/'docs/results/2026-10-02-tui-design/live-software/run.py')
runner=importlib.util.module_from_spec(spec);spec.loader.exec_module(runner)
summary=json.loads((HERE/'summary.json').read_text())
(HERE/'initial-summary.json').write_text(json.dumps({**summary,'error':'Legacy redraw gate expected 阅读：; current modal footer is 阅读层：. Real model delivered before this harness assertion.'},ensure_ascii=False,indent=2)+'\n')
frames=json.loads((HERE/'screens.json').read_text())
events=json.loads((HERE/'events.json').read_text())
f=runner.RealTerminal.__new__(runner.RealTerminal)
f.root=Path(summary['workspace']).parent;f.data=f.root/'data';f.workspace=f.root/'workspace'
f.master,f.slave=pty.openpty();f.original=termios.tcgetattr(f.slave);f.rows,f.cols=24,80
f.resize(f.rows,f.cols);f.output=bytearray();f.answered_queries=0;f.frames=[];f.request_routes={}
f.binary=ROOT/'target/debug/bone';f.binary_sha256=hashlib.sha256(f.binary.read_bytes()).hexdigest()
f.env={**os.environ,'TERM':'xterm-256color'};f.env.pop('NO_COLOR',None)
session=events[0]['session_id']
f.proc=subprocess.Popen([str(f.binary),'--data-dir',str(f.data),'tui','--session',session,'--workspace',str(f.workspace)],stdin=f.slave,stdout=f.slave,stderr=f.slave,env=f.env,start_new_session=True)
before=len(f.calls());started=time.monotonic()
try:
    f.wait(lambda:b'\x1b[?1049h' in f.output,'reopen completed session')
    f.send('/delivery\r');f.wait(lambda:'交付 ' in f.screen() and '阅读层：' in f.screen().splitlines()[-1],'durable delivered result')
    f.capture('09 同一持久会话重新打开 / 80×24 / 彩色')
    f.resize(40,120)
    f.wait(lambda:any(line.count('─')>=90 for line in f.screen().splitlines()) and '阅读层：' in f.screen().splitlines()[-1],'actual 120×40 redraw with session sidebar')
    f.capture('10 同一交付 / 120×40 / 新页脚与侧栏')
    f.send('\x1b');f.pump(.2);f.capture('11 Esc 返回原阅读位置')
    verification=subprocess.run(['python3','-B','-m','unittest','-v'],cwd=f.workspace,capture_output=True,text=True)
    (HERE/'independent-tests.txt').write_text(verification.stdout+verification.stderr)
    diff=subprocess.run(['git','diff','--no-ext-diff','--no-textconv'],cwd=f.workspace,capture_output=True,text=True,check=True).stdout
    (HERE/'final.diff').write_text(diff)
    summary['checks'].update(tests_unchanged=(f.workspace/'test_auth.py').read_text()==runner.TESTS,
        independent_tests_pass=verification.returncode==0,
        tools_have_job=all(e.get('job_id') for e in events if e['kind'] in ('model_started','tool_started','tool_result')),
        explicit_reply_link=any(e['kind']=='input' and e.get('reply_to') in {q['id'] for q in events if q['kind']=='question'} for e in events),
        modified_source_only='test_auth.py' not in diff and 'auth.py' in diff,
        recovery_has_no_model_call=len(f.calls())==before,
        terminal_resize_120x40=True)
    assert all(summary['checks'].values()),summary['checks']
    summary.update(status='PASS',error=None,recovery='Obsolete footer assertion repaired; same completed session reopened for UI and independent tests, no model replay.',recovery_elapsed_seconds=round(time.monotonic()-started,2),recovery_binary_sha256=f.binary_sha256)
finally:
    if f.proc.poll() is None:f.quit()
    summary['checks']['recovery_terminal_restored']=termios.tcgetattr(f.slave)==f.original
    f.close()
frames+=f.frames;summary['frames']=len(frames)
(HERE/'screens.json').write_text(json.dumps(frames,ensure_ascii=False,separators=(',',':'))+'\n')
(HERE/'summary.json').write_text(json.dumps(summary,ensure_ascii=False,indent=2)+'\n')
print(json.dumps(summary,ensure_ascii=False))
