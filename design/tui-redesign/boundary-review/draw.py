"""Design proposals only: synthetic content on a terminal cell grid, not runtime captures."""
from pathlib import Path
from html import escape
import unicodedata

P = Path(__file__).parent
C = dict(bg='#090909', surface='#191919', input='#222222', fg='#f5f5f5',
         muted='#ababab', line='#555555', accent='#ffad32', green='#7cde98',
         cyan='#62dce8', purple='#c59aff', selection='#252525')

def width(t):
    return sum(0 if unicodedata.combining(c) else 2 if unicodedata.east_asian_width(c) in 'WF' else 1 for c in t)

class Board:
    def __init__(self):
        self.parts=[]
        self.rect(0,0,160,40,'bg')
    def rect(self,x,y,w,h,c):
        assert 0<=x<=x+w<=160 and 0<=y<=y+h<=40
        self.parts.append(f'<rect x="{x*9}" y="{y*20}" width="{w*9}" height="{h*20}" fill="{C.get(c,c)}"/>')
    def text(self,x,y,t,c='fg',bold=False):
        assert x+width(t)<=160 and 0<=y<40,(x,y,t)
        spans=[]
        for ch in t:
            spans.append(f'<tspan x="{x*9}">{escape(ch)}</tspan>');x+=width(ch)
        self.parts.append(f'<text y="{y*20+15}" fill="{C.get(c,c)}" font-weight="{600 if bold else 400}">'+''.join(spans)+'</text>')
    def save(self,name):
        (P/(name+'.svg')).write_text('<svg xmlns="http://www.w3.org/2000/svg" width="1440" height="800" viewBox="0 0 1440 800"><title>BONE design proposal — synthetic fixture</title><g font-family="Menlo,PingFang SC,monospace" font-size="14">'+''.join(self.parts)+'</g></svg>')

