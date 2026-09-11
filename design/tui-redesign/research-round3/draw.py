"""Design-only terminal cell drawings; not a BONE renderer or product implementation."""
from pathlib import Path
from html import escape
import re, unicodedata
ROOT=Path(__file__).parent
BG='#111214'; SIDE='#17181b'; USER='#1b1c1f'; CODE='#18191c'; INPUT='#242424'; SEL='#2b2d32'; FG='#e8e8e6'; MUTED='#999ca5'; LINE='#373a41'; ACC='#d1c4a4'; GREEN='#a7bf9c'; RED='#db9c94'; BLUE='#9fbcd5'; PURPLE='#baa9d1'
def width(s): return sum(0 if unicodedata.combining(c) else 2 if unicodedata.east_asian_width(c) in 'WF' else 1 for c in s)
class Screen:
 def __init__(self,w=160,h=40,title='BONE · proposed design'):
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
def row(s,x,y,v,color=FG):s.text(x,y,v,color)
def frame(s,x,y,w,h,focused=True):
 co=MUTED if focused else LINE
 s.rect(x,y,w,h,INPUT);s.text(x,y,'╭'+'─'*(w-2)+'╮',co)
 for yy in range(y+1,y+h-1):s.text(x,yy,'│',co);s.text(x+w-1,yy,'│',co)
 s.text(x,y+h-1,'╰'+'─'*(w-2)+'╯',co)

def shell(w=160,h=40,empty=False,focus='composer',current=0,state='执行中',new=False):
 s=Screen(w,h,'BONE · proposed direction A');l=24 if w>=100 else 0;r=40 if w>=140 else 0;c=w-l-r
 if l:
  s.rect(0,0,l,h,SIDE);s.text(2,1,'BONE',ACC,True);s.text(2,3,'会话',MUTED)
  names=['草稿恢复','Context engine','修复启动错误','API 超时处理','工具权限','会话存储'] if not empty else (['新会话','草稿恢复','Context engine','修复启动错误','API 超时处理','工具权限','会话存储'] if new else ['新会话'])
  for i,n in enumerate(names):
   y=5+i*3
   if y+1>=h-3:break
   if i==current:s.rect(1,y,l-2,2,SEL)
   s.text(3,y,n,FG if i==current else MUTED,i==current)
   st=(('草稿' if empty else state) if i==0 else ['','需要回答','草稿','就绪','就绪','就绪','就绪'][i])
   if new and i:st=['','执行中','需要回答','草稿','就绪','就绪','就绪'][i]
   s.text(3,y+1,st,ACC if st=='需要回答' else MUTED)
  s.text(2,h-2,'~/BONE',MUTED)
 if r:s.rect(w-r,0,r,h,SIDE)
 s.text(l+3,1,'新会话' if empty else '修复会话切换后的草稿恢复',FG,True)
 return s,l,r,c

def compose(s,l,r,focus=True,lines=None,status='正在验证草稿隔离…',stop=True,model='Worker · GPT-5.5',links=True):
 c=s.w-l-r;lines=lines or ['继续补充要求…'];h=len(lines)+4;y=s.h-h-1
 if status:s.text(l+5,y-2,'◌ '+status,ACC)
 frame(s,l+3,y,c-6,h,focus)
 for i,t in enumerate(lines):s.text(l+5,y+1+i,t,MUTED if t=='继续补充要求…' else FG)
 if focus:s.text(l+5+(0 if '…' in lines[-1] else min(width(lines[-1]),c-12)),y+len(lines),'▏',FG)
 s.text(l+5,y+h-2,model if c>=64 else '/model',MUTED)
 action=('esc 停止' if focus else '停止') if stop else 'enter 发送'
 if c>=40:s.text(s.w-r-width(action)-4,y+h-2,action,FG)
 hint='alt+enter 换行   / 命令' if c>=64 else '/ 命令'
 s.text(s.w-r-width(hint)-3,s.h-1,hint,MUTED)
 if links:s.link(s.w-r-10,s.h-1,8,1,'04-menu','打开命令菜单演示')
 return y

