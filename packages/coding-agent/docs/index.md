# Bone Documentation

Bone is a local coding agent with built-in tools, provider integrations, conversations, and local customization resources.

## Install

```bash
npm install -g --ignore-scripts @frelion/bone-coding-agent
bone
```

Authenticate with `/login` or configure an API key before starting Bone.

## Guides

- [Quickstart](quickstart.md) - install, authenticate, and start a conversation
- [Using Bone](usage.md) - interactive mode and CLI reference
- [Settings](settings.md) - global and project configuration
- [Skills](skills.md) - local skill directories and format
- [Prompt Templates](prompt-templates.md) - local prompt resources
- [Themes](themes.md) - local theme resources
- [Providers](providers.md) - model authentication and configuration
- [Sessions](sessions.md) - stored conversations and branches
- [RPC](rpc.md) - process integration

## Resource boundary

Bone supports local skills, prompts, and themes. It does not provide a third-party extension marketplace or package installer. Pi extension packages, Pi manifests, Pi SDK imports, npm/git packages, and extension-path discovery are unsupported. Old package files remain untouched and inert.