def board(boundary='line',empty=False,menu=False):
    b=Board()
    if boundary in ['tone','tone-line']:
        b.rect(32,0,88,40,'#101010');b.rect(0,0,32,40,'#080808');b.rect(120,0,40,40,'#080808')
    elif boundary=='gutter':
        b.rect(0,0,30,40,'#141414');b.rect(122,0,38,40,'#141414')
    if boundary in ['line','tone-line']:
        # Continuous conceptual separator inside one reserved cell; real terminal glyph needs verification.
        for x in [31,120]:
            b.parts.append(f'<rect x="{x*9+4}" y="0" width="1" height="800" fill="{C["line"]}"/>')
    b.text(3,1,'BONE','accent',True);b.text(3,3,'会话','fg',True);b.text(24,3,'/new','muted')
    b.rect(2,5,27,1,'selection');b.text(2,5,'▏','accent');b.text(4,5,'草稿恢复','fg',True)
    # Current selection is already shown by the marker.
    b.text(4,8,'Context engine');b.text(4,9,'需要回答','accent')
    b.text(4,11,'修复启动错误');b.text(4,12,'有草稿','purple')
    b.text(4,14,'API 超时处理');b.text(4,16,'工具权限');b.text(4,18,'会话存储')
    b.text(3,37,'工作目录','muted');b.text(3,38,'~/BONE','muted')
    b.text(38,1,'草稿恢复','fg',True)
    b.rect(36,4,80,3,'surface');b.text(38,5,'▏','accent');b.text(40,5,'切换会话时保留新草稿，补上回归测试。')
    b.text(38,8,'已修复提交回执对新草稿的误清理。','fg',True)
    b.text(38,10,'只有提交版本与当前草稿一致时，输入才会清空。')
    b.text(38,11,'切换会话会分别保留草稿、光标和阅读位置。')
    b.text(38,13,'if','purple');b.text(41,13,'receipt.revision == draft.revision {')
    b.text(42,14,'draft.clear();','cyan');b.text(38,15,'}')
    b.text(38,17,'✓','green');b.text(40,17,'迟到回执与会话切换测试通过','muted')
    b.rect(36,20,80,3,'surface');b.text(38,21,'▏','accent');b.text(40,21,'再检查中文和组合字符，别让光标跳位。')
    b.text(38,24,'我会一起验证中文输入和组合字符的定位。')
    b.text(38,26,'✓','green');b.text(40,26,'读取','muted');b.text(46,26,'editor.rs · input.rs','cyan')
    b.text(38,27,'›','accent');b.text(40,27,'检查组合字符边界','fg');b.text(105,27,'详情','muted')
    b.rect(36,33,80,5,'input')
    b.text(38,34,'›','accent');b.text(40,34,'组合字符之后继续输入时，')
    b.text(40,35,'也保留原来的光标位置。');b.rect(40+width('也保留原来的光标位置。'),35,1,1,'accent')
    b.text(40,37,'GPT-5.5','fg');b.text(49,37,'· ChatGPT','muted')
    b.text(38,39,'/ 命令','muted');b.text(51,39,'alt+enter 换行','muted');b.text(104,39,'enter 追加','accent')
    b.text(106,31,'esc 停止','muted')
    if not empty:
        b.text(124,1,'检查组合字符边界','fg',True)
        b.text(124,3,'任务 · 进行中','green')
        b.text(124,6,'范围','muted');b.text(124,7,'草稿编辑与会话切换')
        b.text(124,9,'完成条件','muted');b.text(124,10,'中文与组合字符的光标位置');b.text(124,11,'在编辑和切换后保持正确。')
        b.text(124,13,'执行结果','muted');b.text(124,14,'等待本次检查完成','muted')
        b.text(124,39,'esc 返回来源','muted')
    if not empty or menu:
        b.rect(38,34,1,1,'input')
        b.rect(40+width('也保留原来的光标位置。'),35,1,1,'input')
        b.rect(36,39,80,1,'bg');b.rect(36,31,80,1,'bg')
    else:
        b.text(38,31,'◌ 正在验证输入定位','accent')
    if menu:
        b.rect(36,17,80,15,'input')
        b.text(39,18,'模型与接入','fg',True);b.text(106,18,'esc 返回','muted')
        b.text(39,20,'模型','muted')
        b.rect(38,21,76,1,'accent');b.text(40,21,'GPT-5.5  ·  ChatGPT','#090909',True);b.text(104,21,'当前','#090909')
        b.text(40,22,'Claude Sonnet');b.text(65,22,'· Anthropic','muted')
        b.text(39,25,'管理接入','muted');b.text(40,26,'编辑 ChatGPT');b.text(40,27,'编辑 Anthropic')
        b.text(40,28,'+ 添加模型或接入')
        b.text(39,30,'↑↓ 选择','muted');b.text(102,30,'enter 确定','muted')
        b.rect(40+width('也保留原来的光标位置。'),35,1,1,'input');b.rect(36,39,80,1,'bg')
    return b

for shade in ['303030','404040','505050']:
    C['line']='#'+shade
    board('line').save('line-'+shade)
C['line']='#505050'
for kind in ['tone','tone-line','gutter','line','flat']:
    board(kind).save('boundary-'+kind)
board('line',empty=True).save('line-empty-right')
board('line',empty=True,menu=True).save('line-model')

html='''<!doctype html><meta charset="utf-8"><title>BONE · 边界与层级讨论稿</title><style>body{margin:24px;background:#ededeb;color:#202020;font:15px system-ui}h1{font-size:22px}button{padding:10px 16px;margin:0 8px 12px 0;cursor:pointer}img{display:block;width:100%;max-width:1440px;height:calc(100vh - 205px);object-fit:contain;object-position:left top}p{max-width:1000px;line-height:1.6}</style><h1>BONE · 三栏边界讨论稿</h1><p>160 × 40 字符格，边界候选使用完全相同的内容和组件。均为设计示意，不是运行截图；分隔线用连续 1px 表示，终端字符接缝需实机验证。示例模型与状态不代表真实配置。</p>'''
for name,label in [('boundary-tone','对照：相近底色'),('boundary-tone-line','原底色 + 细线'),('boundary-gutter','候选：黑色空隙'),('boundary-flat','统一黑底，无线'),('boundary-line','候选：细线分隔'),('line-empty-right','细线：右栏为空'),('line-model','细线：模型菜单')]:
    html+=f'<button onclick="document.getElementById(\'board\').src=\'{name}.svg\'">{label}</button>'
html+='<img id="board" src="boundary-line.svg" alt="终端设计对照图">'
(P/'index.html').write_text(html)