def message(s,l,r,c,selected=False,compact=False,interactive=True,source=False):
 x=l+3
 s.rect(x,4,c-6,3,USER);s.text(x,5,'│',MUTED);s.text(x+2,5,'切换会话时保留新草稿，补上回归测试。')
 s.text(x+2,8,'问题出在提交回执：它清空了用户后来写下的内容。')
 s.text(x+2,9,'只在版本匹配时清空输入，就能保留后续编辑。')
 s.text(x+2,11,'✓ 已读取 2 个文件',MUTED)
 s.text(x+2,13,'修改方案',FG,True)
 s.rect(x+2,15,c-10,6,CODE);s.text(x+4,15,'rust',MUTED)
 s.text(x+4,17,'if ',PURPLE);s.text(x+7,17,'receipt.revision == draft.revision {',FG)
 s.text(x+4,18,'    draft.clear();',FG);s.text(x+4,19,'}',FG)
 s.text(x+2,22,'• 提交回执只清理对应版本。')
 s.text(x+2,23,'• 新草稿与阅读位置仍属于各自会话。')
 if selected:s.rect(x+1,26,c-8,1,SEL)
 s.text(x+2,26,'▾ 草稿恢复' if selected else '▸ 草稿恢复',FG)
 s.text(x+22,26,'查看任务详情',BLUE)
 if source:s.text(x,26,'›',ACC)
 if interactive:s.link(x+1,26,c-8,1,'10-source-focus' if selected else '06-detail','收起详情回到来源演示' if selected else '打开草稿恢复详情演示')

s,l,r,c=shell();message(s,l,r,c);compose(s,l,r);s.save('01-candidate-a')
s,l,r,c=shell();message(s,l,r,c);# candidate B: transcript side gutter and integrated flat editor
s.rect(l,3,2,28,SIDE)
for y,n in [(5,'01'),(8,'02'),(14,'03'),(28,'04')]:s.text(l,y,n,MUTED)
s.text(l+3,32,'正在验证草稿隔离…',ACC);s.rect(l+2,34,c-4,5,INPUT);s.text(l+3,34,'─'*(c-6),LINE);s.text(l+4,36,'继续补充要求…',MUTED);s.text(l+4,38,'Worker · GPT-5.5',MUTED);s.text(l+c-14,38,'esc 停止',FG);s.save('02-candidate-b')
s,l,r,c=shell(empty=True,state='草稿');s.text(l+5,14,'从一个具体问题开始。',FG,True);s.text(l+5,16,'描述修改、贴出错误，或继续已有工作。',MUTED);compose(s,l,r,status='',stop=False,lines=['描述你想完成的事…'],links=False);s.save('03-empty')
s,l,r,c=shell();message(s,l,r,c,interactive=False);compose(s,l,r,focus=False,lines=['/'],status='正在验证草稿隔离…',links=False);frame(s,l+2,18,c-4,13,True)
s.text(l+4,19,'命令',FG,True);s.text(l+c-12,19,'esc 关闭',MUTED)
for y,n,d in [(21,'/new','新建会话'),(22,'/sessions','切换会话'),(23,'/rename','重命名当前会话'),(24,'/model','选择模型'),(25,'/help','快捷键与帮助'),(26,'/quit','保存草稿并退出')]:
 if y==21:s.rect(l+3,y,c-6,1,SEL)
 s.text(l+4,y,n,FG);s.text(l+22,y,d,MUTED)
s.text(l+4,29,'↑↓ 选择   tab 补全   enter 确定',MUTED)
s.link(l+4,21,c-8,1,'11-new-session','演示新建会话并保留旧会话');s.link(l+c-12,19,8,1,'01-candidate-a','关闭菜单演示');s.save('04-menu')
s,l,r,c=shell(state='需要配置');s.rect(l+3,4,c-6,3,USER);s.text(l+3,5,'│',MUTED);s.text(l+5,5,'切换会话时保留新草稿，补上回归测试。');s.text(l+5,9,'输入已保存，尚未开始执行。',FG,True);s.text(l+5,11,'选择模型后即可继续；这条要求不会丢失。',MUTED);compose(s,l,r,status='需要配置模型  · /model',stop=False,model='/model 选择模型');s.save('05-blocked')
s,l,r,c=shell(focus='detail');message(s,l,r,c,True);compose(s,l,r,focus=False);x=s.w-r+3
s.text(x-2,1,'›',ACC);s.text(x,1,'草稿恢复',FG,True);s.text(s.w-7,1,'×',MUTED)
s.text(x,4,'任务详情',MUTED)
for y,label,value in [(7,'目标','保留提交之后的新编辑。'),(11,'完成条件','迟到回执不会清空新草稿。'),(15,'范围','会话输入与持久化'),(19,'状态','执行中')]:
 s.text(x,y,label,MUTED);s.text(x,y+1,value,ACC if y==19 else FG)
