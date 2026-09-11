from pathlib import Path
from html import escape
import unicodedata
P=Path(__file__).parent
W,H=160,40
CW,CH=9,20
BG='#252a35'; SIDE='#20242e'; INK='#edf0f6'; MUT='#aab4c5'; ACC='#adbcff'; DIM='#768296'; SEL='#414d68'; CODE='#202530'; TRAY='#343e53'; GREEN='#a9d6bb'
def width(t):return sum(0 if unicodedata.combining(c) else 2 if unicodedata.east_asian_width(c) in 'WF' else 1 for c in t)
a=[]
def rect(x,y,w,h,c):
 assert x+w<=W and y+h<=H
 a.append(f'<rect x="{x*CW}" y="{y*CH}" width="{w*CW}" height="{h*CH}" fill="{c}"/>')
def text(x,y,t,c=INK,b=False):
 assert x+width(t)<=W and y<H,(x,y,t)
 ts=[]
 for ch in t:ts.append(f'<tspan x="{x*CW}">{escape(ch)}</tspan>');x+=width(ch)
 a.append(f'<text y="{y*CH+15}" fill="{c}" font-weight="{600 if b else 400}">'+''.join(ts)+'</text>')
rect(0,0,W,H,BG);rect(0,0,24,H,SIDE);rect(120,0,40,H,SIDE)
# Navigation: compact consistent rows, calm title plus small new-session entry.
text(2,1,'BONE',INK,True);text(2,3,'会话',MUT);text(18,3,'＋',ACC)
items=[('草稿恢复','执行中'),('Context engine','需要回答'),('修复启动错误','草稿'),('API 超时处理','就绪'),('工具权限','就绪'),('会话存储','就绪')]
for i,(n,st) in enumerate(items):
 y=5+i*3
 if i==0:rect(1,y,22,2,SEL)
 text(3,y,n,INK if i<2 else MUT,i==0);text(3,y+1,st,ACC if i==1 else MUT)
text(2,37,'~/BONE',MUT)
# Header is a shallow workbench edge, not a decorative paragraph separator.
text(28,1,'草稿恢复',INK,True)
# Transcript: consistent left gutter, short blocks, real multi-turn content.
rect(28,4,80,3,'#303746')
for y in [4,5,6]:text(28,y,'│',MUT)
text(31,5,'切换会话时保留新草稿，补上回归测试。')
text(28,8,'问题出在提交回执：它清空了用户后来写下的内容。')
text(28,9,'只在版本匹配时清空输入，就能保留后续编辑。')
text(28,11,'✓',GREEN);text(30,11,'已读取 2 个文件',MUT)
# Code module is visually a source excerpt; path and language share a single header.
rect(28,13,80,1,'#343b4b');text(30,13,'state/update.rs',MUT);text(101,13,'rust',DIM)
rect(28,14,80,5,CODE)
text(30,15,'if ',ACC);text(33,15,'receipt.revision == draft.revision {')
text(30,16,'    draft.clear();');text(30,17,'}')
text(28,20,'• 提交回执只清理对应版本。')
text(28,21,'• 新草稿与阅读位置仍属于各自会话。')
rect(28,23,80,3,'#303746')
for y in [23,24,25]:text(28,y,'│',MUT)
text(31,24,'再检查中文输入和光标恢复，切换回来也要保留。')
text(28,27,'会把光标和草稿一起恢复，覆盖中文与组合字符。')
text(28,28,'回归测试会模拟提交后继续输入，再切换会话。')
# Status belongs to current output and sits directly beneath it.
text(28,31,'●',GREEN);text(30,31,'正在验证草稿隔离',MUT)
# Full-width editor dock changes geometry; no floating rectangle and no thin rules.
rect(24,34,96,6,TRAY)
text(28,34,'Worker · GPT-5.5',MUT)
text(28,36,'不要覆盖我在运行中继续写的要求。');text(28+width('不要覆盖我在运行中继续写的要求。'),36,'▏',ACC)
text(28,38,'/ 命令',MUT);text(41,38,'alt+enter 换行',MUT);text(91,38,'enter 发送',INK);text(105,38,'esc 停止',MUT)
svg=f'<svg xmlns="http://www.w3.org/2000/svg" width="{W*CW}" height="{H*CH}" viewBox="0 0 {W*CW} {H*CH}"><g font-family="SFMono-Regular,Menlo,PingFang SC,monospace" font-size="14">'+''.join(a)+'</g></svg>'
(P/'main.svg').write_text(svg)
print(P/'main.svg')
