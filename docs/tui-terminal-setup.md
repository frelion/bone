# BONE 终端字体与可读性

适用于 Windows / WSL、macOS 和 Linux。2026-09-11。本文中的字体选择只是一项可选的个人偏好，不是 BONE 的运行前提或跨平台修复步骤。

BONE 使用终端字符网格。应用负责文字内容、颜色、字重、内边距和窗口缩放后的布局；宿主终端负责字体、实际字号、抗锯齿和屏幕缩放。进入 alternate screen 不会创建独立字体环境。终端没有普遍支持的设置字体与字号的控制序列，BONE 不向用户的终端发送更换字体指令，也不把修改终端全局设置作为启动前提。BONE 不读取或写入 Windows Terminal settings、终端 profile、shell rc、tmux 配置或字体配置。跨机器的一致性来自固定 Cell 布局、显式颜色、语义字重、能力档位和可重复测试；字体像素仍由显示窗口决定。

WSL 中运行的是 Linux 进程，但 Windows 终端窗口通常使用 Windows 字体；只把字体装进 WSL 并不能改变 Windows Terminal 的字形。如果通过 Linux 图形终端运行，则配置那个图形终端。SSH 同样由本地显示窗口控制字体。

## 推荐的共同基线

- 简体中文和代码混排：Sarasa Fixed SC（更纱黑体），Regular；安装对应的常规与粗体字体。官方 Fixed 系列采用等宽拉丁字形、无连字，SC 对应简体中文。
- 从 **16 pt** 开始调节实际终端字号。它是建议起点，不是应用强制值；不同屏幕、DPI 和终端下仍需看实际效果。字体菜单中的 px 和 pt 不能直接按同一数值比较。
- 保持终端默认字宽与行距，先检查常规字重的效果。横向字符拉伸会使对齐边界和正文分离。
- 确认字体真实安装并被选中。不存在的字体名称会导致回退，设置文件里写了名字并不证明生效。

字体下载与说明：[Sarasa Gothic 官方项目](https://github.com/be5invis/Sarasa-Gothic)。字体属于宿主环境依赖，不打包进 Ratatui 应用冒充字体支持。

## Windows / WSL

在 Windows 侧安装字体。Windows Terminal → 设置 → 当前 WSL 配置 → 外观，选择 Sarasa Fixed SC、字号 16、常规字重。

也可以将以下片段合并进当前 profile，保留原有的 guid、name、commandline 等字段。不要用它覆盖整个 settings.json：

```json
{
  "font": {
    "face": "Sarasa Fixed SC",
    "size": 16,
    "weight": "normal"
  }
}
```

字体、字号及字重字段依据：[Windows Terminal 官方外观配置](https://learn.microsoft.com/zh-cn/windows/terminal/customize-settings/profile-appearance)。其他 Windows 终端应在其自身的字体设置里使用同一基线。

## macOS

安装字体后，Terminal → Settings → Profiles → 当前 profile → Text → Font / Change，选择 Sarasa Fixed SC Regular、16 pt。修改作用于所选 profile。

依据：[Apple Terminal 字体设置](https://support.apple.com/guide/terminal/trmltxt/mac)。其他 macOS 终端在其 profile 的字体设置中选择同一字体与字号。

## Linux

在显示窗口所在的 Linux 系统中安装字体，然后通过终端的字体选择器选用它。

GNOME Terminal：Preferences → 当前 Profile → Text → Custom font，选择 Sarasa Fixed SC Regular、字号 16。依据：[GNOME Terminal 字体设置](https://help.gnome.org/gnome-terminal/app-fonts.html)。

kitty 可以通过 `kitten choose-fonts` 预览并选择已安装字体，再把 `font_size 16.0` 写入 kitty.conf。依据：[kitty 字体设置](https://sw.kovidgoyal.net/kitty/conf/#fonts)。不直接复制未经本机验证的字体内部名称。

## 应用侧可读性与验证边界

BONE 的正文使用 #eeeeee，次要文字 #aeaeae；普通会话标题保持正文亮度，选中项通过背景、语义 label 粗度和竖向标记区分。正文、长消息和 metadata 保持 regular；主阅读文字不使用 dim 或 italic 修饰。输入、用户消息和选中项共用四分之一格标记；左右栏界以整列背景色绘制，不依赖字形，避免字体行距带来的断缝。编辑位置另有应用绘制的 accent Cell，即使宿主 caret 很细或正处于闪烁熄灭阶段也保持可见；原生 caret 仍停在同一 Cell，BONE 不改它的宿主样式。默认输入正文两行，长内容继续增长。

放大终端字体会减少可用列数与行数；BONE 根据实际字符网格切换布局，不按操作系统名称猜测屏幕空间。

本轮已在 Linux / WSL 开发环境运行 TUI 测试和构建；Linux PTY 自动化覆盖键盘能力支持/降级、终端模式恢复和禁止输出检查。macOS 与原生 Windows 的视觉、物理快捷键、IME 和生命周期仍需 CI 与真机认证。上述字体步骤只供用户自愿调整，BONE 不执行这些步骤；跨平台字体说明不等于多平台视觉验收通过。
