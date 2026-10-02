#!/usr/bin/env python3
"""A real subscription model works through the production TUI in an isolated repo.
Uses the existing Codex login through BONE; never copies or prints credentials.
"""
import argparse
import hashlib
import html
import json
import os
from pathlib import Path
import pty
import subprocess
import sys
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[4]
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / 'tests'))
from tui_pty import Fixture

SOURCE = '''def authorize(token, now):
    if not isinstance(token, dict):
        return 401, "unauthorized"
    subject = token.get("subject")
    expiry = token.get("expires_at")
    if not isinstance(subject, str) or not subject:
        return 401, "unauthorized"
    if isinstance(expiry, bool) or not isinstance(expiry, (int, float)):
        return 401, "unauthorized"
    if expiry > now:
        return 401, "unauthorized"
    return 200, subject
'''
TESTS = '''import time
import unittest
from auth import authorize

class AuthorizationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        print("AUTH_CHECK_RUNNING", flush=True)
        time.sleep(12)

    def test_expired(self): self.assertEqual(authorize({"subject":"alice","expires_at":9},10)[0],401)
    def test_boundary(self): self.assertEqual(authorize({"subject":"alice","expires_at":10},10)[0],401)
    def test_valid(self): self.assertEqual(authorize({"subject":"alice","expires_at":11},10),(200,"alice"))
    def test_future(self): self.assertEqual(authorize({"subject":"bob","expires_at":1000},10),(200,"bob"))
    def test_none(self): self.assertEqual(authorize(None,10)[0],401)
    def test_not_dict(self): self.assertEqual(authorize("bad",10)[0],401)
    def test_empty_subject(self): self.assertEqual(authorize({"subject":"","expires_at":11},10)[0],401)
    def test_missing_expiry(self): self.assertEqual(authorize({"subject":"alice"},10)[0],401)
    def test_string_expiry(self): self.assertEqual(authorize({"subject":"alice","expires_at":"11"},10)[0],401)
    def test_boolean_expiry(self): self.assertEqual(authorize({"subject":"alice","expires_at":True},10)[0],401)
    def test_negative_expiry(self): self.assertEqual(authorize({"subject":"alice","expires_at":-1},10)[0],401)
    def test_fractional_expiry(self): self.assertEqual(authorize({"subject":"alice","expires_at":10.5},10),(200,"alice"))
'''

