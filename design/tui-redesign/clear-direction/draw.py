"""Design-only terminal cell drawings; not a BONE renderer or product implementation."""
from pathlib import Path
from html import escape
import re, unicodedata
ROOT=Path(__file__).parent
BG='#fcfcfd'; SIDE='#f1f3f7'; USER='#eef2f8'; CODE='#f2f4f8'; INPUT='#e9f0ff'; SEL='#dce7ff'; FG='#202b3d'; MUTED='#59677e'; LINE='#d7deea'; ACC='#245be8'; GREEN='#157653'; RED='#b93246'; BLUE=ACC; PURPLE='#8055c7'
def width(s): return sum(0 if unicodedata.combining(c) else 2 if unicodedata.east_asian_width(c) in 'WF' else 1 for c in s)
class Screen:
 def __init__(self,w=160,h=40,title='BONE · 清亮方向 / 终端设计稿'):
  self.w=w;self.h=h;self.parts=[];self.title=title;self.rect(0,0,w,h,BG)
 def rect(self,x,y,w,h,c):
  assert x>=0 and y>=0 and x+w<=self.w and y+h<=self.h,(x,y,w,h)
  self.parts.append(f'<rect x="{x*8.4}" y="{y*20}" width="{w*8.4}" height="{h*20}" fill="{c}"/>')
 def text(self,x,y,s,c=FG,bold=False):
  assert x+width(s)<=self.w and y<self.h,(x,y,s)
  # Each glyph occupies an integer number of terminal cells; CJK stays two columns.
  spans=[]
  for ch in s:
   spans.append(f'<tspan x="{x*8.4}">{escape(ch)}</tspan>');x+=width(ch)
  self.parts.append(f'<text y="{y*20+15}" fill="{c}" font-weight="{600 if bold else 400}">{"".join(spans)}</text>')
 def link(self,x,y,w,h,target,label):
  self.parts.append(f'<a href="{target}.svg" aria-label="{escape(label)}"><title>{escape(label)}</title><rect x="{x*8.4}" y="{y*20}" width="{w*8.4}" height="{h*20}" fill="transparent"/></a>')
 def save(self,name):
  p=ROOT/'boards'/f'{name}.svg';p.parent.mkdir(exist_ok=True)
  p.write_text(f'<svg xmlns="http://www.w3.org/2000/svg" width="{self.w*8.4}" height="{self.h*20}" viewBox="0 0 {self.w*8.4} {self.h*20}" role="img"><title>{escape(self.title)}</title><g font-family="Menlo, monospace" font-size="14">'+''.join(self.parts)+'</g></svg>')
def shell(w=160,h=40,detail=False):
 s=Screen(w,h);l=24 if w>=100 else 0;r=40 if w>=140 else 0;c=w-l-r
 if l:
  s.rect(0,0,l,h,SIDE);s.text(3,1,'BONE',ACC,True);s.text(3,3,'会话',MUTED)
  for i,(name,state) in enumerate([('草稿恢复','执行中'),('Context engine','需要回答'),('修复启动错误','草稿'),('API 超时处理','就绪'),('工具权限','就绪'),('会话存储','就绪')]):
   y=5+i*3
   if y+1>=h-3:break
   if i==0:s.rect(1,y,l-2,2,SEL)
   s.text(3,y,name,FG,i==0);s.text(3,y+1,state,ACC if i==1 else MUTED)
  s.text(3,h-2,'~/BONE',MUTED)
 if r:s.rect(w-r,0,r,h,SIDE)
 s.text(l+3,1,'修复会话切换后的草稿恢复',FG,True)
 return s,l,r,c

def composer(s,l,r,lines=None,focused=True,running=True):
 lines=['继续补充要求…'] if lines is None else lines
 c=s.w-l-r;x=l+3;w=c-6;n=len(lines);y=s.h-n-4
 if running:s.text(x,y-2,'●',ACC);s.text(x+2,y-2,'正在验证草稿隔离',MUTED)
 # A borderless editing surface. Prompt is navigation-neutral; cursor owns focus.
 s.rect(x,y,w,n+2,INPUT if focused else USER)
 s.text(x,y+1,'›',ACC if focused else MUTED,True)
 for i,t in enumerate(lines):s.text(x+2,y+1+i,t,MUTED if '…' in t else FG)
 if focused:s.text(x+2+(0 if '…' in lines[-1] else width(lines[-1])),y+n,'▏',ACC)
 meta=y+n+2
 s.text(x,meta,'Worker · GPT-5.5' if c>=64 else '/model',MUTED)
 action='enter 提交 · esc 停止' if running and focused else '停止' if running else 'enter 发送'
 s.text(x+w-width(action),meta,action,ACC)
 if c>=90:s.text(x+23,meta,'/ 命令  ·  alt+enter 换行',MUTED)
 return y

