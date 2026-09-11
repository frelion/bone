"""Design-only terminal cell drawings; not a BONE renderer or product implementation."""
from pathlib import Path
from html import escape
import re, unicodedata
ROOT=Path(__file__).parent
BG='#101010'; SIDE='#141414'; INPUT='#1e1e1e'; SEL='#282828'; FG='#eeeeee'; MUTED='#969696'; LINE='#303030'; ACC='#fab283'; GREEN='#7fd88f'; RED='#e06c75'
def width(s): return sum(0 if unicodedata.combining(c) else 2 if unicodedata.east_asian_width(c) in 'WF' else 1 for c in s)
class Screen:
 def __init__(self,w=160,h=40,title='BONE · proposed design'):
  self.w=w;self.h=h;self.parts=[];self.title=title;self.rect(0,0,w,h,BG)
 def rect(self,x,y,w,h,c):
  assert x>=0 and y>=0 and x+w<=self.w and y+h<=self.h,(x,y,w,h)
  self.parts.append(f'<rect x="{x*9}" y="{y*20}" width="{w*9}" height="{h*20}" fill="{c}"/>')
 def text(self,x,y,s,c=FG,bold=False):
  assert x+width(s)<=self.w and y<self.h,(x,y,s)
  # Each glyph occupies an integer number of terminal cells; CJK stays two columns.
  spans=[]
  for ch in s:
   spans.append(f'<tspan x="{x*9}">{escape(ch)}</tspan>');x+=width(ch)
  self.parts.append(f'<text y="{y*20+15}" fill="{c}" font-weight="{600 if bold else 400}">{"".join(spans)}</text>')
 def save(self,name):
  p=ROOT/'boards'/f'{name}.svg';p.parent.mkdir(exist_ok=True)
  p.write_text(f'<svg xmlns="http://www.w3.org/2000/svg" width="{self.w*9}" height="{self.h*20}" viewBox="0 0 {self.w*9} {self.h*20}" role="img"><title>{escape(self.title)}</title><g font-family="Menlo, monospace" font-size="14">'+''.join(self.parts)+'</g></svg>')
def shell(w=160,h=40,empty=False,status='Ready'):
 s=Screen(w,h);l=24 if w>=100 else 0;r=40 if w>=140 else 0;c=w-l-r
 if l:
  s.rect(0,0,l,h,SIDE);s.text(2,1,'Sessions',MUTED)
  s.rect(1,4,l-2,2,SEL);s.text(1,4,'▎',ACC);s.text(3,4,'New conversation' if empty else 'Draft recovery',FG,True);s.text(3,5,status,MUTED)
  if not empty:
   s.text(3,7,'Context engine',MUTED);s.text(3,8,'Needs you',ACC)
   s.text(3,10,'CLI startup',MUTED);s.text(3,11,'Draft',MUTED)
  s.text(2,h-2,'BONE',MUTED)
 if r:s.rect(w-r,0,r,h,SIDE)
 s.text(l+2,1,('New conversation' if empty else 'Draft recovery'),FG,True)
 if c>60:s.text(w-r-width(status)-2,1,status,MUTED)
 return s,l,r,c

def composer(s,l,r,lines=None,model='Worker · model',busy=False):
 lines=lines or ['Ask anything…'];c=s.w-l-r;y=s.h-len(lines)-4
 s.rect(l+2,y,c-4,len(lines)+3,INPUT)
 for i in range(len(lines)+3):s.text(l+2,y+i,'┃',ACC)
 for i,line in enumerate(lines):s.text(l+5,y+1+i,line,MUTED if line=='Ask anything…' else FG)
 s.text(l+5,s.h-3,model,MUTED)
 hint='esc stop' if busy else 'enter send'
 if c>=60:s.text(s.w-r-width(hint)-4,s.h-3,hint,ACC)
 footer='alt+enter newline   / commands' if c>=60 else '/ commands'
 s.text(s.w-r-width(footer)-2,s.h-1,footer,MUTED)
 return y

