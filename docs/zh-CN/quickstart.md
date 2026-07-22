# 快速开始

[English](../quickstart.md) | **简体中文**

## 1. 安装 Bone

从 [GitHub Releases](https://github.com/frelion/bone/releases) 下载对应平台的
archive，解压后将 `bone` 放进 `PATH`，然后验证：

```bash
bone --version
```

开发构建可使用自包含本地包：

```bash
npm run pack:bone
bun add --global artifacts/frelion-bone-coding-agent-*.tgz
```

通过 package 或源码运行 Bone 时需要 Bun 1.3.14 或更高版本。GitHub Release
archive 内是独立 Bun 可执行文件，不要求用户另行安装 Bun。

## 2. 配置 Provider

进入你准备工作的目录并启动：

```bash
bone
```

打开 `/settings`，选择 **Providers & Models**。创建 Provider 后，在同一张表单里
配置 Base URL、协议、API Key 或 OAuth，以及至少一个模型。`Ctrl+S` 会保存草稿但
不会关闭 settings overlay。

Provider credential 仅存于全局 `~/.bone/agent/auth.json`；项目级设置可以引用
Provider，但不会保存 secret。

## 3. 开始工作

在对话区输入任务。使用 `Shift+Left` / `Shift+Right` 在对话和 Side 之间移动焦点；
焦点位于 Side 时，使用 `↑` / `↓` 选择会话、`Enter` 打开会话。

完整交互请见[会话与 Side](sessions.md)。
