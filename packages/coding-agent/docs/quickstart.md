# Quickstart

Bone is a local coding agent with built-in tools and local customization resources.

## Install

Download the standalone executable for your platform from
[GitHub Releases](https://github.com/frelion/bone/releases). Package installs,
when available, require Bun 1.3.14 or newer. Node.js is not a supported CLI
runtime.

Then start Bone in the project directory:

```bash
cd /path/to/project
bone
```

Removing Bone leaves settings, credentials, and sessions in `~/.bone/agent/`. Legacy package directories such as `~/.bone/agent/npm/` and `.bone/npm/` are also left untouched.

## Authenticate

Run `/login` to configure a subscription or stored API-key provider. You can also set an API key before starting Bone:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
bone
```

See [Providers](providers.md) for supported providers and environment variables.

## First session

Ask Bone to inspect the current project:

```text
Summarize this repository and tell me how to run its checks.
```

Bone provides `read`, `write`, `edit`, and `bash` by default. The read-only `grep`, `find`, and `ls` tools can be enabled through tool options.

## Project instructions

Bone reads `AGENTS.md` or `CLAUDE.md` from `~/.bone/agent/`, parent directories, and the current directory. Add an `AGENTS.md` file for project conventions:

```markdown
# Project Instructions

- Run `npm run check` after code changes.
- Do not run production migrations locally.
- Keep responses concise.
```

Restart Bone or run `/reload` after changing context files.

## Local resources

Bone loads local resources from these directories:

- `~/.bone/agent/skills/`, `~/.bone/agent/prompts/`, and `~/.bone/agent/themes/`
- `.bone/skills/`, `.bone/prompts/`, and `.bone/themes/` in a trusted project
- local paths in the `skills`, `prompts`, and `themes` settings arrays

Use `--skill`, `--prompt-template`, or `--theme` for a temporary local path. See [Skills](skills.md), [Prompt Templates](prompt-templates.md), and [Themes](themes.md).

Bone does not install or discover third-party extensions. Pi extension packages, `package.json#pi` manifests, Pi SDK imports, npm/git resource packages, and legacy extension directories are unsupported. Existing files remain on disk but are inert.

## Common commands

```bash
bone @README.md "Summarize this"
bone -p "Summarize this codebase"
bone -c
bone --name "release audit" -p "Audit this repository"
bone update
```

`bone update` updates Bone itself only. `install`, `remove`, `uninstall`, `list`, and `config` are not available.

## Next steps

- [Using Bone](usage.md) for the CLI and interactive workflow
- [Settings](settings.md) for global and project configuration
- [Providers](providers.md) for authentication and model setup
- [Skills](skills.md), [Prompt Templates](prompt-templates.md), and [Themes](themes.md) for local customization
