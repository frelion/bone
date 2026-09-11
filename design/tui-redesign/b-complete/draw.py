"""B design walkthrough only. Terminal cell diagrams, synthetic content, no product I/O."""
from pathlib import Path
from html import escape
import unicodedata
P=Path(__file__).parent
C={'bg':'#101010','side':'#080808','user':'#1c1c1c','input':'#252525','selected':'#402711','fg':'#f5f5f5','muted':'#c2c2c2','dim':'#999999','line':'#555555','accent':'#ff9d24','code':'#ca8cff','green':'#78e08f','red':'#ff667a','cyan':'#51d6e8'}
def width(t):return sum(0 if unicodedata.combining(c) else 2 if unicodedata.east_asian_width(c) in 'WF' else 1 for c in t)
class Screen:
 def __init__(self,w=160,h=40,title='BONE · B 完整设计'):
  self.w=w;self.h=h;self.title=title;self.parts=[];self.rect(0,0,w,h,'bg')
 def rect(self,x,y,w,h,color):
  assert x>=0 and y>=0 and x+w<=self.w and y+h<=self.h,(x,y,w,h)
  self.parts.append(f'<rect x="{x*9}" y="{y*20}" width="{w*9}" height="{h*20}" fill="{C.get(color,color)}"/>')
 def text(self,x,y,t,color='fg',bold=False):
  assert x>=0 and x+width(t)<=self.w and 0<=y<self.h,(x,y,t)
  spans=[]
  for ch in t:spans.append(f'<tspan x="{x*9}">{escape(ch)}</tspan>');x+=width(ch)
  self.parts.append(f'<text y="{y*20+15}" fill="{C.get(color,color)}" font-weight="{600 if bold else 400}">'+''.join(spans)+'</text>')
 def link(self,x,y,w,h,target,label):
  assert x>=0 and x+w<=self.w and y>=0 and y+h<=self.h
  self.parts.append(f'<a href="{target}.svg" aria-label="{escape(label)}"><title>{escape(label)}</title><rect x="{x*9}" y="{y*20}" width="{w*9}" height="{h*20}" fill="transparent"/></a>')
 def save(self,name):
  d=P/'boards';d.mkdir(exist_ok=True)
  (d/f'{name}.svg').write_text(f'<svg xmlns="http://www.w3.org/2000/svg" width="{self.w*9}" height="{self.h*20}" viewBox="0 0 {self.w*9} {self.h*20}" role="img"><title>{escape(self.title)}</title><g font-family="SFMono-Regular,Menlo,PingFang SC,monospace" font-size="14">'+''.join(self.parts)+'</g></svg>')
def shell(w=160,h=40,state='执行中',title='修复会话切换后的草稿恢复',new=False):
 s=Screen(w,h);l=24 if w>=100 else 0;r=40 if w>=140 else 0;c=w-l-r;x=l+4;cw=min(84,c-8)
 if l:
  s.rect(0,0,l,h,'side');s.rect(3,1,2,1,'accent');s.text(3,1,'B','side',True);s.text(6,1,'BONE','accent',True);s.text(3,3,'会话','fg',True);s.text(20,3,'07' if new else '06','dim')
  items=[('草稿恢复',state),('Context engine','需要回答'),('修复启动错误','草稿'),('API 超时处理','就绪'),('工具权限','就绪'),('会话存储','就绪')]
  if new:items=[('新会话','草稿')]+items
  for i,(t,st) in enumerate(items):
   y=5+i*3
   if y+1>=h-3:break
   if i==0:s.rect(1,y,22,2,'selected');s.text(1,y,'│','accent');s.text(1,y+1,'│','accent')
   s.text(3,y,t,'fg',i==0)
   tone='accent' if st=='需要回答' else 'green' if st=='执行中' else 'code' if st=='草稿' else 'red' if st=='执行失败' else 'cyan' if st in ('就绪','可继续') else 'muted'
   symbol='?' if st=='需要回答' else '●' if st=='执行中' else '✎' if st=='草稿' else '×' if st=='执行失败' else '○'
   s.text(3,y+1,symbol,tone);s.text(5,y+1,st,tone)
  s.text(3,h-3,'工作目录','dim');s.text(3,h-2,'~/BONE','cyan')
 if r:s.rect(w-r,0,r,h,'side')
 s.text(x+2,1,title,'fg',True)
 return s,l,r,x,cw

def user(s,x,y,w,text):
 s.rect(x,y,w,3,'user');s.text(x+1,y+1,'│','dim');s.text(x+4,y+1,text)
