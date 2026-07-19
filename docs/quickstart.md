# Quickstart

**English** | [简体中文](zh-CN/quickstart.md)

## 1. Install Bone

Download the archive for your platform from [GitHub Releases](https://github.com/frelion/bone/releases),
extract it, and place the `bone` executable on your `PATH`. Verify the install:

```bash
bone --version
```

For development builds, create the self-contained local package and install it:

```bash
npm run pack:bone
npm install --global --force --ignore-scripts artifacts/earendil-works-pi-coding-agent-<version>.tgz
```

## 2. Configure a provider

Start Bone in the directory where you want to work:

```bash
bone
```

Open `/settings`, then select **Providers & Models**. Add a provider, configure
its Base URL, protocol, API key or OAuth authentication, and at least one model.
`Ctrl+S` saves the draft without closing the settings overlay.

Provider credentials live only in global `~/.bone/agent/auth.json`; project
settings can reference providers but never contain secrets.

## 3. Start working

Send a prompt in the conversation pane. Use `Shift+Left` and `Shift+Right` to
move focus between the conversation and the Side panel. In Side focus, use
`↑`/`↓` to select a conversation and `Enter` to open it.

See [Sessions](sessions.md) for the complete session interaction model.