def conversation(s,l,r,c):
 x=l+2
 s.rect(x,4,c-4,3,INPUT);s.text(x,4,'│',MUTED);s.text(x,5,'│',MUTED);s.text(x,6,'│',MUTED)
 s.text(x+3,5,'Fix draft recovery when switching sessions.')
 s.text(x+3,9,'The draft is cleared by a late submit receipt.')
 s.text(x+3,10,'I will keep it attached to the revision that was sent.')
 s.text(x+3,13,'› read   state/update.rs',MUTED)
 s.text(x+3,14,'› read   state/model.rs',MUTED)
 s.text(x+3,17,'Preserve the newer draft',FG,True)
 s.text(x+3,19,'Only clear the buffer when both revisions match.')
 s.rect(x+3,21,c-9,5,INPUT)
 s.text(x+5,21,'rust',MUTED)
 s.text(x+5,23,'if receipt.revision == draft.revision {',ACC)
 s.text(x+5,24,'    draft.clear();',FG)
 s.text(x+5,25,'}',ACC)
 s.text(x+3,28,'• Keep drafts independent for each session.')
 s.text(x+3,29,'• Leave the reading position unchanged.')

s,l,r,c=shell(empty=True);s.text(l+5,17,'What would you like to work on?',FG,True);s.text(l+5,19,'Describe a change, paste an error, or ask a question.',MUTED);composer(s,l,r);s.save('01-empty')
s,l,r,c=shell();conversation(s,l,r,c);composer(s,l,r);s.save('02-conversation')
s,l,r,c=shell(status='Working');conversation(s,l,r,c);s.text(l+5,32,'· Running checks…',ACC);composer(s,l,r,busy=True);s.save('03-running')
s,l,r,c=shell();conversation(s,l,r,c);composer(s,l,r,lines=['/']);s.rect(l+2,23,c-4,11,INPUT);s.text(l+5,24,'Commands',MUTED);s.text(l+c-12,24,'esc close',MUTED)
for y,name,desc in [(26,'/new','Create a session'),(27,'/sessions','Switch session'),(28,'/rename','Rename current session'),(29,'/model','Choose model'),(30,'/help','Keyboard and commands'),(31,'/quit','Save drafts and exit')]:
 if y==26:s.rect(l+3,y,c-6,1,SEL);s.text(l+3,y,'›',ACC)
 s.text(l+5,y,name,FG);s.text(l+22,y,desc,MUTED)
