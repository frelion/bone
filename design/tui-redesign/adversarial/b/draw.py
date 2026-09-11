from pathlib import Path
from html import escape
import unicodedata
P=Path(__file__).parent
W,H=160,40; CW,CH=9,20
C={'bg':'#1b1b1b','side':'#141414','input':'#292929','text':'#ece9e3','muted':'#aaa6a0','line':'#44413d','selected':'#32302d','accent':'#d7c6ad','code':'#c9c0b4','rose':'#d0aba1'}
def width(s): return sum(0 if unicodedata.combining(c) else 2 if unicodedata.east_asian_width(c) in 'WF' else 1 for c in s)
a=[]
def rect(x,y,w,h,c):
 assert 0<=x<=x+w<=W and 0<=y<=y+h<=H
 a.append(f'<rect x="{x*CW}" y="{y*CH}" width="{w*CW}" height="{h*CH}" fill="{C.get(c,c)}"/>')
def text(x,y,t,c='text',bold=False):
 assert x+width(t)<=W and y<H,(x,y,t)
 spans=[]
 for ch in t:
  spans.append(f'<tspan x="{x*CW}">{escape(ch)}</tspan>');x+=width(ch)
 a.append(f'<text y="{y*CH+15}" fill="{C.get(c,c)}" font-weight="{600 if bold else 400}">{"".join(spans)}</text>')
rect(0,0,W,H,'bg');rect(0,0,24,H,'side');rect(120,0,40,H,'side')
text(3,1,'BONE','accent',True);text(3,3,'会话','muted')
for i,(name,st) in enumerate([('草稿恢复','执行中'),('Context engine','需要回答'),('修复启动错误','草稿'),('API 超时处理','已结束'),('工具权限','已结束'),('会话存储','已结束')]):
 y=5+i*3
 if i==0:rect(1,y,22,2,'selected')
 text(3,y,name,'text' if i==0 else 'muted',i==0);text(3,y+1,st,'accent' if i==1 else 'muted')
text(3,38,'~/BONE','muted')
text(30,1,'修复会话切换后的草稿恢复','text',True)
# Narrow reading column; no detached code tile. Two real turns expose conversation rhythm.
rect(28,4,84,3,'#252525');text(29,5,'│','muted');text(32,5,'切换会话时保留新草稿，补上回归测试。')
text(30,8,'已修复提交回执对新草稿的误清理。','text',True)
text(30,10,'现在只有提交版本与当前草稿一致时，输入才会清空。')
text(30,11,'会话切换会分别保留草稿、光标和阅读位置。')
text(30,13,'if','rose');text(33,13,'receipt.revision == draft.revision {','text')
text(30,14,'    draft.clear();','text');text(30,15,'}','code')
text(30,17,'✓ 迟到回执   ✓ 会话切换','muted');text(68,17,'草稿恢复 ›','muted')
# New user turn is close to current activity, previous response remains legible.
rect(28,20,84,3,'#252525');text(29,21,'│','muted');text(32,21,'再检查中文和组合字符，别让光标跳位。')
text(30,24,'我会把输入定位按实际字符宽度一起验证。')
text(30,26,'读取','muted');text(38,26,'editor.rs · input.rs','muted')
text(30,27,'检查','accent');text(38,27,'中文输入与 grapheme 边界','muted')
# Current editing surface is five rows, with real two-line draft instead of padding.
text(30,31,'◌ 验证输入定位','accent');text(100,31,'esc 停止','muted')
rect(28,33,84,5,'input')
text(30,34,'›','accent');text(32,34,'组合字符之后继续输入时，');text(32,35,'也保留原来的光标位置。');text(54,35,'▏','accent')
text(32,37,'Worker · GPT-5.5','muted');text(83,37,'/ 命令  alt+enter 换行','muted')

(P/'main.svg').write_text('<svg xmlns="http://www.w3.org/2000/svg" width="1440" height="800" viewBox="0 0 1440 800"><title>BONE · B 连续对话与写作台 · 160×40</title><g font-family="SFMono-Regular,Menlo,PingFang SC,monospace" font-size="14">'+''.join(a)+'</g></svg>')
print(P/'main.svg')