s.text(x,37,'esc 返回来源',FG);s.link(s.w-8,1,4,1,'10-source-focus','返回来源演示');s.link(x,37,25,1,'10-source-focus','返回来源演示');s.save('06-detail')
s,l,r,c=shell(focus='conversation');message(s,l,r,c,source=True);compose(s,l,r,focus=False);s.link(l+3,34,c-6,5,'01-candidate-a','点击输入继续编辑演示');s.save('10-source-focus')
s,l,r,c=shell(empty=True,new=True);s.text(l+5,14,'从一个具体问题开始。',FG,True);s.text(l+5,16,'描述修改、贴出错误，或继续已有工作。',MUTED);compose(s,l,r,status='',stop=False,lines=['描述你想完成的事…'],links=False);s.link(1,8,22,2,'01-candidate-a','切回原会话演示');s.save('11-new-session')
s,l,r,c=shell(120,30);s.rect(l+3,4,c-6,3,USER);s.text(l+3,5,'│',MUTED);s.text(l+5,5,'切换会话时保留新草稿。');s.text(l+5,9,'只在版本匹配时清空输入。');s.text(l+5,12,'✓ 已读取 2 个文件',MUTED);s.text(l+5,15,'• 新草稿仍属于各自会话。');compose(s,l,r,lines=['同时检查中文输入、组合字符和 emoji。','保留光标与阅读位置。'],status='正在验证草稿隔离…',links=False);s.save('07-medium')
s,l,r,c=shell(80,24);s.rect(3,4,c-6,3,USER);s.text(3,5,'│',MUTED);s.text(5,5,'切换会话时保留新草稿。');s.text(5,9,'只在版本匹配时清空输入。');s.text(5,12,'✓ 已读取 2 个文件',MUTED);compose(s,l,r,status='正在验证草稿隔离…',links=False);s.save('08-narrow')
s=Screen(40,12);s.text(2,0,'草稿恢复',FG,True);s.text(2,2,'输入已保存，等待选择模型。');compose(s,0,0,status='',stop=False,model='/model',links=False);s.save('09-minimum')
# Additional content states below

# Content stress states: full viewport, question, failure. All text is synthetic fixture.
s,l,r,c=shell(state='需要回答');x=l+5
s.rect(l+3,4,c-6,3,USER);s.text(x,5,'再检查一下：切换到其他会话后，迟到回执会怎样？')
s.text(x,8,'回执必须携带会话与草稿版本，不能只检查当前编辑器。')
s.text(x,10,'✓ 检查完成：提交、切换、持久化三个入口',MUTED)
s.text(x,13,'需要你决定',FG,True);s.text(x,15,'重启之后，尚未提交的草稿也需要恢复吗？')
s.text(x,18,'1  恢复每个会话的未提交草稿');s.text(x,20,'2  只保留本次运行中的草稿')
s.text(x,23,'也可以直接输入你的要求。',MUTED)
compose(s,l,r,status='等待你的回答',stop=False,lines=['重启后也要恢复，并保留光标位置。'],links=False);s.save('12-question')
s,l,r,c=shell(state='执行失败');x=l+5
s.rect(l+3,4,c-6,3,USER);s.text(x,5,'执行回归测试，并报告结果。')
s.text(x,8,'已经完成修改，正在检查草稿隔离。')
s.text(x,11,'× 测试未能运行',RED,True);s.rect(x,13,c-10,4,CODE)
s.text(x+2,14,'error: failed to download dependency',RED);s.text(x+2,15,'network request timed out',MUTED)
s.text(x,19,'本次没有测试结果，不能据此判断修改通过。')
s.text(x,21,'恢复网络后，可要求重新运行测试。',MUTED)
compose(s,l,r,status='执行失败 · 网络请求超时',stop=False,lines=['网络恢复后重新运行测试。'],links=False);s.save('13-failure')
s,l,r,c=shell(state='就绪');x=l+5
s.text(x,3,'↑ 较早消息',MUTED)
s.rect(l+3,5,c-6,3,USER);s.text(x,6,'切换会话时保留新草稿，补上回归测试。')
s.text(x,9,'已按会话保存草稿；回执只清理对应版本。')
s.text(x,11,'✓ 读取完成  ·  ✓ 修改完成',MUTED)
s.text(x,13,'涉及以下边界：',FG,True)
for yy,t in [(15,'• A 提交后继续编辑，不受旧回执影响。'),(16,'• 切到 B 后收到 A 回执，只更新 A。'),(17,'• 中文组合输入不按字节截断。')]:s.text(x,yy,t)
s.rect(l+3,20,c-6,3,USER);s.text(x,21,'重启后也要恢复，并保留光标位置。')
s.text(x,24,'会将草稿与光标作为同一份会话状态保存。')
s.text(x,26,'下一步先验证重启恢复，再检查窗口缩放。')
s.text(x,29,'↓ 回到最新',BLUE)
compose(s,l,r,status='',stop=False,lines=[''],links=False);s.save('14-history')