s.text(l+5,33,'↑↓ choose   tab complete   enter select',MUTED);s.save('04-commands')
s,l,r,c=shell(status='Needs setup');s.rect(l+2,4,c-4,3,INPUT);s.text(l+5,5,'检查切换会话后的草稿恢复。');s.text(l+5,9,'Model setup required',ACC,True);s.text(l+5,11,'Your request is saved. Execution has not started.',FG);s.text(l+5,13,'Choose a model with /model to continue.',MUTED);composer(s,l,r,model='Choose model · /model');s.save('05-needs-setup')
s,l,r,c=shell(status='Working');conversation(s,l,r,c);composer(s,l,r,busy=True);x=s.w-r+2;s.text(x,1,'Job',MUTED);s.text(s.w-8,1,'× close',MUTED);s.text(x,4,'Draft recovery',FG,True);s.text(x,6,'Working',ACC);s.text(x,9,'Goal',MUTED);s.text(x,11,'Keep edits made after submission.');s.text(x,15,'Done when',MUTED);s.text(x,17,'Late receipts preserve new drafts.');s.text(x,21,'Scope',MUTED);s.text(x,23,'Session input and persistence');s.text(x,28,'Related input',MUTED);s.text(x,30,'Fix draft recovery…');s.save('06-job-detail')
s,l,r,c=shell(120,32);s.rect(l+2,4,c-4,3,INPUT);s.text(l+2,5,'│',MUTED);s.text(l+5,5,'Fix draft recovery when switching sessions.');s.text(l+5,9,'The newer draft will remain intact.');s.text(l+5,12,'› read   state/update.rs',MUTED);s.text(l+5,15,'• Match the submit revision before clearing.');composer(s,l,r,lines=['Keep the new draft after submitting.', 'Also cover Chinese input and emoji.','Do not move the reading position.']);s.save('07-two-column')
s,l,r,c=shell(80,24);s.text(2,1,'Draft recovery');s.text(59,1,'/sessions',MUTED);s.rect(2,4,c-4,3,INPUT);s.text(2,5,'│',MUTED);s.text(5,5,'Fix draft recovery when switching sessions.');s.text(5,8,'The newer draft will remain intact.');s.text(5,11,'› read   state/update.rs',MUTED);s.text(5,14,'• Match the revision before clearing.');composer(s,l,r);s.save('08-narrow')
s=Screen(40,12);s.text(2,0,'Draft recovery',FG,True);s.text(2,2,'Your request is saved.');s.text(2,4,'Choose a model to continue.',ACC);composer(s,0,0,model='/model');s.save('09-minimum')
s,l,r,c=shell(status='Needs you');s.rect(l+2,4,c-4,3,INPUT);s.text(l+5,5,'Fix draft recovery when switching sessions.');s.text(l+5,10,'Should unsent drafts survive an application restart?');s.text(l+5,12,'This answer will apply to draft recovery.',MUTED);s.text(l+5,30,'Replying to this question · esc cancel reply',ACC);composer(s,l,r,lines=['Yes, keep them until I explicitly discard them.']);s.save('10-question')
s,l,r,c=shell();conversation(s,l,r,c);s.text(l+5,32,'↓ 2 new · return to latest',ACC);composer(s,l,r);s.save('11-reading-history')
# Convert tmux's real terminal captures for visual inspection. SGR foreground/background only.
ansi16=['#101010','#e06c75','#98c379','#e5c07b','#61afef','#c678dd','#56b6c2','#eeeeee']*2
def color(n):
 if n<16:return ansi16[n]
 if n>=232:return '#'+('%02x'%(8+10*(n-232)))*3
 n-=16;v=[0,95,135,175,215,255];return '#%02x%02x%02x'%(v[n//36],v[n//6%6],v[n%6])
for p in (ROOT/'evidence').glob('*.ansi'):
 rows=p.read_text().splitlines();plain=[re.sub(r'\x1b\[[0-9;]*m','',z) for z in rows];w=max(width(z) for z in plain);s=Screen(w,len(rows),p.stem+' · actual tmux capture');fg=FG;bg=BG
 for y,row in enumerate(rows):
  x=0
  for part in re.split(r'(\x1b\[[0-9;]*m)',row):
   if part.startswith('\x1b'):
    ns=[int(n or 0) for n in part[2:-1].split(';')];i=0
    while i<len(ns):
     n=ns[i]
     if n==0:fg=FG;bg=BG
     elif n==39:fg=FG
     elif n==49:bg=BG
     elif 30<=n<=37:fg=ansi16[n-30]
     elif 40<=n<=47:bg=ansi16[n-40]
     elif n in (38,48) and i+2<len(ns):
      if ns[i+1]==2 and i+4<len(ns):co='#%02x%02x%02x'%tuple(ns[i+2:i+5]);i+=4
      elif ns[i+1]==5:co=color(ns[i+2]);i+=2
      else:i+=1;continue
      if n==38:fg=co
      else:bg=co
     i+=1
   elif part:
    n=width(part)
    if n:s.rect(x,y,n,1,bg);s.text(x,y,part,fg);x+=n
 s.save('actual-'+p.stem)
print('Rendered',len(list((ROOT/'boards').glob('*.svg'))),'SVG boards.')
