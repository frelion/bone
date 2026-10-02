#!/usr/bin/env python3
"""Render production evidence already captured by the terminal harnesses."""
import html
import json
import re
import unicodedata
from pathlib import Path

HERE = Path(__file__).resolve().parent

def read(path, default=None):
    source = HERE / path
    return json.loads(source.read_text()) if source.exists() else default

def embedded(value):
    return json.dumps(value, ensure_ascii=False).replace('<', '\\u003c').replace('&', '\\u0026')

def cell_rows(screen):
    rows = []
    for line in screen.splitlines():
        cells = []
        for char in line:
            zero = unicodedata.combining(char) or char in ('\u200d', '\ufe0e', '\ufe0f') or 0x1f3fb <= ord(char) <= 0x1f3ff
            if cells and (zero or cells[-1][0].endswith('\u200d')):
                cells[-1][0] += char
            else:
                cells.append([char, 2 if unicodedata.east_asian_width(char) in 'WF' else 1])
        rows.append(cells)
    return rows

def grid_html(screen):
    return ''.join('<div class="termrow">'+''.join(f'<span class="cell" style="width:{width}ch">'+html.escape(text)+'</span>' for text,width in row)+'</div>' for row in cell_rows(screen))

live = read('live-software/summary.json', {})
frames = read('live-software/screens.json', [])
for frame in frames:
    frame['cells'] = cell_rows(frame['screen'])
gates = read('gates.json', {})
before = read('before/failure-0.6.json', {'frames': []})
after = read('pty/80x24/failure.json', {'frames': []})
old_frame = before['frames'][-1]['screen'] if before['frames'] else '待采集'
new_frame = next((f['screen'] for f in after['frames'] if 'HTTP failure' in f['step']), after['frames'][-1]['screen'] if after['frames'] else '待采集')
rows = []
for size in ('80x24', '120x40'):
    report = read(f'pty/{size}/summary.json', {'results': []})
    cases = report['results']
    rows.append(f'<tr><td>{html.escape(size)}</td><td>{sum(c["status"] == "PASS" for c in cases)} / {len(cases)}</td><td><a href="pty/{size}/summary.json">独立结果</a></td></tr>')
