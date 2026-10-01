#!/usr/bin/env python3
"""Independent CLI contract verifier. Never edits candidate source or requests a model."""
import argparse, hashlib, json, os, sqlite3, subprocess, tempfile
from pathlib import Path

SCHEMA = '''CREATE TABLE sessions(id TEXT PRIMARY KEY,revision INTEGER NOT NULL CHECK(revision>=0),snapshot TEXT NOT NULL);CREATE TABLE events(sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,session_id TEXT NOT NULL REFERENCES sessions(id),call_id TEXT,revision INTEGER NOT NULL CHECK(revision>=0),payload TEXT NOT NULL,job_id TEXT,metadata TEXT);CREATE INDEX events_session_order ON events(session_id,sequence);CREATE INDEX events_call ON events(session_id,call_id);CREATE INDEX events_job_order ON events(session_id,job_id,sequence);'''

def event(sid, eid, n):
    return dict(id=eid,session_id=sid,job_id=None,call_id=None,reply_to=None,root_input=None,kind='audit_fixture',revision=0,data={'n':n,'text':'原文🙂','nested':{'source_ids':['not-a-live-reference'],'message':'literal'}},timestamp=str(900000-n))

def fixture(path):
    path.mkdir(); c=sqlite3.connect(path/'sessions.sqlite3'); c.executescript(SCHEMA)
    for sid in ['primary','other','empty','one','damaged']:
        state=dict(id=sid,workspace=str(path),focus=None,revision=0,pending_inputs=[],jobs={},budgets={},paused=False,unknown_writes={})
        c.execute('INSERT INTO sessions VALUES(?,0,?)',(sid,json.dumps(state)))
    originals=[]
    for n in range(103):
        e=event('primary',f'p-{102-n:04}',n); originals.append(e); insert(c,e)
        insert(c,event('other',f'o-{n:04}',n))
    insert(c,event('one','only',0))
    insert(c,event('damaged','good',0))
    bad=event('damaged','bad',1)
    c.execute('INSERT INTO events(id,session_id,revision,payload,metadata) VALUES(?,?,0,?,?)',('bad','damaged','{not json',json.dumps(bad)))
    c.commit(); c.close(); return originals

def insert(c,e):
    c.execute('INSERT INTO events(id,session_id,revision,payload,metadata) VALUES(?,?,0,?,?)',(e['id'],e['session_id'],json.dumps(e,ensure_ascii=False),json.dumps(e,ensure_ascii=False)))

def run(binary,data,sid,*args,ok=True):
    env=os.environ.copy()
    for key in ['BONE_MODEL','BONE_DATA_DIR','OPENAI_API_KEY','COHERE_API_KEY']:
        env.pop(key,None)
    p=subprocess.run([str(binary),'--data-dir',str(data),'history',sid,*args],capture_output=True,text=True,env=env,timeout=20)
    if ok: assert p.returncode==0, (p.args,p.stderr)
    else: assert p.returncode!=0,(p.args,p.stdout)
    return p

def page(binary,data,sid,limit=None,after=None):
    args=['--json']
    if limit is not None: args+=['--limit',str(limit)]
    if after is not None: args+=['--after',after]
    return json.loads(run(binary,data,sid,*args).stdout)

def expect(p,events,more):
    assert p['events']==events, 'Event contents/order mismatch'
    assert p['next_cursor']==(events[-1]['id'] if events else None),p
    assert p['has_more'] is more,p

def main():
    ap=argparse.ArgumentParser();ap.add_argument('--binary',type=Path,required=True);ap.add_argument('--output',type=Path,required=True);a=ap.parse_args()
    binary=a.binary.resolve(); results=[]
    with tempfile.TemporaryDirectory(prefix='bone-history-verifier-') as temp:
        data=Path(temp)/'data'; es=fixture(data)
        def check(name,fn):
            try: fn();results.append({'name':name,'passed':True})
            except Exception as e:results.append({'name':name,'passed':False,'error':str(e)})
        check('legacy_array',lambda:expect_legacy(binary,data,es))
        check('first_page_raw_order',lambda:expect(page(binary,data,'primary',2),es[:2],True))
        check('default_100_after',lambda:expect(page(binary,data,'primary',after=es[0]['id']),es[1:101],True))
        check('exact_boundary',lambda:expect(page(binary,data,'primary',3,es[99]['id']),es[100:],False))
        check('last_cursor_empty',lambda:expect(page(binary,data,'primary',1,es[-1]['id']),[],False))
        check('empty',lambda:expect(page(binary,data,'empty',1),[],False))
        check('one',lambda:expect(page(binary,data,'one',1),[event('one','only',0)],False))
        check('max_limit',lambda:expect(page(binary,data,'primary',1000),es,False))
        check('mixed_limit_full_walk',lambda:walk(binary,data,es))
        for sid,args in [('missing',['--limit','1']),('primary',['--after','absent']),('primary',['--after','o-0000']),('primary',['--after',"x' OR 1=1 --"])]:
            check('reject_'+sid+'_'+args[-1],lambda sid=sid,args=args:run(binary,data,sid,*args,'--json',ok=False))
        for v in ['0','1001','-1','abc']:
            check('invalid_limit_'+v,lambda v=v:run(binary,data,'primary','--limit',v,'--json',ok=False))
        check('lookahead_does_not_parse_damage',lambda:expect(page(binary,data,'damaged',1),[event('damaged','good',0)],True))
        check('requested_damage_fails',lambda:run(binary,data,'damaged','--limit','1','--after','good','--json',ok=False))
        check('append_and_reopen',lambda:append_check(binary,data,es))
        check('text_cursor',lambda:text_check(binary,data,es[0]['id']))
        check('snapshot_unchanged_no_config',lambda:snapshot_check(data))
    output={'binary':str(binary),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'passed':all(x['passed'] for x in results),'checks':results}
    a.output.write_text(json.dumps(output,ensure_ascii=False,indent=2)+'\n');print(json.dumps(output,ensure_ascii=False,indent=2));return 0 if output['passed'] else 1

def expect_legacy(b,d,es):
    assert json.loads(run(b,d,'primary','--json').stdout)==es

def walk(b,d,es):
    got=[];after=None
    for i in range(200):
        p=page(b,d,'primary',[1,7,13][i%3],after);got+=p['events']
        if not p['has_more']:break
        after=p['next_cursor']
    assert got==es

def append_check(b,d,es):
    expect(page(b,d,'primary',1,es[-2]['id']),es[-1:],False)
    c=sqlite3.connect(d/'sessions.sqlite3');e=event('primary','appended',104);insert(c,e);c.commit();c.close()
    expect(page(b,d,'primary',1,es[-1]['id']),[e],False)

def text_check(b,d,cursor):
    p=run(b,d,'primary','--limit','1');assert cursor in p.stdout,p.stdout
    assert 'audit_fixture' in p.stdout,p.stdout
    assert any(x in p.stdout.lower() for x in ['has_more','more','更多']),p.stdout

def snapshot_check(d):
    assert not (d/'config.toml').exists()
    c=sqlite3.connect(d/'sessions.sqlite3')
    for rev,snapshot in c.execute('SELECT revision,snapshot FROM sessions'):
        s=json.loads(snapshot);assert rev==0 and s['revision']==0 and s['jobs']=={} and s['budgets']=={}
    c.close()

if __name__=='__main__':raise SystemExit(main())