class RealTerminal(Fixture):
    def __init__(self, binary):
        self.root = Path(tempfile.mkdtemp(prefix='bone-design-live-'))
        self.data = self.root / 'data'
        self.workspace = self.root / 'workspace'
        self.data.mkdir(); self.workspace.mkdir()
        (self.data / 'config.toml').write_text('default_profile="subscription"\n[profiles.subscription]\nmodel="chatgpt:gpt-6-luna"\nreuse_codex_login=true\n')
        (self.workspace / 'auth.py').write_text(SOURCE)
        (self.workspace / 'test_auth.py').write_text(TESTS)
        (self.workspace / 'README.md').write_text('# Token validation\nRun: python3 -B -m unittest -v\nKeep authorize(token, now) API. Expired/boundary tokens are unauthorized.\n')
        subprocess.run(['git','init','--quiet'],cwd=self.workspace,check=True)
        subprocess.run(['git','add','.'],cwd=self.workspace,check=True)
        subprocess.run(['git','-c','user.name=BONE Acceptance','-c','user.email=acceptance@localhost','commit','--quiet','-m','fixture baseline'],cwd=self.workspace,check=True)
        self.master,self.slave=pty.openpty()
        self.original=termios.tcgetattr(self.slave)
        self.rows,self.cols=24,80
        self.resize(self.rows,self.cols)
        self.output=bytearray(); self.answered_queries=0; self.frames=[]
        self.binary=binary
        self.binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest()
        self.env=dict(os.environ,TERM='xterm-256color')
        self.proc=subprocess.Popen([str(binary),'--data-dir',str(self.data),'--profile','subscription','--model','chatgpt:gpt-6-luna','tui','--workspace',str(self.workspace),'--max-parallel','1','--max-calls','24','--timeout-seconds','600'],stdin=self.slave,stdout=self.slave,stderr=self.slave,env=self.env,start_new_session=True)
    def calls(self): return [e for e in self.events() if e['kind']=='model_started']
    def close(self):
        if self.proc.poll() is None: self.proc.terminate(); self.proc.wait(timeout=8)
        os.close(self.master); os.close(self.slave)

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,default=ROOT/'target/debug/bone')
    parser.add_argument('--output-dir', type=Path, default=HERE,
                        help='Keep a new execution separate from the original evidence.')
    args=parser.parse_args()
    output_dir = args.output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    f=RealTerminal(args.binary.resolve())
    status='FAIL'; error=None; checks={}; timeline=[]
    def stage(name):
        f.capture(name); timeline.append({'stage':name,'elapsed_seconds':round(time.monotonic()-started,2)}); print(name,flush=True)
    def wait(predicate,label):
        f.wait(lambda: predicate() or any(e['kind']=='failure' for e in f.events()),label,timeout=180)
        failures=[e for e in f.events() if e['kind']=='failure']
        if failures: raise AssertionError('Model/runtime failure: '+str(failures[-1]['data']))
    started=time.monotonic()
    try:
        f.wait(lambda:b'\x1b[?1049h' in f.output,'startup')
        stage('01 新版生产 TUI / 80×24')
        f.send('修复过期 token 被接受的问题。先运行 python3 -B -m unittest -v 确认失败，再定位源码。保留 authorize(token, now) API，不修改测试，不提交 Git。\r')
        wait(lambda:any(e['kind']=='tool_started' and e['data'].get('tool_name')=='shell' for e in f.events()),'first real verification starts')
        stage('02 Agent 真实检查正在执行')
        f.send('只解释原因，先不要改文件。请用 ask_user 问我过期时希望返回哪个状态码，等我回答再改。\r')
        wait(lambda:any(e['kind']=='question' for e in f.events()),'explicit question after steering')
        question=next(e for e in reversed(f.events()) if e['kind']=='question')
        checks['source_unchanged_before_answer']=(f.workspace/'auth.py').read_text()==SOURCE
        assert checks['source_unchanged_before_answer'],'source modified before explicit authorization'
        stage('03 插话已纳入 / 问题到达 / 草稿仍默认新要求')
        f.send('/reply '+question['id']+'\r')
        f.pump(.3)
        stage('04 显式选择回复目标')
        f.send('返回 401；现在允许修改 auth.py，并运行原有12项测试验证。不要改测试、不要提交Git。\r')
        wait(lambda:any(e['kind']=='delivery' and e.get('reply_to') and e.get('revision',0)>=f.state()['revision'] for e in f.events()) or (any(e['kind']=='delivery' for e in f.events()) and (f.workspace/'auth.py').read_text()!=SOURCE),'real fix delivery')
        # Wait until the latest input has a durable final delivery and no live call.
        f.wait(lambda:all(j['state'] in ['Idle','Waiting','Paused','Closed'] for j in f.state()['jobs'].values()),'execution settles',timeout=90)
        stage('05 修复交付 / 检查与文件证据')
        f.send('/delivery\r'); f.pump(.4)
        stage('06 一次命令直达完整交付')
        f.resize(40,120); f.pump(.2)
        f.wait(lambda: any(line.count('─') >= 120 for line in f.screen().splitlines()) and '阅读：' in f.screen().splitlines()[-1], 'production terminal redraws at 120x40')
        stage('07 同一交付 / 120×40')
        f.send('\x1b'); f.pump(.2)
        stage('08 返回原对话阅读位置')
        checks['tests_unchanged']=(f.workspace/'test_auth.py').read_text()==TESTS
        verification=subprocess.run(['python3','-B','-m','unittest','-v'],cwd=f.workspace,capture_output=True,text=True)
        (output_dir/'independent-tests.txt').write_text(verification.stdout+verification.stderr)
        checks['independent_tests_pass']=verification.returncode==0
        checks['tools_have_job'] = all(e.get('job_id') for e in f.events() if e['kind'] in ('model_started','tool_started','tool_result'))
        checks['explicit_reply_link']=any(e['kind']=='input' and e.get('reply_to')==question['id'] for e in f.events())
        diff=subprocess.run(['git','diff','--no-ext-diff','--no-textconv'],cwd=f.workspace,capture_output=True,text=True,check=True).stdout
        (output_dir/'final.diff').write_text(diff)
        checks['modified_source_only']='test_auth.py' not in diff and 'auth.py' in diff
        assert all(checks.values()),checks
        status='PASS'
    except Exception as exc:
        error=str(exc); stage('失败或需要进一步核查')
    finally:
        events=f.events()
        if f.proc.poll() is None:f.quit()
        checks['terminal_restored']=termios.tcgetattr(f.slave)==f.original
        summary={'status':status,'error':error,'scope':'Production TUI, real gpt-6-luna subscription calls, reused existing Codex login; isolated Python repo','binary_sha256':f.binary_sha256,'workspace':str(f.workspace),'checks':checks,'timeline':timeline,'model_calls':len(f.calls()),'tool_calls':sum(e['kind']=='tool_started' for e in events),'frames':len(f.frames),'cost':None}
        (output_dir/'summary.json').write_text(json.dumps(summary,ensure_ascii=False,indent=2)+'\n')
        (output_dir/'screens.json').write_text(json.dumps(f.frames,ensure_ascii=False,indent=2)+'\n')
        (output_dir/'events.json').write_text(json.dumps(events,ensure_ascii=False,indent=2)+'\n')
        (output_dir/'auth.py').write_text((f.workspace/'auth.py').read_text())
        (output_dir/'test_auth.py').write_text(TESTS)
        frames=''.join('<section><h2>'+html.escape(x['step'])+'</h2><pre>'+html.escape(x['screen'])+'</pre></section>' for x in f.frames)
        records=''.join('<details><summary>'+html.escape(f'{i+1:03} · {event["kind"]} · Job {event.get("job_id") or "Session"} · reply_to {event.get("reply_to") or "—"}')+'</summary><pre>'+html.escape(json.dumps(event,ensure_ascii=False,indent=2))+'</pre></details>' for i,event in enumerate(events))
        (output_dir/'report.html').write_text('<!doctype html><meta charset="utf-8"><title>BONE real model</title><style>body{max-width:1150px;margin:32px auto;padding:20px;font:15px system-ui;background:#f3f1ea;color:#272d29}pre{font:13px/1.5 Menlo,monospace;white-space:pre;overflow:auto;padding:18px;background:#18201b;color:#dbe2da}section{margin:30px 0}details{border-top:1px solid #c9d2c7;padding:12px 0}summary{cursor:pointer}</style><h1>真实模型 / 生产 TUI / 鉴权修复</h1><p>'+html.escape(summary['scope'])+'</p><pre>'+html.escape(json.dumps(summary,ensure_ascii=False,indent=2))+'</pre>'+frames+'<h2>持久执行记录：对话、Job 与行动</h2><p>按 SQLite 事件顺序，点击展开原生记录；费用未知。记录仅来自本次隔离仓库任务。</p>'+records)
        print(json.dumps(summary,ensure_ascii=False),flush=True);f.close()
    return 0 if status=='PASS' else 1

if __name__=='__main__':sys.exit(main())