body = '''<!doctype html>
<html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>BONE 0.7 · 生产终端交付</title>
<style>
:root{color-scheme:light;--ink:#202623;--muted:#66706a;--line:#d9ded6;--green:#275d45;--paper:#f6f7f1}*{box-sizing:border-box}body{margin:0;background:var(--paper);color:var(--ink);font:16px/1.7 system-ui,-apple-system,sans-serif}main{max-width:1180px;margin:auto;padding:48px 28px 80px}header{max-width:840px}.eyebrow{color:var(--green);font:700 12px/1.5 ui-monospace,monospace;letter-spacing:.14em}h1{font-size:clamp(36px,5vw,64px);font-weight:650;line-height:1.13;letter-spacing:-.04em;margin:14px 0 22px}h2{font-size:27px;letter-spacing:-.03em;margin:0 0 12px}h3{font-size:17px;margin:0 0 8px}p{margin:0 0 14px}.intro{font-size:20px;color:#505d54}a{color:var(--green);text-underline-offset:3px}section{margin-top:52px}.facts{display:flex;gap:36px;flex-wrap:wrap;border-block:1px solid var(--line);padding:22px 0;margin:28px 0}.facts strong{display:block;font-size:24px;font-weight:600}.facts span,.note{color:var(--muted);font-size:14px}.controls{display:flex;gap:10px;align-items:center;margin:18px 0;flex-wrap:wrap}button,select{font:inherit;border:1px solid var(--line);border-radius:6px;background:#fff;color:var(--ink);padding:8px 12px;cursor:pointer}button:hover{border-color:var(--green)}button:focus-visible,select:focus-visible{outline:2px solid var(--green);outline-offset:3px}select{max-width:100%;flex:1}button:disabled{opacity:.45;cursor:default}.screen{background:#171e1b;color:#e7ece5;border-radius:10px;overflow:auto;min-height:380px;padding:18px}.terminal-grid{font:14px/1.44 "SFMono-Regular",Consolas,monospace;font-variant-ligatures:none}.termrow{display:flex;white-space:pre;height:1.44em}.cell{flex:none;white-space:pre;display:inline-block}pre{font:14px/1.44 "SFMono-Regular",Consolas,"Noto Sans Mono CJK SC",monospace;white-space:pre;margin:0;font-variant-ligatures:none}.meta{display:flex;justify-content:space-between;gap:16px;margin:10px 0;font:12px/1.5 ui-monospace,monospace;color:var(--muted)}.cards{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:22px}.comparison>div,.card{min-width:0}.card{border-top:2px solid var(--green);padding-top:16px}.card p{color:#57635b;font-size:15px}table{width:100%;border-collapse:collapse;margin:16px 0;text-align:left}th,td{padding:12px;border-bottom:1px solid var(--line)}th{font-size:13px;color:var(--muted)}.links{display:flex;gap:16px;flex-wrap:wrap}.comparison{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:16px}iframe{width:100%;height:390px;border:1px solid var(--line);border-radius:8px;background:#fff}code{font:14px/1.6 ui-monospace,monospace;background:#e8ece2;padding:2px 5px;border-radius:3px}.install{background:#eaf0e5;padding:20px;border-radius:8px}.install pre{white-space:pre-wrap;font-size:16px}footer{margin-top:64px;border-top:1px solid var(--line);padding-top:20px;font-size:13px;color:var(--muted)}@media(max-width:740px){main{padding:30px 18px}.cards,.comparison{grid-template-columns:1fr}.screen{padding:10px}.facts{gap:22px}.controls select{flex-basis:100%}}
</style>
<main>
<header><div class="eyebrow">BONE / IMPLEMENTATION 2026.10.02</div><h1>从工作段落<br>读懂 Agent 的行动。</h1><p class="intro">0.7 的生产 TUI 已重做。这里展示实际可执行程序在 PTY 中运行的软件修复、插话、提问、回复与交付；屏幕来自终端字节重放。</p></header>
<div class="facts"><div><strong>__VERSION__</strong><span>本机安装版本</span></div><div><strong>__LIVE__ · 12 项</strong><span>真实 6-luna 修复 / 独立原测试</span></div><div><strong>__CALLS__ 次 / __TOOLS__ 次</strong><span>模型调用 / 工具调用 · 费用未知</span></div><div><strong>__RUST__ 项</strong><span>Rust 自动验收</span></div></div>
<div class="install"><pre>bone --profile subscription --model chatgpt:gpt-6-luna tui --workspace /path/to/project</pre><p class="note">在目标项目目录也可直接运行 bone。F6 进入阅读，d 展开完整结果，D 看审计；Ctrl+C 暂停，Ctrl+R 恢复；Esc 返回上一层。</p></div>
<section aria-labelledby="real-title"><h2 id="real-title">一段真实的软件工作</h2><p>一个临时 Git 仓库里，过期时间判断写反了。Agent 先跑原有测试；运行中收到“先解释，不要修改”，于是提问。明确回答后只修改 auth.py，再运行原测试；测试文件和 Git 提交未改。</p>
<div class="controls"><button id="prev" aria-label="上一屏">← 上一屏</button><select id="step" aria-label="执行阶段"></select><button id="next" aria-label="下一屏">下一屏 →</button></div><div class="screen"><div id="terminal" class="terminal-grid" role="img" aria-label="当前阶段终端屏幕" aria-live="polite"></div></div><div class="meta"><span id="stage-meta"></span><span>生产 TUI · 现有订阅 · gpt-6-luna</span></div><p class="note">80×24 与 120×40 均实际缩放；页面保留终端文本，未重建 ANSI 颜色。这个任务说明交互链路可用，不代表大型工程任务成功率。</p><div class="links"><a href="live-software/report.html">完整对话、行动与 Job 记录</a><a href="live-software/final.diff">实际源码修改</a><a href="live-software/independent-tests.txt">独立测试输出</a><a href="live-software/run.py">重现脚本</a></div></section>
<section><h2>设计改动如何落实</h2><div class="cards"><div class="card"><h3>正文优先，证据逐层展开</h3><p>成功工具一行；实时 stdout/stderr 短预览；失败靠前显示退出码和明确错误。完整结果先于审计参数。模型、用量在会话详情查看。</p><a href="pty/80x24/tool-density.html">失败与成功的实际区别 →</a></div><div class="card"><h3>输入目标清楚，草稿不被夺走</h3><p>新问题只提醒。明确选择问题才回复；切回新要求用 /message。各目标保留文字、光标与选区；核查用独立表单。</p><a href="pty/80x24/reconcile-reply.html">核查返回原回复草稿 →</a></div><div class="card"><h3>阅读位置保持，返回有层次</h3><p>新记录不抢走旧阅读位置。长交付可完整滚动；缩放按原文位置恢复。/delivery 直达最近交付，Esc 一层层返回。</p><a href="pty/120x40/reader-delivery.html">长交付与缩放 →</a></div></div></section>
<section><h2>终端失败状态：改前与改后</h2><p class="note">同一类本地 HTTP 422 fixture。0.6 的“就绪”会掩盖失败；0.7 将失败作为持续事实，操作提示有独立生命周期。两者均在退出之前记录。</p><div class="comparison"><div><h3>0.6 / 安装旧版</h3><div class="screen"><div class="terminal-grid">__OLD__</div></div><a href="before/failure-0.6.html">完整旧版记录</a></div><div><h3>0.7 / 生产新版</h3><div class="screen"><div class="terminal-grid">__NEW__</div></div><a href="pty/80x24/failure.html">完整新版记录</a></div></div></section>
<section><h2>独立验收与范围</h2><table><thead><tr><th>实际尺寸</th><th>通过场景</th><th>记录</th></tr></thead><tbody>__PTY__</tbody></table><p>覆盖编辑、补全、显式问答、多 Job 等待、并发插话、旧结果拒绝、实际 Shell 中断、未知写核查、长历史搜索、阅读回退、外部编辑器和终端恢复。协议由本地 fixture 提供；工作区工具和终端进程实际执行。</p><p class="note">首轮独立审查暴露了退出码漏显示、错误摘要误选、/latest 丢光标和审计返回漂移，已补回归。屏幕重放的宽字符覆盖与 resize 通知也修正了。模型流预览转为最终交付时尚不保证原阅读锚点连续，/delivery 可直达持久结果；复杂 Markdown 锚点仍为近似。非当前目标草稿只保存在本次进程内。Linux 原生剪贴板与不同终端配色尚未逐项实机验证。</p><div class="links"><a href="independent-acceptance.md">独立验收结论</a><a href="gates.json">编译、检查及安装门禁</a><a href="../../tui.md">使用说明</a><a href="../../design-lab/2026-10-02/index.html">设计取舍来源</a></div></section>
<footer>实现保持 Ratatui / Crossterm / ratatui-textarea / tui-markdown；一个 Session、一个 Agent，Job 仍为内部工作单元。未新增模型调用包装或 UI 执行内核。__PROVENANCE__</footer>
</main><script>
const frames=__FRAMES__;const select=document.querySelector('#step');let index=0;frames.forEach((f,i)=>{const o=document.createElement('option');o.value=i;o.textContent=f.step;select.append(o)});function show(i){if(!frames.length)return;index=Math.max(0,Math.min(i,frames.length-1));select.value=index;const f=frames[index];const terminal=document.querySelector('#terminal');terminal.replaceChildren();for(const cells of f.cells){const row=document.createElement('div');row.className='termrow';for(const [text,width] of cells){const span=document.createElement('span');span.className='cell';span.style.width=width+'ch';span.textContent=text;row.append(span)}terminal.append(row)}document.querySelector('#stage-meta').textContent=`${index+1} / ${frames.length} · ${f.size?f.size[1]+'×'+f.size[0]:'真实终端'} · ${f.events} durable events`;document.querySelector('#prev').disabled=index===0;document.querySelector('#next').disabled=index===frames.length-1}select.addEventListener('change',()=>show(Number(select.value)));document.querySelector('#prev').addEventListener('click',()=>show(index-1));document.querySelector('#next').addEventListener('click',()=>show(index+1));document.addEventListener('keydown',e=>{if(e.target.tagName==='SELECT')return;if(e.key==='ArrowLeft'){show(index-1);e.preventDefault()}if(e.key==='ArrowRight'){show(index+1);e.preventDefault()}});show(4);
</script></html>'''
replacements = {
    '__VERSION__': html.escape(gates.get('installed_version', '等待最终安装')),
    '__LIVE__': html.escape(live.get('status', '待验收')),
    '__CALLS__': str(live.get('model_calls', '?')),
    '__TOOLS__': str(live.get('tool_calls', '?')),
    '__RUST__': str(gates.get('rust_tests', '?')),
    '__PTY__': ''.join(rows),
    '__OLD__': grid_html(old_frame),
    '__NEW__': grid_html(new_frame),
    '__PROVENANCE__': html.escape('源码指纹 ' + gates.get('source_sha256', '待最终编译')[:12]),
    '__FRAMES__': embedded(frames),
}
for token,value in replacements.items(): body=body.replace(token,value)
(HERE/'overview.html').write_text(body)
# Keep terminal padding intact without literal trailing whitespace in HTML sources.
for page in HERE.rglob('*.html'):
    source = page.read_text()
    source = re.sub(r'(<pre(?:\s[^>]*)?>)([\s\S]*?)(</pre>)', lambda match: match[1]+match[2].replace('\n', '&#10;')+match[3], source)
    page.write_text(source.rstrip()+'\n')