def content(s,l,r,c,detail=False):
 x=l+3
 s.rect(x,4,c-6,3,USER);s.text(x+2,5,'切换会话时保留新草稿，补上回归测试。',FG)
 s.text(x+2,8,'问题出在提交回执：它清空了用户后来写下的内容。')
 s.text(x+2,9,'只在版本匹配时清空输入，就能保留后续编辑。')
 s.text(x+2,11,'✓',GREEN);s.text(x+4,11,'已读取 2 个文件',MUTED)
 s.text(x+2,13,'修改方案',FG,True)
 s.rect(x+2,15,c-10,5,CODE);s.text(x+4,15,'rust',MUTED)
 s.text(x+4,17,'if ',PURPLE);s.text(x+7,17,'receipt.revision == draft.revision {')
 s.text(x+4,18,'    draft.clear();');s.text(x+4,19,'}')
 s.text(x+2,22,'• 提交回执只清理对应版本。');s.text(x+2,23,'• 新草稿与阅读位置仍属于各自会话。')
 if detail:s.rect(x+1,26,c-8,1,SEL)
 s.text(x+2,26,'▾ 草稿恢复' if detail else '▸ 草稿恢复',ACC,True)
 s.text(x+22,26,'任务详情',MUTED)
 s.link(x+1,26,c-8,1,'03-return' if detail else '02-detail','收起任务详情' if detail else '查看任务详情')

s,l,r,c=shell();content(s,l,r,c);composer(s,l,r);s.save('01-main')
s,l,r,c=shell();content(s,l,r,c,True);composer(s,l,r,focused=False);x=s.w-r+3
s.text(x-2,1,'›',ACC,True);s.text(x,1,'草稿恢复',FG,True)
for y,label,value in [(5,'目标','保留提交之后的新编辑。'),(9,'完成条件','迟到回执不会清空新草稿。'),(13,'范围','会话输入与持久化'),(17,'状态','执行中')]:
 s.text(x,y,label,MUTED);s.text(x,y+1,value)
s.text(x,37,'esc 返回来源',ACC);s.link(x,37,26,1,'03-return','返回来源');s.save('02-detail')
s,l,r,c=shell();content(s,l,r,c);s.text(l+3,26,'›',ACC,True);composer(s,l,r,focused=False);s.link(l+3,35,c-6,3,'01-main','继续编辑');s.save('03-return')
s,l,r,c=shell();content(s,l,r,c);composer(s,l,r,lines=['继续检查中文输入、组合字符和 emoji。','切换会话后也要保留光标位置。']);s.save('04-multiline')
s,l,r,c=shell(80,24);s.rect(3,4,c-6,3,USER);s.text(5,5,'切换会话时保留新草稿。');s.text(5,9,'只在版本匹配时清空输入。');s.text(5,12,'✓ 已读取 2 个文件',GREEN);composer(s,l,r);s.save('05-narrow')
# Compact component sheet: same cell size as the full design, no magnified UI.
s=Screen(100,30);s.text(3,1,'输入区 · 单行、多行与失焦',FG,True)
def specimen(y,title,lines,focused):
 s.text(3,y,title,MUTED);s.rect(3,y+2,94,len(lines)+2,INPUT if focused else USER);s.text(4,y+3,'›',ACC if focused else MUTED,True)
 for i,t in enumerate(lines):s.text(6,y+3+i,t,FG)
 if focused:s.text(6+width(lines[-1]),y+2+len(lines),'▏',ACC)
 m=y+4+len(lines);s.text(3,m,'Worker · GPT-5.5',MUTED);s.text(30,m,'/ 命令  ·  alt+enter 换行',MUTED);s.text(87 if focused else 93,m,'enter 发送' if focused else '发送',ACC)
specimen(3,'01  输入中',['再检查一下迟到回执。'],True)
specimen(10,'02  多行输入',['继续检查中文输入、组合字符和 emoji。','切换会话后也要保留光标位置。'],True)
specimen(18,'03  焦点移到详情，草稿保留',['切换会话后也要保留光标位置。'],False)
s.text(3,27,'失焦时隐藏光标，输入面退回浅灰。',MUTED);s.save('06-composer')
print('6 design boards generated')
