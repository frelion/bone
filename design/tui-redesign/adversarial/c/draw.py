"""Independent C terminal-native design study. Design assets only."""
from pathlib import Path
from html import escape
import unicodedata
OUT=Path(__file__).parent
BG='#232323'; SIDE='#1a1a1a'; FG='#efefec'; MUTED='#b0b0ae'; DIM='#888888'; SEL='#383838'; USER='#2c2c2c'; CODE='#232323'; ACC='#b8c7e4'; BLUE='#b8c7e4'
def width(s): return sum(0 if unicodedata.combining(c) else 2 if unicodedata.east_asian_width(c) in 'WF' else 1 for c in s)
class Screen:
 def __init__(self): self.parts=[];self.rect(0,0,160,40,BG)
 def rect(self,x,y,w,h,c):
  assert x>=0 and y>=0 and x+w<=160 and y+h<=40
  self.parts.append(f'<rect x="{x*9}" y="{y*20}" width="{w*9}" height="{h*20}" fill="{c}"/>')
 def text(self,x,y,t,c=FG,b=False):
  assert x+width(t)<=160 and y<40,(x,y,t)
  spans=[]
  for ch in t:spans.append(f'<tspan x="{x*9}">{escape(ch)}</tspan>');x+=width(ch)
  self.parts.append(f'<text y="{y*20+15}" fill="{c}" font-weight="{600 if b else 400}">'+''.join(spans)+'</text>')
 def save(self,name):
  (OUT/(name+'.svg')).write_text('<svg xmlns="http://www.w3.org/2000/svg" width="1440" height="800" viewBox="0 0 1440 800"><title>BONE C · native input design study</title><g font-family="Menlo, monospace" font-size="14">'+''.join(self.parts)+'</g></svg>')
def base():
 s=Screen();s.rect(0,0,24,40,SIDE);s.rect(120,0,40,40,SIDE)
 s.text(3,1,'BONE',FG,True);s.text(3,4,'会话',MUTED)
 sessions=[('草稿恢复','执行中'),('Context engine','需要回答'),('修复启动错误','草稿'),('API 超时处理','已结束'),('工具权限','已结束'),('会话存储','已结束')]
 for i,(title,state) in enumerate(sessions):
  y=6+i*3
  if i==0:s.rect(1,y,22,2,SEL)
  s.text(3,y,title,FG if i==0 else MUTED,i==0);s.text(3,y+1,state,ACC if i==1 else DIM)
 s.text(3,38,'~/BONE',MUTED)
 s.text(29,1,'修复会话切换后的草稿恢复',FG,True)
 # Two turns with compact semantic groups; no source excerpt card.
 s.rect(27,4,87,2,USER);s.text(27,4,'│',DIM);s.text(27,5,'│',DIM)
 s.text(29,4,'切换会话时保留新草稿，补上回归测试。')
 s.text(29,7,'问题出在提交回执：它清空了用户后来写下的内容。')
 s.text(29,8,'只在版本匹配时清空输入，就能保留后续编辑。')
 s.text(29,10,'✓ 已读取 2 个文件',MUTED);s.text(51,10,'展开',BLUE)
 s.text(29,12,'state/update.rs',MUTED);s.text(48,12,'· rust',DIM)
 s.text(31,14,'if',BLUE);s.text(34,14,'receipt.revision == draft.revision {')
 s.text(35,15,'draft.clear();');s.text(31,16,'}')
 s.text(29,18,'• 提交回执只清理对应版本。')
 s.text(29,19,'• 新草稿与阅读位置仍属于各自会话。')
 s.rect(27,22,87,2,USER);s.text(27,22,'│',DIM);s.text(27,23,'│',DIM)
 s.text(29,22,'还要覆盖快速切换，以及提交后继续输入的情况。')
 s.text(29,25,'已补上两个回归场景，正在验证草稿隔离。')
 s.text(29,27,'◌ 运行回归测试',ACC);s.text(47,27,'· 4 / 6 通过',MUTED)
 s.text(29,29,'› 查看草稿恢复任务',BLUE)
 return s

def compose(s,lines,empty=False):
 # Transparent native prompt: 2+ rows, tightly adjacent metadata, no selection-like strip.
 y=34 if len(lines)<=2 else 33
 s.text(27,y,'›',ACC,True)
 for i,t in enumerate(lines):s.text(29,y+i,t,MUTED if empty else FG)
 if empty:s.text(29,y,'▏',FG)
 else:s.text(29+width(lines[-1]),y+len(lines)-1,'▏',FG)
 s.text(29,37,'Worker · GPT-5.5',MUTED)
 s.text(63,37,'enter 发送  alt+enter 换行  / 命令',MUTED)
 s.text(104,39,'esc 停止',MUTED)
s=base();compose(s,['同时检查中文输入和光标恢复。','']);s.save('01-main')
s=base();compose(s,['继续保留输入中的中文和 emoji。','回执返回后，也不要移动我正在阅读的位置。','并验证快速切换会话后的草稿归属。']);s.save('02-multiline')
s=base();compose(s,['输入下一步要求…',''],True);s.save('03-empty-input')