def compose(s,x,w,lines=None,status='验证输入定位',running=True,focus=True,menu=False,send=None,queue=False,model='Worker · GPT-5.5',stop_target=None,interactive=True,action_override=None):
 lines=['继续补充要求…'] if lines is None else lines
 n=len(lines);h=n+2;y=s.h-h-3
 if status:
  s.text(x+2,y-2,'◌ '+status,'accent')
  if running:
   label='停止全部' if queue else 'esc 停止' if focus else '停止'
   s.text(x+w-width(label)-2,y-2,label,'muted')
   if interactive and stop_target:s.link(x+w-width(label)-2,y-2,width(label),1,stop_target,'演示停止全部' if queue else '演示停止执行')
 s.rect(x,y,w,h,'input' if focus else 'user');s.text(x+2,y+1,'›','accent' if focus else 'dim',True)
 for i,t in enumerate(lines):s.text(x+4,y+1+i,t,'muted' if '…' in t else 'fg')
 if focus:s.rect(x+4+(0 if '…' in lines[-1] else width(lines[-1])),y+n,1,1,'accent')
 meta=y+h;s.text(x+4,meta,model if w>=60 else '/model','muted')
 if w>=76:s.text(x+28,meta,'/ 命令','muted');s.text(x+39,meta,'alt+enter 换行','dim')
 elif w>=60:s.text(x+24,meta,'/ 命令','muted')
 action=action_override or (('enter 追加' if running else 'enter 提交') if focus else '提交');s.text(x+w-width(action)-2,meta,action,'accent')
 if menu and w>=60:s.link(x+24 if w<76 else x+28,meta,8,1,'05-menu','打开命令菜单')
 if send and any(t.strip() and '…' not in t for t in lines):s.link(x+w-width(action)-2,meta,width(action),1,send,'提交当前示例输入')
 return y

def conversation(s,x,w,links=True,selected=False):
 user(s,x,4,w,'切换会话时保留新草稿，补上回归测试。')
 s.text(x+2,8,'已修复提交回执对新草稿的误清理。','fg',True)
 s.text(x+2,10,'现在只有提交版本与当前草稿一致时，输入才会清空。')
 s.text(x+2,11,'会话切换会分别保留草稿、光标和阅读位置。')
 s.text(x+2,13,'if','code');s.text(x+5,13,'receipt.revision == draft.revision {')
 s.text(x+2,14,'    draft.');s.text(x+12,14,'clear','cyan');s.text(x+17,14,'();');s.text(x+2,15,'}')
 s.text(x+2,17,'✓ 迟到回执   ✓ 会话切换','green')
 if selected:s.rect(x+38,17,18,1,'selected')
 s.text(x+40,17,'草稿恢复 ›','accent')
 if links:s.link(x+38,17,18,1,'11-detail','打开草稿恢复详情')
 user(s,x,20,w,'再检查中文和组合字符，别让光标跳位。')
 s.text(x+2,24,'我会把输入定位按实际字符宽度一起验证。')
 s.text(x+2,26,'读取','muted');s.text(x+10,26,'editor.rs · input.rs','cyan')
 if links:s.link(x+2,26,42,1,'13-tool','打开读取记录')
 s.text(x+2,27,'检查','accent');s.text(x+10,27,'中文输入与 grapheme 边界','muted')

DRAFT=['组合字符之后继续输入时，','也保留原来的光标位置。']
s,l,r,x,w=shell();conversation(s,x,w);compose(s,x,w,DRAFT,send='15-queued',menu=True,stop_target='51-main-stopping');s.save('01-main')
# Contrast control: identical geometry and content. This variant is not automatically chosen.
original=C.copy();C.update({'bg':'#1c1c1c','side':'#101010','user':'#292929','input':'#383838','selected':'#454545','fg':'#fafafa','muted':'#c4c4c4','dim':'#a5a5a5','accent':'#dce6f7','code':'#dcc3f0'})
s,l,r,x,w=shell();conversation(s,x,w);compose(s,x,w,DRAFT,send='15-queued');s.save('02-neutral-contrast');C.update(original)
s,l,r,x,w=shell();conversation(s,x,w,False);compose(s,x,w,DRAFT+['切回会话后，从离开的位置继续。'],send='29-multi-accepted');s.save('03-multiline')
s,l,r,x,w=shell(state='就绪');conversation(s,x,w,False);s.rect(x,24,w,5,'bg');s.text(x+2,24,'中文、组合字符与会话切换场景已检查。');s.text(x+2,26,'✓ 验证完成，结果可从原任务继续查看。','green');compose(s,x,w,status='',running=False,send='15-queued');s.save('04-idle')
s,l,r,x,w=shell();conversation(s,x,w,False);compose(s,x,w,['/'],menu=False)
s.rect(x,17,w,15,'input');s.text(x+2,18,'命令','fg',True);s.text(x+w-10,18,'esc 关闭','muted')
for i,(name,desc,target) in enumerate([('/new','新建会话','06-new'),('/sessions','切换会话','17-sessions'),('/model','选择模型','10-model'),('/help','快捷键与帮助','18-help')]):
 yy=21+i*2
 if i==0:s.rect(x+1,yy,w-2,1,'selected')
 s.text(x+3,yy,name);s.text(x+22,yy,desc,'muted');s.link(x+1,yy,w-2,1,target,'执行'+name)
