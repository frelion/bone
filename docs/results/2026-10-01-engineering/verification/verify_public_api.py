#!/usr/bin/env python3
"""Supplementary external public API / raw audit / live execution lock checks."""
import argparse, copy, fcntl, hashlib, json, sqlite3, subprocess, tempfile
from pathlib import Path
from verify_cli import fixture, event, insert, expect, run

def main():
    ap=argparse.ArgumentParser();ap.add_argument('--caller',type=Path,required=True);ap.add_argument('--binary',type=Path,required=True);ap.add_argument('--output',type=Path,required=True);a=ap.parse_args();checks=[]
    def check(name,fn):
        try:fn();checks.append({'name':name,'passed':True})
        except Exception as e:checks.append({'name':name,'passed':False,'error':str(e)})
    with tempfile.TemporaryDirectory(prefix='bone-history-api-') as t:
        data=Path(t)/'data';es=fixture(data)
        c=sqlite3.connect(data/'sessions.sqlite3')
        raw=[]
        for n,kind in enumerate(['input','model_message','tool_result','summary']):
            e=event('primary',f'raw-{n}',200+n);e['kind']=kind
            e['data']={'message':{'role':'user','content':'EXACT 原文🙂 '+kind},'response':{'choice':[{'type':'text','text':'native response '+kind}]},'stream_items':[{'part':'large-original'}],'covered_ids':['raw-0','raw-1'],'retained':'visible projection'}
            metadata=copy.deepcopy(e);metadata['data']={'retained':'visible projection'}
            c.execute('INSERT INTO events(id,session_id,revision,payload,metadata) VALUES(?,?,0,?,?)',(e['id'],'primary',json.dumps(e,ensure_ascii=False),json.dumps(metadata,ensure_ascii=False)));raw.append(e)
        c.commit();c.close()
        def call(sid='primary',after='-',limit=1,success=True):
            p=subprocess.run([str(a.caller.resolve()),str(data),sid,after,str(limit)],capture_output=True,text=True,timeout=20)
            assert (p.returncode==0)==success,(p.args,p.stdout,p.stderr)
            return json.loads(p.stdout) if success else None
        check('api_first_page',lambda:expect(call(),es[:1],True))
        check('api_original_payload_not_metadata',lambda:expect(call(after=es[-1]['id'],limit=4),raw,False))
        check('cli_original_payload_not_metadata',lambda:expect(json.loads(run(a.binary,data,'primary','--after',es[-1]['id'],'--limit','4','--json').stdout),raw,False))
        check('api_max_limit',lambda:expect(call(limit=1000),es+raw,False))
        check('api_empty_page',lambda:expect(call(after='raw-3'),[],False))
        check('api_unknown_session',lambda:call(sid='absent',success=False))
        check('api_unknown_cursor',lambda:call(after='absent',success=False))
        check('api_cross_session_cursor',lambda:call(after='o-0000',success=False))
        for limit in [0,1001,2**64-1]:check('api_invalid_limit_'+str(limit),lambda limit=limit:call(limit=limit,success=False))
        check('api_lookahead_bad_payload',lambda:expect(call(sid='damaged'),[event('damaged','good',0)],True))
        check('api_requested_bad_payload',lambda:call(sid='damaged',after='good',success=False))
        # Use precisely the same advisory lock file / exclusive flock as Store::acquire_session.
        locks=(data/'sessions.sqlite3').resolve().with_suffix('.session-locks');locks.mkdir(exist_ok=True)
        with (locks/'primary.lock').open('a+b') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
            check('api_audit_while_execution_locked',lambda:expect(call(),es[:1],True))
            check('cli_audit_while_execution_locked',lambda:expect(json.loads(run(a.binary,data,'primary','--limit','1','--json').stdout),es[:1],True))
        check('no_config_created',lambda:assert_no_config(data))
        c=sqlite3.connect(data/'sessions.sqlite3');rev=c.execute('SELECT revision,snapshot FROM sessions WHERE id=?',('primary',)).fetchone();c.close()
        check('snapshot_unchanged',lambda:assert_snapshot(rev))
    result={'passed':all(x['passed'] for x in checks),'checks':checks,'caller':str(a.caller.resolve()),'caller_sha256':hashlib.sha256(a.caller.read_bytes()).hexdigest(),'scope':'additional contract checks; frozen CLI verifier remains unchanged'}
    a.output.write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2));return 0 if result['passed'] else 1

def assert_no_config(data):assert not (data/'config.toml').exists()
def assert_snapshot(rev):
    state=json.loads(rev[1]);assert rev[0]==state['revision']==0 and state['jobs']=={} and state['budgets']=={}
if __name__=='__main__':raise SystemExit(main())
