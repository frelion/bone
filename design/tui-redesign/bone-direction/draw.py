"""Design-only terminal cell drawings; not a BONE renderer or product implementation."""
from pathlib import Path
from html import escape
import re, unicodedata
ROOT=Path(__file__).parent
BG='#171b1c'; SIDE='#131718'; INPUT='#1d2324'; SEL='#263234'; FG='#e5e2d9'; MUTED='#a1aaa8'; LINE='#3c4748'; ACC='#94b9b3'; GREEN='#aab9a1'; RED='#d6a09a'
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
def rule(s,x,y,n,color=LINE):s.text(x,y,'─'*n,color)
def shell(w=160,h=40,status='Ready',empty=False):
 s=Screen(w,h,'BONE · independent visual direction');l=24 if w>=100 else 0;r=40 if w>=140 else 0;c=w-l-r
 if l:
  s.rect(0,0,l,h,SIDE);s.text(2,1,'BONE',FG,True);s.text(2,3,'Sessions',MUTED)
  s.rect(1,6,l-2,2,SEL);s.text(3,6,'New conversation' if empty else 'Draft recovery',FG,True);s.text(3,7,status,MUTED)
  if not empty:
   s.text(3,10,'Context engine',FG);s.text(3,11,'Needs you',MUTED);s.text(3,14,'CLI startup',FG);s.text(3,15,'Draft',MUTED)
  s.text(2,h-2,'workspace / BONE',MUTED)
 if r:s.rect(w-r,0,r,h,SIDE)
 s.text(l+3,1,'New conversation' if empty else 'Draft recovery',FG,True)
 if c>=64:s.text(w-r-width(status)-3,1,status,MUTED)
 rule(s,l+3,3,c-6)
 return s,l,r,c

def composer(s,l,r,lines=None,active=True,mode='Ready'):
 lines=lines or ['Describe the next step…'];c=s.w-l-r;top=s.h-len(lines)-5
 rule(s,l+3,top,c-6,ACC if active else LINE)
 s.rect(l+3,top+1,c-6,len(lines)+2,INPUT)
 for i,t in enumerate(lines):s.text(l+5,top+2+i,t,MUTED if 'Describe the next' in t else FG)
 if active:
  # One terminal cell cursor; static representative, not animation.
  x=l+5 if 'Describe the next' in lines[-1] else l+5+width(lines[-1]);s.text(x,top+1+len(lines),'▏',FG)
 model='/model · choose model' if mode=='Needs setup' else 'model / configured'
 s.text(l+3,s.h-2,model,MUTED)
 hint=('esc stop' if active else 'stop') if mode=='Working' else 'enter send'
 if c>=64:s.text(s.w-r-width(hint)-3,s.h-2,hint,FG)
 s.text(s.w-r-13,s.h-1,'/ commands',MUTED)
 return top

def transcript(s,l,r,c,selected=False):
 x=l+3;s.rect(x,6,c-6,3,INPUT);s.text(x,7,'│',MUTED);s.text(x+2,7,'Fix draft recovery when switching sessions.')
 s.text(x+2,11,'A late receipt clears text written after submission.')
 s.text(x+2,12,'I will match the draft revision before clearing it.')
 s.text(x+2,15,'read   state/update.rs',MUTED);s.text(x+2,16,'read   state/model.rs',MUTED)
 if selected:s.rect(x+1,18,c-8,1,SEL)
 s.text(x+2,18,'[-] Draft recovery' if selected else '[+] Draft recovery',ACC if selected else FG)
 s.text(s.w-r-13,18,'Working',MUTED)
 s.text(x+2,21,'Preserve the newer draft',FG,True)
 s.rect(x+2,23,c-10,5,INPUT);s.text(x+4,23,'rust',MUTED)
 s.text(x+4,25,'if receipt.revision == draft.revision {',FG)
 s.text(x+4,26,'    draft.clear();',FG);s.text(x+4,27,'}',FG)
 s.text(x+2,30,'The next edit stays in its own session.',MUTED)

s,l,r,c=shell(empty=True);s.text(l+5,14,'A place to work through a problem.',FG,True);s.text(l+5,16,'Ask, inspect, and continue in the same conversation.',MUTED);composer(s,l,r);s.save('01-empty')
s,l,r,c=shell(status='Working');transcript(s,l,r,c);composer(s,l,r,mode='Working');s.save('02-conversation')
s,l,r,c=shell(status='Working');transcript(s,l,r,c,True);composer(s,l,r,active=False,mode='Working');x=s.w-r+3;s.text(x,1,'Job / Draft recovery',FG,True);rule(s,x,3,r-6,ACC);s.text(x,6,'Working',FG);s.text(x,9,'Goal',MUTED);s.text(x,11,'Keep edits made after submission.');s.text(x,15,'Done when',MUTED);s.text(x,17,'Late receipts preserve new drafts.');s.text(x,21,'Scope',MUTED);s.text(x,23,'Session input and persistence');s.text(x,28,'Opened from this conversation',MUTED);s.text(x,37,'esc back',FG);s.save('03-object-focus')
s,l,r,c=shell(status='Working');transcript(s,l,r,c);composer(s,l,r,lines=['/'],active=False,mode='Working');s.rect(l+3,21,c-6,12,INPUT);rule(s,l+3,21,c-6,ACC);s.text(l+5,22,'Commands',FG,True);s.text(l+c-12,22,'esc close',MUTED)
for y,n,d in [(24,'/new','Create session'),(25,'/sessions','Switch session'),(26,'/rename','Rename session'),(27,'/model','Choose model'),(28,'/help','Keyboard and commands'),(29,'/quit','Save drafts and exit')]:
 if y==24:s.rect(l+4,y,c-8,1,SEL)
 s.text(l+5,y,n,FG, y==24);s.text(l+22,y,d,MUTED)
s.text(l+5,31,'↑↓ select    tab complete    enter choose',MUTED);s.save('04-commands')
s,l,r,c=shell(status='Needs setup');s.rect(l+3,6,c-6,3,INPUT);s.text(l+3,7,'│',MUTED);s.text(l+5,7,'检查切换会话后的草稿恢复。');s.text(l+5,12,'Your request is saved.',FG,True);s.text(l+5,14,'Execution is waiting for a model.');s.text(l+5,16,'/model  Choose a model to continue',ACC);composer(s,l,r,mode='Needs setup');s.save('05-needs-setup')
s,l,r,c=shell(80,24,status='Ready');s.rect(3,5,74,3,INPUT);s.text(3,6,'│',MUTED);s.text(5,6,'Keep the new draft when I switch sessions.');s.text(5,10,'The draft remains attached to this session.');s.text(5,12,'read   state/update.rs',MUTED);composer(s,l,r,lines=['也保留中文输入和光标位置。']);s.save('06-narrow')
s,l,r,c=shell(40,12);s.text(3,5,'Your request is saved.',FG);composer(s,l,r,mode='Needs setup');s.save('07-minimum')
print('7 BONE direction boards generated')
