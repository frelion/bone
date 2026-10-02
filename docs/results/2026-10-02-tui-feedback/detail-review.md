# 输入细节独立复验

日期：2026-10-02。使用真实 PTY、现有 `tests/tui_pty.py` Fixture 和空 scripted turns；两项均未发起模型请求。只复验此前独立审查确认的问题，没有修改生产代码或测试。

最终 debug binary SHA-256：`06b369416b51bffa2944ce21fc080d39b68a3402dcc870f63d50ae752e6f18fb`。

| 项目 | 原复现 | 最终结果 |
| --- | --- | --- |
| NO_COLOR 选区 | `NO_COLOR=1`，80×24，输入 `首中文字abc`，Shift+Left 两次；选择 `bc`，但 `c` 没有可见样式 | 通过：`c` 保留 underline；硬件 cursor 可见，位于 `b`，零基坐标 `(20,10)`；Ctrl+Y 实际复制 `bc` |
| 缩放恢复 | 80×24 粘贴 `首中文字abc\n末尾`，缩到 12×10 再恢复；原首行隐藏，末尾误占首输入行 | 通过：首行与末尾均完整显示，恢复前后输入框内容一致；硬件 cursor 可见并回到第二行末尾 `(20,5)`；Ctrl+Q 保存原始完整两行文字 |

## 复验脚本

在仓库根目录运行；Fixture 仅使用临时目录、合成凭证与 loopback 服务。

```python
import hashlib, importlib.util, os
from pathlib import Path

binary = Path("target/debug/bone").resolve()
assert hashlib.sha256(binary.read_bytes()).hexdigest() == (
    "06b369416b51bffa2944ce21fc080d39b68a3402dcc870f63d50ae752e6f18fb"
)
spec = importlib.util.spec_from_file_location("review", "tests/tui_pty.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
original_no_color = os.environ.get("NO_COLOR")
try:
    os.environ["NO_COLOR"] = "1"
    fixture = module.Fixture(binary, [], size=(24, 80))
    try:
        fixture.pump(.6)
        fixture.send("首中文字abc")
        fixture.pump(.3)
        fixture.send(b"\x1b[1;2D\x1b[1;2D")
        fixture.pump(.3)
        fixture.screen()
        selected_c = next(run for run in fixture.cell_styles
            if run["row"] == 20 and run["start"] <= 11 < run["end"])
        assert "underline" in selected_c["attributes"]
        assert fixture.cursor_visible and fixture.terminal_cursor == (20, 10)
        fixture.send(b"\x19")
        fixture.wait(lambda: fixture.clipboard.exists(), "clipboard", timeout=5)
        assert fixture.clipboard.read_text() == "bc"
        assert len(fixture.calls()) == 0
        fixture.quit()
    finally:
        fixture.close()

    os.environ.pop("NO_COLOR", None)
    fixture = module.Fixture(binary, [], size=(24, 80))
    try:
        draft = "首中文字abc\n末尾"
        fixture.pump(.6)
        fixture.send(b"\x1b[200~" + draft.encode() + b"\x1b[201~")
        fixture.pump(.3)
        before = fixture.screen().splitlines()[-6:]
        assert before[1].startswith("│首中文字abc")
        assert before[2].startswith("│末尾")
        fixture.resize(10, 12)
        fixture.pump(.3)
        fixture.resize(24, 80)
        fixture.pump(.3)
        after = fixture.screen().splitlines()[-6:]
        assert after == before
        assert fixture.cursor_visible and fixture.terminal_cursor == (20, 5)
        assert len(fixture.calls()) == 0
        fixture.quit()
        assert any(item.get("draft") == draft for item in fixture.saved_drafts())
    finally:
        fixture.close()
finally:
    if original_no_color is None:
        os.environ.pop("NO_COLOR", None)
    else:
        os.environ["NO_COLOR"] = original_no_color
```