s.text(x+3,30,'↑↓ 选择   enter 确定','muted');s.link(x+w-10,18,8,1,'01-main','关闭菜单回到草稿');s.save('05-menu')
s,l,r,x,w=shell(title='新会话',new=True);s.text(x+4,13,'开始一件新的工作。','fg',True);s.text(x+4,15,'描述修改，或贴出需要分析的错误。','muted');compose(s,x,w,['检查提交回执与草稿持久化。'],status='',running=False,send='01-main');s.link(1,8,22,2,'01-main','返回原会话');s.save('06-new')
s,l,r,x,w=shell(state='需要回答');user(s,x,4,w,'重启后也要恢复之前未提交的内容。');s.text(x+2,9,'需要确认草稿的保存范围。','fg',True);s.text(x+2,12,'恢复所有会话的草稿，还是只恢复最后一个？')
for yy,t in [(16,'所有会话'),(18,'只恢复最后一个')]:
 s.text(x+4,yy,'○ '+t,'accent');s.link(x+2,yy,w-4,1,'08-answer','选择'+t)
s.text(x+2,22,'也可以直接输入你的要求。','muted');compose(s,x,w,['恢复所有会话，包括光标位置。'],status='回答当前问题',running=False,send='53-free-answer',action_override='enter 回答');s.save('07-question')
s,l,r,x,w=shell();user(s,x,4,w,'恢复所有会话，包括光标位置。');s.text(x+2,9,'已收到回答，将按所有会话保存草稿与光标。');s.text(x+2,12,'问题与回答保持关联，可在历史中继续查看。','muted');compose(s,x,w,status='继续处理草稿恢复',send='15-queued');s.save('08-answer')
s,l,r,x,w=shell(state='需要配置');user(s,x,4,w,'检查提交回执与草稿持久化。');s.text(x+2,9,'输入已保存，尚未开始执行。','fg',True);s.text(x+2,12,'当前没有可用模型。选择后继续处理这条输入。','muted');s.text(x+2,16,'选择模型 ›','accent');s.link(x+2,16,16,1,'10-model','配置模型');compose(s,x,w,status='需要配置模型',running=False);s.save('09-config')
s,l,r,x,w=shell(state='需要配置');s.text(x+2,6,'选择模型','fg',True);s.text(x+2,8,'仅列出当前配置中可用的模型。','muted');s.rect(x,12,w,3,'input');s.text(x+3,13,'GPT-5.5','fg',True);s.text(x+24,13,'已配置','muted');s.link(x,12,w,3,'19-resume','选择示例模型');s.text(x+2,19,'esc 返回','muted');s.link(x+2,19,12,1,'09-config','返回配置提示');s.save('10-model')
s,l,r,x,w=shell();conversation(s,x,w,False,True);compose(s,x,w,DRAFT,focus=False,menu=False,interactive=False);rx=s.w-r+3;s.text(rx-2,1,'›','accent');s.text(rx,1,'草稿恢复','fg',True)
for yy,t,v in [(5,'目标','保留提交之后的新编辑'),(9,'完成条件','旧回执不清空新草稿'),(13,'范围','会话输入与持久化'),(17,'状态','执行中')]:s.text(rx,yy,t,'muted');s.text(rx,yy+1,v)
s.text(rx,36,'esc 返回来源','accent');s.link(rx,36,16,1,'12-source','返回来源');s.save('11-detail')
s,l,r,x,w=shell();conversation(s,x,w,True,True);s.text(x+38,17,'›','accent');compose(s,x,w,DRAFT,focus=False,menu=False,interactive=False);s.link(x,33,w,5,'01-main','恢复输入焦点');s.save('12-source')
s,l,r,x,w=shell();user(s,x,4,w,'再检查中文和组合字符，别让光标跳位。');s.text(x+2,9,'读取记录','fg',True);s.text(x+2,12,'✓ editor.rs','green');s.text(x+4,14,'编辑缓冲、光标定位和显示宽度。','muted');s.text(x+2,17,'✓ input.rs','green');s.text(x+4,19,'键盘事件、粘贴和提交入口。','muted');s.text(x+2,24,'收起记录','accent');s.link(x+2,24,16,1,'01-main','收起读取记录');compose(s,x,w,DRAFT,send='15-queued');s.save('13-tool')
s,l,r,x,w=shell(state='执行失败');user(s,x,4,w,'执行回归测试，并报告结果。');s.text(x+2,9,'× 测试未能运行','red',True);s.text(x+2,12,'error: failed to download dependency','red');s.text(x+2,13,'network request timed out','muted');s.text(x+2,17,'这次没有测试结果，不能据此判断修改通过。');s.text(x+2,21,'将重试要求放入输入区','accent');s.link(x+2,21,28,1,'20-retry-draft','准备重试草稿');compose(s,x,w,status='网络请求超时',running=False);s.save('14-failure')
s,l,r,x,w=shell();conversation(s,x,w);s.rect(x,26,w,3,'bg');s.text(x+2,26,'补充要求已保存，等待处理。','accent');s.text(x+2,28,'停止全部会取消当前执行及待处理输入。','muted');compose(s,x,w,status='执行中 · 有待处理输入',queue=True,stop_target='35-stopping');s.save('15-queued')
s,l,r,x,w=shell(state='可继续');s.text(x+2,7,'执行已停止。','fg',True);s.text(x+2,10,'待处理输入也已取消，历史记录仍保留。','muted');s.text(x+2,14,'恢复补充要求到草稿','accent');s.link(x+2,14,30,1,'21-restored','恢复已取消输入到草稿');compose(s,x,w,status='',running=False);s.save('16-stopped')
s,l,r,x,w=shell();conversation(s,x,w,False);compose(s,x,w,DRAFT,focus=False,menu=False);s.text(1,8,'›','accent');s.link(1,8,22,2,'07-question','切到需要回答的会话');s.link(1,5,22,2,'01-main','返回当前会话');s.save('17-sessions')
s,l,r,x,w=shell();s.text(x+2,6,'快捷键','fg',True)
for yy,k,t in [(10,'Ctrl+方向键','移动焦点'),(13,'Enter','提交输入'),(16,'Alt/Shift+Enter','插入换行'),(19,'Esc','先关闭当前层，再停止执行'),(22,'Ctrl+C / Ctrl+Q','保存普通草稿并退出')]:s.text(x+2,yy,k,'accent');s.text(x+25,yy,t)
s.text(x+2,28,'返回输入','accent');s.link(x+2,28,16,1,'01-main','关闭帮助');s.save('18-help')
s,l,r,x,w=shell(state='可继续');s.text(x+2,8,'模型已选择，先前输入仍保留。','fg',True);s.text(x+2,11,'恢复动作按真实 App 状态提供。','muted');s.text(x+2,16,'返回已保存输入','accent');s.link(x+2,16,26,1,'04-idle','返回已保存输入演示');compose(s,x,w,status='',running=False);s.save('19-resume')
s,l,r,x,w=shell(state='执行失败');s.text(x+2,8,'测试未能运行：网络请求超时。','red');compose(s,x,w,['重新运行回归测试并报告结果。'],status='',running=False,send='01-main');s.save('20-retry-draft')
s,l,r,x,w=shell(state='可继续');s.text(x+2,8,'已恢复补充要求，你可以修改后重新提交。');compose(s,x,w,DRAFT,status='',running=False,send='01-main');s.save('21-restored')
for name,ww,hh in [('22-medium',120,30),('23-narrow',80,24)]:
 s,l,r,x,w=shell(ww,hh,title='草稿恢复');user(s,x,4,w,'再检查中文和组合字符。');s.text(x+2,9,'输入定位会按实际字符宽度验证。');s.text(x+2,12,'读取  editor.rs · input.rs','muted');compose(s,x,w,['保留草稿与光标位置。'],status='验证输入定位',menu=False);s.save(name)
