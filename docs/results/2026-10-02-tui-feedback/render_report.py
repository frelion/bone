#!/usr/bin/env python3
"""Browse recorded production terminal frames; never generate synthetic screens."""
import html
import json
from pathlib import Path
import re
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[2] / 'tests'))
from tui_pty import styled_frame


def read(path):
    return json.loads((HERE / path).read_text())


def panel(title, frames, opened=None):
    return '<h2>' + html.escape(title) + '</h2>' + ''.join(
        ('<details open>' if index == opened else '<details>') + '<summary>' + html.escape(frame['step']) + '</summary><pre>'
        + styled_frame(frame) + '</pre><p class="note">'
        + html.escape(f"{frame['size'][1]}×{frame['size'][0]} · cursor {frame.get('cursor')} · visible {frame.get('cursor_visible')} · running {frame.get('running_jobs')} · paused {frame.get('paused')}")
        + '</p></details>' for index, frame in enumerate(frames))


gates = read('gates.json')
sections = []
for size in ('80x24', '120x40'):
    for variant, label in [('', '0.7.1 改后'), ('before', '0.7.0 改前')]:
        path = Path('pty') / variant / size / 'feedback-flow.json'
        if (HERE / path).exists():
            result = read(path)
            # Keep this overview short; complete recordings stay in the linked case.
            indices = [8] if variant else ([1, 5, 8, 10] if size == '80x24' else [1, 8])
            frames = [result['frames'][index] for index in indices]
            sections.append(panel(f'{label} / {size}', frames, 2 if variant == '' and size == '80x24' else None)
                            + '<p><a href="' + str(path.with_suffix('.html')) + '">完整逐屏记录 →</a></p>')
colored = Path('pty/color/80x24/feedback-flow.json')
if (HERE / colored).exists():
    frames = read(colored)['frames']
    sections.insert(0, panel('0.7.1 / 80×24 / 启用颜色', [frames[1], frames[8]], 1)
                    + '<p><a href="' + str(colored.with_suffix('.html')) + '">颜色模式完整过程 →</a></p>')
live = read('live-software/summary.json')
sections.append(panel('安装版本 · 真实 gpt-6-luna 软件修复', read('live-software/screens.json')))
body = '''<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>BONE 0.7.1 · 输入与运行反馈</title><style>
*{box-sizing:border-box}body{margin:0;background:#f4f5ee;color:#25302a;font:16px/1.65 system-ui}main{max-width:1150px;margin:40px auto;padding:20px}h1{font-size:38px;letter-spacing:-.04em}h2{margin-top:42px}a{color:#286847}details{border-top:1px solid #ced7cb;padding:12px 0}summary{cursor:pointer}pre{background:#18232b;color:#e5edf2;padding:14px;overflow:auto;font:13px/1.35 "SFMono-Regular",Menlo,Consolas,monospace;font-variant-ligatures:none}pre>span{font-family:inherit!important}.note{font-size:13px;color:#657068}code{font:14px monospace}li{margin:6px 0}.facts{padding:16px;background:#e6ecdf}details[open] summary{margin-bottom:12px}
</style><main><h1>输入看得见，运行看得清。</h1>
<p>本轮针对实际反馈修正生产 TUI。下面的屏幕来自终端进程的 VT 格与样式记录，含真实光标位置；浏览器调色板近似终端颜色。</p>
<ul><li>默认正文去掉“你 / Agent”标题，原话用左侧细竖线区分。</li><li>输入框有稳定边界与真实终端光标，阅读和弹层隐藏光标。</li><li>动作、耗时和变化中的标记固定在输入框上方；发送回执独立显示。</li><li>后台仍在执行时，旧交付不再遮住运行；等待回复不显示执行动画。</li><li>修正运行中短通知不消失的问题。</li></ul>
<p>另外修正无颜色模式下选区不可见、缩小窗口再恢复后首行暂时藏住的问题，<a href="detail-review.md">独立复验记录</a>保留具体按键与结果。</p>
<p class="facts">__FACTS__</p><p>在项目目录运行 <code>bone</code>。F6 切换编辑与阅读；Ctrl+C 暂停，Ctrl+R 恢复。</p>
<p><a href="acceptance-results.md">独立终端验收</a> · <a href="gates.json">构建与安装记录</a> · <a href="live-software/report.html">真实模型的对话、Job 与工具记录</a> · <a href="live-software/final.diff">实际修改</a></p>
<p class="note">本地终端场景：先编辑长中文草稿、观察模型流；旧任务在后台执行静默命令时，发一个独立新要求并得到交付；旧命令仍在执行，最后暂停并保留未知写核查。真实模型的软件修复记录在下方。</p>__SCREENS__
<p class="note">本地协议场景验证界面和执行状态；真实订阅场景在隔离 Python 仓库完成修复。复杂 Markdown 锚点仍为近似，模型预览转为持久交付时尚不保证阅读锚点连续。</p></main></html>'''
facts = f"{gates['installed_version']} · Rust {gates['rust_tests']} 项通过 · PTY {gates['pty_passed']} 个场景通过 · 真实模型 {live['status']} / {live['model_calls']} 次调用 / 12 项独立原测试，费用未知"
(HERE / 'overview.html').write_text(body.replace('__FACTS__', html.escape(facts)).replace('__SCREENS__', ''.join(sections)) + '\n')
# Preserve terminal padding without literal trailing spaces in saved HTML.
for page in HERE.rglob('*.html'):
    text = re.sub(r'(<pre(?:\s[^>]*)?>)([\s\S]*?)(</pre>)', lambda m: m[1] + m[2].replace('\n', '&#10;') + m[3], page.read_text())
    page.write_text(text.rstrip() + '\n')