s,l,r,x,w=shell(40,12,title='草稿恢复');s.text(x+2,3,'输入已保存。');compose(s,x,w,['保留光标位置。'],status='',running=False,menu=False);s.save('24-minimum')
print('24 design state boards generated')
# Revised transitions. Each connected path keeps its own fixture identity.
def multiline_user(s,x,y,w,lines):
 s.rect(x,y,w,len(lines)+2,'user')
 for i,t in enumerate(lines):s.text(x+1,y+1+i,'│','dim');s.text(x+4,y+1+i,t)
def tail(s,x,w,lines=DRAFT,canceled=False):
 s.text(x+2,4,'↑ 查看较早对话','accent');# Historical text remains visible; no cross-fixture history link.
 user(s,x,7,w,'再检查中文和组合字符，别让光标跳位。')
 s.text(x+2,12,'我会把输入定位按实际字符宽度一起验证。')
 s.text(x+2,15,'读取  editor.rs · input.rs','muted')
 multiline_user(s,x,19,w,lines)
 s.text(x+4,20+len(lines)+1,'已取消' if canceled else '已保存 · 待处理','muted' if canceled else 'accent')

def menu_board(name='05-menu',model=False,back='01-main'):
 s,l,r,x,w=shell();conversation(s,x,w,False);compose(s,x,w,['/model' if model else '/'],interactive=False,focus=False,action_override='选择命令')
 top=24 if model else 21
 s.rect(x,top,w,31-top,'input');s.text(x+2,top+1,'选择模型' if model else '命令','fg',True);s.text(x+w-10,top+1,'esc 关闭','muted')
 choices=[('GPT-5.5','已配置','44-model-selected')] if model else [('/new','新建会话','06-new'),('/sessions','切换会话','17-sessions'),('/model','选择模型','43-model-menu'),('/help','快捷键与帮助','18-help')]
 for i,(a,b,t) in enumerate(choices):
  y=top+3+i
  if i==0:s.rect(x+1,y,w-2,1,'selected')
  s.text(x+3,y,a);s.text(x+23,y,b,'muted');s.link(x+1,y,w-2,1,t,'选择'+a)
 s.text(x+3,29,'↑↓ 选择   enter 确定','muted');s.link(x+w-10,top+1,8,1,back,'关闭当前菜单');s.save(name)
menu_board();menu_board('43-model-menu',True)
s,l,r,x,w=shell();conversation(s,x,w);compose(s,x,w,DRAFT,send='15-queued',menu=True,stop_target='51-main-stopping');s.save('44-model-selected')
s,l,r,x,w=shell(title='新会话',new=True);s.text(x+4,13,'开始一件新的工作。','fg',True);s.text(x+4,15,'描述修改，或贴出需要分析的错误。','muted');compose(s,x,w,['检查提交回执与草稿持久化。'],status='',running=False,send='26-new-sent');s.link(1,8,22,2,'01-main','返回原会话');s.save('06-new')
s,l,r,x,w=shell(title='新会话',new=True);user(s,x,4,w,'检查提交回执与草稿持久化。');s.text(x+2,9,'输入已保存，正在读取相关代码。');compose(s,x,w,status='检查提交回执');s.link(1,8,22,2,'01-main','返回原会话');s.save('26-new-sent')
s,l,r,x,w=shell(state='需要回答');user(s,x,4,w,'重启后也要恢复之前未提交的内容。');s.text(x+2,9,'需要确认草稿的保存范围。','fg',True);s.text(x+2,12,'恢复所有会话的草稿，还是只恢复最后一个？')
for yy,t,dest in [(16,'所有会话','08-answer'),(18,'只恢复最后一个','25-answer-last')]:s.text(x+4,yy,'○ '+t,'accent');s.link(x+2,yy,w-4,1,dest,'选择'+t)
s.text(x+2,22,'也可以直接输入你的要求。','muted');compose(s,x,w,['恢复所有会话，包括光标位置。'],status='回答当前问题',running=False,send='53-free-answer',action_override='enter 回答');s.save('07-question')
s,l,r,x,w=shell();user(s,x,4,w,'只恢复最后一个会话。');s.text(x+2,9,'已收到回答，只恢复最后一个会话的草稿。');compose(s,x,w,status='继续处理草稿恢复');s.save('25-answer-last')
s,l,r,x,w=shell(state='需要配置');user(s,x,4,w,'检查提交回执与草稿持久化。');s.text(x+2,9,'输入已保存，尚未开始执行。','fg',True);s.text(x+2,12,'当前会话尚未选择模型。','muted');s.text(x+2,16,'选择模型 ›','accent');s.link(x+2,16,16,1,'10-model','配置模型');compose(s,x,w,status='需要选择模型',running=False,model='/model 选择模型');s.save('09-config')
s,l,r,x,w=shell(state='需要配置');user(s,x,4,w,'检查提交回执与草稿持久化。');compose(s,x,w,status='需要选择模型',running=False,model='/model 选择模型',interactive=False,focus=False,action_override='选择模型')
s.rect(x,24,w,7,'input');s.text(x+2,25,'选择模型','fg',True);s.text(x+w-10,25,'esc 关闭','muted');s.rect(x+1,27,w-2,1,'selected');s.text(x+3,27,'GPT-5.5');s.text(x+23,27,'已配置','muted');s.text(x+3,29,'↑↓ 选择   enter 确定','muted');s.link(x+1,27,w-2,1,'19-resume','选择示例模型');s.link(x+w-10,25,8,1,'09-config','返回配置提示');s.save('10-model')
s,l,r,x,w=shell();user(s,x,4,w,'检查提交回执与草稿持久化。');s.text(x+2,9,'模型已选择，原输入仍保留。','fg',True);s.text(x+2,12,'开始读取提交回执和草稿持久化相关代码。');compose(s,x,w,status='处理已保存输入');s.save('19-resume')
s,l,r,x,w=shell();tail(s,x,w);compose(s,x,w,status='执行中 · 有待处理输入',queue=True,stop_target='35-stopping');s.save('15-queued')
s,l,r,x,w=shell(state='停止中');tail(s,x,w);s.text(x+2,27,'停止请求已发出，等待执行结束。','accent');s.text(x+2,29,'待处理输入也在本次停止范围内。','muted');compose(s,x,w,status='',running=False,focus=False,interactive=False,action_override='停止中');s.text(x+2,31,'等待执行停止…','muted');s.link(x+2,31,32,1,'16-stopped','模拟收到停止结果');s.save('35-stopping')
s,l,r,x,w=shell(state='可继续');tail(s,x,w,canceled=True);s.text(x+2,27,'执行已停止，待处理输入已取消。','muted');s.text(x+2,29,'恢复补充要求到草稿','accent');s.link(x+2,29,30,1,'21-restored','恢复已取消输入到草稿');compose(s,x,w,status='',running=False);s.save('16-stopped')
s,l,r,x,w=shell(state='可继续');tail(s,x,w,canceled=True);compose(s,x,w,DRAFT,status='',running=False,send='28-restored-sent');s.save('21-restored')
s,l,r,x,w=shell();tail(s,x,w);compose(s,x,w,status='处理重新提交的补充要求');s.save('28-restored-sent')
s,l,r,x,w=shell();tail(s,x,w,DRAFT+['切回会话后，从离开的位置继续。']);compose(s,x,w,status='已保存补充要求',queue=True,stop_target='31-multi-stopping');s.save('29-multi-accepted')
s,l,r,x,w=shell(state='停止中');tail(s,x,w,DRAFT+['切回会话后，从离开的位置继续。']);compose(s,x,w,status='正在停止，待处理输入也会取消',running=False,interactive=False,action_override='停止中');s.text(x+2,28,'等待执行停止…','muted');s.link(x+2,28,32,1,'30-multi-stopped','模拟三行输入停止结果');s.save('31-multi-stopping')
s,l,r,x,w=shell(state='可继续');tail(s,x,w,DRAFT+['切回会话后，从离开的位置继续。'],True);s.text(x+2,28,'恢复三行要求到草稿','accent');s.link(x+2,28,30,1,'32-multi-restored','恢复完整三行草稿');compose(s,x,w,status='',running=False);s.save('30-multi-stopped')
s,l,r,x,w=shell(state='可继续');s.text(x+2,9,'三行补充要求均已恢复，可继续编辑。');compose(s,x,w,DRAFT+['切回会话后，从离开的位置继续。'],status='',running=False,send='29-multi-accepted');s.save('32-multi-restored')
s,l,r,x,w=shell();s.text(x+2,6,'先前测试因网络超时未能运行。','muted');user(s,x,10,w,'重新运行回归测试并报告结果。');s.text(x+2,16,'重试要求已保存，正在重新检查。');compose(s,x,w,status='重新运行测试');s.save('27-retry-sent')
s,l,r,x,w=shell(state='执行失败');s.text(x+2,8,'测试未能运行：网络请求超时。','red');compose(s,x,w,['重新运行回归测试并报告结果。'],status='',running=False,send='27-retry-sent');s.save('20-retry-draft')
s,l,r,x,w=shell();conversation(s,x,w,False);compose(s,x,w,DRAFT,focus=False,interactive=False);s.text(1,8,'›','accent');s.link(1,8,22,2,'45-other-session','切到Context engine');s.link(1,5,22,2,'01-main','返回当前会话');s.save('17-sessions')
s,l,r,x,w=shell(title='Context engine');s.rect(1,5,22,2,'side');s.text(3,5,'草稿恢复');s.text(3,6,'执行中','muted');s.rect(1,8,22,2,'selected');s.text(3,8,'Context engine','fg',True);s.text(3,9,'需要回答','accent');user(s,x,4,w,'为代码索引设计增量更新。');s.text(x+2,10,'索引是否需要包含未跟踪文件？','fg',True);compose(s,x,w,['包含未跟踪文件，但排除构建产物。'],status='回答索引范围问题',running=False);s.link(1,5,22,2,'01-main','切回并恢复原草稿');s.save('45-other-session')
s,l,r,x,w=shell();s.text(x+2,3,'正在阅读较早消息','muted');user(s,x,6,w,'切换会话时保留新草稿，补上回归测试。');s.text(x+2,11,'旧回执与新的编辑内容需要用版本隔离。');s.text(x+2,14,'• 提交时记录草稿版本。');s.text(x+2,16,'• 回执只清理同一版本。');s.text(x+2,19,'• 会话切换不改变其它会话的编辑位置。');s.text(x+2,27,'↓ 有新回复 · 回到最新','accent');s.link(x+2,27,34,1,'01-main','返回最新对话');compose(s,x,w,DRAFT,focus=False,interactive=False);s.save('33-history')
s,l,r,x,w=shell(state='可继续');s.text(x+2,7,'这条问题已结束，答案尚未提交。','fg',True);s.text(x+2,10,'你写的内容仍然保留。','muted');s.text(x+2,15,'转为普通补充要求','accent');s.link(x+2,15,30,1,'46-converted-answer','将过期答案转为普通输入');compose(s,x,w,['恢复所有会话，包括光标位置。'],status='问题已过期 · 回答不可提交',running=False,interactive=False,action_override='回答已结束');s.save('34-expired-question')
s,l,r,x,w=shell(state='可继续');s.text(x+2,8,'已转为普通补充要求，不再关联过期问题。');compose(s,x,w,['恢复所有会话，包括光标位置。'],status='',running=False);s.save('46-converted-answer')
s,l,r,x,w=shell();multiline_user(s,x,6,w,DRAFT);s.text(x+4,10,'提交中 · 尚未收到保存回执','muted');s.text(x+2,16,'可继续写下一条，原提交不会清空新的编辑。');compose(s,x,w,['再检查快速切换会话的情况。'],status='提交确认中',interactive=False,action_override='等待确认');s.text(x+2,23,'正在等待保存回执…','muted');s.link(x+2,23,26,1,'37-uncertain','模拟回执未确认');s.save('36-submitting')
s,l,r,x,w=shell(state='需要处理');multiline_user(s,x,6,w,DRAFT);s.text(x+2,13,'未确认是否保存，不能当作提交失败。','red');s.text(x+2,17,'使用原请求身份重试','accent');s.link(x+2,17,32,1,'47-confirmed','以同一请求身份重试演示');compose(s,x,w,['再检查快速切换会话的情况。'],status='等待确认原提交',running=False,interactive=False,action_override='等待确认');s.save('37-uncertain')
s,l,r,x,w=shell();multiline_user(s,x,6,w,DRAFT);s.text(x+2,13,'已确认原提交保存成功。','green');s.text(x+2,17,'后来写下的内容仍保留在输入区。','muted');compose(s,x,w,['再检查快速切换会话的情况。'],status='继续处理');s.save('47-confirmed')
# Narrow object readers use the same source and draft; only the presentation changes.
for name,ww,hh,back in [('38-detail-medium',120,30,'40-source-medium'),('39-detail-narrow',80,24,'41-source-narrow')]:
 s,l,r,x,w=shell(ww,hh,title='草稿恢复 · 任务详情');s.text(x+2,5,'目标','muted');s.text(x+2,6,'保留提交之后的新编辑。');s.text(x+2,9,'完成条件','muted');s.text(x+2,10,'旧回执不清空新草稿。');s.text(x+2,13,'状态','muted');s.text(x+2,14,'执行中');s.text(x+2,hh-3,'esc 返回来源','accent');s.link(x+2,hh-3,26,1,back,'返回窄屏来源');s.save(name)
for name,ww,hh,dest in [('40-source-medium',120,30,'38-detail-medium'),('41-source-narrow',80,24,'39-detail-narrow')]:
 s,l,r,x,w=shell(ww,hh,title='草稿恢复');user(s,x,4,w,'再检查中文和组合字符。');s.text(x+2,9,'输入定位会按实际字符宽度验证。');s.rect(x+1,12,22,1,'selected');s.text(x+2,12,'› 草稿恢复详情','accent');s.link(x+1,12,24,1,dest,'重新打开详情');y=compose(s,x,w,['保留草稿与光标位置。'],status='',focus=False,interactive=False);s.link(x,y,w,4,'22-medium' if ww==120 else '23-narrow','恢复窄屏输入焦点');s.save(name)
s=Screen(40,12);s.text(3,0,'命令','fg',True)
for y,t,target in [(3,'/new','06-new'),(4,'/sessions','17-sessions'),(5,'/help','18-help')]:s.text(3,y,t);s.link(2,y,35,1,target,'窄屏'+t)
s.text(3,9,'↑↓ 选择   enter 确定','muted');s.text(3,11,'esc 返回输入','accent');s.link(3,11,26,1,'24-minimum','返回40列输入');s.save('42-menu-minimum')
s,l,r,x,w=shell();s.text(x+2,5,'读取记录 · editor.rs','fg',True)
for yy,t in [(8,'编辑缓冲按字符边界处理。'),(10,'光标定位使用显示宽度。'),(12,'中文组合输入不按字节截断。'),(14,'粘贴内容进入当前草稿。'),(16,'会话切换保留阅读位置。'),(18,'长输出继续滚动可达。')]:s.text(x+2,yy,t,'muted')
s.text(x+2,23,'↓ 继续阅读','accent');s.link(x+2,23,20,1,'49-tool-tail','阅读记录下一段');s.text(x+2,28,'返回工具来源','accent');s.link(x+2,28,24,1,'50-tool-source','返回工具来源');s.save('48-tool-long')
s,l,r,x,w=shell();s.text(x+2,5,'读取记录 · editor.rs','fg',True);s.text(x+2,9,'较后内容：按会话保存草稿版本。');s.text(x+2,12,'到达记录末尾。','muted');s.text(x+2,19,'↑ 返回上一段','accent');s.link(x+2,19,24,1,'48-tool-long','返回记录上一段');s.text(x+2,25,'返回工具来源','accent');s.link(x+2,25,24,1,'50-tool-source','回到工具来源');s.save('49-tool-tail')
s,l,r,x,w=shell();conversation(s,x,w,False);s.text(x,26,'›','accent');s.link(x+2,26,42,1,'13-tool','再次打开记录');y=compose(s,x,w,DRAFT,focus=False,interactive=False);s.link(x,y,w,5,'01-main','返回编辑');s.save('50-tool-source')
s,l,r,x,w=shell();user(s,x,4,w,'再检查中文和组合字符，别让光标跳位。');s.text(x+2,9,'读取记录','fg',True);s.text(x+2,12,'✓ editor.rs','green');s.text(x+4,14,'编辑缓冲、光标定位和显示宽度。','muted');s.text(x+4,16,'查看完整记录 ›','accent');s.link(x+4,16,24,1,'48-tool-long','打开完整读取记录');s.text(x+2,19,'✓ input.rs','green');s.text(x+4,21,'键盘事件、粘贴和提交入口。','muted');s.text(x+2,26,'返回工具来源','accent');s.link(x+2,26,24,1,'50-tool-source','返回工具来源');s.save('13-tool')
print('50 B design and transition snapshots generated')
# Long content stress fixture. Actual terminal implementation uses stable scroll anchors.
s,l,r,x,w=shell();s.text(x+2,4,'读取记录 · editor.rs','fg',True);s.text(x+2,6,'片段 1 / 2','muted')
code=['fn apply_receipt(&mut self, receipt: Receipt) {','    let Some(draft) = self.drafts.get_mut(&receipt.session) else {','        return;','    };','','    if draft.revision == receipt.revision {','        draft.text.clear();','        draft.cursor = 0;','    }','','    self.pending.remove(&receipt.request_id);','}','','fn select_session(&mut self, id: SessionId) {','    self.save_current_draft();','    self.save_reading_anchor();','','    self.current = id;','    self.restore_draft(id);','    self.restore_cursor(id);','    self.restore_reading_anchor(id);','}']
for i,line in enumerate(code):s.text(x+2,8+i,line,'code' if line.startswith('fn ') else 'fg')
s.text(x+2,33,'下一段 ↓','accent');s.link(x+2,33,16,1,'49-tool-tail','阅读记录下一段');s.text(x+2,37,'返回工具来源','accent');s.link(x+2,37,24,1,'50-tool-source','返回工具来源');s.save('48-tool-long')
s,l,r,x,w=shell(state='停止中');conversation(s,x,w,False);compose(s,x,w,DRAFT,status='正在停止当前执行',running=False,interactive=False,action_override='停止中');s.text(x+2,29,'等待执行停止…','muted');s.link(x+2,29,26,1,'52-main-stopped','设计演示收到当前执行停止结果');s.save('51-main-stopping')
s,l,r,x,w=shell(state='可继续');conversation(s,x,w,False);compose(s,x,w,DRAFT,status='已停止 · 草稿保留',running=False,interactive=False);s.save('52-main-stopped')
s,l,r,x,w=shell();user(s,x,4,w,'恢复所有会话。');s.text(x+2,9,'已收到回答，将保存所有会话的草稿。');compose(s,x,w,status='继续处理草稿恢复');s.save('08-answer')
s,l,r,x,w=shell();user(s,x,4,w,'恢复所有会话，包括光标位置。');s.text(x+2,9,'已收到回答，将保存所有会话的草稿与光标。');compose(s,x,w,status='继续处理草稿恢复');s.save('53-free-answer')
s,l,r,x,w=shell(title='新会话',new=True);s.text(x+4,13,'开始一件新的工作。','fg',True);s.text(x+4,15,'描述修改，或贴出需要分析的错误。','muted');compose(s,x,w,['描述你想完成的事…'],status='',running=False);s.save('54-empty')
